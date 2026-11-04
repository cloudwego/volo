"""Idle unary checks retain exactly one socket across initial and reused waits."""
from __future__ import annotations

from concurrent.futures import ThreadPoolExecutor
import hashlib
import json
import os
from pathlib import Path
import tempfile

from .network import result_event
from .process import Server, run


def idle_unary(*, go: Path, rust: Path, output: Path, transports: list[str], timeout: float,
               profile: str, idle_seconds: float, env_overrides: dict[str, str] | None = None) -> dict:
    output.mkdir(parents=True, exist_ok=True)
    environment = os.environ.copy()
    environment.update(env_overrides or {})
    fingerprints = {str(p): hashlib.sha256(p.read_bytes()).hexdigest() for p in (go, rust)}

    def one_case(server_kind, binary, transport):
        directory = output / server_kind / transport
        directory.mkdir(parents=True, exist_ok=True)
        row = {'server': server_kind, 'transport': transport, 'idle_seconds': idle_seconds}
        if transport not in ('tcp', 'uds'):
            return dict(row, status='INCOMPLETE', reason='requires TCP or UDS')
        with tempfile.TemporaryDirectory(prefix='tts-', dir='/tmp') as socket_dir:
            addr = 'tcp://127.0.0.1:0' if transport == 'tcp' else 'unix://' + socket_dir + '/server.sock'
            server = Server([str(binary), '--role', 'server', '--addr', addr],
                            cwd=directory, log=directory / 'server.log', env=environment)
            try:
                ready = server.start(min(timeout, 30))
                row['ready'] = ready
                if server_kind == 'go-baseline' and ready.get('profile') != profile:
                    raise ValueError('Go endpoint profile differs from requested profile')
                command = [str(go), '--role', 'unary-idle', '--addr', ready['addr'],
                           '--idle', str(idle_seconds) + 's', '--report', str(directory / 'wire.json')]
                log = directory / 'client.log'
                executed = run(command, cwd=directory, timeout=timeout, log=log, env=environment)
                row['execution'] = executed
                observed = result_event(log)
                row['actual'] = observed
                if executed['returncode'] != 0 or executed['timed_out'] or not observed.get('ok'):
                    raise ValueError('idle unary client failed or timed out')
                if observed.get('profile') != profile or observed.get('dial_count') != 1:
                    raise ValueError('wrong profile or connection was recreated')
                phases = observed.get('phases', [])
                if len(phases) != 2 or any(p['idle_elapsed_seconds'] < idle_seconds for p in phases):
                    raise ValueError('both initial and reused idle durations must be observed')
                for index, phase in enumerate(phases, 1):
                    if phase.get('response') != {'value': 'idle-unary-中文', 'seq': index}:
                        raise ValueError('unary response differs')
                row['status'] = 'PASS' if idle_seconds >= 300 else 'INCOMPLETE'
                if idle_seconds < 300:
                    row['reason'] = 'codec smoke only; acceptance requires two waits of at least 300 seconds'
            except Exception as error:
                row['status'], row['reason'] = 'FAIL', str(error)
            finally:
                server.close()
        return row

    # Separate endpoints and short socket paths allow the independent 10-minute
    # cases to run together without changing the on-wire idle interval.
    with ThreadPoolExecutor(max_workers=4) as executor:
        futures = [executor.submit(one_case, kind, binary, transport)
                   for kind, binary in [('rust', rust), ('go-baseline', go)] for transport in transports]
        cases = [future.result() for future in futures]
    after = {str(p): hashlib.sha256(p.read_bytes()).hexdigest() for p in (go, rust)}
    if after != fingerprints:
        cases.append({'status': 'FAIL', 'reason': 'endpoint binaries changed while suite was running'})
    report = {'schemaVersion': 1, 'suite': 'idle-unary', 'cases': cases, 'binaries': fingerprints,
              'binaries_after': after, 'environment_overrides': env_overrides or {},
              'scope': 'same raw socket with Go-generated unary args and actual TTHeader codec; no reconnect; mixed-service servers'}
    (output / 'report.json').write_text(json.dumps(report, ensure_ascii=False, indent=2) + '\n')
    return report
