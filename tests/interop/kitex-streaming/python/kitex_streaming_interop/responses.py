"""Real typed clients receiving Go-codec boundary and malformed responses."""
from __future__ import annotations

import hashlib
import json
import os
from pathlib import Path
import tempfile

from .network import result_event
from .process import Server, run
from .wire import send


MESSAGE = {'value': 'response-probe', 'seq': 1}


def response_scenarios():
    head = lambda: send(0, 2, metadata={'fixture-header': 'response'})
    data = lambda: send(0, 3, message=MESSAGE)
    trailer = lambda **kw: send(0, 4, **kw)
    app = {'code': 6, 'message': 'response-terminal'}
    return [
        ('valid', [head(), data(), trailer()], [MESSAGE], 'ok', 0),
        ('data-trailing-bytes', [head(), send(0, 3, message=MESSAGE, payload_suffix_hex='a1b2'), trailer()],
         [MESSAGE], 'ok', 0),
        ('application', [head(), trailer(exception=app)], [], 'application', 12001),
        ('application-trailing-bytes', [head(), trailer(exception=app, payload_suffix_hex='a1b2')],
         [], 'application', 12001),
        ('duplicate-header', [head(), head()], [], 'unknown', 12002),
        ('malformed-trailer', [head(), {'op': 'send', 'frame': {'kind': 4, 'payload_hex': 'ff'}}],
         [], 'unknown', 12004),
        ('malformed-rst', [head(), {'op': 'send', 'frame': {'kind': 5, 'payload_hex': 'ff',
                                                          'metadata': {'ttscp': 'raw-server'}}}],
         [], 'unknown', 13),
        ('empty-rst', [head(), send(0, 5)], [], 'unknown', 13),
        ('cancel-rst', [head(), send(0, 5, metadata={'ttscp': 'parent,raw-server'},
                                   exception={'code': 12007, 'message': 'canceled'})],
         [], 'unknown', 12007),
        ('invalid-biz-code', [head(), trailer(metadata={'biz-status': 'not-an-int', 'biz-message': 'bad'})],
         [], 'ok', 0),
        ('invalid-biz-extra', [head(), trailer(metadata={'biz-status': '4123', 'biz-message': 'bad', 'biz-extra': '{'})],
         [], 'unknown', 12003),
        ('zero-biz-code', [head(), trailer(metadata={'biz-status': '0', 'biz-message': 'ignored'})],
         [], 'ok', 0),
        ('application-before-biz', [head(), trailer(exception=app, metadata={'biz-status': '4123',
                                                                           'biz-message': 'ignored'})],
         [], 'application', 12001),
    ]


def validate_response(actual, responses, kind, code, client_kind):
    if not actual.get('ok') or actual.get('api') != 'response-probe':
        raise ValueError('client did not complete an actual response observation')
    if actual.get('stage', 'recv') != 'recv':
        raise ValueError('client failed before receiving the response: ' + str(actual.get('stage')))
    if (actual.get('responses') or []) != responses:
        raise ValueError('decoded DATA differs: expected %r, got %r' % (responses, actual.get('responses')))
    terminal = actual.get('terminal') or {}
    actual_kind = terminal.get('kind')
    kind_matches = actual_kind == kind or (kind == 'unknown' and actual_kind in ('other', 'canceled'))
    # An EOF is not a Kitex exception. Some observers report the absence as
    # null, others use success code 0. Never relax an actual error's code.
    code_matches = terminal.get('kitex_code') == code or (
        kind == 'ok' and actual_kind == 'ok' and terminal.get('code') == 0
        and terminal.get('kitex_code') is None and not terminal.get('error'))
    if not kind_matches or not code_matches:
        raise ValueError('terminal differs: expected %r/%r, got %r' % (kind, code, terminal))
    if kind == 'application':
        if client_kind == 'rust':
            if terminal.get('code') != 6 or terminal.get('message') != 'response-terminal':
                raise ValueError('Rust application payload differs')
        elif terminal.get('code') is not None or 'response-terminal' not in (terminal.get('error') or ''):
            raise ValueError('Go private cause was fabricated or actual message lost')
    elapsed = actual.get('open_elapsed_seconds')
    if not isinstance(elapsed, (float, int)) or not 0 <= elapsed < 4:
        raise ValueError('open did not complete before peer response/deadline')


