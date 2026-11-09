"""Offline environment/dispatch tests; these do not execute RPC fixtures."""
import contextlib
import importlib
import io
import json
import os
from pathlib import Path
import tempfile
import unittest
from unittest import mock

cli = importlib.import_module('kitex_streaming_interop.__main__')


class EnvironmentTest(unittest.TestCase):
    def test_required_is_checked_after_override_merge(self):
        overrides, contract = cli.validated_environment(
            {'environment': {'set': {'PORT': 'ready'}, 'required': ['PORT', 'TOKEN']}},
            'direct', {'PORT': '', 'TOKEN': 'private-value'})
        self.assertEqual(overrides, {'PORT': 'ready'})
        self.assertEqual(contract['required_names'], ['PORT', 'TOKEN'])
        self.assertNotIn('private-value', json.dumps(contract))
        self.assertNotIn('TOKEN', overrides)
        with self.assertRaisesRegex(ValueError, 'PORT'):
            cli.validated_environment({'environment': {'set': {'PORT': ''}, 'required': ['PORT']}},
                                      None, {'PORT': 'original'})

    def test_missing_and_whitespace_values_rejected(self):
        for inherited in ({}, {'TOKEN': ''}, {'TOKEN': ' \t'}):
            with self.assertRaisesRegex(ValueError, 'TOKEN'):
                cli.validated_environment({'environment': {'required': ['TOKEN']}}, None, inherited)

    def test_required_schema_is_not_coerced(self):
        for required in ('TOKEN', None, [1], [''], [' '], ['TOKEN', 'TOKEN'], [{}], ['A=B'], ['A\0B']):
            with self.assertRaisesRegex(ValueError, 'required'):
                cli.validated_environment({'environment': {'required': required}}, None, {})

    def test_invalid_environment_types_do_not_dump_values(self):
        for environment in ('private-value', {'set': {'TOKEN': {'secret': 'private-value'}}}, {'set': {'': 'private-value'}}, {'set': {'A=B': 'private-value'}},
                            {'set': {'A\0B': 'private-value'}}, {'set': {'TOKEN': 'private-value\0'}}):
            with self.assertRaises(ValueError) as error:
                cli.validated_environment({'environment': environment}, None, {})
            self.assertNotIn('private-value', str(error.exception))

    def test_only_direct_and_truthful_group_isolation_supported(self):
        for environment in ({'profile': 'mesh'}, {'process_isolation': 'case'},
                            {'fresh_process_per_case': True}, {'fresh_process_per_case': 'false'}):
            with self.assertRaises(ValueError):
                cli.validated_environment({'environment': environment}, None, {})
        self.assertEqual(cli.validated_environment({}, 'direct', {})[1]['process_isolation'], 'scenario-group')
        with self.assertRaises(ValueError):
            cli.validated_environment({}, 'nonexistent', {})

    def test_cli_rejects_unknown_env_profile_before_dispatch(self):
        self._invalid_cli('--env-profile', 'does-not-exist')

    def test_cli_rejects_unsupported_protocol_before_dispatch(self):
        self._invalid_cli('--protocol', 'grpc')

    def _invalid_cli(self, option, value):
        with tempfile.TemporaryDirectory() as directory, mock.patch.object(cli, 'basic') as basic:
            args = ['runner', '--suite', 'typed-basic', '--go-bin', '/bin/true', '--rust-bin', '/bin/true',
                    '--out', directory, option, value]
            with mock.patch('sys.argv', args), contextlib.redirect_stderr(io.StringIO()):
                with self.assertRaises(SystemExit) as exited:
                    cli.main()
            self.assertEqual(exited.exception.code, 2)
            basic.assert_not_called()

    def test_successful_direct_cli_keeps_existing_suite_environment(self):
        with tempfile.TemporaryDirectory() as directory:
            profile = Path(directory) / 'profile.toml'
            profile.write_text('name="fixture"\n[environment]\nprofile="direct"\nprocess_isolation="scenario-group"\nrequired=["TOKEN"]\n[environment.set]\nGOPKG_DEBUG_ADDR="127.0.0.1:0"\n')
            args = ['runner', '--suite', 'typed-basic', '--go-bin', '/bin/true', '--rust-bin', '/bin/true',
                    '--profile', str(profile), '--out', directory, '--env-profile', 'direct']
            with mock.patch('sys.argv', args), mock.patch.dict(os.environ, {'TOKEN': 'private-value'}), \
                    mock.patch.object(cli, 'basic', return_value={'cases': []}) as basic, \
                    contextlib.redirect_stdout(io.StringIO()):
                self.assertEqual(cli.main(), 1)  # No actual cases; never fabricate an RPC PASS.
            self.assertEqual(basic.call_args[1]['env_overrides'], {'GOPKG_DEBUG_ADDR': '127.0.0.1:0'})
            self.assertNotIn('private-value', (Path(directory) / 'summary.json').read_text())

    def test_idl_and_codec_dispatch_receive_the_validated_merged_environment(self):
        for suite, function in [('idl-matrix', 'matrix'), ('codec-golden', 'golden')]:
            with tempfile.TemporaryDirectory() as directory:
                profile = Path(directory) / 'profile.toml'
                profile.write_text('name="fixture"\n[environment]\nrequired=["FIXTURE_CONFIG"]\n[environment.set]\nFIXTURE_CONFIG="configured"\n')
                args = ['runner', '--suite', suite, '--kitex-bin', '/bin/true', '--go-bin', '/bin/true',
                        '--profile', str(profile), '--out', directory]
                result = {'cases': [{'status': 'INCOMPLETE'}]} if suite == 'idl-matrix' else {'status': 'INCOMPLETE'}
                with mock.patch('sys.argv', args), mock.patch.object(cli, function, return_value=result) as dispatch, \
                        contextlib.redirect_stdout(io.StringIO()):
                    self.assertEqual(cli.main(), 1)
                self.assertEqual(dispatch.call_args[1]['env']['FIXTURE_CONFIG'], 'configured')

    def test_codec_environment_reaches_real_children_in_all_three_roles(self):
        from kitex_streaming_interop.codec import golden
        import sys
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            oracle = root / 'oracle.py'
            oracle.write_text('#!' + sys.executable + '\nimport json,os,sys\n'
                              'ok=os.environ.get("FIXTURE_CONFIG")=="configured"\n'
                              'path=sys.argv[sys.argv.index("--report")+1]\n'
                              'open(path,"w").write(json.dumps({"environment_received":ok}))\n'
                              'sys.exit(0 if ok else 3)\n')
            oracle.chmod(0o755)
            rust_bytes = root / 'rust.json'
            rust_bytes.write_text('{}')
            env = dict(os.environ, FIXTURE_CONFIG='configured')
            report = golden(oracle=oracle, fixture_root=root, output=root / 'out', regenerate=True,
                            rust_bytes=rust_bytes, timeout=3, env=env)
            self.assertEqual(len(report['commands']), 3)
            self.assertTrue(all(command['returncode'] == 0 for command in report['commands']))
            for name in ('golden.json', 'go-verify.json', 'rust-to-go.json'):
                self.assertTrue(json.loads((root / 'out' / name).read_text())['environment_received'])
