"""Go TTHeader codec META frames observed through a custom context fixture."""
from __future__ import annotations

import hashlib
import json
import os
from pathlib import Path
import tempfile

from .network import result_event
from .process import Server, run
from .wire import send


def scenarios():
    first = {'tc': 'actual-cluster', 'ti': 'actual-idc', 'te': 'actual-env',
             'rip': '127.0.0.1:9988'}
    for name, late, before_header, remote in [
            ('complete', None, False, first['rip']),
            ('before-header', None, True, first['rip']),
            ('late-empty', {'tc': '', 'ti': '', 'te': '', 'rip': ''}, False, first['rip']),
            ('late-invalid-address', {'tc': '', 'ti': '', 'te': '', 'rip': 'invalid-remote-address'}, False,
             'invalid-remote-address'),
            ('late-unknown', {'unknown-late': 'opaque'}, False, first['rip']),
            ('ipv6', None, False, '[::1]:9988'),
            ('derived-idc', {'rip': '127.0.0.2:9999'}, False, '127.0.0.2:9999'),
            ('empty-ti-derived-idc', {'ti': '', 'rip': '127.0.0.2:9999'}, False, '127.0.0.2:9999')]:
        meta = send(0, 1, metadata=dict(first, rip=remote if name == 'ipv6' else first['rip']))
        meta['frame']['metadata_hex'] = {'binary-unknown': 'ff006d657461'}
        header = send(0, 2)
        actions = [meta, header] if before_header else [header, meta]
        actions.append(send(0, 3, message={'value': 'before-late-meta', 'seq': 1}))
        if late is not None:
            actions.append(send(0, 1, metadata=late))
        actions.append(send(0, 4, metadata={'test-trailer': 'done'}))
        expected = {'cluster': first['tc'], 'idc': first['ti'], 'env': first['te'],
                    'address': remote if name == 'ipv6' or 'derived-idc' in name else first['rip'],
                    'raw_remote': remote, 'backward_remote': remote, 'has_deadline': True}
        if 'derived-idc' in name:
            expected['idc'] = '$derived_idc'
        yield name, actions, expected
    empty = send(0, 1, metadata={'tc': '', 'ti': '', 'te': '', 'rip': '', 'unknown-only': 'opaque'})
    empty['frame']['metadata_hex'] = {'binary-unknown': 'ff006d657461'}
    yield 'empty-without-prior-meta', [send(0, 2), empty,
        send(0, 3, message={'value': 'before-late-meta', 'seq': 1}),
        send(0, 4, metadata={'test-trailer': 'done'})], {
        'cluster': 'destination-cluster', 'idc': 'destination-idc', 'env': None,
        'address': '$initial_address', 'raw_remote': None, 'backward_remote': None, 'has_deadline': True}


def validate_metadata(actual, actions, expected):
    if (actual.get('api') != 'internal-metadata' or actual.get('ok') is not True
            or actual.get('terminal_code') != 0 or actual.get('terminal_error')):
        raise ValueError('client did not observe successful META/DATA/EOF')
    if actual.get('responses') != [['before-late-meta', 1]]:
        raise ValueError('META changed application DATA delivery')
    wanted = dict(expected)
    if wanted.get('idc') == '$derived_idc':
        derived = actual.get('snapshot', {}).get('derived_idc')
        if not isinstance(derived, str) or derived == 'actual-idc':
            raise ValueError('independent IDC lookup did not exercise replacement of the explicit fixture value')
        wanted['idc'] = derived
    if wanted.get('address') == '$initial_address':
        wanted['address'] = actual.get('initial_snapshot', {}).get('address')
        if not wanted['address']:
            raise ValueError('missing actual initial destination address')
    if any(actual.get('snapshot', {}).get(key) != value for key, value in wanted.items()):
        raise ValueError('authoritative final context differs: %r' % actual.get('snapshot'))
    frames = [a['frame'] for a in actions if a['frame']['kind'] == 1]
    observed = actual.get('metas') or []
    if len(frames) != len(observed):
        raise ValueError('META hook count differs')
    for frame, meta in zip(frames, observed):
        wanted = {k.encode().hex(): v.encode().hex() for k, v in frame['metadata'].items()}
        wanted.update({k.encode().hex(): v for k, v in frame.get('metadata_hex', {}).items()})
        if meta.get('string_hex') != wanted or meta.get('int_hex', {}).get('27') != '31':
            raise ValueError('META hook lost raw metadata or frame type')
    if actual.get('trailer', {}).get('string_hex', {}).get('test-trailer'.encode().hex()) != 'done'.encode().hex():
        raise ValueError('final Trailer metadata was lost')


