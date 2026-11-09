"""Three actual endpoints verify transient/persistent propagation per hop."""
from __future__ import annotations

import hashlib
import json
import os
from pathlib import Path
import tempfile

from .network import result_event
from .process import Server, run
from .wire import header, send


def seed_for(identity, mutation):
    reset, persistent = {}, {}
    if mutation == 'reset-transient':
        reset['interop-t'] = identity + '-reset'
    elif mutation == 'overwrite-and-add':
        persistent['interop-p'] = identity + '-new-p'
        reset['interop-b'] = identity + '-from-b'
    seed = {'persistent': {'interop-p': identity}, 'transient': {'interop-t': identity},
            'request': {'schemaVersion': 1, 'id': identity, 'reset_transient': reset,
                        'set_persistent': persistent, 'observations': []}}
    if mutation == 'empty-key-value':
        seed['persistent'].update({'': 'ignored', 'interop-empty-p': ''})
        seed['transient'].update({'': 'ignored', 'interop-empty-t': ''})
    elif mutation == 'same-key-priority':
        seed['persistent']['interop-same'] = identity + '-persistent'
        seed['transient']['interop-same'] = identity + '-transient'
        reset['interop-same'] = identity + '-reset-at-b'
    return seed


def nonempty(values):
    return {key: value for key, value in values.items() if key.startswith('interop-') and value}


def validate(seed, observed, topology, mode):
    if not observed.get('ok') or observed.get('api') != 'metainfo':
        raise ValueError('missing successful metainfo result')
    nodes = observed.get('observations', [])
    if len(nodes) != 2 or [node.get('node') for node in nodes] != ['B', 'C']:
        raise ValueError('expected exactly B then C observations')
    persistent = nonempty(seed['persistent'])
    transient = nonempty(seed['transient'])
    downstream_persistent = dict(persistent, **nonempty(seed['request']['set_persistent']))
    legacy = topology[1] == 'g' and mode in ('legacy', 'default')
    downstream_transient = dict(transient) if legacy else {}
    downstream_transient.update(nonempty(seed['request']['reset_transient']))
    upstream_visible = dict(seed.get('upstream', {})) if legacy else {}
    upstream_visible.update(transient)
    expected = [(persistent, upstream_visible), (downstream_persistent, downstream_transient)]
    for index, (node, (want_p, want_t)) in enumerate(zip(nodes, expected)):
        if node.get('persistent') != want_p or node.get('transient') != want_t:
            raise ValueError('%s metainfo differs: expected persistent=%r transient=%r, got %r' %
                             (node['node'], want_p, want_t, node))
        if node.get('has_deadline') is not False:
            raise ValueError('client local timeout unexpectedly became a server deadline')
        strings, integers = node.get('header_string'), node.get('header_int')
        if topology[index + 1] == 'r' and (strings is None or integers is None):
            raise ValueError('Rust receiver must report actual incoming HEADER metadata')
        if strings is not None:
            wire_p = {key[len('RPC_PERSIST_'):]: value for key, value in strings.items()
                      if key.startswith('RPC_PERSIST_interop-')}
            wire_t = {key[len('RPC_TRANSIT_'):]: value for key, value in strings.items()
                      if key.startswith('RPC_TRANSIT_interop-')}
            if wire_p != want_p or wire_t != want_t:
                raise ValueError('received raw metainfo HEADER differs from handler observation')
            if any('deadline' in key.lower() or 'timeout' in key.lower() for key in strings):
                raise ValueError('unexpected timeout/deadline string header')
        if integers is not None and {'12', '13', '17'}.intersection(integers):
            raise ValueError('unexpected RPC/read/connect timeout int header')