def response_boundaries(*, go: Path, rust: Path, output: Path, transports: list[str], timeout: float,
                        profile: str, env_overrides: dict[str, str] | None = None,
                        reference_only: bool = False, rust_api: str = 'typed') -> dict:
    output.mkdir(parents=True, exist_ok=True)
    environment = os.environ.copy()
    environment.update(env_overrides or {})
    binaries = {str(p): hashlib.sha256(p.read_bytes()).hexdigest() for p in (go, rust)}
    clients = [('go', go)] if reference_only else ([('go', go), ('rust', rust)] if rust_api == 'typed' else [('rust', rust)])
    cases = []
    for client_kind, binary in clients:
        for transport in transports:
            if transport not in ('tcp', 'uds'):
                cases.append({'client': client_kind, 'transport': transport, 'status': 'INCOMPLETE',
                              'reason': 'response suite requires TCP or UDS'})
                continue
            for name, actions, expected_data, kind, code in response_scenarios():
                directory = output / client_kind / transport / name
                directory.mkdir(parents=True, exist_ok=True)
                scenario = directory / 'input.json'
                scenario.write_text(json.dumps({'schemaVersion': 1, 'actions': actions},
                                               ensure_ascii=False, indent=2) + '\n')
                row = {'name': name, 'client': client_kind, 'transport': transport,
                       'input': str(scenario), 'rust_api': rust_api,
                       'expected': {'responses': expected_data, 'kind': kind, 'kitex_code': code}}
                with tempfile.TemporaryDirectory(prefix='tts-', dir='/tmp') as socket_dir:
                    addr = 'tcp://127.0.0.1:0' if transport == 'tcp' else 'unix://' + socket_dir + '/peer.sock'
                    server = Server([str(go), '--role', 'raw-response-server', '--addr', addr, '--input', str(scenario)],
                                    cwd=directory, log=directory / 'server.log', env=environment)
                    try:
                        ready = server.start(timeout)
                        row['ready'] = ready
                        if ready.get('profile') != profile or ready.get('api') != 'go-codec-response-peer':
                            raise ValueError('raw peer profile/API differs')
                        log = directory / 'client.log'
                        command = [str(binary), '--role', 'response-client', '--addr', ready['addr'],
                                   '--pool', 'mux', '--report', str(directory / 'client.json')]
                        if client_kind == 'rust' and rust_api != 'typed':
                            command += ['--rust-api', rust_api]
                        executed = run(command,
                                       cwd=directory, timeout=timeout, log=log, env=environment)
                        row['execution'] = executed
                        if executed['returncode'] != 0 or executed['timed_out']:
                            raise ValueError('response observer failed or timed out')
                        actual = result_event(log)
                        row['actual'] = actual
                        if client_kind == 'go' and actual.get('profile') != profile:
                            raise ValueError('client profile differs')
                        if client_kind == 'rust' and actual.get('rust_api', 'typed') != rust_api:
                            raise ValueError('actual Rust response API differs')
                        validate_response(actual, expected_data, kind, code, client_kind)
                        if client_kind == 'rust' and rust_api != 'typed' and name == 'data-trailing-bytes':
                            payloads = actual.get('raw_payload_hex') or []
                            if len(payloads) != 1 or not payloads[0].endswith('a1b2'):
                                raise ValueError('generic API did not preserve actual incoming payload suffix evidence')
                        row['status'] = 'PASS'
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
                    row['status'], row['reason'] = 'FAIL', 'missing exact sent bytes/request-before-response proof'
                cases.append(row)
    after = {str(p): hashlib.sha256(p.read_bytes()).hexdigest() for p in (go, rust)}
    if after != binaries:
        cases.append({'status': 'FAIL', 'reason': 'endpoint binary changed during execution'})
    report = {'schemaVersion': 1, 'suite': 'response-boundaries', 'cases': cases,
              'binaries': binaries, 'binaries_after': after, 'environment_overrides': env_overrides or {},
              'scope': 'Go-codec peer to actual typed client; errors, tolerated suffixes, open before response; single-stream only'}
    (output / 'report.json').write_text(json.dumps(report, ensure_ascii=False, indent=2) + '\n')
    return report
