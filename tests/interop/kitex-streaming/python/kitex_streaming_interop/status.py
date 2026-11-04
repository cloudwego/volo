"""DATA-before-error ordering through real typed clients and Go wire codecs."""
from __future__ import annotations

import hashlib
import json
import os
from pathlib import Path
import tempfile

from .network import result_event
from .process import Server, run
from .wire import header, send


def request_for(identity, mode):
    return {'schemaVersion': 1, 'id': identity, 'kind': mode,
            'code': 4123 if mode == 'biz' else 6, 'message': 'fixture-terminal',
            'extra': {'detail': 'interop-中文'}}


def validate_response(message, identity, mode):
    if message.get('seq') != 1 or json.loads(message.get('value', '')) != request_for(identity, mode):
        raise ValueError('DATA before terminal did not preserve the request')


def validate_terminal(value, mode, client_kind, server_kind):
    if value.get('kind') != mode:
        raise ValueError('terminal kind/code differs: %r' % value)
    unavailable = client_kind == 'go' and mode == 'application' and value.get('application_code_available') is False
    if unavailable:
        # Kitex Exception does not expose its cause. The raw-codec cases check
        # payload code/message separately; never substitute request values here.
        if value.get('code') is not None:
            raise ValueError('Go must explicitly report unavailable private application cause')
        if 'fixture-terminal' not in value.get('error', ''):
            raise ValueError('Go application diagnostic lost its message')
    else:
        if value.get('code') != (4123 if mode == 'biz' else 6):
            raise ValueError('terminal code differs: %r' % value)
        expected_message = ('biz error: fixture-terminal' if mode == 'application' and server_kind == 'go'
                            else 'fixture-terminal')
        if value.get('message') != expected_message:
            raise ValueError('terminal lost the original message: %r' % value)
    if mode == 'biz' and value.get('extra') != {'detail': 'interop-中文'}:
        raise ValueError('terminal lost business extra metadata')
    if mode == 'application' and value.get('kitex_code') != 12001:
        raise ValueError('Application payload code must remain distinct from outer Kitex 12001')
    if value.get('data_and_error_same_recv') is True and (client_kind != 'go' or mode != 'biz'):
        raise ValueError('only the observed Go business-status race permits DATA and error in one Recv')


