"""Event-synchronized drain and disconnect checks on owned local processes."""
from __future__ import annotations

import hashlib
import json
import os
from pathlib import Path
import signal
import tempfile

from .network import result_event
from .process import Server
from .wire import header, send


def events(log):
    result = []
    for line in log.read_text().splitlines():
        try:
            value = json.loads(line)
        except ValueError:
            continue
        if isinstance(value, dict):
            result.append(value)
    return result


def finish(process, timeout):
    code = process.process.wait(timeout=timeout)
    process.reader.join(timeout=5)
    if process.reader.is_alive():
        raise ValueError('observer log reader did not finish')
    if code != 0:
        raise ValueError('observer failed with exit code %d' % code)
    return result_event(process.log)


def message(action, identity):
    return {'value': json.dumps({'schemaVersion': 1, 'action': action, 'id': identity}, separators=(',', ':')), 'seq': 1}


def validate_data(frame, action, identity):
    value = frame.get('message') or {}
    body = json.loads(value.get('value', 'null'))
    if value.get('seq') != 1 or body != {'schemaVersion': 1, 'action': action, 'id': identity}:
        raise ValueError('lifecycle DATA identity/action differs')


def validate_drain(actual, identity):
    actions = actual.get('actions') or []
    if not actual.get('ok') or len(actions) != 9 or not actions[3].get('continued'):
        raise ValueError('drain wire actions did not complete across the explicit barrier')
    for index, sid, kinds in [(2, 1, [2, 3]), (5, 2, [5]), (8, 1, [3, 4])]:
        frames = actions[index].get('frames') or []
        if any(f.get('sid') != sid or f.get('flags', 0) & 2 == 0 for f in frames):
            raise ValueError('foreign stream/non-streaming frame during drain')
        if [int(f['int_metadata']['27']) for f in frames] != kinds:
            raise ValueError('drain response sequence differs')
    validate_data(actions[2]['frames'][1], 'ack', identity)
    rejected = actions[5]['frames'][0].get('exception') or {}
    if rejected.get('code') != 6 or 'drain' not in rejected.get('message', ''):
        raise ValueError('new stream during drain must get explicit RST(6)')
    validate_data(actions[8]['frames'][0], 'released', identity)
    if actions[8]['frames'][1].get('exception'):
        raise ValueError('existing stream did not complete normally during drain')


