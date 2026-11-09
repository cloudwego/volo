import copy
import json
import unittest

from kitex_streaming_interop.budget import validate_burst, validate_budget_wire, validate_resources
from kitex_streaming_interop.controls import message
from kitex_streaming_interop.wire import read, response


class BudgetEvidenceTests(unittest.TestCase):
    def setUp(self):
        self.actual = {'ok': True, 'api': 'budget', 'id': 'case', 'ack': True, 'probe': True,
                       'cancel': False, 'target_messages': 128, 'payload_bytes': 1024,
                       'send_attempted': 128, 'send_succeeded': 128, 'probe_elapsed_ns': 10,
                       'terminal': {'code': 0},
                       'responses': [{'value': json.dumps({'schemaVersion': 1, 'action': 'released', 'id': 'case'}), 'seq': 128}]}

    def test_default_requires_every_queued_message(self):
        validate_burst(self.actual, 'case', False, False)
        actual = copy.deepcopy(self.actual)
        actual['responses'][0]['seq'] = 127
        with self.assertRaisesRegex(ValueError, 'every queued'):
            validate_burst(actual, 'case', False, False)

    def test_reset_requires_real_go_error_class_and_later_probe(self):
        actual = dict(self.actual, responses=[], terminal={'code': 6, 'streaming_canceled': True})
        validate_burst(actual, 'case', True, False)
        with self.assertRaisesRegex(ValueError, 'ErrStreamingCanceled'):
            validate_burst(dict(actual, terminal={'code': 6}), 'case', True, False)
        with self.assertRaisesRegex(ValueError, '500ms'):
            validate_burst(dict(actual, probe_elapsed_ns=501_000_000), 'case', True, False)

    def test_budget_checks_retained_bytes_and_reset_counter(self):
        config = {'recv_messages': 8, 'recv_bytes': 8192, 'connection_recv_bytes': 65536, 'shared_recv_bytes': 65536}
        observed = {'resources': {'high_water_messages': 7, 'high_water_bytes': 7273, 'buffer_limit_resets': 1, 'read_pauses': 0, 'control_queue_delay_nanos': 100}}
        validate_resources(observed, config, True, 'stream-bytes')
        observed['resources']['control_queue_delay_nanos'] = 0
        with self.assertRaisesRegex(ValueError, 'control queue delay'):
            validate_resources(observed, config, True, 'stream-bytes')
        observed['resources']['control_queue_delay_nanos'] = 100
        observed['resources']['high_water_bytes'] = 8193
        with self.assertRaisesRegex(ValueError, 'retained receive bytes'):
            validate_resources(observed, config, True, 'stream-bytes')

    def test_json_key_order_is_allowed_but_identity_and_frame_order_are_strict(self):
        actions = [read(1, [response(2), response(3, message('ack', 'case'))], max_frames=2, until_sid=None)]
        actual = {'ok': True, 'actions': [{'frames': [
            {'sid': 1, 'flags': 2, 'int_metadata': {'27': '2'}},
            {'sid': 1, 'flags': 2, 'int_metadata': {'27': '3'},
             'message': {'value': '{"id":"case","action":"ack","schemaVersion":1}', 'seq': 1}},
        ]}]}
        validate_budget_wire(actions, actual)
        actual['actions'][0]['frames'][1]['message']['value'] = '{"id":"other","action":"ack","schemaVersion":1}'
        with self.assertRaisesRegex(ValueError, 'frame sequence'):
            validate_budget_wire(actions, actual)
