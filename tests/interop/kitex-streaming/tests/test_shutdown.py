import copy
import json
import unittest

from kitex_streaming_interop.shutdown import validate_shutdown


def frame(kind, sid, action=None, identity=None):
    value = {'sid': sid, 'int_metadata': {'27': str(kind)}}
    if action is not None:
        value['message'] = {'value': json.dumps({'schemaVersion': 1, 'action': action,
                                               'id': identity}), 'seq': 1}
    return value


class ShutdownEvidenceTests(unittest.TestCase):
    def setUp(self):
        self.actual = {
            'ok': True, 'api': 'shutdown-wire', 'unary_dials': 1, 'stream_dials': 1,
            'unary_phases': [{'ok': True, 'seq': 1}, {'ok': True, 'seq': 2}],
            'ack_frames': [frame(2, 1), frame(3, 1, 'ack', 'shutdown-live')],
            'rejected_new_stream': True, 'admission_elapsed_ms': 1,
            'probes': [{'sid': 2, 'frames': [dict(frame(5, 2), exception={'code': 6})]}],
            'live_finish_frames': [frame(3, 1, 'released', 'shutdown-live'), frame(4, 1)],
        }

    def test_success_requires_retained_sockets_and_live_ack(self):
        validate_shutdown(self.actual)
        for key in ('unary_dials', 'stream_dials'):
            with self.assertRaisesRegex(ValueError, 'redialed'):
                validate_shutdown(dict(self.actual, **{key: 2}))
        with self.assertRaisesRegex(ValueError, 'not acknowledged'):
            validate_shutdown(dict(self.actual, ack_frames=[]))

    def test_boolean_rejection_does_not_replace_actual_rst(self):
        actual = copy.deepcopy(self.actual)
        actual['probes'][0]['frames'][0]['exception']['code'] = 7
        with self.assertRaisesRegex(ValueError, 'RST'):
            validate_shutdown(actual)
        with self.assertRaisesRegex(ValueError, '500ms'):
            validate_shutdown(dict(self.actual, admission_elapsed_ms=2000))

    def test_hook_to_mark_race_must_still_preserve_old_stream(self):
        actual = copy.deepcopy(self.actual)
        actual['probes'] = [
            {'sid': 2, 'frames': [frame(2, 2), frame(3, 2, 'probe', 'shutdown-probe-2'), frame(4, 2)]},
            {'sid': 3, 'frames': [dict(frame(5, 3), exception={'code': 6})]},
        ]
        validate_shutdown(actual)
        actual['live_finish_frames'][-1]['exception'] = {'code': 6}
        with self.assertRaisesRegex(ValueError, 'ended in error'):
            validate_shutdown(actual)
