"""Kitex service lookup, explicit Rust improvements, and public registration."""
from __future__ import annotations

import hashlib
import json
import os
from pathlib import Path
import tempfile

from .network import result_event
from .process import Server, run
from .wire import header, send


# (isn, method, selected handler). None is a lookup miss. "unary" means the
# lookup selected a unary descriptor and must not incorrectly route to B's stream.
ROUTES = {
    'unique': [('A', 'OnlyA', 'A'), ('B', 'OnlyB', 'B'), ('Wrong', 'OnlyA', 'A'),
               ('', 'OnlyB', 'B'), ('A', 'OnlyB', None), ('Wrong', 'Absent', None), ('', 'Absent', None)],
    'fallback': [('', 'Shared', 'A'), ('Wrong', 'Shared', None), ('B', 'Shared', 'B'),
                 ('A', 'Shared', 'A'), ('Wrong', 'OnlyB', 'B'), ('', 'OnlyB', 'B'), ('A', 'OnlyB', None)],
    'strict': [('', 'OnlyA', None), ('Wrong', 'OnlyA', None), ('B', 'Shared', 'B'), ('A', 'Shared', 'A')],
    'combine': [('Wrong', 'OnlyA', 'Combined'), ('', 'OnlyB', 'Combined'),
                ('A', 'Shared', 'Combined'), ('Wrong', 'Absent', None)],
    'unknown': [('Wrong', 'Absent', 'unknown'), ('A', 'Absent', 'unknown'), ('', 'Absent', 'unknown'),
                ('A', 'OnlyB', 'unknown'), ('Wrong', 'OnlyB', 'B'), ('B', 'OnlyB', 'B')],
    'dynamic': [('Wrong', 'OnlyB', 'B'), ('', 'OnlyB', 'B'), ('B', 'OnlyB', 'B'),
                ('A', 'OnlyB', None), ('B', 'Absent', None)],
    'binary-v2': [('B', 'Absent', 'B'), ('B', 'OnlyB', 'B'), ('Wrong', 'OnlyB', None),
                  ('', 'OnlyB', None), ('Wrong', 'OnlyA', 'A')],
    'mixed': [('B', 'Shared', 'B'), ('Wrong', 'Shared', None), ('', 'Shared', 'unary'), ('A', 'Shared', 'unary')],
}
REGISTRATION = ['duplicate-service', 'duplicate-fallback', 'duplicate-unknown', 'conflicting-methods',
                'fallback-missing-conflict', 'combine-multi', 'combine-unknown', 'binary-v1-multi', 'binary-v1-unknown']


def request_actions(sid, service, method, value, silent):
    return [header(sid, method, metadata={'isn': service} if service else {}),
            send(sid, 3, method, message={'value': value, 'seq': 7}), send(sid, 4, method),
            {'op': 'read', 'until_sid': sid, 'timeout_ms': 150 if silent else 2000, 'allow_timeout': silent}]


def validate_route(actual, sid, service, method, selected, value, server_kind):
    frames = actual.get('frames') or []
    silent = selected is None and server_kind == 'go'
    if bool(actual.get('timed_out')) != silent:
        raise ValueError('lookup silence differs: %r' % actual)
    kinds = []
    for frame in frames:
        if frame.get('sid') != sid or frame.get('flags', 0) & 2 == 0:
            raise ValueError('foreign stream or non-streaming response')
        kinds.append(int(frame['int_metadata']['27']))
    if silent:
        if frames:
            raise ValueError('Go missing-method baseline unexpectedly responded')
        return
    if selected in (None, 'unary'):
        # Calling a unary descriptor with a TTStream frame is a misconfiguration.
        # Go panics in its generated unary adapter and returns internal error;
        # Rust rejects it without calling either unary or B's streaming handler.
        code = 6 if selected == 'unary' and server_kind == 'go' else 1
        if kinds != [2, 4] or frames[-1].get('exception', {}).get('code') != code:
            raise ValueError('expected explicit lookup/unsupported-method exception %d, got %r' % (code, frames))
        return
    if kinds != [2, 3, 4] or frames[-1].get('exception'):
        raise ValueError('selected route did not produce exactly one DATA and successful terminal')
    message = frames[1].get('message') or {}
    if message.get('seq') != 7:
        raise ValueError('routing message sequence differs')
    decoded = json.loads(message['value'])
    resolved = service if selected == 'unknown' else selected
    expected = {'schemaVersion': 1, 'handler': selected, 'service': resolved, 'method': method, 'request': value}
    if decoded != expected:
        raise ValueError('resolved route/context differs: expected %r, got %r' % (expected, decoded))