def validate_request_header(header):
    raw = header.get('raw_metadata') or {}
    integers = raw.get('int_hex') or {}
    strings = raw.get('string_hex') or {}
    expected = {'3': 'upstream.psm', '4': 'source-cluster', '5': 'source-idc',
                '6': 'Interop', '7': 'destination-cluster', '8': 'destination-idc',
                '9': 'Bidi', '27': '2'}
    for key, value in expected.items():
        if integers.get(key) != value.encode().hex():
            raise ValueError('outgoing internal integer metadata key ' + key + ' differs')
    if integers.get('777') != 'ff00726177':
        raise ValueError('outgoing unknown integer metadata key 777 lost its raw value')
    if strings.get('unknown-field'.encode().hex()) != 'ff0076616c7565':
        raise ValueError('outgoing unknown string metadata key lost its raw value')
    if strings.get('isn'.encode().hex()) != 'Interop'.encode().hex():
        raise ValueError('outgoing service routing metadata differs')
    if any(key in integers for key in ('12', '13', '17')):
        raise ValueError('local client deadline leaked into HEADER')


def metadata_wire(*, go: Path, rust: Path, output: Path, transports: list[str], timeout: float,
                  profile: str, env_overrides=None):
    output.mkdir(parents=True, exist_ok=True)
    environment = dict(os.environ, **(env_overrides or {}))
    hashes = {str(p): hashlib.sha256(p.read_bytes()).hexdigest() for p in (go, rust)}
    cases = []
    for transport in transports:
        for name, actions, expected in scenarios():
            directory = output / transport / name
            directory.mkdir(parents=True, exist_ok=True)
            input_path = directory / 'input.json'
            input_path.write_text(json.dumps({'schemaVersion': 1, 'actions': actions}, indent=2) + '\n')
            row = {'name': name, 'transport': transport, 'expected': expected, 'status': 'PASS'}
            with tempfile.TemporaryDirectory(prefix='ttmeta-', dir='/tmp') as sockets:
                addr = 'tcp://127.0.0.1:0' if transport == 'tcp' else 'unix://' + sockets + '/peer.sock'
                server = Server([str(go), '--role', 'raw-response-server', '--addr', addr,
                                 '--input', str(input_path)], cwd=directory,
                                log=directory / 'server.log', env=environment)
                try:
                    if transport not in ('tcp', 'uds'):
                        raise ValueError('metadata-wire requires TCP or UDS')
                    row['ready'] = ready = server.start(timeout)
                    if ready.get('profile') != profile or ready.get('api') != 'go-codec-response-peer':
                        raise ValueError('Go codec profile/API differs')
                    row['execution'] = execution = run(
                        [str(rust), '--role', 'metadata-wire-client', '--addr', ready['addr']],
                        cwd=directory, log=directory / 'client.log', env=environment, timeout=timeout)
                    if execution['returncode'] != 0 or execution['timed_out']:
                        raise ValueError('Rust metadata observer failed')
                    row['actual'] = actual = result_event(directory / 'client.log')
                    validate_metadata(actual, actions, expected)
                except Exception as error:
                    row['status'], row['reason'] = 'FAIL', str(error)
                finally:
                    server.close()
            events = []
            for line in (directory / 'server.log').read_text().splitlines():
                try:
                    event = json.loads(line)
                except ValueError:
                    continue
                if isinstance(event, dict) and event.get('event') == 'wire_sent':
                    events.append(event)
            row['wire_events'] = events
            if len(events) != 1 or not events[0].get('request_complete_before_response'):
                row['status'], row['reason'] = 'FAIL', 'missing Go codec wire proof'
            else:
                header = events[0]['received'][0]
                try:
                    validate_request_header(header)
                except ValueError as error:
                    row['status'], row['reason'] = 'FAIL', str(error)
            cases.append(row)
    after = {str(p): hashlib.sha256(p.read_bytes()).hexdigest() for p in (go, rust)}
    if after != hashes:
        cases.append({'status': 'FAIL', 'reason': 'endpoint binary changed'})
    report = {'schemaVersion': 1, 'suite': 'metadata-wire', 'cases': cases,
              'binaries': hashes, 'binaries_after': after, 'environment_overrides': env_overrides or {},
              'scope': 'actual Go TTHeader codec to custom fixture; META ordering, explicit fields, empty/invalid/unknown values and raw bytes; final public context, not intermediate hook order or real Mesh'}
    (output / 'report.json').write_text(json.dumps(report, ensure_ascii=False, indent=2) + '\n')
    return report