def status_errors(*, go: Path, rust: Path, output: Path, transports: list[str], timeout: float,
                  profile: str, env_overrides: dict[str, str] | None = None,
                  reference_only: bool = False) -> dict:
    output.mkdir(parents=True, exist_ok=True)
    environment = os.environ.copy()
    environment.update(env_overrides or {})
    binaries = {str(p): hashlib.sha256(p.read_bytes()).hexdigest() for p in (go, rust)}
    cases = []
    endpoints = [('go', go)] if reference_only else [('go', go), ('rust', rust)]
    for server_kind, server_binary in endpoints:
        for transport in transports:
            directory = output / server_kind / transport
            directory.mkdir(parents=True, exist_ok=True)
            server = None
            with tempfile.TemporaryDirectory(prefix='ttstatus-', dir='/tmp') as sockets:
                try:
                    if transport not in ('tcp', 'uds'):
                        raise ValueError('error-status requires TCP or UDS')
                    address = 'tcp://127.0.0.1:0' if transport == 'tcp' else 'unix://' + sockets + '/s.sock'
                    server = Server([str(server_binary), '--role', 'server', '--scenario', 'errors', '--addr', address],
                                    cwd=directory, log=directory / 'server.log', env=environment)
                    ready = server.start(timeout)
                    if server_kind == 'go' and ready.get('profile') != profile:
                        raise ValueError('Go server profile differs')
                    for client_kind, binary in endpoints + [('go-wire', go)]:
                        for mode in ['biz', 'application']:
                            identity = '-'.join([client_kind, server_kind, transport, mode])
                            log = directory / (identity + '.log')
                            row = {'name': identity, 'client': client_kind, 'server': server_kind,
                                   'transport': transport, 'mode': mode, 'ready': ready}
                            if client_kind == 'go-wire':
                                request = request_for(identity, mode)
                                source = directory / (identity + '.input.json')
                                source.write_text(json.dumps({'schemaVersion': 1, 'actions': [header(),
                                                  send(1, 3, message={'value': json.dumps(request), 'seq': 1}),
                                                  {'op': 'read', 'until_sid': 1, 'timeout_ms': 3000}]}, indent=2) + '\n')
                                command = [str(binary), '--role', 'wire', '--addr', ready['addr'], '--input', str(source)]
                            else:
                                command = [str(binary), '--role', 'error-client', '--addr', ready['addr'],
                                           '--mode', mode, '--payload', identity, '--pool', 'mux']
                            execution = run(command, cwd=directory, timeout=timeout, log=log, env=environment)
                            row['execution'] = execution
                            try:
                                observed = result_event(log)
                                row['actual'] = observed
                                if execution['returncode'] != 0 or execution['timed_out'] or not observed.get('ok'):
                                    raise ValueError('error fixture failed or timed out')
                                if client_kind == 'go-wire':
                                    frames = observed['actions'][-1]['frames']
                                    if [frame['int_metadata']['27'] for frame in frames] != ['2', '3', '4']:
                                        raise ValueError('expected exactly HEADER/DATA/TRAILER')
                                    if any(frame['sid'] != 1 for frame in frames):
                                        raise ValueError('unexpected response stream ID')
                                    validate_response(frames[1]['message'], identity, mode)
                                    terminal = frames[-1]
                                    if frames[0].get('metadata', {}).get('interop-error-header') != 'header-value' or terminal.get('metadata', {}).get('interop-error-trailer') != 'trailer-value':
                                        raise ValueError('wire Header/Trailer lost custom metadata')
                                    if mode == 'application':
                                        error = terminal.get('exception', {})
                                        expected_message = 'biz error: fixture-terminal' if server_kind == 'go' else 'fixture-terminal'
                                        if error.get('code') != 6 or error.get('message') != expected_message:
                                            raise ValueError('wire ApplicationException differs')
                                    else:
                                        meta = terminal['metadata']
                                        if terminal.get('exception') or meta.get('biz-status') != '4123' or meta.get('biz-message') != 'fixture-terminal':
                                            raise ValueError('wire business status differs')
                                        if json.loads(meta.get('biz-extra', 'null')) != {'detail': 'interop-中文'}:
                                            raise ValueError('wire business extra differs')
                                else:
                                    if observed.get('api') != 'errors' or observed.get('id') != identity or observed.get('mode') != mode:
                                        raise ValueError('wrong error client API or identity')
                                    if client_kind == 'go' and observed.get('profile') != profile:
                                        raise ValueError('Go client profile differs')
                                    responses = observed.get('responses', [])
                                    if len(responses) != 1 or observed.get('probe_ok') is not True:
                                        raise ValueError('missing pre-error DATA or successful recovery call')
                                    validate_response(responses[0], identity, mode)
                                    validate_terminal(observed.get('terminal', {}), mode, client_kind, server_kind)
                                    head, tail = observed.get('header') or {}, observed.get('trailer') or {}
                                    head = head.get('string', head)
                                    tail = tail.get('string', tail)
                                    if head.get('interop-error-header') != 'header-value' or tail.get('interop-error-trailer') != 'trailer-value':
                                        raise ValueError('public Header/Trailer query lost custom metadata')
                                    if observed['terminal'].get('data_and_error_same_recv') is True:
                                        row['compatibility_difference'] = 'Kitex returned a decoded DATA value and the already observed BizStatusErr in the same Recv; raw-codec cases independently verify wire order'
                                row['status'] = 'PASS'
                            except (ValueError, KeyError, TypeError) as error:
                                row['status'], row['reason'] = 'FAIL', str(error)
                            cases.append(row)
                except Exception as error:
                    cases.append({'server': server_kind, 'transport': transport, 'status': 'FAIL', 'reason': str(error)})
                finally:
                    if server is not None:
                        server.close()
    after = {str(p): hashlib.sha256(p.read_bytes()).hexdigest() for p in (go, rust)}
    if binaries != after:
        cases.append({'status': 'FAIL', 'reason': 'endpoint binaries changed during error-status suite'})
    report = {'schemaVersion': 1, 'suite': 'error-status', 'cases': cases, 'binaries': binaries,
              'binaries_after': after, 'environment_overrides': env_overrides or {},
              'scope': 'one DATA then Biz or Application terminal; original codes/messages/extra and recovery; raw Go codec checks terminal wire; malformed error frames, KeepOpen and other fault families are separate gates'}
    (output / 'report.json').write_text(json.dumps(report, ensure_ascii=False, indent=2) + '\n')
    return report
