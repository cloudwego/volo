"""离线验证 fixture 证据，不启动 socket 或修改共享 runner。"""
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

from kitex_streaming_interop import inherited
from kitex_streaming_interop.metadata_wire import validate_request_header


OFFSETS = {'Plain': 10000, 'Unary': 20000, 'Upload': 30000,
           'Watch': 40000, 'Chat': 50000, 'WatchResult': 60000}


def request_header():
    values = {'3': 'upstream.psm', '4': 'source-cluster', '5': 'source-idc',
              '6': 'Interop', '7': 'destination-cluster', '8': 'destination-idc',
              '9': 'Bidi', '27': '2'}
    integers = {key: value.encode().hex() for key, value in values.items()}
    integers['777'] = 'ff00726177'
    return {'raw_metadata': {'int_hex': integers,
                            'string_hex': {'isn'.encode().hex(): 'Interop'.encode().hex(),
                                           'unknown-field'.encode().hex(): 'ff0076616c7565'}},
            # Keep both values in unrelated raw bytes: that alone must never pass.
            'hex': 'ff00726177ff0076616c7565'}


class FixtureEvidenceTests(unittest.TestCase):
    def test_correct_raw_header_and_unrelated_extension_are_accepted(self):
        header = request_header()
        header['raw_metadata']['int_hex']['999'] = '657874656e73696f6e'
        validate_request_header(header)

    def test_unknown_integer_key_swap_or_value_damage_is_rejected(self):
        header = request_header()
        header['raw_metadata']['int_hex']['778'] = header['raw_metadata']['int_hex'].pop('777')
        with self.assertRaises(ValueError):
            validate_request_header(header)
        header = request_header()
        header['raw_metadata']['int_hex']['777'] = 'efbfbd00726177'
        with self.assertRaises(ValueError):
            validate_request_header(header)

    def test_unknown_string_key_swap_or_value_damage_is_rejected(self):
        header = request_header()
        strings = header['raw_metadata']['string_hex']
        strings['other-field'.encode().hex()] = strings.pop('unknown-field'.encode().hex())
        with self.assertRaises(ValueError):
            validate_request_header(header)
        header = request_header()
        header['raw_metadata']['string_hex']['unknown-field'.encode().hex()] = 'efbfbd0076616c7565'
        with self.assertRaises(ValueError):
            validate_request_header(header)

    def test_missing_raw_evidence_and_deadline_headers_are_rejected(self):
        header = request_header()
        del header['raw_metadata']
        with self.assertRaises(ValueError):
            validate_request_header(header)
        for key in ('12', '13', '17'):
            with self.subTest(key=key):
                header = request_header()
                header['raw_metadata']['int_hex'][key] = '31303030'
                with self.assertRaises(ValueError):
                    validate_request_header(header)

    def replay_inherited(self, swapped):
        # Reproduce server-selected methods independently of expected_calls().
        # Return the real fixture's method-specific offset; client reports the
        # requested name, so only DATA can disambiguate the original blind spot.
        servers = []

        class FakeServer:
            def __init__(self, *args, **kwargs):
                self.log = kwargs['log']
                self.calls = 0
                servers.append(self)

            def start(self, timeout):
                self.log.write_text('')
                return {'api': 'inherited', 'service': 'Child', 'profile': 'test',
                        'addr': 'tcp://127.0.0.1:1234'}

            def close(self):
                pass

        def fake_run(command, **kwargs):
            count = int(command[command.index('--messages') + 1])
            server = servers[-1]
            calls = []
            for requested in OFFSETS:
                selected = swapped.get(requested, requested)
                server.calls += 1
                with server.log.open('a') as log:
                    log.write(json.dumps({'event': 'handler', 'method': selected,
                                          'call_index': server.calls}) + '\n')
                values = [count] if requested in ('Plain', 'Unary', 'Upload') else range(count)
                calls.append({'method': requested,
                              'responses': [OFFSETS[selected] + value for value in values]})
            kwargs['log'].write_text(json.dumps({'event': 'result', 'ok': True, 'api': 'inherited',
                                                'service': 'Child', 'profile': 'test', 'calls': calls}) + '\n')
            return {'returncode': 0, 'timed_out': False}

        with tempfile.TemporaryDirectory(prefix='ttstream-fixture-audit-') as temporary:
            root = Path(temporary)
            go, rust = root / 'go', root / 'rust'
            go.write_text('offline Go observation')
            rust.write_text('offline Rust observation')
            with patch.object(inherited, 'Server', FakeServer), patch.object(inherited, 'run', fake_run):
                return inherited.inherited_service(go=go, rust=rust, output=root / 'report',
                    transports=['tcp'], timeout=1, profile='test')

    def test_correct_six_method_observations_are_accepted(self):
        report = self.replay_inherited({})
        self.assertEqual(len(report['cases']), 20)
        self.assertTrue(all(case['status'] == 'PASS' for case in report['cases']), report)

    def test_plain_unary_dispatch_swap_is_rejected(self):
        report = self.replay_inherited({'Plain': 'Unary', 'Unary': 'Plain'})
        self.assertEqual(len(report['cases']), 20)
        self.assertTrue(all(case['status'] == 'FAIL' for case in report['cases']), report)
        self.assertTrue(all(case['reason'] == 'actual inherited method responses differ'
                            for case in report['cases'] if case['name'] != 'shared-handler'))

    def test_watch_watchresult_dispatch_swap_is_rejected_for_nonempty_streams(self):
        report = self.replay_inherited({'Watch': 'WatchResult', 'WatchResult': 'Watch'})
        clients = [case for case in report['cases'] if case['name'] != 'shared-handler']
        self.assertEqual(sum(case['status'] == 'FAIL' for case in clients), 12)
        self.assertTrue(all(case['status'] == 'FAIL' for case in clients
                            if not case['name'].endswith('-0')))
        # Empty streams have no response payload to discriminate a method; the
        # complete unchanged matrix must still reject this routing mutation.
        self.assertEqual(sum(case['status'] == 'FAIL' for case in report['cases']), 14)


if __name__ == '__main__':
    unittest.main()