def raw_upstream_case(go, target, directory, timeout, environment, topology, mode, transport, ready_nodes):
    identity = topology + '-' + mode + '-' + transport + '-raw-upstream'
    seed = seed_for(identity, 'forward-only')
    seed['upstream'] = {'interop-upstream': 'expired', 'interop-t': 'shadowed-by-current'}
    metadata = {'isn': 'Interop', 'unrecognized-key': 'preserved-at-receiver'}
    for prefix, values in [('RPC_PERSIST_', seed['persistent']), ('RPC_TRANSIT_', seed['transient']),
                           ('RPC_TRANSIT_UPSTREAM_', seed['upstream'])]:
        metadata.update({prefix + key: value for key, value in values.items()})
    actions = [header(metadata=metadata), send(1, 3, message={'value': json.dumps(seed['request']), 'seq': 1}),
               send(1, 4), {'op': 'read', 'until_sid': 1, 'timeout_ms': 3000}]
    source = directory / 'raw-upstream.input.json'
    source.write_text(json.dumps({'schemaVersion': 1, 'actions': actions}, indent=2) + '\n')
    log = directory / 'raw-upstream.log'
    execution = run([str(go), '--role', 'wire', '--addr', target, '--input', str(source),
                     '--report', str(directory / 'raw-upstream.wire.json')],
                    cwd=directory, timeout=timeout, log=log, env=environment)
    row = {'name': 'raw-upstream', 'topology': topology, 'mode': mode, 'transport': transport,
           'api': 'go-wire', 'concurrency': 1, 'execution': execution, 'input': str(source), 'ready': ready_nodes}
    try:
        wire = result_event(log)
        if execution['returncode'] != 0 or execution['timed_out'] or not wire.get('ok'):
            raise ValueError('raw upstream client failed')
        frames = wire['actions'][-1]['frames']
        if [frame['int_metadata']['27'] for frame in frames] != ['2', '3', '4']:
            raise ValueError('raw upstream response did not produce exactly HEADER/DATA/TRAILER')
        if any(frame['sid'] != 1 or frame.get('exception') for frame in frames):
            raise ValueError('raw upstream response has wrong sid or exception')
        message = frames[1]['message']
        response = json.loads(message['value'])
        if response['id'] != identity or response['schemaVersion'] != 1 or message['seq'] != 1:
            raise ValueError('raw upstream response identity differs')
        observed = dict(response, ok=True, api='metainfo')
        validate(seed, observed, topology, mode)
        if topology[1] == 'r' and observed['observations'][0]['header_string'].get('unrecognized-key') != 'preserved-at-receiver':
            raise ValueError('Rust lost an unknown incoming metadata key')
        row['actual'], row['status'] = observed, 'PASS'
    except (KeyError, ValueError) as error:
        row['status'], row['reason'] = 'FAIL', str(error)
    return row


