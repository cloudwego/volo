"""Real typed A -> B -> C business status propagation and recovery."""
from __future__ import annotations

import hashlib
import json
import os
from pathlib import Path
import tempfile
import time

from .network import result_event
from .process import Server, run
from .status import validate_response, validate_terminal


def forward_events(path):
    rows = []
    if path.exists():
        for line in path.read_text(errors='replace').splitlines():
            try:
                value = json.loads(line)
            except ValueError:
                continue
            if isinstance(value, dict) and value.get('event') == 'error_forward_finish':
                rows.append(value)
    return rows


def error_multihop(*, go: Path, rust: Path, output: Path, transports: list[str], timeout: float,
                  profile: str, topologies: list[str], env_overrides=None):
    output.mkdir(parents=True, exist_ok=True)
    environment = dict(os.environ, **(env_overrides or {}))
    hashes = {str(p): hashlib.sha256(p.read_bytes()).hexdigest() for p in (go, rust)}
    cases = []
    for topology in topologies:
        if len(topology) != 3 or any(kind not in 'gr' for kind in topology):
            raise ValueError('business topology must have three g/r letters')
        for transport in transports:
            identity = topology + '-' + transport + '-biz'
            directory = output / identity
            directory.mkdir(parents=True, exist_ok=True)
            row = {'name': identity, 'topology': topology, 'transport': transport, 'status': 'PASS'}
            servers = []
            with tempfile.TemporaryDirectory(prefix='ttbiz-', dir='/tmp') as sockets:
                try:
                    if transport not in ('tcp', 'uds'):
                        raise ValueError('business multihop requires TCP or UDS')
                    target = None
                    row['ready'] = {}
                    for index, node in ((2, 'C'), (1, 'B')):
                        binary = go if topology[index] == 'g' else rust
                        addr = 'tcp://127.0.0.1:0' if transport == 'tcp' else 'unix://' + sockets + '/' + node + '.sock'
                        command = [str(binary), '--role', 'server', '--scenario', 'errors', '--node', node, '--addr', addr]
                        if target:
                            command += ['--forward', target]
                        server = Server(command, cwd=directory, log=directory / (node + '.log'), env=environment)
                        servers.append(server)
                        ready = server.start(timeout)
                        if topology[index] == 'g' and ready.get('profile') != profile:
                            raise ValueError('Go server profile differs')
                        row['ready'][node] = ready
                        target = ready['addr']
                    client = go if topology[0] == 'g' else rust
                    row['execution'] = execution = run(
                        [str(client), '--role', 'error-client', '--addr', target,
                         '--mode', 'biz', '--payload', identity, '--pool', 'mux'],
                        cwd=directory, log=directory / 'client.log', env=environment, timeout=timeout)
                    row['actual'] = actual = result_event(directory / 'client.log')
                    if (execution['returncode'] != 0 or execution['timed_out'] or actual.get('ok') is not True
                            or actual.get('api') != 'errors' or actual.get('id') != identity or actual.get('mode') != 'biz'
                            or actual.get('probe_ok') is not True):
                        raise ValueError('business propagation or recovery failed')
                    if topology[0] == 'g' and actual.get('profile') != profile:
                        raise ValueError('Go client profile differs')
                    responses = actual.get('responses') or []
                    if len(responses) != 1:
                        raise ValueError('missing actual DATA before business terminal')
                    validate_response(responses[0], identity, 'biz')
                    validate_terminal(actual.get('terminal', {}), 'biz', 'go' if topology[0] == 'g' else 'rust',
                                      'go' if topology[1] == 'g' else 'rust')
                    for field, key, value in [('header', 'interop-error-header', 'header-value'),
                                              ('trailer', 'interop-error-trailer', 'trailer-value')]:
                        meta = actual.get(field) or {}
                        if meta.get('string', meta).get(key) != value:
                            raise ValueError('forwarding lost custom ' + field)
                    deadline = time.monotonic() + min(timeout, 2)
                    while True:
                        observed = forward_events(directory / 'B.log')
                        if len(observed) >= 2:
                            break
                        if time.monotonic() >= deadline:
                            raise ValueError('missing actual child completion and recovery events')
                        time.sleep(0.01)
                    row['forward_events'] = observed
                    for call_id, kind in ((identity, 'biz'), (identity + '-probe', 'ok')):
                        matches = [event for event in observed if event.get('id') == call_id]
                        if len(matches) != 1:
                            raise ValueError('forwarding call did not finish exactly once')
                        event = matches[0]
                        if kind == 'biz':
                            validate_terminal(event.get('terminal', {}), 'biz', 'go' if topology[1] == 'g' else 'rust',
                                              'go' if topology[2] == 'g' else 'rust')
                        elif event.get('terminal', {}).get('kind') != 'ok':
                            raise ValueError('forwarded recovery call did not finish normally')
                        if topology[1] == 'r':
                            if (event.get('child_finished') is not True or event.get('queued_bytes') != 0
                                    or event.get('send_buffer_bytes') != 0):
                                raise ValueError('Rust forwarded child retained stream resources')
                        elif event.get('active_handlers') != 0:
                            raise ValueError('Go forwarding handler remained active')
                except Exception as error:
                    row['status'], row['reason'] = 'FAIL', str(error)
                    row['forward_events'] = forward_events(directory / 'B.log')
                finally:
                    for server in reversed(servers):
                        server.close()
            cases.append(row)
    after = {str(p): hashlib.sha256(p.read_bytes()).hexdigest() for p in (go, rust)}
    if after != hashes:
        cases.append({'status': 'FAIL', 'reason': 'endpoint binary changed'})
    report = {'schemaVersion': 1, 'suite': 'error-multihop', 'cases': cases,
              'binaries': hashes, 'binaries_after': after, 'environment_overrides': env_overrides or {},
              'scope': 'actual typed A/B/C business code/message/extra, DATA, Header/Trailer, child finish and recovery; does not assert transparent forwarding of Go-private ApplicationException causes'}
    (output / 'report.json').write_text(json.dumps(report, ensure_ascii=False, indent=2) + '\n')
    return report
