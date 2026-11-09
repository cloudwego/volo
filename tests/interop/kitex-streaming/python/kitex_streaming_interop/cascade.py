"""Actual three-hop cancellation without manually linked fixture contexts."""
from __future__ import annotations

import hashlib
import json
import os
from pathlib import Path
import tempfile
import time

from .network import result_event
from .process import Server, run


def events(path):
    result = []
    if path.exists():
        for line in path.read_text(errors='replace').splitlines():
            try:
                row = json.loads(line)
            except ValueError:
                continue
            if isinstance(row, dict) and row.get('event') in ('cascade_finish', 'cascade_child_finish'):
                result.append(row)
    return result


def validate_client(observed, identity, mode):
    expected = 12007 if mode == 'cancel' else 12015
    if not observed.get('ok') or observed.get('api') != 'cascade' or observed.get('id') != identity:
        raise ValueError('missing successful cascade result or wrong identity')
    if observed.get('mode') != mode or observed.get('code') != expected:
        raise ValueError('local cancellation code differs: %r' % observed)
    if observed.get('ack') is not True or observed.get('probe_ok') is not True:
        raise ValueError('must acknowledge a live downstream and recover through the same configured client')
    if mode == 'deadline' and not 2500 <= observed.get('elapsed_ms', -1) < 8000:
        raise ValueError('deadline did not expire around the configured three seconds')


def validate_server(observed, node, identity, mode):
    expected = 12007 if mode == 'cancel' else 12015
    terminal = [row for row in observed if row.get('event') == 'cascade_finish' and row.get('id') == identity]
    probe = [row for row in observed if row.get('event') == 'cascade_finish' and row.get('id') == identity + '-probe']
    if len(terminal) != 1 or len(probe) != 1:
        raise ValueError('expected one canceled and one recovery handler finish at ' + node)
    current = terminal[0]
    if current.get('node') != node or current.get('code') != expected:
        raise ValueError('server cancellation identity/code differs: %r' % current)
    if current.get('ttscp') != (['A'] if node == 'B' else ['A', 'B']):
        raise ValueError('actual cancellation path differs: %r' % current)
    if any(row.get('has_deadline') is not False for row in terminal + probe):
        raise ValueError('local deadline unexpectedly propagated to server context')
    if probe[0].get('code') != 0 or probe[0].get('error'):
        raise ValueError('recovery handler did not finish normally')
    finishes = [row for row in observed if row.get('event') == 'cascade_finish']
    if finishes[-1].get('active_handlers') != 0:
        raise ValueError('handler count did not return to zero at ' + node)
    if node == 'B':
        children = [row for row in observed if row.get('event') == 'cascade_child_finish' and row.get('id') == identity]
        if len(children) != 1 or children[0].get('code') != expected:
            raise ValueError('B did not release exactly one canceled child call')


def cascading(*, go: Path, rust: Path, output: Path, transports: list[str], timeout: float,
              profile: str, topologies: list[str], env_overrides: dict[str, str] | None = None) -> dict:
    output.mkdir(parents=True, exist_ok=True)
    environment = os.environ.copy()
    environment.update(env_overrides or {})
    binaries = {str(p): hashlib.sha256(p.read_bytes()).hexdigest() for p in (go, rust)}
    cases = []
    for topology in topologies:
        if len(topology) != 3 or any(kind not in 'gr' for kind in topology):
            raise ValueError('topology must contain three g/r letters')
        for transport in transports:
            for mode in ['cancel', 'deadline']:
                identity = '-'.join([topology, transport, mode])
                directory = output / identity
                directory.mkdir(parents=True, exist_ok=True)
                row = {'name': identity, 'topology': topology, 'transport': transport, 'mode': mode}
                servers = []
                with tempfile.TemporaryDirectory(prefix='ttcancel-', dir='/tmp') as sockets:
                    try:
                        if transport not in ('tcp', 'uds'):
                            raise ValueError('cascade suite requires TCP or UDS')
                        target = None
                        ready_nodes = {}
                        for index, node in [(2, 'C'), (1, 'B')]:
                            kind = topology[index]
                            binary = go if kind == 'g' else rust
                            address = ('tcp://127.0.0.1:0' if transport == 'tcp' else
                                       'unix://' + sockets + '/' + node + '.sock')
                            command = [str(binary), '--role', 'server', '--addr', address,
                                       '--scenario', 'cascade', '--node', node]
                            if target:
                                command += ['--forward', target]
                            server = Server(command, cwd=directory, log=directory / (node + '.log'), env=environment)
                            servers.append(server)
                            ready = server.start(timeout)
                            if kind == 'g' and ready.get('profile') != profile:
                                raise ValueError('Go server profile differs')
                            ready_nodes[node] = ready
                            target = ready['addr']
                        client = go if topology[0] == 'g' else rust
                        log = directory / 'client.log'
                        execution = run([str(client), '--role', 'cascade-client', '--addr', target,
                                         '--mode', mode, '--payload', identity, '--pool', 'mux',
                                         '--report', str(directory / 'client.json')],
                                        cwd=directory, timeout=timeout, log=log, env=environment)
                        row['execution'], row['ready'] = execution, ready_nodes
                        observed = result_event(log)
                        row['actual'] = observed
                        if execution['returncode'] != 0 or execution['timed_out']:
                            raise ValueError('cascade client failed or timed out')
                        if topology[0] == 'g' and observed.get('profile') != profile:
                            raise ValueError('Go client profile differs')
                        validate_client(observed, identity, mode)
                        # Wait for concrete finish events, bounded by a deadline. A
                        # successful response alone does not prove handler cleanup.
                        deadline = time.monotonic() + min(timeout, 5)
                        while True:
                            server_events = {node: events(directory / (node + '.log')) for node in ('B', 'C')}
                            if all(sum(event.get('event') == 'cascade_finish' for event in items) >= 2
                                   for items in server_events.values()):
                                break
                            if any(server.process.poll() is not None for server in servers):
                                raise ValueError('server exited before cancellation cleanup')
                            if time.monotonic() >= deadline:
                                raise ValueError('missing cancellation/recovery finish events')
                            time.sleep(0.02)
                        row['server_events'] = server_events
                        for node, observed in server_events.items():
                            validate_server(observed, node, identity, mode)
                        row['status'] = 'PASS'
                    except Exception as error:
                        row['status'], row['reason'] = 'FAIL', str(error)
                        row['server_events'] = {node: events(directory / (node + '.log')) for node in ('B', 'C')}
                    finally:
                        for server in reversed(servers):
                            server.close()
                cases.append(row)
    after = {str(p): hashlib.sha256(p.read_bytes()).hexdigest() for p in (go, rust)}
    if binaries != after:
        cases.append({'status': 'FAIL', 'reason': 'endpoint binaries changed during cascade suite'})
    report = {'schemaVersion': 1, 'suite': 'cascade', 'cases': cases, 'binaries': binaries,
              'binaries_after': after, 'environment_overrides': env_overrides or {},
              'scope': 'actual A->B->C Bidi, explicit cancel and local 3s deadline; framework-linked child contexts, receiver error paths, handler/child completion, recovery call; resource budgets and soak remain separate gates'}
    (output / 'report.json').write_text(json.dumps(report, ensure_ascii=False, indent=2) + '\n')
    return report
