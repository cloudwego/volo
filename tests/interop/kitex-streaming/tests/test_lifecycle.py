import copy
import json
import unittest

from kitex_streaming_interop.lifecycle import validate_client, validate_server


class LifecycleEvidenceTests(unittest.TestCase):
    def client(self):
        return {'event': 'result', 'ok': True, 'api': 'lifecycle', 'id': 'case', 'scenario': 'keep-open',
                'terminal': {'kind': 'error', 'code': 12014, 'kitex_code': 12014, 'path': [], 'error': 'actual timeout'},
                'keep_open': True, 'probe': True,
                'final_terminal': {'kind': 'ok', 'code': 0, 'path': []},
                'responses': [{'value': json.dumps({'schemaVersion': 1, 'action': 'released', 'id': 'case'}), 'seq': 1}]}

    def test_frozen_rust_schema_accepts_ack_event_and_omitted_success_fields(self):
        validate_client(self.client(), [{'event': 'ack', 'id': 'case'}], 'case', 'keep-open')

    def test_go_schema_includes_the_actual_ack_data(self):
        value = self.client()
        value['responses'].insert(0, {'value': json.dumps({'schemaVersion': 1, 'action': 'ack', 'id': 'case'}), 'seq': 1})
        validate_client(value, [{'event': 'ack', 'id': 'case'}], 'case', 'keep-open')

    def test_boolean_success_cannot_replace_post_timeout_data_and_eof(self):
        for key in ('responses', 'final_terminal', 'probe'):
            value = self.client()
            value[key] = [] if key == 'responses' else None
            with self.subTest(key=key), self.assertRaises(ValueError):
                validate_client(value, [{'event': 'ack', 'id': 'case'}], 'case', 'keep-open')

    def test_wrong_identity_or_deadline_code_is_rejected(self):
        for mutation in ('identity', 'code', 'ack'):
            value = self.client()
            events = [{'event': 'ack', 'id': 'case'}]
            if mutation == 'identity': value['id'] = 'other'
            elif mutation == 'code': value['terminal']['code'] = 12015
            else: events = []
            with self.subTest(mutation=mutation), self.assertRaises(ValueError):
                validate_client(value, events, 'case', 'keep-open')

    def test_cancel_requires_actual_upstream_path(self):
        rows = [{'event': 'started', 'id': 'case'}, {'event': 'observed', 'id': 'case',
                'terminal': {'kind': 'error', 'code': 12014, 'kitex_code': 12014, 'path': ['A'], 'error': 'remote cancellation'}}]
        validate_server(rows, 'case', 'cancel')
        corrupted = copy.deepcopy(rows)
        corrupted[1]['terminal']['path'] = []
        with self.assertRaises(ValueError): validate_server(corrupted, 'case', 'cancel')
        with self.assertRaises(ValueError): validate_server(rows + [rows[0]], 'case', 'cancel')


if __name__ == '__main__':
    unittest.main()
