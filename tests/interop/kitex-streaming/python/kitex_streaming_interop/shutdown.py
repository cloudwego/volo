"""Generated mixed Server.run signal handling with two retained sockets."""
from __future__ import annotations

import hashlib
import json
import os
from pathlib import Path
import signal
import socket
import tempfile
import time

from .controls import finish, validate_data
from .process import Server
from .budget import validate_budget_wire
from .controls import events, message
from .wire import header, send, read, response


def validate_shutdown(actual):
    if not actual.get('ok') or actual.get('api') != 'shutdown-wire':
        raise ValueError('shutdown wire observer did not complete')
    if actual.get('unary_dials') != 1 or actual.get('stream_dials') != 1:
        raise ValueError('shutdown check redialed a retained connection')
    phases = actual.get('unary_phases') or []
    if len(phases) != 2 or any(not p.get('ok') for p in phases):
        raise ValueError('existing unary connection failed across the signal')
    if [p.get('seq') for p in phases] != [1, 2]:
        raise ValueError('unary probe identities differ across the signal')
    ack = actual.get('ack_frames') or []
    if [int(f['int_metadata']['27']) for f in ack] != [2, 3] or any(f.get('sid') != 1 for f in ack):
        raise ValueError('original stream was not acknowledged before the signal')
    validate_data(ack[1], 'ack', 'shutdown-live')
    if not actual.get('rejected_new_stream'):
        raise ValueError('new streaming calls remained admitted throughout 64 post-hook probes')
    if not isinstance(actual.get('admission_elapsed_ms'), int) or actual['admission_elapsed_ms'] > 500:
        raise ValueError('streaming admission did not close within 500ms; unary two-second grace leaked into TTStream')
    probes = actual.get('probes') or []
    if not 1 <= len(probes) <= 64:
        raise ValueError('missing or unbounded shutdown probes')
    for index, probe in enumerate(probes):
        frames = probe.get('frames') or []
        if probe.get('sid') != index + 2 or any(f.get('sid') != probe['sid'] for f in frames):
            raise ValueError('probe stream identity differs')
        kinds = [int(f['int_metadata']['27']) for f in frames]
        if index == len(probes) - 1:
            if kinds != [5] or frames[0].get('exception', {}).get('code') != 6:
                raise ValueError('draining new stream did not receive one RST(6)')
        else:
            if kinds != [2, 3, 4] or frames[-1].get('exception'):
                raise ValueError('hook-to-mark race produced invalid accepted response')
            validate_data(frames[1], 'probe', 'shutdown-probe-%d' % probe['sid'])
    final = actual.get('live_finish_frames') or []
    if [int(f['int_metadata']['27']) for f in final] != [3, 4] or any(f.get('sid') != 1 for f in final):
        raise ValueError('preexisting stream did not drain normally')
    validate_data(final[0], 'released', 'shutdown-live')
    if final[-1].get('exception'):
        raise ValueError('preexisting stream ended in error during drain')


