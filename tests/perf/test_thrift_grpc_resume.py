"""Checkpoint validation only; these tests do not execute benchmark RPCs."""
import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[2]
RUNNER = ROOT / 'tests/perf/thrift-grpc.py'
NAME = 'rust-to-rust-tcp-unary-1-64-identity'


class Resume(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix='grpc-checkpoint-')
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.binaries = {key: self.root / key for key in ('go', 'rust')}
        for key, path in self.binaries.items():
            path.write_text(key)
        self.report = {
            'schemaVersion': 1,
            'scope': 'new capability; short RPCs, client hooks/exporters only; no protobuf comparison or long-stream claim',
            'seconds': 1, 'cpu_list': None, 'hooks': 'off', 'observability': 'off',
            'binaries': {key: hashlib.sha256(path.read_bytes()).hexdigest()
                         for key, path in self.binaries.items()},
            'cases': [{'name': NAME, 'status': 'FAIL'}], 'status': 'FAIL',
        }

    def execute(self, expected, extra_args=()):
        path = self.root / 'report.json'
        path.write_text(json.dumps(self.report))
        before = path.read_bytes()
        env = dict(os.environ, PYTHONPATH=str(ROOT / 'tests/interop/kitex-streaming/python'))
        result = subprocess.run([
            sys.executable, str(RUNNER), '--go-bin', str(self.binaries['go']),
            '--rust-bin', str(self.binaries['rust']), '--out', str(self.root),
            '--seconds', '1', '--directions', 'rust-to-rust',
            '--case-filter', 'tcp-unary-1-64-identity', '--resume',
        ] + list(extra_args), env=env, stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=10)
        self.assertEqual(result.returncode, expected, result.stderr.decode())
        if expected == 2:
            self.assertEqual(path.read_bytes(), before)
        self.assertFalse((self.root / NAME / '0').exists(), 'validation must not start a peer')
        return json.loads(path.read_text()), result

    def test_completed_failure_is_retained_without_rerun(self):
        report, result = self.execute(1)
        self.assertEqual(report['cases'], self.report['cases'])
        self.assertEqual(report['status'], 'FAIL')
        self.assertEqual(result.stdout, b'')

    def test_completed_partial_samples_retain_all_three_failures(self):
        self.report.update(cases=[], status='RUNNING')
        directory = self.root / NAME
        directory.mkdir()
        samples = [{'status': 'FAIL', 'error': 'original failure'}] * 3
        (directory / 'samples.json').write_text(json.dumps(samples))
        report, _ = self.execute(1)
        self.assertEqual(report['cases'], [{'name': NAME, 'status': 'FAIL'}])
        self.assertEqual(json.loads((directory / 'samples.json').read_text()), samples)

    def test_changed_binary_is_rejected_before_checkpoint_mutation(self):
        self.binaries['rust'].write_text('different candidate')
        self.execute(2)

    def test_changed_budget_is_rejected(self):
        self.report['seconds'] = 2
        self.execute(2)

    def test_enabling_phase_timing_cannot_reuse_uninstrumented_samples(self):
        self.execute(2, ['--phase-latency', 'on'])

    def test_changed_case_order_is_rejected(self):
        self.report['cases'][0]['name'] = 'go-to-rust-tcp-unary-1-64-identity'
        self.execute(2)

    def test_binary_mutation_in_the_previous_run_cannot_be_repaired_by_resume(self):
        self.report['binaries_after'] = {'go': 'changed', 'rust': 'changed'}
        self.execute(2)


if __name__ == '__main__':
    unittest.main()
