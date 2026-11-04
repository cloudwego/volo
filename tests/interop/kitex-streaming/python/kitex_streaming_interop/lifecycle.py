"""Real per-receive timeout policies and server-local deadlines over TCP/UDS."""
from __future__ import annotations

import hashlib
import json
import os
from pathlib import Path
import tempfile
import time

from .network import result_event
from .process import Server, run


def read_events(path: Path) -> list[dict]:
    rows = []
    if path.exists():
        for line in path.read_text(errors='replace').splitlines():
            try:
                item = json.loads(line)
            except ValueError:
                continue
            if isinstance(item, dict) and 'event' in item:
                rows.append(item)
    return rows


def terminal_code(terminal: dict, expected: int, *, path: list[str] | None = None) -> None:
    if not isinstance(terminal, dict) or terminal.get('code') != expected:
        raise ValueError(f'actual terminal code differs: expected={expected}, actual={terminal!r}')
    if terminal.get('kind') != ('ok' if expected == 0 else 'error'):
        raise ValueError('terminal kind differs from code')
    if expected and terminal.get('kitex_code') != expected:
        raise ValueError('actual Kitex error code is missing or differs')
    if expected and not terminal.get('error'):
        raise ValueError('error terminal lacks actual diagnostic')
    if not expected and terminal.get('error'):
        raise ValueError('successful terminal carries an error')
    if path is not None and terminal.get('path') != path:
        raise ValueError(f'actual cancellation path differs: expected={path}, actual={terminal!r}')


def validate_client(observed: dict, events: list[dict], identity: str, scenario: str) -> None:
    if observed.get('ok') is not True or observed.get('api') != 'lifecycle':
        raise ValueError('missing successful lifecycle observation')
    if observed.get('id') != identity or observed.get('scenario') != scenario:
        raise ValueError('lifecycle identity/scenario differs')
    acknowledgments = [e for e in events if e.get('event') == 'ack' and e.get('id') == identity]
    if len(acknowledgments) != 1:
        raise ValueError('expected exactly one explicit live-stream acknowledgment event')
    terminal_code(observed.get('terminal'), 12015 if scenario == 'server-timeout' else 12014)
    if observed.get('probe') is not True:
        raise ValueError('same configured client recovery probe did not finish normally')
    actions = []
    for message in observed.get('responses', []):
        if message.get('seq') != 1 or not isinstance(message.get('value'), str):
            raise ValueError('response is not an actual Message with seq=1')
        body = json.loads(message['value'])
        if body.get('schemaVersion') != 1 or body.get('id') != identity:
            raise ValueError('response identity/schema differs')
        if body.get('action') not in ('ack', 'released'):
            raise ValueError('unexpected lifecycle response action')
        actions.append(body['action'])
    # The Go fixture includes its first DATA in responses; the Rust fixture
    # records that DATA only via its explicit ack event. Neither is fabricated.
    if actions.count('ack') > 1:
        raise ValueError('duplicate initial acknowledgment DATA')
    if scenario == 'keep-open':
        if observed.get('keep_open') is not True or actions.count('released') != 1:
            raise ValueError('KeepOpen did not send/receive release after the actual 12014')
        terminal_code(observed.get('final_terminal'), 0, path=[])
    elif observed.get('keep_open') is not False or 'released' in actions:
        raise ValueError('terminal cancellation/deadline unexpectedly continued the same stream')


def validate_server(events: list[dict], identity: str, scenario: str) -> None:
    starts = [e for e in events if e.get('event') == 'started' and e.get('id') == identity]
    finishes = [e for e in events if e.get('event') == 'observed' and e.get('id') == identity]
    if len(starts) != 1 or len(finishes) != 1:
        raise ValueError('expected exactly one actual handler start and finish for the requested id')
    code = {'keep-open': 0, 'cancel': 12014, 'server-timeout': 12015}[scenario]
    terminal_code(finishes[0].get('terminal'), code, path=['A'] if scenario == 'cancel' else [])
    if 'active_handlers' in finishes[0] and finishes[0]['active_handlers'] != 0:
        raise ValueError('Go fixture still retains its main lifecycle handler')


