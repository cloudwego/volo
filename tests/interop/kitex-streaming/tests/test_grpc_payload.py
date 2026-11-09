import copy
import json
from pathlib import Path
import unittest

from kitex_streaming_interop.grpc_payload import validate_golden


class GrpcPayloadTest(unittest.TestCase):
    def setUp(self):
        # Use the same actual Go-produced fixture as the runner, rather than
        # constructing a second "valid" schema in the test implementation.
        # Volo keeps its own copy so this unit test does not need a second repo.
        local = Path(__file__).resolve().parents[1] / 'fixtures/grpc-payload-golden.json'
        self.value = json.loads(local.read_text())

    def test_fixed_go_golden_is_valid(self):
        validate_golden(self.value)

    def test_missing_or_duplicate_cases_cannot_pass(self):
        value = copy.deepcopy(self.value)
        value['cases'].pop()
        with self.assertRaisesRegex(ValueError, '19 distinct'):
            validate_golden(value)
        value['cases'].append(value['cases'][0])
        with self.assertRaisesRegex(ValueError, '19 distinct'):
            validate_golden(value)

    def test_payload_cannot_disagree_with_prefix(self):
        value = copy.deepcopy(self.value)
        value['cases'][0]['frame_hex'] = '0000000000'
        with self.assertRaisesRegex(ValueError, 'prefix or payload'):
            validate_golden(value)

    def test_other_oracle_version_cannot_be_reported_as_the_fixed_baseline(self):
        self.value['modules']['github.com/cloudwego/kitex'] = 'v0.16.3'
        with self.assertRaisesRegex(ValueError, 'module versions'):
            validate_golden(self.value)
