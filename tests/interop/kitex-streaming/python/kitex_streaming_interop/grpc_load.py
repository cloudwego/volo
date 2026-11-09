"""Local bounded-load/resource evidence, separate from throughput regression gates."""
from __future__ import annotations

import hashlib
import json
from pathlib import Path
import tempfile
import time

from .process import Server, run


def snapshot(pid):
    root = Path('/proc') / str(pid)
    values = {'unix_ms': int(time.time() * 1000), 'fds': len(list((root / 'fd').iterdir()))}
    for line in (root / 'status').read_text().splitlines():
        if line.startswith('VmRSS:'):
            values['rss_kib'] = int(line.split()[1])
        elif line.startswith('Threads:'):
            values['threads'] = int(line.split()[1])
    return values


def load(*, go: Path, rust: Path, output: Path, timeout: float,
         transports: list[str], direction='rust-to-go', env=None,
         duration=10, concurrency=16, payload_bytes=128, cooldown=0,
         compression='identity') -> dict:
    if not 1 <= duration <= 3600 or not 1 <= concurrency <= 128 or not 0 <= cooldown <= 120:
        raise ValueError('load duration/concurrency/cooldown exceeds fixture bounds')
    if not 0 <= payload_bytes <= 1024 * 1024 or compression not in ('identity', 'gzip'):
        raise ValueError('invalid load payload/compression')
    if direction not in ('rust-to-go', 'rust-to-rust'):
        raise ValueError('load fixture requires rust-to-go or rust-to-rust')
    if timeout < duration + cooldown + 15:
        raise ValueError('load --timeout must allow duration + cooldown + 15 seconds')
    output.mkdir(parents=True, exist_ok=True)
    hashes = {key: hashlib.sha256(path.read_bytes()).hexdigest() for key, path in (('go', go), ('rust', rust))}
    # Frozen before measurement. Four connections/endpoint, one worker plus at
    # most three stream-owned tasks per in-flight call, and fixed actor slack.
    limits = {'client_extra_fds': 4, 'client_extra_tasks': concurrency * 4 + 16,
              'client_recovered_extra_fds': 1, 'client_recovered_extra_tasks': 2,
              'server_extra_fds': 4, 'server_recovered_extra_fds': 1,
              'pool_idle_ttl_secs': 90, 'scheduler_slack_secs': 5}
    rows = []
    for transport in transports:
        if transport not in ('tcp', 'uds'):
            raise ValueError('load fixture supports tcp/uds only')
        work = output / (transport + '-' + direction)
        work.mkdir(parents=True, exist_ok=True)
        row = {'transport': transport, 'direction': direction, 'status': 'FAIL'}
        server = client = None
        with tempfile.TemporaryDirectory(prefix='grpc-') as sockets:
            address = 'tcp://127.0.0.1:0' if transport == 'tcp' else 'unix://' + sockets + '/rpc.sock'
            try:
                server = Server([str(go if direction == 'rust-to-go' else rust), '--role',
                                 'grpc-thrift-server', '--addr', address, '--compression', compression],
                                cwd=work, log=work / 'server.log', env=env)
                ready = server.start(min(timeout, 30))
                cold = snapshot(server.process.pid)
                # Readiness is printed after bind. Runtime/SDK initialization
                # and first-call lazy resources must be warm before a recovery
                # comparison; preserve the cold snapshot separately.
                warm = run([str(rust), '--role', 'grpc-thrift-client', '--addr', ready['addr'],
                            '--compression', compression, '--report', str(work / 'warmup.json')],
                           cwd=work, timeout=min(timeout, 30), log=work / 'warmup.log', env=env)
                if warm['returncode'] != 0 or warm['timed_out']:
                    raise ValueError('server warmup failed')
                warm_report = json.loads((work / 'warmup.json').read_text())
                if warm_report.get('status') != 'PASS':
                    raise ValueError('server warmup did not verify exchanges')
                baseline = snapshot(server.process.pid)
                command = [str(rust), '--role', 'grpc-thrift-load-client', '--addr', ready['addr'],
                           '--duration-secs', str(duration), '--concurrency', str(concurrency),
                           '--payload-bytes', str(payload_bytes), '--cooldown-secs', str(cooldown),
                           '--compression', compression, '--report', str(work / 'client.json')]
                row['command'] = command
                client = Server(command, cwd=work, log=work / 'client.log', env=env,
                                ready_event='load-started')
                client.start(min(timeout, 30))
                deadline = time.monotonic() + timeout
                samples = [baseline]
                while client.process.poll() is None:
                    if time.monotonic() >= deadline:
                        raise ValueError('load client exceeded bounded runtime')
                    samples.append(snapshot(server.process.pid))
                    time.sleep(1)
                if client.process.returncode != 0:
                    raise ValueError('load client failed; see client.log')
                report = json.loads((work / 'client.json').read_text())
                if report['status'] != 'PASS' or report['calls'] == 0 or report['errors'] != 0:
                    raise ValueError('load exchanges failed')
                after = snapshot(server.process.pid)
                resource = {'limits': limits, 'cold': cold, 'warmup': warm, 'baseline': baseline, 'after': after, 'samples': samples}
                (work / 'server-resources.json').write_text(json.dumps(resource, indent=2) + '\n')
                client_base, client_after = report['baseline'], report['after']
                client_peak_fds = max(value['fds'] for value in report['samples'])
                client_peak_tasks = max(value['tasks'] for value in report['samples'])
                checks = {
                    'client_peak_fds': client_peak_fds <= client_base['fds'] + limits['client_extra_fds'],
                    'client_peak_tasks': client_peak_tasks <= client_base['tasks'] + limits['client_extra_tasks'],
                    'server_peak_fds': max(value['fds'] for value in samples) <= baseline['fds'] + limits['server_extra_fds'],
                }
                if cooldown >= limits['pool_idle_ttl_secs'] + limits['scheduler_slack_secs']:
                    checks.update(
                        client_recovered_fds=client_after['fds'] <= client_base['fds'] + limits['client_recovered_extra_fds'],
                        client_recovered_tasks=client_after['tasks'] <= client_base['tasks'] + limits['client_recovered_extra_tasks'],
                        server_recovered_fds=after['fds'] <= baseline['fds'] + limits['server_recovered_extra_fds'])
                row.update(status='PASS' if all(checks.values()) else 'FAIL', resource_checks=checks,
                           recovery='VERIFIED' if cooldown >= 95 else 'NOT_EXECUTED_SHORT_COOLDOWN',
                           client_report=str(work / 'client.json'), server_resources=str(work / 'server-resources.json'),
                           calls=report['calls'], errors=report['errors'], qps=report['qps'],
                           latency_us=report['latency_us'],
                           latency_sample='latest 8192 calls per worker; not a full-run histogram')
            except (OSError, ValueError, RuntimeError, KeyError) as error:
                row['error'] = str(error)
            finally:
                if client is not None:
                    client.close()
                if server is not None:
                    server.close()
        rows.append(row)
    after_hashes = {key: hashlib.sha256(path.read_bytes()).hexdigest() for key, path in (('go', go), ('rust', rust))}
    if hashes != after_hashes:
        rows.append({'status': 'FAIL', 'error': 'endpoint binary changed during measurement'})
    report = {'schemaVersion': 1, 'suite': 'grpc-thrift-load', 'protocol': 'grpc-thrift',
              'scope': 'short bidi, 10% response Drop; resource gate only; no throughput regression claim',
              'duration_secs': duration, 'cooldown_secs': cooldown, 'compression': compression,
              'status': 'PASS' if rows and all(row['status'] == 'PASS' for row in rows) else 'FAIL',
              'binaries': hashes, 'binaries_after': after_hashes, 'cases': rows}
    (output / 'report.json').write_text(json.dumps(report, indent=2) + '\n')
    return report
