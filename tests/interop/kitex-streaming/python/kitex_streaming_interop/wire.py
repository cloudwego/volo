"""Go-encoded frame boundaries against actual Go and generated Rust servers."""
from __future__ import annotations

import hashlib
import json
import os
from pathlib import Path
import tempfile

from .network import result_event
from .process import Server, run


def send(sid, kind, method='Bidi', metadata=None, message=None, **options):
    action = {'op': 'send', 'frame': {'sid': sid, 'kind': kind, 'method': method,
                                    'metadata': metadata or {}}}
    if message is not None:
        action['message'] = message
    action.update(options)
    return action


def header(sid=1, method='Bidi', metadata=None, **options):
    return send(sid, 2, method, {'isn': 'Interop'} if metadata is None else metadata, **options)


def response(kind, message=None, code=None):
    value = {'kind': kind}
    if message is not None:
        value['message'] = message
    if code is not None:
        value['exception_code'] = code
    return value


def read(sid, responses, **options):
    action = {'op': 'read', 'until_sid': sid, 'timeout_ms': 2000,
              'expect': {str(sid): responses}}
    action.update(options)
    return action


def echo(sid=1):
    message = {'value': 'wire-中文-🦀', 'seq': 42}
    return [header(sid), send(sid, 3, message=message), send(sid, 4),
            read(sid, [response(2), response(3, message), response(4)])]


def scenarios(server_kind):
    cases = []
    for sid in (0, -1, -2147483648, 2147483647):
        cases.append(('signed-id-' + str(sid), echo(sid)))
    unknown = [send(91, kind) for kind in (1, 3, 4, 5)]
    cases.append(('unknown-id-frames-ignored', unknown + echo()))
    after_reset = echo(11)
    if server_kind == 'go':
        # Kitex's handler cleanup races the peer reset and can still emit this
        # retired stream's header/exception. Preserve it as a baseline variant;
        # Rust's terminal-once contract remains strict.
        after_reset[-1]['optional_retired_prefix'] = {'10': [response(2), response(4, code=6)]}
    cases.append(('rst-then-compatibility-trailer', [
        header(10), send(10, 5, metadata={'ttscp': 'wire-client'},
                         exception={'code': 13, 'message': 'rst'}), send(10, 4)] + after_reset))
    cases.append(('header-delayed-until-first-response', [
        header(), read(1, [], timeout_ms=150, allow_timeout=True, expect_timeout=True)] + echo()[1:]))
    request = {'value': 'first', 'seq': 1}
    server_response = [response(2), response(3, {'value': 'first', 'seq': 0}), response(4)]
    cases.append(('server-responds-before-client-trailer', [
        header(method='Server'), send(1, 3, 'Server', message=request), read(1, server_response),
        send(1, 4, 'Server')] + echo(2)))
    cases.append(('server-extra-data-tolerated', [
        header(method='Server'), send(1, 3, 'Server', message=request),
        send(1, 3, 'Server', message={'value': 'ignored', 'seq': 10}), send(1, 4, 'Server'),
        read(1, server_response)] + echo(2)))
    cases.append(('server-empty-input', [header(method='Server'), send(1, 4, 'Server'),
        read(1, [response(2), response(4, code=6)])] + echo(2)))
    cases.append(('client-early-empty-trailer', [header(method='Client'), send(1, 4, 'Client'),
        read(1, [response(2), response(3, {'value': '', 'seq': 0}), response(4)])]))
    cases.append(('bidi-early-empty-trailer', [header(), send(1, 4), read(1, [response(2), response(4)])]))
    for name, meta in [('mismatched-isn', {'isn': 'NotInterop'}), ('missing-isn', {})]:
        actions = echo()
        actions[0] = header(metadata=meta)
        cases.append((name + '-unique-method', actions))
    unknown_read = (read(1, [response(2), response(4, code=1)]) if server_kind == 'rust' else
                    read(1, [], timeout_ms=150, allow_timeout=True, expect_timeout=True))
    cases.append(('unknown-method-enhancement' if server_kind == 'rust' else 'unknown-method-go-timeout',
                  [header(method='AbsentMethod'), unknown_read] + echo(2)))
    if server_kind == 'rust':
        duplicate = [header(), header(), read(1, [response(5, code=7)]), send(1, 4)]
    else:
        duplicate = [header()] + echo()
    cases.append(('duplicate-active-id', duplicate + echo(2)))
    fragmented = echo()
    for action in fragmented:
        if action['op'] == 'send':
            action['fragment_bytes'] = 1
    cases.append(('one-byte-writes', fragmented))
    trailing = echo()
    trailing[1]['payload_suffix_hex'] = 'a1b2'
    cases.append(('data-struct-trailing-bytes-tolerated', trailing))
    left, right = {'value': 'left', 'seq': 11}, {'value': 'right', 'seq': 12}
    cases.append(('interleaved-independent-streams', [header(11), header(12),
        send(12, 3, message=right), send(11, 3, message=left), send(11, 4), send(12, 4),
        {'op': 'read', 'max_frames': 6, 'timeout_ms': 2000,
         'expect': {'11': [response(2), response(3, left), response(4)],
                    '12': [response(2), response(3, right), response(4)]}}]))
    return cases


