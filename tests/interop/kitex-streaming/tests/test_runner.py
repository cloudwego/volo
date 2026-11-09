import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

from kitex_streaming_interop.network import basic, expected, result_event
from kitex_streaming_interop.process import ProcessFailure, Server, run
from kitex_streaming_interop.wire import validate
from kitex_streaming_interop.metainfo import seed_for, validate as validate_metainfo
from kitex_streaming_interop.cascade import validate_client, validate_server
from kitex_streaming_interop.status import validate_terminal
from kitex_streaming_interop.responses import validate_response
from kitex_streaming_interop.routing import validate_route


class RunnerTest(unittest.TestCase):
    def test_routing_recovery_rejects_late_terminal_from_previous_stream(self):
        actual = {'frames': [{'sid': 1, 'flags': 2, 'int_metadata': {'27': '5'},
                              'exception': {'code': 6}}]}
        with self.assertRaisesRegex(ValueError, 'foreign stream'):
            validate_route(actual, 2, 'A', 'OnlyA', 'A', 'probe', 'rust')

    def test_routing_missing_method_rust_enhancement_cannot_be_silent(self):
        value = {'timed_out': True, 'frames': []}
        validate_route(value, 1, 'Wrong', 'Absent', None, 'request', 'go')
        with self.assertRaisesRegex(ValueError, 'silence differs'):
            validate_route(value, 1, 'Wrong', 'Absent', None, 'request', 'rust')

    def test_response_observer_cannot_count_send_failure_as_received_error(self):
        value = {'ok': True, 'api': 'response-probe', 'stage': 'send', 'responses': [],
                 'open_elapsed_seconds': 0.01, 'terminal': {'kind': 'unknown', 'kitex_code': 12004}}
        with self.assertRaisesRegex(ValueError, 'before receiving'):
            validate_response(value, [], 'unknown', 12004, 'rust')

    def test_response_compatibility_requires_tolerated_data_and_real_application(self):
        value = {'ok': True, 'api': 'response-probe', 'responses': [], 'open_elapsed_seconds': 0.01,
                 'terminal': {'kind': 'ok', 'kitex_code': 0}}
        with self.assertRaisesRegex(ValueError, 'decoded DATA differs'):
            validate_response(value, [{'value': 'response-probe', 'seq': 1}], 'ok', 0, 'rust')
        value['terminal'] = {'kind': 'application', 'kitex_code': 12001, 'code': None,
                             'error': 'response-terminal'}
        validate_response(value, [], 'application', 12001, 'go')
        with self.assertRaisesRegex(ValueError, 'application payload differs'):
            validate_response(value, [], 'application', 12001, 'rust')

    def test_response_eof_without_exception_code_does_not_relax_error_codes(self):
        value = {'ok': True, 'api': 'response-probe', 'responses': [], 'open_elapsed_seconds': 0.01,
                 'terminal': {'kind': 'ok', 'code': 0, 'kitex_code': None, 'error': ''}}
        validate_response(value, [], 'ok', 0, 'rust')
        value['terminal'] = {'kind': 'canceled', 'code': 13, 'kitex_code': 13, 'error': 'rst'}
        validate_response(value, [], 'unknown', 13, 'rust')
        value['terminal']['kitex_code'] = None
        with self.assertRaisesRegex(ValueError, 'terminal differs'):
            validate_response(value, [], 'unknown', 13, 'rust')

    def test_go_private_application_code_cannot_be_fabricated(self):
        value = {'kind': 'application', 'code': None, 'kitex_code': 12001,
                 'application_code_available': False, 'error': 'application: fixture-terminal'}
        validate_terminal(value, 'application', 'go', 'rust')
        with self.assertRaisesRegex(ValueError, 'private application cause'):
            validate_terminal(dict(value, code=6), 'application', 'go', 'rust')
        with self.assertRaisesRegex(ValueError, 'code differs'):
            validate_terminal(value, 'application', 'rust', 'rust')

    def test_go_biz_observation_does_not_relax_rust_ordering(self):
        value = {'kind': 'biz', 'code': 4123, 'message': 'fixture-terminal',
                 'extra': {'detail': 'interop-中文'}, 'data_and_error_same_recv': True}
        validate_terminal(value, 'biz', 'go', 'rust')
        with self.assertRaisesRegex(ValueError, 'only the observed Go'):
            validate_terminal(value, 'biz', 'rust', 'rust')

    def test_cascade_requires_real_path_and_single_terminal(self):
        finish = {'event': 'cascade_finish', 'node': 'C', 'id': 'case',
                  'has_deadline': False, 'code': 12007, 'ttscp': ['A', 'B'],
                  'error': 'canceled', 'active_handlers': 0}
        probe = dict(finish, id='case-probe', code=0, error='', ttscp=[])
        validate_server([finish, probe], 'C', 'case', 'cancel')
        with self.assertRaisesRegex(ValueError, 'path differs'):
            validate_server([dict(finish, ttscp=['B']), probe], 'C', 'case', 'cancel')
        with self.assertRaisesRegex(ValueError, 'expected one'):
            validate_server([finish, finish, probe], 'C', 'case', 'cancel')

    def test_cascade_recovery_does_not_hide_missing_cleanup(self):
        finish = {'event': 'cascade_finish', 'node': 'B', 'id': 'case',
                  'has_deadline': False, 'code': 12015, 'ttscp': ['A'],
                  'error': 'deadline', 'active_handlers': 0}
        probe = dict(finish, id='case-probe', code=0, error='', ttscp=[])
        child = dict(finish, event='cascade_child_finish')
        validate_server([child, finish, probe], 'B', 'case', 'deadline')
        with self.assertRaisesRegex(ValueError, 'child call'):
            validate_server([finish, probe], 'B', 'case', 'deadline')
        with self.assertRaisesRegex(ValueError, 'count did not return'):
            validate_server([child, finish, dict(probe, active_handlers=1)], 'B', 'case', 'deadline')

    def test_cascade_does_not_accept_timeout_before_live_ack(self):
        result = {'ok': True, 'api': 'cascade', 'id': 'case', 'mode': 'deadline',
                  'code': 12015, 'ack': True, 'probe_ok': True, 'elapsed_ms': 3001}
        validate_client(result, 'case', 'deadline')
        with self.assertRaisesRegex(ValueError, 'acknowledge'):
            validate_client(dict(result, ack=False), 'case', 'deadline')
        with self.assertRaisesRegex(ValueError, 'three seconds'):
            validate_client(dict(result, elapsed_ms=2), 'case', 'deadline')

    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix='kitex-runner-test-')
        self.root = Path(self.temp.name)

    def tearDown(self):
        self.temp.cleanup()

    def test_timeout_reaps_owned_child(self):
        result = run([sys.executable, '-c', 'import time; time.sleep(60)'],
                     cwd=self.root, timeout=0.1, log=self.root / 'timeout.log')
        self.assertTrue(result['timed_out'])
        self.assertNotEqual(result['returncode'], 0)
        self.assertLess(result['duration_seconds'], 5)

    def test_ready_event_and_cleanup(self):
        server = Server([sys.executable, '-u', '-c',
                         'import time; print(\'{"event":"ready","addr":"fixture"}\'); time.sleep(60)'],
                        cwd=self.root, log=self.root / 'server.log')
        ready = server.start(timeout=3)
        self.assertEqual(ready['addr'], 'fixture')
        server.close()
        self.assertIsNotNone(server.process.poll())
        self.assertFalse(server.reader.is_alive())

    def test_explicit_control_waits_for_acknowledged_event(self):
        server = Server([sys.executable, '-u', '-c',
                         'import json,sys,time; print(json.dumps({"event":"barrier"})); '
                         'line=sys.stdin.readline(); print(json.dumps({"event":"draining","input":line.strip()})); time.sleep(60)'],
                        cwd=self.root, log=self.root / 'controlled.log', stdin=True,
                        ready_event='barrier', capture_events=True)
        try:
            self.assertEqual(server.start(timeout=3)['event'], 'barrier')
            server.send_line('continue')
            event = server.wait_event('draining', timeout=3)
            self.assertEqual(event['input'], 'continue')
        finally:
            server.close()
        self.assertFalse(server.reader.is_alive())

    def test_failure_before_ready_is_not_success(self):
        server = Server([sys.executable, '-c', 'raise SystemExit(3)'],
                        cwd=self.root, log=self.root / 'failed.log')
        with self.assertRaises(ProcessFailure):
            server.start(timeout=3)
        self.assertEqual(server.process.returncode, 3)

    def test_report_accepts_logs_before_formatted_json(self):
        log = self.root / 'client.log'
        log.write_text('framework log {not json}\n' + json.dumps({'event': 'result', 'ok': True}, indent=2))
        self.assertTrue(result_event(log)['ok'])

    def test_missing_result_is_not_success(self):
        log = self.root / 'client.log'
        log.write_text('process exited without result')
        with self.assertRaises(ValueError):
            result_event(log)

    def test_mode_specific_empty_stream(self):
        self.assertEqual(expected('client', 0, 'not sent'), [{'value': '', 'seq': 0}])
        self.assertEqual(expected('bidi', 0, 'not sent'), [])
        self.assertEqual(expected('server', 0, 'request'), [])

    def test_long_artifact_path_and_explicit_profile_environment(self):
        endpoint = self.root / 'endpoint'
        endpoint.write_text('#!' + sys.executable + '\n' + '''
import argparse, json, os, socket
p = argparse.ArgumentParser()
p.add_argument('--role'); p.add_argument('--addr'); p.add_argument('--mode')
p.add_argument('--messages', type=int); p.add_argument('--payload')
a, _ = p.parse_known_args()
assert os.environ['TTSTREAM_RUNNER_TEST_ENV'] == 'provided-to-both-endpoints'
if a.role == 'server':
    listener = socket.socket(socket.AF_UNIX)
    listener.bind(a.addr.removeprefix('unix://') if hasattr(str, 'removeprefix') else a.addr[7:])
    listener.listen()
    print(json.dumps({'event': 'ready', 'addr': a.addr, 'profile': 'test'}), flush=True)
    listener.accept()
else:
    print(json.dumps({'event': 'result', 'ok': True, 'profile': 'test',
                     'responses': [{'value': a.payload, 'seq': i} for i in range(a.messages)]}))
''')
        endpoint.chmod(0o700)
        output = self.root / ('artifacts-' * 20)
        report = basic(go=endpoint, rust=endpoint, output=output, transports=['uds'], modes=['bidi'],
                       timeout=3, directions=['go-to-rust', 'rust-to-go'], api='typed', pools=['mux'],
                       profile='test', env_overrides={'TTSTREAM_RUNNER_TEST_ENV': 'provided-to-both-endpoints'})
        self.assertEqual(len(report['cases']), 6)
        self.assertTrue(all(c['status'] == 'PASS' for c in report['cases']), report)
        for row in report['cases']:
            socket_path = Path(row['ready']['addr'][7:])
            self.assertLess(len(str(socket_path).encode()), 108)
            self.assertFalse(socket_path.exists())

    def test_wire_validation_rejects_extra_terminal_on_reset_stream(self):
        actions = [{'op': 'read', 'expect': {'2': [{'kind': 4}]}}]
        observed = {'ok': True, 'actions': [{'frames': [
            {'flags': 2, 'sid': 1, 'int_metadata': {'27': '4'}},
            {'flags': 2, 'sid': 2, 'int_metadata': {'27': '4'}}]}]}
        with self.assertRaisesRegex(ValueError, 'frame sequence differs'):
            validate(actions, observed)

    def test_metainfo_legacy_leak_is_only_accepted_for_legacy_reference(self):
        seed = seed_for('isolated-stream', 'forward-only')
        observed = {'ok': True, 'api': 'metainfo', 'observations': [
            {'node': node, 'persistent': seed['persistent'], 'transient': seed['transient'],
             'has_deadline': False, 'header_string': None, 'header_int': None}
            for node in ['B', 'C']]}
        validate_metainfo(seed, observed, 'ggg', 'legacy')
        with self.assertRaisesRegex(ValueError, 'metainfo differs'):
            validate_metainfo(seed, observed, 'ggg', 'single-hop')

    def test_metainfo_timeout_header_fails_even_without_context_deadline(self):
        seed = seed_for('isolated-stream', 'forward-only')
        observed = {'ok': True, 'api': 'metainfo', 'observations': [
            {'node': 'B', 'persistent': seed['persistent'], 'transient': seed['transient'],
             'has_deadline': False, 'header_string': {'RPC_PERSIST_interop-p': 'isolated-stream',
                 'RPC_TRANSIT_interop-t': 'isolated-stream'}, 'header_int': {'12': '10000'}},
            {'node': 'C', 'persistent': seed['persistent'], 'transient': {},
             'has_deadline': False, 'header_string': None, 'header_int': None}]}
        with self.assertRaisesRegex(ValueError, 'timeout int header'):
            validate_metainfo(seed, observed, 'grg', 'single-hop')


if __name__ == '__main__':
    unittest.main()