def multihop(*, go: Path, rust: Path, output: Path, transports: list[str], timeout: float,
             profile: str, topologies: list[str], env_overrides: dict[str, str] | None = None,
             concurrency: int = 1) -> dict:
    output.mkdir(parents=True, exist_ok=True)
    environment = os.environ.copy()
    environment.update(env_overrides or {})
    binaries = {str(p): hashlib.sha256(p.read_bytes()).hexdigest() for p in (go, rust)}
    cases = []
    for topology in topologies:
        if len(topology) != 3 or any(kind not in 'gr' for kind in topology):
            raise ValueError('topology must contain three g/r letters for client A, server B and server C')
        modes = ['single-hop', 'legacy', 'default'] if topology[1] == 'g' else ['single-hop']
        for mode in modes:
            for transport in transports:
                directory = output / topology / mode / transport
                directory.mkdir(parents=True, exist_ok=True)
                if transport not in ('tcp', 'uds'):
                    cases.append({'topology': topology, 'mode': mode, 'transport': transport,
                                  'status': 'INCOMPLETE', 'reason': 'requires TCP or UDS'})
                    continue
                servers = []
                with tempfile.TemporaryDirectory(prefix='ttmeta-', dir='/tmp') as socket_dir:
                    try:
                        ready_nodes = {}
                        target = None
                        for index, node in [(2, 'C'), (1, 'B')]:
                            kind = topology[index]
                            binary = go if kind == 'g' else rust
                            address = ('tcp://127.0.0.1:0' if transport == 'tcp' else
                                       'unix://' + socket_dir + '/' + node + '.sock')
                            command = [str(binary), '--role', 'server', '--addr', address,
                                       '--scenario', 'metainfo', '--node', node]
                            if kind == 'g':
                                command += ['--metainfo-mode', mode if node == 'B' else 'single-hop']
                            if target:
                                command += ['--forward', target]
                            server = Server(command, cwd=directory, log=directory / (node + '.log'), env=environment)
                            servers.append(server)
                            ready = server.start(timeout)
                            if kind == 'g' and ready.get('profile') != profile:
                                raise ValueError('Go server profile differs from requested profile')
                            target = ready['addr']
                            ready_nodes[node] = ready
                        client = go if topology[0] == 'g' else rust
                        for mutation in ['forward-only', 'reset-transient', 'overwrite-and-add',
                                         'empty-key-value', 'same-key-priority']:
                            identity = topology + '-' + mode + '-' + transport + '-' + mutation
                            seeds = [seed_for(identity + '-stream-' + str(index), mutation)
                                     for index in range(concurrency)]
                            path = directory / (mutation + '.input.json')
                            path.write_text(json.dumps(seeds if concurrency > 1 else seeds[0], ensure_ascii=False, indent=2) + '\n')
                            log = directory / (mutation + '.log')
                            role = 'metadata-batch-client' if concurrency > 1 else 'metadata-client'
                            command = [str(client), '--role', role, '--addr', target,
                                       '--input', str(path), '--pool', 'mux', '--report', str(directory / (mutation + '.json'))]
                            execution = run(command, cwd=directory, timeout=timeout, log=log, env=environment)
                            row = {'topology': topology, 'mode': mode, 'transport': transport, 'name': mutation,
                                   'execution': execution, 'ready': ready_nodes, 'input': str(path), 'concurrency': concurrency,
                                   'expected_legacy_leak': topology[1] == 'g' and mode != 'single-hop'}
                            try:
                                observed = result_event(log)
                                row['actual'] = observed
                                if execution['returncode'] != 0 or execution['timed_out']:
                                    raise ValueError('metadata client failed or timed out')
                                if topology[0] == 'g' and observed.get('profile') != profile:
                                    raise ValueError('Go client profile differs from requested profile')
                                results = observed.get('results', []) if concurrency > 1 else [observed]
                                if concurrency > 1 and (not observed.get('ok') or observed.get('api') != 'metainfo-batch'):
                                    raise ValueError('expected a successful batch using one configured client')
                                if len(results) != len(seeds):
                                    raise ValueError('metainfo result count differs from request count')
                                for seed, result in zip(seeds, results):
                                    if concurrency > 1 and result.get('id') != seed['request']['id']:
                                        raise ValueError('batch result identity/order differs')
                                    validate(seed, result, topology, mode)
                                row['status'] = 'PASS'
                            except ValueError as error:
                                row['status'], row['reason'] = 'FAIL', str(error)
                            cases.append(row)
                        if topology[0] == 'g':
                            cases.append(raw_upstream_case(go, target, directory, timeout, environment,
                                                           topology, mode, transport, ready_nodes))
                    except Exception as error:
                        cases.append({'topology': topology, 'mode': mode, 'transport': transport,
                                      'status': 'FAIL', 'reason': str(error), 'logs': str(directory)})
                    finally:
                        for server in reversed(servers):
                            server.close()
    after = {str(p): hashlib.sha256(p.read_bytes()).hexdigest() for p in (go, rust)}
    if binaries != after:
        cases.append({'status': 'FAIL', 'reason': 'endpoint binaries changed during metainfo suite'})
    report = {'schemaVersion': 1, 'suite': 'metainfo', 'cases': cases, 'binaries': binaries,
              'binaries_after': after, 'environment_overrides': env_overrides or {},
              'concurrency': concurrency,
              'scope': 'actual A->B->C typed Bidi; five metadata variants plus Go codec raw upstream headers; Go Legacy/default difference; Rust raw HEADER and server deadline observation; batch shares one configured mux client, physical pool reuse and cascading cancellation remain separate gates'}
    (output / 'report.json').write_text(json.dumps(report, ensure_ascii=False, indent=2) + '\n')
    return report
