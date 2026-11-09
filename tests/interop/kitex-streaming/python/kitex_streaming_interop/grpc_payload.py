"""Kitex gRPC codec oracle, shared with consumers of the offline payload golden."""
from __future__ import annotations

import hashlib
import json
from pathlib import Path

from .process import run


def validate_golden(value: dict) -> None:
    if value.get('schemaVersion') != 1 or value.get('producer') != 'kitex-grpc-codec':
        raise ValueError('expected Kitex gRPC codec schema 1')
    modules = value.get('modules', {})
    if modules.get('github.com/cloudwego/kitex') != 'v0.16.4' or modules.get('github.com/cloudwego/thriftgo') != 'v0.4.5':
        raise ValueError('unexpected oracle module versions')
    cases = value.get('cases', [])
    names = {row['name'] for row in cases}
    if len(cases) != 19 or len(names) != len(cases):
        raise ValueError('expected 19 distinct raw/Args/Result cases')
    by_name = {row['name']: row for row in cases}
    for row in cases:
        payload, frame = bytes.fromhex(row['hex']), bytes.fromhex(row['frame_hex'])
        if len(frame) < 5 or frame[0] != 0 or int.from_bytes(frame[1:5], 'big') != len(payload) or frame[5:] != payload:
            raise ValueError('gRPC prefix or payload differs: ' + row['name'])
    for name in ('empty-stop', 'void-result', 'void-throws-success'):
        if by_name[name]['hex'] != '00':
            raise ValueError('empty Thrift payload requires STOP: ' + name)
    if by_name['streaming-raw']['hex'] == by_name['unary-args']['hex']:
        raise ValueError('unary must retain Args wrapping')
    if not value.get('services') or not value.get('apis') or len(value.get('boundary', [])) != 4:
        raise ValueError('missing service/API/boundary oracle')


def golden(*, oracle: Path, fixture_root: Path, output: Path, regenerate: bool,
           rust_bytes: Path | None, timeout: float, env: dict[str, str] | None = None) -> dict:
    output.mkdir(parents=True, exist_ok=True)
    observed = output / 'observed-golden.json'
    fixture = observed if regenerate else fixture_root / 'grpc-payload-golden.json'
    commands = [run([str(oracle), '--role', 'grpc-thrift-golden', '--report', str(observed)],
                    cwd=output, timeout=timeout, log=output / 'generate.log', env=env)]
    errors = []
    if all(command['returncode'] == 0 and not command['timed_out'] for command in commands):
        try:
            actual, expected = json.loads(observed.read_text()), json.loads(fixture.read_text())
            validate_golden(actual)
            validate_golden(expected)
            if actual != expected:
                errors.append('committed golden differs from the fixed Go oracle')
        except (OSError, ValueError, KeyError, TypeError) as error:
            errors.append(str(error))
    if rust_bytes is not None:
        commands.append(run([str(oracle), '--role', 'grpc-thrift-codec-verify', '--input', str(rust_bytes),
                             '--report', str(output / 'rust-to-go.json')], cwd=output, timeout=timeout,
                            log=output / 'rust-to-go.log', env=env))
    command_failure = any(command['returncode'] != 0 or command['timed_out'] for command in commands)
    report = {'schemaVersion': 1, 'suite': 'grpc-thrift-codec', 'protocol': 'grpc-thrift',
              'status': 'FAIL' if errors or command_failure else 'PASS' if rust_bytes else 'INCOMPLETE',
              'scope': '19 bidirectional payload cases; network interoperability is a separate gate',
              'reverse_verified': rust_bytes is not None and not errors and not command_failure,
              'errors': errors, 'commands': commands,
              'fixture_sha256': hashlib.sha256(fixture.read_bytes()).hexdigest() if fixture.is_file() else None,
              'oracle_sha256': hashlib.sha256(oracle.read_bytes()).hexdigest(),
              'rust_bytes_sha256': hashlib.sha256(rust_bytes.read_bytes()).hexdigest() if rust_bytes and rust_bytes.is_file() else None}
    (output / 'report.json').write_text(json.dumps(report, indent=2) + '\n')
    return report
