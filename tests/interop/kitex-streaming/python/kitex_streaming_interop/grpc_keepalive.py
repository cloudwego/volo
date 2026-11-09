"""Real Kitex enforcement, with explicitly accelerated local ping periods."""
from __future__ import annotations

import hashlib
import json
from pathlib import Path
import tempfile

from .process import Server, run


def keepalive(*, go: Path, rust: Path, output: Path, timeout: float,
              transports: list[str], direction='both', env=None):
    if direction not in ('both', 'rust-to-go'):
        raise ValueError('Kitex enforcement fixture checks Rust client -> Kitex server')
    output.mkdir(parents=True, exist_ok=True)
    hashes = {key: hashlib.sha256(path.read_bytes()).hexdigest() for key, path in (('go', go), ('rust', rust))}
    rows = []
    for transport in transports:
        if transport not in ('tcp', 'uds'):
            raise ValueError('keepalive fixture supports tcp/uds only')
        for scenario in ('valid', 'excessive'):
            work = output / (transport + '-' + scenario)
            work.mkdir(parents=True, exist_ok=True)
            row = {'transport': transport, 'direction': 'rust-to-go', 'scenario': scenario,
                   'minimum_ping_ms': 80 if scenario == 'valid' else 1000,
                   'client_ping_ms': 100 if scenario == 'valid' else 20, 'status': 'FAIL'}
            with tempfile.TemporaryDirectory(prefix='grpc-') as sockets:
                address = 'tcp://127.0.0.1:0' if transport == 'tcp' else 'unix://' + sockets + '/rpc.sock'
                server = Server([str(go), '--role', 'grpc-thrift-keepalive-server', '--addr', address,
                                 '--scenario', scenario], cwd=work, log=work / 'server.log', env=env)
                try:
                    ready = server.start(min(timeout, 30))
                    result = run([str(rust), '--role', 'grpc-thrift-keepalive-client', '--addr', ready['addr'],
                                  '--scenario', scenario, '--report', str(work / 'client.json')],
                                 cwd=work, timeout=timeout, log=work / 'client.log', env=env)
                    row['command'] = result
                    if result['returncode'] != 0 or result['timed_out']:
                        raise ValueError('keepalive client failed')
                    report = json.loads((work / 'client.json').read_text())
                    if report.get('status') != 'PASS' or report.get('scenario') != scenario:
                        raise ValueError('keepalive assertions missing')
                    if scenario == 'excessive':
                        if not report.get('transport_failure') or 'Got too many pings' not in (work / 'server.log').read_text():
                            raise ValueError('missing actual Kitex excessive-ping rejection evidence')
                    row.update(status='PASS', observed=report)
                except (OSError, ValueError, RuntimeError) as error:
                    row['error'] = str(error)
                finally:
                    server.close()
            rows.append(row)
    after = {key: hashlib.sha256(path.read_bytes()).hexdigest() for key, path in (('go', go), ('rust', rust))}
    if hashes != after:
        rows.append({'status': 'FAIL', 'error': 'endpoint binary changed during suite'})
    report = {'schemaVersion': 1, 'suite': 'grpc-thrift-keepalive', 'protocol': 'grpc-thrift',
              'scope': 'accelerated explicit Kitex policy; not a production-default timing test',
              'status': 'PASS' if rows and all(row['status'] == 'PASS' for row in rows) else 'FAIL',
              'binaries': hashes, 'binaries_after': after, 'cases': rows}
    (output / 'report.json').write_text(json.dumps(report, indent=2) + '\n')
    return report