def lifecycle(*, go: Path, rust: Path, output: Path, transports: list[str], timeout: float,
              profile: str, env_overrides: dict[str, str] | None = None,
              scenarios: list[str] | None = None, topologies: list[str] | None = None) -> dict:
    """16 timeout-policy combinations plus four Rust server-deadline combinations.

    Disconnect and drain require the common runner's explicit process/control API
    and are intentionally separate suites; no timing sleeps imitate those cases.
    """
    output.mkdir(parents=True, exist_ok=True)
    environment = os.environ.copy()
    environment.update(env_overrides or {})
    scenarios = scenarios or ['keep-open', 'cancel', 'server-timeout']
    topologies = topologies or ['gg', 'gr', 'rg', 'rr']
    if any(s not in ('keep-open', 'cancel', 'server-timeout') for s in scenarios):
        raise ValueError('unsupported lifecycle timeout scenario')
    if any(t not in ('gg', 'gr', 'rg', 'rr') for t in topologies):
        raise ValueError('topology must name client then server with g/r')
    if any(t not in ('tcp', 'uds') for t in transports):
        raise ValueError('lifecycle timeout suite supports TCP/UDS')
    binaries = {str(p): hashlib.sha256(p.read_bytes()).hexdigest() for p in (go, rust)}
    cases = []
    for scenario in scenarios:
        for topology in topologies:
            if scenario == 'server-timeout' and topology[1] == 'g':
                continue  # No Go server-local timeout option is being claimed.
            for transport in transports:
                identity = '-'.join([topology, transport, scenario])
                directory = output / identity
                directory.mkdir(parents=True, exist_ok=True)
                row = {'name': identity, 'scenario': scenario, 'topology': topology,
                       'transport': transport, 'recv_timeout_ms': 500,
                       'server_deadline_ms': 250 if scenario == 'server-timeout' else None}
                server = None
                with tempfile.TemporaryDirectory(prefix='ttlife-', dir='/tmp') as sockets:
                    try:
                        address = ('tcp://127.0.0.1:0' if transport == 'tcp' else
                                   'unix://' + sockets + '/server.sock')
                        server_binary = go if topology[1] == 'g' else rust
                        command = [str(server_binary), '--role', 'server', '--addr', address,
                                   '--scenario', scenario]
                        if scenario == 'server-timeout':
                            command += ['--server-deadline-ms', '250']
                        row['server_command'] = command
                        server = Server(command, cwd=directory, log=directory / 'server.log', env=environment)
                        ready = server.start(timeout)
                        row['ready'] = ready
                        if topology[1] == 'g' and ready.get('profile') != profile:
                            raise ValueError('Go server profile differs')
                        client = go if topology[0] == 'g' else rust
                        command = [str(client), '--role', 'lifecycle-client' if topology[0] == 'g' else 'client',
                                   '--addr', ready['addr'], '--scenario', scenario, '--id', identity,
                                   '--timeout-ms', '500']
                        if topology[0] == 'g':
                            command += ['--pool', 'mux', '--report', str(directory / 'client.json')]
                        execution = run(command, cwd=directory, timeout=timeout,
                                        log=directory / 'client.log', env=environment)
                        row['execution'] = execution
                        observed = result_event(directory / 'client.log')
                        row['actual'] = observed
                        row['client_events'] = read_events(directory / 'client.log')
                        if execution['returncode'] != 0 or execution['timed_out']:
                            raise ValueError('lifecycle client failed or timed out')
                        if topology[0] == 'g' and observed.get('profile') != profile:
                            raise ValueError('Go client profile differs')
                        validate_client(observed, row['client_events'], identity, scenario)
                        deadline = time.monotonic() + min(timeout, 5)
                        while True:
                            current = read_events(directory / 'server.log')
                            if any(e.get('event') == 'observed' and e.get('id') == identity for e in current):
                                break
                            if server.process.poll() is not None:
                                raise ValueError('server exited before terminal observation')
                            if time.monotonic() >= deadline:
                                raise ValueError('missing bounded server finish observation')
                            time.sleep(0.01)
                        row['server_events'] = current
                        validate_server(current, identity, scenario)
                        row['status'] = 'PASS'
                    except Exception as error:
                        row['status'], row['reason'] = 'FAIL', str(error)
                        row['server_events'] = read_events(directory / 'server.log')
                    finally:
                        if server:
                            server.close()
                cases.append(row)
    after = {str(p): hashlib.sha256(p.read_bytes()).hexdigest() for p in (go, rust)}
    if binaries != after:
        cases.append({'status': 'FAIL', 'reason': 'endpoint binaries changed during lifecycle suite'})
    report = {'schemaVersion': 1, 'suite': 'lifecycle-timeouts', 'cases': cases,
              'binaries': binaries, 'binaries_after': after, 'environment_overrides': env_overrides or {},
              'scope': 'real per-Recv 12014 KeepOpen/Cancel, acknowledged live stream, release+EOF continuation, same-client recovery probe, server-local 12015 and observed cancellation path; no wire deadline propagation, Mesh, disconnect or drain in this suite',
              'schema_notes': ['Go responses includes initial ack DATA; Rust exposes it through explicit ack event.',
                               'Go probe id uses case id plus -probe; frozen Rust probe id is probe; only main-stream events are matched by id.',
                               'Successful Rust terminal omits kitex_code/error; kind=ok/code=0/path=[] is checked, absent error accepted.']}
    (output / 'report.json').write_text(json.dumps(report, ensure_ascii=False, indent=2) + '\n')
    return report