def controlled_lifecycle(*, go: Path, rust: Path, output: Path, transports: list[str], timeout: float,
                         profile: str, env_overrides: dict[str, str] | None = None) -> dict:
    output.mkdir(parents=True, exist_ok=True)
    environment = os.environ.copy()
    environment.update(env_overrides or {})
    hashes = {str(p): hashlib.sha256(p.read_bytes()).hexdigest() for p in (go, rust)}
    cases = []
    for transport in transports:
        if transport not in ('tcp', 'uds'):
            cases.append({'transport': transport, 'status': 'INCOMPLETE', 'reason': 'TCP/UDS only'})
            continue
        for server_kind, server_bin in [('go', go), ('rust', rust)]:
            for client_kind, client_bin in [('go', go), ('rust', rust)]:
                for fault in (['kill', 'close-connection'] if server_kind == 'rust' else ['kill']):
                    identity = '-'.join([server_kind, client_kind, transport, fault])
                    directory = output / identity
                    directory.mkdir(parents=True, exist_ok=True)
                    row = {'name': identity, 'server': server_kind, 'client': client_kind,
                           'transport': transport, 'fault': fault, 'status': 'PASS'}
                    with tempfile.TemporaryDirectory(prefix='tts-', dir='/tmp') as socket_dir:
                        addr = 'tcp://127.0.0.1:0' if transport == 'tcp' else 'unix://' + socket_dir + '/server.sock'
                        server = Server([str(server_bin), '--role', 'server', '--addr', addr, '--scenario', 'disconnect'],
                                        cwd=directory, log=directory / 'server.log', env=environment,
                                        stdin=server_kind == 'rust', capture_events=True)
                        client = None
                        try:
                            ready = server.start(timeout)
                            row['ready'] = ready
                            if server_kind == 'go' and ready.get('profile') != profile:
                                raise ValueError('Go server profile differs')
                            command = [str(client_bin), '--role', 'lifecycle-client' if client_kind == 'go' else 'client',
                                       '--addr', ready['addr'], '--scenario', 'disconnect', '--id', identity]
                            client = Server(command, cwd=directory, log=directory / 'client.log', env=environment,
                                            ready_event='ack', capture_events=True)
                            ack = client.start(timeout)
                            row['ack'] = ack
                            if ack.get('id') != identity:
                                raise ValueError('client did not acknowledge the live stream')
                            if fault == 'kill':
                                os.killpg(server.process.pid, signal.SIGKILL)
                                server.process.wait(timeout=5)
                            else:
                                server.send_line('{"action":"close"}')
                                row['close_event'] = server.wait_event('closed', timeout)
                            observed = finish(client, timeout)
                            row['actual'], row['command'] = observed, command
                            terminal = observed.get('terminal') or {}
                            expected_code = 12004 if client_kind == 'go' else 12013
                            if not observed.get('ok') or observed.get('api') != 'lifecycle' or terminal.get('code') != expected_code:
                                raise ValueError('committed stream did not report its actual closure code %d' % expected_code)
                            if client_kind == 'go' and terminal.get('code') == 12004:
                                row['compatibility_difference'] = 'Kitex wraps netpoll EOF as illegal-frame 12004; Rust reports transport closure 12013'
                            if client_kind == 'go' and observed.get('profile') != profile:
                                raise ValueError('Go client profile differs')
                        except Exception as error:
                            row['status'], row['reason'] = 'FAIL', str(error)
                        finally:
                            if client is not None:
                                client.close()
                            server.close()
                    starts = [e for e in events(directory / 'server.log') if e.get('event') == 'started' and e.get('id') == identity]
                    row['started'] = starts
                    if len(starts) != 1:
                        row['status'], row['reason'] = 'FAIL', 'committed request missing or replayed at server'
                    cases.append(row)
        identity = 'drain-' + transport
        directory = output / identity
        directory.mkdir(parents=True, exist_ok=True)
        row = {'name': identity, 'server': 'rust', 'client': 'go-codec', 'transport': transport, 'status': 'PASS'}
        actions = [header(1), send(1, 3, message=message('start', identity)),
                   {'op': 'read', 'max_frames': 2, 'timeout_ms': 2000}, {'op': 'barrier'},
                   header(2), {'op': 'read', 'until_sid': 2, 'timeout_ms': 2000},
                   send(1, 3, message=message('release', identity)), send(1, 4),
                   {'op': 'read', 'until_sid': 1, 'timeout_ms': 2000}]
        input_path = directory / 'input.json'
        input_path.write_text(json.dumps({'schemaVersion': 1, 'actions': actions}, indent=2) + '\n')
        with tempfile.TemporaryDirectory(prefix='tts-', dir='/tmp') as socket_dir:
            addr = 'tcp://127.0.0.1:0' if transport == 'tcp' else 'unix://' + socket_dir + '/server.sock'
            server = Server([str(rust), '--role', 'server', '--addr', addr, '--scenario', 'drain'],
                            cwd=directory, log=directory / 'server.log', env=environment, stdin=True, capture_events=True)
            client = None
            try:
                ready = server.start(timeout)
                row['ready'] = ready
                command = [str(go), '--role', 'wire', '--addr', ready['addr'], '--input', str(input_path),
                           '--report', str(directory / 'wire.json')]
                client = Server(command, cwd=directory, log=directory / 'client.log', env=environment,
                                stdin=True, ready_event='barrier')
                row['barrier'] = client.start(timeout)
                server.send_line('{"action":"drain"}')
                row['drain_event'] = server.wait_event('draining', timeout)
                client.send_line('continue')
                actual = finish(client, timeout)
                validate_drain(actual, identity)
                row['command'], row['input'] = command, str(input_path)
            except Exception as error:
                row['status'], row['reason'] = 'FAIL', str(error)
            finally:
                if client is not None:
                    client.close()
                server.close()
        started = [e for e in events(directory / 'server.log') if e.get('event') == 'started']
        if len(started) != 1 or started[0].get('id') != identity:
            row['status'], row['reason'] = 'FAIL', 'rejected drain request invoked a handler or live request missing'
        cases.append(row)
    after = {str(p): hashlib.sha256(p.read_bytes()).hexdigest() for p in (go, rust)}
    if after != hashes:
        cases.append({'status': 'FAIL', 'reason': 'endpoint binary changed during controlled test'})
    report = {'schemaVersion': 1, 'suite': 'drain-disconnect', 'cases': cases, 'binaries': hashes, 'binaries_after': after,
              'environment_overrides': env_overrides or {},
              'scope': 'live ack then process kill/connection close; same-socket new-stream drain rejection and active completion; low-level Connection drain, not full Server.run signal lifecycle'}
    (output / 'report.json').write_text(json.dumps(report, ensure_ascii=False, indent=2) + '\n')
    return report
