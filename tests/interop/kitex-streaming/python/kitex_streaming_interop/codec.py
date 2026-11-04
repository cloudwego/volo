from __future__ import annotations

import hashlib
import json
from pathlib import Path

from .process import run


def golden(*, oracle: Path, fixture_root: Path, output: Path, regenerate: bool,
           rust_bytes: Path | None, timeout: float, env: dict[str, str] | None = None) -> dict:
    output.mkdir(parents=True, exist_ok=True)
    fixture = output / 'golden.json' if regenerate else fixture_root / 'golden.json'
    commands = []
    if regenerate:
        commands.append(run([str(oracle), '--role', 'golden', '--report', str(fixture)],
                            cwd=output, timeout=timeout, log=output / 'generate.log', env=env))
    if not fixture.is_file():
        return {'suite': 'codec-golden', 'status': 'BLOCKED', 'reason': f'missing fixture {fixture}'}
    commands.append(run([str(oracle), '--role', 'codec-verify', '--input', str(fixture),
                         '--report', str(output / 'go-verify.json')],
                        cwd=output, timeout=timeout, log=output / 'verify.log', env=env))
    if rust_bytes is not None:
        commands.append(run([str(oracle), '--role', 'codec-verify', '--input', str(rust_bytes),
                             '--report', str(output / 'rust-to-go.json')],
                            cwd=output, timeout=timeout, log=output / 'rust-to-go.log', env=env))
    report = {'schemaVersion': 1, 'suite': 'codec-golden',
              'status': 'PASS' if all(c['returncode'] == 0 and not c['timed_out'] for c in commands) else 'FAIL',
              'fixture': str(fixture), 'fixture_sha256': hashlib.sha256(fixture.read_bytes()).hexdigest(),
              'oracle_sha256': hashlib.sha256(oracle.read_bytes()).hexdigest(),
              'commands': commands, 'reverse_verified': rust_bytes is not None,
              'rust_bytes_sha256': hashlib.sha256(rust_bytes.read_bytes()).hexdigest() if rust_bytes else None,
              'scope': 'Go fixture verification; reverse Go decoding only verified when --rust-bytes is supplied; offline Rust tests are separate'}
    (output / 'report.json').write_text(json.dumps(report, indent=2) + '\n')
    return report
