"""Control-driven gRPC cancellation with actual handler/producer/child exit evidence."""
from __future__ import annotations

import hashlib
import json
import os
from pathlib import Path
import queue
import signal
from subprocess import TimeoutExpired
import tempfile
import time

from .process import Server, run
from .grpc_network import EXPECTED

SCENARIOS = ('cancel-before', 'cancel-after', 'half-close-cancel',
             'drop-response', 'deadline-before', 'deadline-after', 'disconnect')


def exited(server, identity, implementation, timeout):
    expected = {'handler-producer', 'managed-child'} if implementation == 'rust' else {'handler'}
    observed = []
    deadline = time.monotonic() + timeout
    while expected:
        try:
            event = server.messages.get(timeout=max(0.001, deadline - time.monotonic()))
        except queue.Empty as error:
            raise ValueError('owned handler/producer/child did not exit: ' + ','.join(sorted(expected))) from error
        if event.get('event') == 'exited' and event.get('id') == identity:
            kind = event.get('kind')
            if kind not in expected:
                raise ValueError('duplicate or unexpected owned-task exit: ' + str(kind))
            expected.remove(kind)
            observed.append(event)
        if time.monotonic() >= deadline:
            raise ValueError('timed out collecting actual exit events')
    return observed


def lifecycle(*, go: Path, rust: Path, output: Path, timeout: float,
              transports: list[str], direction='both', env=None, multihop=False) -> dict:
    output.mkdir(parents=True, exist_ok=True)
    rows = []
    hashes = {'go': hashlib.sha256(go.read_bytes()).hexdigest(), 'rust': hashlib.sha256(rust.read_bytes()).hexdigest()}
    directions = ['go-to-rust', 'rust-to-go'] if direction == 'both' else [direction]
    for transport in transports:
        if transport not in ('tcp', 'uds'):
            raise ValueError('gRPC lifecycle supports tcp/uds only')
        for selected in directions:
            server_bin, client_bin = (rust, go) if selected == 'go-to-rust' else (go, rust)
            server_kind = 'rust' if selected == 'go-to-rust' else 'go'
            work = output / (transport + '-' + selected)
            work.mkdir(parents=True, exist_ok=True)
            with tempfile.TemporaryDirectory(prefix='grpc-') as sockets:
                address = 'tcp://127.0.0.1:0' if transport == 'tcp' else 'unix://' + sockets + '/rpc.sock'
                downstream = None
                command = [str(server_bin), '--role', 'grpc-thrift-server', '--addr', address]
                server = None
                try:
                    if multihop:
                        downstream_address = 'tcp://127.0.0.1:0' if transport == 'tcp' else 'unix://' + sockets + '/downstream.sock'
                        downstream = Server([str(client_bin), '--role', 'grpc-thrift-server', '--addr', downstream_address], cwd=work, log=work / 'downstream.log', env=env, capture_events=True)
                        downstream_ready = downstream.start(timeout)
                        command.extend(['--forward', downstream_ready['addr']])
                    server = Server(command, cwd=work, log=work / 'server.log', env=env, capture_events=True)
                    ready = server.start(timeout)
                    for scenario in SCENARIOS:
                        identity = '-'.join((transport, selected, scenario))
                        case = work / scenario
                        case.mkdir(parents=True, exist_ok=True)
                        command = [str(client_bin), '--role', 'grpc-thrift-lifecycle-client',
                                   '--addr', ready['addr'], '--scenario', scenario,
                                   '--id', identity, '--timeout-ms', '1000',
                                   '--report', str(case / 'client.json')]
                        row = {'name': identity, 'transport': transport, 'direction': selected,
                               'scenario': scenario, 'command': command, 'status': 'FAIL'}
                        client = Server(command, cwd=case, log=case / 'client.log', env=env,
                                        stdin=True, ready_event='armed', capture_events=True)
                        try:
                            row['armed'] = client.start(timeout)
                            started = server.wait_event('started', timeout)
                            if started.get('id') != identity:
                                raise ValueError('server started a different call')
                            if started.get('transient') != 'seed' or started.get('persistent') != 'seed':
                                raise ValueError('single-hop transient or persistent seed missing')
                            row['started'] = started
                            if multihop:
                                child = downstream.wait_event('started', timeout)
                                if child.get('id') != identity or child.get('persistent') != 'seed':
                                    raise ValueError('downstream identity or persistent metadata differs')
                                if server_kind == 'rust' and child.get('transient'):
                                    raise ValueError('Rust forwarded an upstream transient beyond one hop')
                                if server_kind == 'go' and child.get('transient') == 'seed':
                                    row['compatibility_difference'] = 'Go default Legacy can forward an upstream transient; Rust always uses single-hop semantics, with no compatibility flag'
                                if scenario.startswith('deadline-') and not (0 < child.get('remaining_ms', -1) <= started.get('remaining_ms', 0) - 50):
                                    raise ValueError('downstream deadline did not deduct the intentional queue time')
                                row['downstream_started'] = child
                            if scenario == 'disconnect':
                                os.killpg(client.process.pid, signal.SIGKILL)
                            elif not scenario.startswith('deadline-'):
                                client.send_line('cancel')
                            code = client.process.wait(timeout=timeout)
                            client.reader.join(timeout=5)
                            if client.reader.is_alive():
                                raise ValueError('client log reader did not finish')
                            if scenario == 'disconnect':
                                if code != -signal.SIGKILL:
                                    raise ValueError('disconnect fixture did not kill the client')
                            else:
                                if code != 0:
                                    raise ValueError('lifecycle client exited with code {}; see client.log'.format(code))
                                actual = json.loads((case / 'client.json').read_text())
                                expected_code = 4 if scenario.startswith('deadline-') else 1
                                if actual.get('status') != 'PASS' or actual.get('code') != expected_code:
                                    raise ValueError('client did not verify its cancellation result')
                                row['actual'] = actual
                            row['exited'] = exited(server, identity, server_kind, min(timeout, 5))
                            if multihop:
                                row['downstream_exited'] = exited(downstream, identity, 'go' if server_kind == 'rust' else 'rust', min(timeout, 5))
                            row['status'] = 'PASS'
                        except (OSError, ValueError, RuntimeError, TimeoutExpired) as error:
                            row['error'] = str(error)
                        finally:
                            client.close()
                        rows.append(row)
                    if multihop:
                        work_biz = work / 'business-and-declared-errors'
                        work_biz.mkdir(parents=True, exist_ok=True)
                        row = {'name': transport + '-' + selected + '-business', 'transport': transport,
                               'direction': selected, 'scenario': 'business-and-declared-errors', 'status': 'FAIL'}
                        try:
                            result = run([str(client_bin), '--role', 'grpc-thrift-client', '--addr', ready['addr'],
                                          '--report', str(work_biz / 'client.json')], cwd=work_biz, timeout=timeout,
                                         log=work_biz / 'client.log', env=env)
                            row['command'] = result
                            if result['returncode'] != 0 or result['timed_out']:
                                raise ValueError('three-hop business client failed')
                            observed = json.loads((work_biz / 'client.json').read_text())
                            if observed.get('status') != 'PASS' or set(observed.get('cases', [])) != EXPECTED:
                                raise ValueError('three-hop business/declared exception assertions missing')
                            row['forwarded'] = server.wait_event('biz-forwarded', min(timeout, 5))
                            row['emitted'] = downstream.wait_event('biz-emitted', min(timeout, 5))
                            row.update(status='PASS', cases=observed['cases'])
                        except (OSError, ValueError, RuntimeError) as error:
                            row['error'] = str(error)
                        rows.append(row)
                except (OSError, ValueError, RuntimeError, TimeoutExpired) as error:
                    rows.append({'transport': transport, 'direction': selected, 'status': 'FAIL', 'error': str(error)})
                finally:
                    if server is not None:
                        server.close()
                    if downstream is not None:
                        downstream.close()
    after = {'go': hashlib.sha256(go.read_bytes()).hexdigest(), 'rust': hashlib.sha256(rust.read_bytes()).hexdigest()}
    if after != hashes:
        rows.append({'status': 'FAIL', 'error': 'endpoint binary changed during the suite'})
    report = {'schemaVersion': 1, 'suite': 'grpc-thrift-multihop' if multihop else 'grpc-thrift-lifecycle', 'protocol': 'grpc-thrift',
              'status': 'PASS' if rows and all(row['status'] == 'PASS' for row in rows) else 'FAIL',
              'cases': rows, 'binaries': hashes, 'binaries_after': after,
              'scope': 'three-process deadline/cancel, business/declared errors and fixed transient/persistent semantics' if multihop else 'direct socket cancellation/deadline/connection loss; actual owned task exit, no Mesh'}
    (output / 'report.json').write_text(json.dumps(report, indent=2) + '\n')
    return report