def server_shutdown(*, go: Path, rust: Path, output: Path, transports: list[str], timeout: float,
                    profile: str, env_overrides: dict[str, str] | None = None) -> dict:
    output.mkdir(parents=True, exist_ok=True)
    environment = os.environ.copy()
    environment.update(env_overrides or {})
    hashes = {str(p): hashlib.sha256(p.read_bytes()).hexdigest() for p in (go, rust)}
    cases = []
    for transport in transports:
        directory = output / transport
        directory.mkdir(parents=True, exist_ok=True)
        row = {'transport': transport, 'status': 'PASS'}
        with tempfile.TemporaryDirectory(prefix='tts-', dir='/tmp') as socket_dir:
            if transport not in ('tcp', 'uds'):
                cases.append({'transport': transport, 'status': 'INCOMPLETE', 'reason': 'TCP/UDS only'})
                continue
            addr = 'tcp://127.0.0.1:0' if transport == 'tcp' else 'unix://' + socket_dir + '/server.sock'
            server = Server([str(rust), '--role', 'mixed-server', '--addr', addr], cwd=directory,
                            log=directory / 'server.log', env=environment, capture_events=True)
            client = None
            try:
                row['ready'] = ready = server.start(timeout)
                if ready.get('api') != 'lifecycle-mixed':
                    raise ValueError('server is not the actual generated mixed Server.run fixture')
                command = [str(go), '--role', 'shutdown-wire', '--addr', ready['addr'],
                           '--report', str(directory / 'wire.json')]
                row['command'] = command
                client = Server(command, cwd=directory, log=directory / 'client.log', env=environment,
                                stdin=True, ready_event='barrier')
                row['barrier'] = client.start(timeout)
                os.kill(server.process.pid, signal.SIGTERM)
                row['shutdown_hook'] = server.wait_event('shutdown-hook', timeout)
                # The hook precedes the internal admission mark. Allow that
                # scheduling window through bounded probes, not a fixed sleep.
                client.send_line('continue')
                actual = finish(client, timeout)
                row['actual'] = actual
                if actual.get('profile') != profile:
                    raise ValueError('Go codec profile differs')
                validate_shutdown(actual)
                row['server_exit_code'] = server.process.wait(timeout=10)
                if row['server_exit_code'] != 0:
                    raise ValueError('generated server did not exit normally after active streams drained')
            except Exception as error:
                row['status'], row['reason'] = 'FAIL', str(error)
            finally:
                if client is not None:
                    client.close()
                server.close()
        cases.append(row)
    after = {str(p): hashlib.sha256(p.read_bytes()).hexdigest() for p in (go, rust)}
    if hashes != after:
        cases.append({'status': 'FAIL', 'reason': 'endpoint binary changed'})
    report = {'schemaVersion': 1, 'suite': 'server-shutdown', 'cases': cases, 'binaries': hashes,
              'binaries_after': after, 'environment_overrides': env_overrides or {},
              'scope': 'actual generated mixed Server.run + SIGTERM; retained unary and streaming sockets; admission before unary grace, old stream release, normal process exit'}
    (output / 'report.json').write_text(json.dumps(report, ensure_ascii=False, indent=2) + '\n')
    return report