def service_routing(*, go: Path, rust: Path, output: Path, transports: list[str], timeout: float,
                    profile: str, env_overrides: dict[str, str] | None = None) -> dict:
    output.mkdir(parents=True, exist_ok=True)
    environment = os.environ.copy()
    environment.update(env_overrides or {})
    hashes = {str(p): hashlib.sha256(p.read_bytes()).hexdigest() for p in (go, rust)}
    cases = []
    for kind, binary in [('go', go), ('rust', rust)]:
        directory = output / kind
        directory.mkdir(parents=True, exist_ok=True)
        log = directory / 'registrations.log'
        execution = run([str(binary), '--role', 'routing-registration', '--report', str(directory / 'registrations.json')],
                        cwd=directory, log=log, timeout=timeout, env=environment)
        try:
            observed = result_event(log)
            if execution['returncode'] != 0 or execution['timed_out'] or not observed.get('ok'):
                raise ValueError('registration observer failed')
            if observed.get('api') != 'routing-registration' or [c['name'] for c in observed['cases']] != REGISTRATION:
                raise ValueError('registration cases/API differ')
            if kind == 'go' and observed.get('profile') != profile:
                raise ValueError('registration Go profile differs')
            for index, case in enumerate(observed['cases']):
                row = {'name': case['name'], 'server': kind, 'category': 'registration', 'actual': case,
                       'execution': execution, 'status': 'PASS'}
                expected = 'panic' if kind == 'go' and index < 3 else 'error'
                if case.get('outcome') != expected or not case.get('error'):
                    row['status'], row['reason'] = 'FAIL', 'wrong public registration behavior'
                cases.append(row)
        except Exception as error:
            cases.append({'server': kind, 'category': 'registration', 'status': 'FAIL', 'reason': str(error), 'execution': execution})
        for transport in transports:
            if transport not in ('tcp', 'uds'):
                cases.append({'server': kind, 'transport': transport, 'status': 'INCOMPLETE', 'reason': 'TCP/UDS only'})
                continue
            for mode, routes in ROUTES.items():
                directory = output / kind / transport / mode
                directory.mkdir(parents=True, exist_ok=True)
                with tempfile.TemporaryDirectory(prefix='tts-', dir='/tmp') as socket_dir:
                    addr = 'tcp://127.0.0.1:0' if transport == 'tcp' else 'unix://' + socket_dir + '/routing.sock'
                    server = Server([str(binary), '--role', 'server', '--addr', addr, '--scenario', 'routing', '--routing-profile', mode],
                                    cwd=directory, log=directory / 'server.log', env=environment)
                    try:
                        ready = server.start(timeout)
                        if kind == 'go' and (ready.get('profile') != profile or ready.get('routing_profile') != mode):
                            raise ValueError('Go routing profile differs')
                        for index, (service, method, selected) in enumerate(routes):
                            identity = '%s-%s-%s-%s' % (mode, index, service or 'missing', method)
                            actions = request_actions(1, service, method, identity, selected is None and kind == 'go')
                            actions += request_actions(2, 'A', 'OnlyA', identity + '-probe', False)
                            input_path = directory / (identity + '.input.json')
                            input_path.write_text(json.dumps({'schemaVersion': 1, 'actions': actions}, ensure_ascii=False, indent=2) + '\n')
                            log = directory / (identity + '.log')
                            executed = run([str(go), '--role', 'wire', '--addr', ready['addr'], '--input', str(input_path),
                                            '--report', str(directory / (identity + '.json'))],
                                           cwd=directory, log=log, timeout=timeout, env=environment)
                            row = {'name': identity, 'server': kind, 'transport': transport, 'routing_profile': mode,
                                   'execution': executed, 'input': str(input_path), 'ready': ready,
                                   'expected_handler': selected, 'status': 'PASS'}
                            try:
                                actual = result_event(log)
                                if executed['returncode'] != 0 or executed['timed_out'] or not actual.get('ok') or len(actual.get('actions', [])) != 8:
                                    raise ValueError('wire actions failed or did not complete')
                                validate_route(actual['actions'][3], 1, service, method, selected, identity, kind)
                                validate_route(actual['actions'][7], 2, 'A', 'OnlyA',
                                               'Combined' if mode == 'combine' else 'A', identity + '-probe', kind)
                            except Exception as error:
                                row['status'], row['reason'] = 'FAIL', str(error)
                            cases.append(row)
                    except Exception as error:
                        cases.append({'server': kind, 'transport': transport, 'routing_profile': mode,
                                      'status': 'FAIL', 'reason': str(error), 'log': str(directory / 'server.log')})
                    finally:
                        server.close()
    after = {str(p): hashlib.sha256(p.read_bytes()).hexdigest() for p in (go, rust)}
    if hashes != after:
        cases.append({'status': 'FAIL', 'reason': 'endpoint binary changed'})
    report = {'schemaVersion': 1, 'suite': 'service-routing', 'cases': cases, 'binaries': hashes,
              'binaries_after': after, 'environment_overrides': env_overrides or {},
              'scope': 'real SearchService and Rust router; actual context identity, known/unknown/generic/combine, registration and same-socket recovery'}
    (output / 'report.json').write_text(json.dumps(report, ensure_ascii=False, indent=2) + '\n')
    return report
