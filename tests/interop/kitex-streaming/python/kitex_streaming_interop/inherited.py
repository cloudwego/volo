"""Actual Child clients/servers sharing inherited unary and streaming handlers."""
from __future__ import annotations

from collections import Counter
import hashlib
import json
import os
from pathlib import Path
import tempfile

from .network import result_event
from .process import Server, run


METHODS = ['Plain', 'Unary', 'Upload', 'Watch', 'Chat', 'WatchResult']


def expected_calls(count):
    return [{'method': method, 'responses': [(index + 1) * 10000 + value for value in
                                            ([count] if index < 3 else range(count))]}
            for index, method in enumerate(METHODS)]


def inherited_service(*, go: Path, rust: Path, output: Path, transports: list[str], timeout: float,
                      profile: str, env_overrides=None):
    output.mkdir(parents=True, exist_ok=True)
    environment = dict(os.environ, **(env_overrides or {}))
    hashes = {str(p): hashlib.sha256(p.read_bytes()).hexdigest() for p in (go, rust)}
    cases = []
    for direction in ('go-to-rust', 'rust-to-go'):
        client_bin, server_bin = (go, rust) if direction == 'go-to-rust' else (rust, go)
        for transport in transports:
            directory = output / direction / transport
            directory.mkdir(parents=True, exist_ok=True)
            with tempfile.TemporaryDirectory(prefix='ttchild-', dir='/tmp') as sockets:
                addr = 'tcp://127.0.0.1:0' if transport == 'tcp' else 'unix://' + sockets + '/child.sock'
                role = 'inherited-server' if direction == 'rust-to-go' else 'server'
                server = Server([str(server_bin), '--role', role, '--addr', addr], cwd=directory,
                                log=directory / 'server.log', env=environment)
                try:
                    if transport not in ('tcp', 'uds'):
                        raise ValueError('inherited suite requires TCP or UDS')
                    ready = server.start(timeout)
                    if ready.get('api') != 'inherited' or ready.get('service') != 'Child':
                        raise ValueError('server is not the actual generated Child service')
                    if direction == 'rust-to-go' and ready.get('profile') != profile:
                        raise ValueError('Go server profile differs')
                    expected_handlers = 0
                    for pool in ('short', 'long', 'mux'):
                        for count in (0, 1, 8):
                            name = '%s-%s' % (pool, count)
                            role = 'inherited-client' if direction == 'go-to-rust' else 'client'
                            cmd = [str(client_bin), '--role', role, '--addr', ready['addr'],
                                   '--method', 'all', '--messages', str(count), '--pool', pool]
                            row = {'name': name, 'direction': direction, 'transport': transport,
                                   'ready': ready, 'expected': expected_calls(count)}
                            try:
                                row['execution'] = execution = run(cmd, cwd=directory,
                                    log=directory / (name + '.log'), env=environment, timeout=timeout)
                                row['actual'] = actual = result_event(directory / (name + '.log'))
                                if execution['returncode'] != 0 or execution['timed_out']:
                                    raise ValueError('inherited client failed')
                                if (actual.get('ok') is not True or actual.get('api') != 'inherited'
                                        or actual.get('service') != 'Child' or actual.get('calls') != row['expected']):
                                    raise ValueError('actual inherited method responses differ')
                                if direction == 'go-to-rust' and actual.get('profile') != profile:
                                    raise ValueError('Go client profile differs')
                                expected_handlers += len(METHODS)
                                row['status'] = 'PASS'
                            except Exception as error:
                                row['status'], row['reason'] = 'FAIL', str(error)
                            cases.append(row)
                    handlers = []
                    for line in (directory / 'server.log').read_text().splitlines():
                        try:
                            event = json.loads(line)
                        except ValueError:
                            continue
                        if isinstance(event, dict) and event.get('event') == 'handler':
                            handlers.append(event)
                    shared = {'name': 'shared-handler', 'direction': direction, 'transport': transport,
                              'events': handlers, 'expected_calls': expected_handlers, 'status': 'PASS'}
                    if (expected_handlers != 54 or len(handlers) != 54
                            or sorted(event.get('call_index', -1) for event in handlers) != list(range(1, 55))
                            or Counter(event.get('method') for event in handlers) != Counter({name: 9 for name in METHODS})):
                        shared['status'], shared['reason'] = 'FAIL', 'inherited and own methods did not share one handler state'
                    cases.append(shared)
                except Exception as error:
                    cases.append({'direction': direction, 'transport': transport, 'status': 'FAIL', 'reason': str(error)})
                finally:
                    server.close()
    after = {str(p): hashlib.sha256(p.read_bytes()).hexdigest() for p in (go, rust)}
    if after != hashes:
        cases.append({'status': 'FAIL', 'reason': 'endpoint binary changed'})
    report = {'schemaVersion': 1, 'suite': 'inherited-service', 'cases': cases,
              'binaries': hashes, 'binaries_after': after, 'environment_overrides': env_overrides or {},
              'scope': 'actual audit.Child extends audit_parent.Parent; one client calls six inherited/own methods at one address; TCP/UDS and three pool configurations; handler state shared across unary/streaming; pool reuse/capacity is a separate gate'}
    (output / 'report.json').write_text(json.dumps(report, ensure_ascii=False, indent=2) + '\n')
    return report