def shutdown_boundaries(*, go: Path, rust: Path, output: Path, transports: list[str], timeout: float,
                        profile: str, env_overrides: dict[str, str] | None = None) -> dict:
    output.mkdir(parents=True, exist_ok=True)
    environment = os.environ.copy()
    environment.update(env_overrides or {})
    hashes = {str(path): hashlib.sha256(path.read_bytes()).hexdigest() for path in (go, rust)}
    cases = []
    for transport in transports:
        if transport not in ('tcp', 'uds'):
            cases.append({'transport': transport, 'status': 'INCOMPLETE', 'reason': 'TCP/UDS only'})
            continue
        for mode in ('no-first-byte', 'partial-first-frame', 'drain-deadline', 'peer-close'):
            identity = transport + '-' + mode
            directory = output / identity
            directory.mkdir(parents=True, exist_ok=True)
            row = {'name': identity, 'transport': transport, 'mode': mode, 'status': 'PASS'}
            with tempfile.TemporaryDirectory(prefix='tts-', dir='/tmp') as socket_dir:
                address = 'tcp://127.0.0.1:0' if transport == 'tcp' else 'unix://' + socket_dir + '/server.sock'
                command = [str(rust), '--role', 'mixed-server', '--addr', address,
                           '--drain-timeout-ms', '100', '--shutdown-flush-timeout-ms', '100']
                row['server_command'] = command
                server = Server(command, cwd=directory, log=directory / 'server.log', env=environment,
                                capture_events=True)
                client = peer = None
                try:
                    ready = server.start(timeout)
                    row['ready'] = ready
                    if ready.get('api') != 'lifecycle-mixed':
                        raise ValueError('expected production mixed Server.run')
                    if mode in ('no-first-byte', 'partial-first-frame'):
                        if transport == 'tcp':
                            host, port = ready['addr'][len('tcp://'):].rsplit(':', 1)
                            peer = socket.create_connection((host, int(port)), timeout=timeout)
                        else:
                            peer = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
                            peer.settimeout(timeout)
                            peer.connect(ready['addr'][len('unix://'):])
                        if mode == 'partial-first-frame':
                            # Valid TTHeader frame-length prefix only: the initial
                            # eight-byte dispatcher probe cannot complete yet.
                            peer.sendall(bytes.fromhex('0000002e'))
                        row['accepted'] = server.wait_event('accepted', timeout)
                    else:
                        actions = [header(1), send(1, 3, message=message('start', identity)),
                                   read(1, [response(2), response(3, message('ack', identity))],
                                        max_frames=2, until_sid=None), {'op': 'barrier'}]
                        actions += ([read(1, [response(5, code=6)])] if mode == 'drain-deadline' else [{'op': 'close'}])
                        path = directory / 'input.json'
                        path.write_text(json.dumps({'schemaVersion': 1, 'actions': actions}, indent=2) + '\n')
                        command = [str(go), '--role', 'wire', '--addr', ready['addr'], '--input', str(path),
                                   '--report', str(directory / 'wire.json')]
                        row['client_command'] = command
                        client = Server(command, cwd=directory, log=directory / 'client.log', env=environment,
                                        stdin=True, ready_event='barrier')
                        row['live_barrier'] = client.start(timeout)
                    os.kill(server.process.pid, signal.SIGTERM)
                    row['shutdown_hook'] = server.wait_event('shutdown-hook', timeout)
                    started = time.monotonic()
                    if peer is not None:
                        peer.settimeout(0.5)
                        if peer.recv(1) != b'':
                            raise ValueError('incomplete first frame received protocol bytes during shutdown')
                        row['closed_without_protocol_response'] = True
                    else:
                        client.send_line('continue')
                        row['actual'] = actual = finish(client, timeout)
                        validate_budget_wire(actions, actual)
                        if mode == 'drain-deadline':
                            terminal = actual['actions'][-1]['frames'][0].get('exception', {})
                            if terminal.get('message') != 'ttstream server draining':
                                raise ValueError('RST(6) did not report the ServerDraining terminal')
                    row['elapsed_after_hook_ms'] = (time.monotonic() - started) * 1000
                    if row['elapsed_after_hook_ms'] > (500 if peer is not None else 1500):
                        raise ValueError('shutdown boundary exceeded its fixed completion bound')
                    row['server_exit_code'] = server.process.wait(timeout=10)
                    if row['server_exit_code'] != 0:
                        raise ValueError('production server did not exit normally')
                    server.reader.join(timeout=5)
                    observed = [event for event in events(directory / 'server.log')
                                if event.get('event') == 'observed' and event.get('id') == identity]
                    row['observed'] = observed
                    if mode in ('drain-deadline', 'peer-close'):
                        if len(observed) != 1:
                            raise ValueError('active stream did not finish exactly once')
                        code = observed[0].get('terminal', {}).get('code')
                        if code != (6 if mode == 'drain-deadline' else 12013):
                            raise ValueError('actual server terminal differs from deadline/peer EOF')
                except Exception as error:
                    row['status'], row['reason'] = 'FAIL', str(error)
                finally:
                    if peer is not None:
                        peer.close()
                    if client is not None:
                        client.close()
                    server.close()
            cases.append(row)
    after = {str(path): hashlib.sha256(path.read_bytes()).hexdigest() for path in (go, rust)}
    if hashes != after:
        cases.append({'status': 'FAIL', 'reason': 'endpoint binary changed'})
    report = {'schemaVersion': 1, 'suite': 'shutdown-boundaries', 'cases': cases, 'binaries': hashes,
              'binaries_after': after, 'environment_overrides': env_overrides or {},
              'scope': 'production Server.run: accepted idle/partial-first-frame shutdown, forced drain RST(6), peer EOF after signal; injected AsyncWrite flush stalls remain a driver test'}
    (output / 'report.json').write_text(json.dumps(report, ensure_ascii=False, indent=2) + '\n')
    return report