def validate(actions, observed):
    if not observed.get('ok') or len(observed.get('actions', [])) != len(actions):
        raise ValueError('wire actions did not complete successfully')
    for expected, actual in zip(actions, observed['actions']):
        if expected['op'] != 'read':
            continue
        if bool(actual.get('timed_out')) != bool(expected.get('expect_timeout')):
            raise ValueError('read timeout differs from expected')
        by_sid = {}
        for frame in actual.get('frames') or []:
            if frame['flags'] & 2 == 0:
                raise ValueError('response is not a TTStream frame')
            kind = int(frame['int_metadata']['27'])
            item = response(kind, frame.get('message'), frame.get('exception', {}).get('code'))
            if kind in (4, 5) and frame.get('exception'):
                if frame['exception']['sid'] != frame['sid']:
                    raise ValueError('exception envelope sid differs from frame sid')
            by_sid.setdefault(str(frame['sid']), []).append(item)
        # An empty expected response means a deliberate silent bounded read.
        want = {sid: frames for sid, frames in expected['expect'].items() if frames}
        for sid, allowed in expected.get('optional_retired_prefix', {}).items():
            retired = by_sid.pop(sid, [])
            if len(retired) > len(allowed) or retired != allowed[:len(retired)]:
                raise ValueError('unexpected frames for retired Go baseline stream %s: %r' % (sid, retired))
        if by_sid != want:
            raise ValueError('frame sequence differs: expected %r, got %r' % (want, by_sid))


def boundaries(*, go: Path, rust: Path, output: Path, transports: list[str], timeout: float,
               profile: str, env_overrides: dict[str, str] | None = None) -> dict:
    output.mkdir(parents=True, exist_ok=True)
    environment = os.environ.copy()
    environment.update(env_overrides or {})
    cases = []
    for server_kind, binary in [('rust', rust), ('go-baseline', go)]:
        for transport in transports:
            directory = output / server_kind / transport
            directory.mkdir(parents=True, exist_ok=True)
            if transport not in ('tcp', 'uds'):
                cases.append({'server': server_kind, 'transport': transport, 'status': 'INCOMPLETE',
                              'reason': 'wire suite requires TCP or UDS'})
                continue
            with tempfile.TemporaryDirectory(prefix='tts-', dir='/tmp') as socket_dir:
                addr = 'tcp://127.0.0.1:0' if transport == 'tcp' else 'unix://' + socket_dir + '/server.sock'
                server = Server([str(binary), '--role', 'server', '--addr', addr],
                                cwd=directory, log=directory / 'server.log', env=environment)
                try:
                    ready = server.start(timeout)
                    if server_kind == 'go-baseline' and ready.get('profile') != profile:
                        raise ValueError('Go endpoint profile differs from requested profile')
                    for name, actions in scenarios('rust' if server_kind == 'rust' else 'go'):
                        scenario = directory / (name + '.input.json')
                        scenario.write_text(json.dumps({'schemaVersion': 1, 'actions': actions}, ensure_ascii=False, indent=2) + '\n')
                        log = directory / (name + '.log')
                        report = directory / (name + '.json')
                        command = [str(go), '--role', 'wire', '--addr', ready['addr'],
                                   '--input', str(scenario), '--report', str(report)]
                        executed = run(command, cwd=directory, timeout=timeout, log=log, env=environment)
                        row = {'name': name, 'server': server_kind, 'transport': transport,
                               'execution': executed, 'ready': ready, 'input': str(scenario),
                               'wire_report': str(report)}
                        try:
                            observed = result_event(log)
                            if executed['returncode'] != 0 or executed['timed_out']:
                                raise ValueError('wire client failed or timed out')
                            validate(actions, observed)
                            row['status'] = 'PASS'
                        except ValueError as error:
                            row['status'], row['reason'] = 'FAIL', str(error)
                        cases.append(row)
                    name = 'kitex-client-unknown-method'
                    log = directory / (name + '.log')
                    command = [str(go), '--role', 'unknown-method', '--addr', ready['addr'],
                               '--report', str(directory / (name + '.json'))]
                    executed = run(command, cwd=directory, timeout=timeout, log=log, env=environment)
                    row = {'name': name, 'server': server_kind, 'transport': transport,
                           'execution': executed, 'ready': ready, 'api': 'kitex-streamx'}
                    try:
                        observed = result_event(log)
                        row['actual'] = observed
                        if executed['returncode'] != 0 or executed['timed_out'] or not observed.get('ok'):
                            raise ValueError('Kitex client or following valid call failed')
                        if observed.get('profile') != profile:
                            raise ValueError('Go client profile differs from requested profile')
                        if server_kind == 'rust':
                            if observed.get('type_id') != 12001 or observed.get('deadline_expired'):
                                raise ValueError('Rust unknown-method must produce Go application exception 12001 before deadline')
                        elif observed.get('type_id') != 12015 or not observed.get('streaming_timeout'):
                            raise ValueError('Go unknown-method baseline must end by local stream timeout 12015')
                        row['status'] = 'PASS'
                    except ValueError as error:
                        row['status'], row['reason'] = 'FAIL', str(error)
                    cases.append(row)
                except Exception as error:
                    cases.append({'server': server_kind, 'transport': transport, 'status': 'FAIL',
                                  'reason': str(error), 'log': str(directory / 'server.log')})
                finally:
                    server.close()
    report = {'schemaVersion': 1, 'suite': 'wire-boundaries', 'cases': cases,
              'environment_overrides': env_overrides or {},
              'binaries': {str(p): hashlib.sha256(p.read_bytes()).hexdigest() for p in (go, rust)},
              'scope': 'Go TTHeader encoder/decoder and generated payload codec over real sockets; actual Kitex StreamX unknown-method outer error mapping and recovery'}
    (output / 'report.json').write_text(json.dumps(report, ensure_ascii=False, indent=2) + '\n')
    return report
