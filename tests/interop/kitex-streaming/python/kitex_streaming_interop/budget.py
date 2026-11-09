"""Real Go burst and cancellation against gated Go/Rust consumers."""
from __future__ import annotations

import hashlib
import copy
import json
import os
from pathlib import Path
import tempfile
import time

from .controls import events, finish, message
from .process import Server, run
from .network import result_event
from .wire import header, send, read, response, validate as validate_wire


COUNT = 128
PAYLOAD_BYTES = 1024
PROBE_LIMIT_NS = 500_000_000
LIMITS = {
    'default': {},
    'stream-messages': {'recv-messages': 8, 'max-read-pause-ms': 20},
    'stream-bytes': {'recv-bytes': 8192, 'max-read-pause-ms': 20},
    'connection-bytes': {'connection-recv-bytes': 8192, 'max-read-pause-ms': 20},
    'shared-bytes': {'shared-recv-bytes': 8192, 'max-read-pause-ms': 20},
}


def validate_burst(actual, identity, limited, canceled, count=COUNT, payload_bytes=PAYLOAD_BYTES):
    if not actual.get('ok') or actual.get('api') != 'budget' or actual.get('id') != identity:
        raise ValueError('budget client did not complete the requested observation')
    if not actual.get('ack') or not actual.get('probe') or actual.get('cancel') != canceled:
        raise ValueError('missing live ACK, post-burst probe, or cancellation mode')
    if actual.get('target_messages') != count or actual.get('payload_bytes') != payload_bytes:
        raise ValueError('burst differs from the fixed Go reference input')
    if not 0 < actual.get('send_attempted', 0) <= count or not 0 <= actual.get('send_succeeded', -1) <= actual['send_attempted']:
        raise ValueError('missing actual send attempt/success counts')
    if not 0 <= actual.get('probe_elapsed_ns', -1) <= PROBE_LIMIT_NS:
        raise ValueError('post-burst new stream was delayed beyond 500ms')
    terminal = actual.get('terminal') or {}
    want_code = 6 if limited else 12007 if canceled else 0
    if terminal.get('code') != want_code:
        raise ValueError('actual Go terminal code differs: expected %d, got %r' % (want_code, terminal))
    if limited and not terminal.get('streaming_canceled'):
        raise ValueError('budget reset did not satisfy Go ErrStreamingCanceled')
    responses = actual.get('responses') or []
    if limited or canceled:
        if responses:
            raise ValueError('canceled stream unexpectedly delivered a released response')
    else:
        if actual['send_succeeded'] != count or len(responses) != 1:
            raise ValueError('default budget lost burst messages')
        response = responses[0]
        body = json.loads(response.get('value', 'null'))
        if response.get('seq') != count or body != {'schemaVersion': 1, 'action': 'released', 'id': identity}:
            raise ValueError('consumer did not receive every queued burst message')


def snapshot(server, timeout):
    server.send_line('{"action":"snapshot"}')
    return server.wait_event('snapshot', timeout)


def settled_snapshot(server, timeout):
    deadline = time.monotonic() + 2
    history = []
    while True:
        value = snapshot(server, timeout)
        history.append(value)
        if value.get('active_streams') == 0 and value.get('held') == []:
            return history
        if time.monotonic() >= deadline:
            raise ValueError('stream/gate resources did not settle within two seconds: %r' % history)
        # Resource convergence sampling only; never delays a protocol response.
        time.sleep(0.01)


def validate_resources(observed, config, limited, scope):
    values = observed.get('resources') or {}
    required = ('high_water_messages', 'high_water_bytes', 'buffer_limit_resets', 'read_pauses')
    if any(not isinstance(values.get(key), int) for key in required):
        raise ValueError('missing actual Rust receive resource counters')
    if values['high_water_messages'] > config['recv_messages']:
        raise ValueError('per-stream message budget was exceeded')
    cap = min(config['recv_bytes'], config['connection_recv_bytes'], config['shared_recv_bytes'])
    if values['high_water_bytes'] > cap:
        raise ValueError('retained receive bytes exceeded configured admission budget')
    if values['buffer_limit_resets'] != (1 if limited else 0):
        raise ValueError('resource protection counter differs from actual reset outcome')
    if limited and values.get('control_queue_delay_nanos', 0) <= 0:
        raise ValueError('flushed RST lacks its real control queue delay in the terminal observation')
    if scope == 'stream-messages' and values['read_pauses'] < 1:
        raise ValueError('full receive queue never exercised the bounded read pause')


def validate_budget_wire(actions, actual):
    # Fixture control messages are JSON inside a Thrift string. Go and Sonic may
    # serialize object keys in different orders; retain original wire evidence,
    # compare the control identity semantically, and keep frame checks strict.
    expected, observed = copy.deepcopy(actions), copy.deepcopy(actual)
    def canonicalize(message_value):
        if message_value is not None:
            body = json.loads(message_value['value'])
            if not isinstance(body, dict) or set(body) != {'schemaVersion', 'action', 'id'}:
                raise ValueError('unexpected control payload structure')
            message_value['value'] = json.dumps(body, sort_keys=True, separators=(',', ':'))
    for action in expected:
        if action['op'] == 'read':
            for frames in action['expect'].values():
                for frame in frames:
                    canonicalize(frame.get('message'))
    for action in observed.get('actions', []):
        for frame in action.get('frames') or []:
            canonicalize(frame.get('message'))
    validate_wire(expected, observed)


def wire_budget_case(go, rust, directory, transport, environment, timeout, all_full):
    identity = 'wire-full-' + transport if all_full else 'wire-reset-' + transport
    ids = [identity + '-1', identity + '-2'] if all_full else [identity + '-1']
    actions = []
    for sid, call_id in enumerate(ids, 1):
        actions += [header(sid), send(sid, 3, message=message('hold', call_id)),
                    read(sid, [response(2), response(3, message('ack', call_id))], max_frames=2, until_sid=None)]
    payload = {'value': 'x' * PAYLOAD_BYTES, 'seq': 1}
    if all_full:
        actions += [send(1, 3, message=payload), send(2, 3, message=payload), {'op': 'barrier'}]
        actions += [send(1, 5, metadata={'ttscp': 'A'}, exception={'code': 12007, 'message': 'budget cancel'}), send(1, 4)]
    else:
        actions += [send(1, 3, message=payload) for _ in range(9)]
        actions += [read(1, [response(5, code=6)])]
    for sid, call_id in [(3, identity + '-probe'), (4, identity + '-recovery')]:
        if all_full and sid == 4:
            actions += [send(2, 5, metadata={'ttscp': 'A'}, exception={'code': 12007, 'message': 'budget cancel'}), send(2, 4)]
        request = message('probe', call_id)
        actions += [header(sid), send(sid, 3, message=request), send(sid, 4),
                    read(sid, [response(2), response(3, request), response(4)])]
    directory.mkdir(parents=True, exist_ok=True)
    input_path = directory / 'input.json'
    input_path.write_text(json.dumps({'schemaVersion': 1, 'actions': actions}, indent=2) + '\n')
    row = {'name': identity, 'transport': transport, 'server': 'rust', 'status': 'PASS',
           'scope': 'same-socket all-full RST and later HEADER' if all_full else 'wire RST(6) and same-socket recovery'}
    with tempfile.TemporaryDirectory(prefix='tts-', dir='/tmp') as socket_dir:
        address = 'tcp://127.0.0.1:0' if transport == 'tcp' else 'unix://' + socket_dir + '/server.sock'
        command = [str(rust), '--role', 'server', '--scenario', 'budget', '--addr', address,
                   '--recv-messages', '1' if all_full else '8', '--max-read-pause-ms', '20']
        row['server_command'] = command
        server = Server(command, cwd=directory, log=directory / 'server.log', env=environment,
                        stdin=True, capture_events=True)
        client = None
        try:
            ready = server.start(timeout)
            row['ready'] = ready
            command = [str(go), '--role', 'wire', '--addr', ready['addr'], '--input', str(input_path),
                       '--report', str(directory / 'wire.json')]
            row['client_command'] = command
            if all_full:
                client = Server(command, cwd=directory, log=directory / 'client.log', env=environment,
                                stdin=True, ready_event='barrier')
                row['client_event'] = client.start(timeout)
                deadline = time.monotonic() + 2
                row['full_snapshots'] = []
                while True:
                    current = snapshot(server, timeout)
                    row['full_snapshots'].append(current)
                    held = {item['id']: item['resources'] for item in current.get('held', [])}
                    if current.get('connections') == 1 and current.get('active_streams') == 2 and all(
                            held.get(call_id, {}).get('queued_messages') == 1 for call_id in ids):
                        break
                    if time.monotonic() >= deadline:
                        raise ValueError('two streams never reached full queues on one connection')
                    time.sleep(0.01)
                continued = time.monotonic()
                client.send_line('continue')
                actual = finish(client, timeout)
            else:
                row['execution'] = execution = run(command, cwd=directory, log=directory / 'client.log',
                                                    env=environment, timeout=timeout)
                if execution['returncode'] != 0 or execution['timed_out']:
                    raise ValueError('wire reset observer failed or timed out')
                actual = result_event(directory / 'client.log')
            row['actual'] = actual
            if all_full:
                row['control_elapsed_ns'] = int((time.monotonic() - continued) * 1_000_000_000)
                if row['control_elapsed_ns'] > PROBE_LIMIT_NS:
                    raise ValueError('RST and later HEADER stalled beyond 500ms with all streams full')
            validate_budget_wire(actions, actual)
            row['settled'] = settled_snapshot(server, timeout)
            observed = [event for event in events(directory / 'server.log')
                        if event.get('event') == 'observed' and event.get('id') in ids]
            row['observed'] = observed
            if sorted(event['id'] for event in observed) != sorted(ids):
                raise ValueError('held streams did not terminate exactly once')
            for event in observed:
                code = 12007 if all_full else 6
                if event.get('terminal', {}).get('code') != code:
                    raise ValueError('held stream terminal differs')
                validate_resources(event, ready['config'], not all_full, 'default')
            if all_full and not any(event['resources']['read_pauses'] > 0 for event in observed):
                raise ValueError('all-full scenario did not exercise the bounded read pause')
        except Exception as error:
            row['status'], row['reason'] = 'FAIL', str(error)
        finally:
            if client is not None:
                client.close()
            server.close()
    return row


def aggregate_budget_case(go, rust, directory, transport, environment, timeout, shared):
    scope = 'shared' if shared else 'connection'
    identity = scope + '-aggregate-' + transport
    first, second = identity + '-first', identity + '-second'
    payload = {'value': 'x' * PAYLOAD_BYTES, 'seq': 1}

    def held(sid, call_id):
        return [header(sid), send(sid, 3, message=message('hold', call_id)),
                read(sid, [response(2), response(3, message('ack', call_id))], max_frames=2, until_sid=None)] + [
                    send(sid, 3, message=payload) for _ in range(4)]

    def probe(sid):
        request = message('probe', identity + '-probe-' + str(sid))
        return [header(sid), send(sid, 3, message=request), send(sid, 4),
                read(sid, [response(2), response(3, request), response(4)])]

    competing = held(2, second) + [read(2, [response(5, code=6)])] + probe(3)
    release = message('released', first)
    release['seq'] = 4
    first_actions = held(1, first) + ([] if shared else competing) + [{'op': 'barrier'}] + [
        send(1, 4), read(1, [response(3, release), response(4)])] + probe(4)
    directory.mkdir(parents=True, exist_ok=True)
    first_input = directory / 'first-input.json'
    first_input.write_text(json.dumps({'schemaVersion': 1, 'actions': first_actions}, indent=2) + '\n')
    second_input = directory / 'second-input.json'
    if shared:
        second_input.write_text(json.dumps({'schemaVersion': 1, 'actions': competing}, indent=2) + '\n')
    row = {'name': identity, 'transport': transport, 'server': 'rust', 'scope': scope + ' aggregate',
           'status': 'PASS', 'semantic_enhancement': True}
    with tempfile.TemporaryDirectory(prefix='tts-', dir='/tmp') as socket_dir:
        address = 'tcp://127.0.0.1:0' if transport == 'tcp' else 'unix://' + socket_dir + '/server.sock'
        command = [str(rust), '--role', 'server', '--scenario', 'budget', '--addr', address,
                   '--' + scope + '-recv-bytes', '8192']
        row['server_command'] = command
        server = Server(command, cwd=directory, log=directory / 'server.log', env=environment,
                        stdin=True, capture_events=True)
        client = contender = None
        try:
            ready = server.start(timeout)
            row['ready'] = ready
            command = [str(go), '--role', 'wire', '--addr', ready['addr'], '--input', str(first_input),
                       '--report', str(directory / 'first-wire.json')]
            row['first_command'] = command
            client = Server(command, cwd=directory, log=directory / 'first-client.log', env=environment,
                            stdin=True, ready_event='barrier')
            row['first_barrier'] = client.start(timeout)
            deadline = time.monotonic() + 2
            row['first_snapshots'] = []
            while True:
                current = snapshot(server, timeout)
                row['first_snapshots'].append(current)
                values = {item['id']: item['resources'] for item in current.get('held', [])}
                if values.get(first, {}).get('queued_messages') == 4:
                    break
                if time.monotonic() >= deadline:
                    raise ValueError('first stream did not retain its four admitted messages')
                time.sleep(0.01)
            if shared:
                command = [str(go), '--role', 'wire', '--addr', ready['addr'], '--input', str(second_input),
                           '--report', str(directory / 'second-wire.json')]
                row['second_command'] = command
                row['second_execution'] = execution = run(command, cwd=directory, log=directory / 'second-client.log',
                                                          env=environment, timeout=timeout)
                if execution['returncode'] != 0 or execution['timed_out']:
                    raise ValueError('shared-budget contender failed or timed out')
                row['second_actual'] = result_event(directory / 'second-client.log')
                validate_budget_wire(competing, row['second_actual'])
            row['before_release'] = current = snapshot(server, timeout)
            values = {item['id']: item['resources'] for item in current.get('held', [])}
            if current.get('connections') != (2 if shared else 1) or values.get(first, {}).get('queued_messages') != 4:
                raise ValueError('competing budget reset did not preserve the first stream and expected connection count')
            server.send_line(json.dumps({'action': 'release', 'id': first}))
            row['release'] = server.wait_event('gate_released', timeout)
            if not row['release'].get('found'):
                raise ValueError('non-offending stream was discarded by aggregate protection')
            client.send_line('continue')
            row['first_actual'] = finish(client, timeout)
            validate_budget_wire(first_actions, row['first_actual'])
            row['settled'] = settled_snapshot(server, timeout)
            observed = [event for event in events(directory / 'server.log')
                        if event.get('event') == 'observed' and event.get('id') in (first, second)]
            row['observed'] = observed
            if sorted(event['id'] for event in observed) != sorted([first, second]):
                raise ValueError('aggregate streams did not terminate exactly once')
            for event in observed:
                limited = event['id'] == second
                terminal = event.get('terminal', {})
                if terminal.get('code') != (6 if limited else 0):
                    raise ValueError('aggregate protection affected the wrong stream')
                if limited and scope + ' bytes' not in terminal.get('error', ''):
                    raise ValueError('reset was not caused by the intended aggregate budget')
                validate_resources(event, ready['config'], limited, 'default')
            if sum(event['resources']['high_water_bytes'] for event in observed) > 8192:
                raise ValueError('simultaneously retained aggregate payload exceeded the budget')
        except Exception as error:
            row['status'], row['reason'] = 'FAIL', str(error)
        finally:
            if contender is not None:
                contender.close()
            if client is not None:
                client.close()
            server.close()
    return row


def budget_limits(*, go: Path, rust: Path, output: Path, transports: list[str], timeout: float,
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
        specs = [('go', 'default', False), ('go', 'default', True),
                 ('rust', 'default', False), ('rust', 'default', True)]
        specs.extend(('rust', scope, False) for scope in LIMITS if scope != 'default')
        samples = [(kind, scope, cancel, COUNT, PAYLOAD_BYTES) for kind, scope, cancel in specs]
        samples.extend((kind, 'default', False, 2048, 16384) for kind in ('go', 'rust'))
        for server_kind, scope, canceled, count, payload_bytes in samples:
            identity = '-'.join([transport, server_kind, scope, 'cancel' if canceled else 'complete'])
            if count != COUNT:
                identity += '-large'
            directory = output / identity
            directory.mkdir(parents=True, exist_ok=True)
            limited = scope != 'default'
            row = {'name': identity, 'transport': transport, 'server': server_kind, 'scope': scope,
                   'cancel': canceled, 'messages': count, 'payload_bytes': payload_bytes,
                   'status': 'PASS', 'semantic_enhancement': limited}
            with tempfile.TemporaryDirectory(prefix='tts-', dir='/tmp') as socket_dir:
                address = 'tcp://127.0.0.1:0' if transport == 'tcp' else 'unix://' + socket_dir + '/server.sock'
                server_command = [str(go if server_kind == 'go' else rust), '--role', 'server',
                                  '--addr', address, '--scenario', 'budget']
                for key, value in LIMITS[scope].items():
                    server_command += ['--' + key, str(value)]
                row['server_command'] = server_command
                server = Server(server_command, cwd=directory, log=directory / 'server.log',
                                env=environment, stdin=True, capture_events=True)
                client = None
                try:
                    ready = server.start(timeout)
                    row['ready'] = ready
                    if server_kind == 'go' and ready.get('profile') != profile:
                        raise ValueError('Go reference profile differs')
                    if server_kind == 'rust':
                        for key, value in LIMITS[scope].items():
                            if ready.get('config', {}).get(key.replace('-', '_')) != value:
                                raise ValueError('Rust server ignored requested budget configuration')
                    command = [str(go), '--role', 'budget-client', '--addr', ready['addr'], '--pool', 'mux',
                               '--id', identity, '--mode', 'cancel' if canceled else 'complete',
                               '--messages', str(count), '--payload-bytes', str(payload_bytes),
                               '--timeout', '%ss' % timeout, '--report', str(directory / 'client.json')]
                    row['client_command'] = command
                    client = Server(command, cwd=directory, log=directory / 'client.log', env=environment,
                                    ready_event='ack', capture_events=True)
                    row['ack'] = client.start(timeout)
                    row['burst'] = client.wait_event('burst_sent', timeout)
                    row['probe'] = client.wait_event('probe_done', timeout)
                    if any(row[key].get('id') != identity for key in ('ack', 'burst', 'probe')):
                        raise ValueError('budget control event belongs to another request')
                    if not row['probe'].get('ok'):
                        raise ValueError('later HEADER could not complete while the consumer was gated')
                    if server_kind == 'rust':
                        row['before_release'] = snapshot(server, timeout)
                        if row['before_release'].get('connections') != 1:
                            raise ValueError('Go mux probe redialed instead of preserving its connection')
                    if not canceled:
                        server.send_line(json.dumps({'action': 'release', 'id': identity}))
                        row['release'] = server.wait_event('budget_release' if server_kind == 'go' else 'gate_released', timeout)
                    actual = finish(client, timeout)
                    row['actual'] = actual
                    if actual.get('profile') != profile:
                        raise ValueError('Go client profile differs')
                    validate_burst(actual, identity, limited, canceled, count, payload_bytes)
                    if server_kind == 'rust':
                        row['settled'] = settled_snapshot(server, timeout)
                    observed = [event for event in events(directory / 'server.log')
                                if event.get('event') == 'observed' and event.get('id') == identity]
                    row['observed'] = observed
                    if len(observed) != 1:
                        raise ValueError('server did not report exactly one terminal for the held stream')
                    expected_server_code = 6 if limited else 12007 if canceled else 0
                    if observed[0].get('terminal', {}).get('code') != expected_server_code:
                        raise ValueError('server terminal differs from the observed client outcome')
                    if server_kind == 'rust':
                        validate_resources(observed[0], ready['config'], limited, scope)
                    elif not canceled and observed[0].get('received') != count:
                        raise ValueError('Go reference lost queued burst messages')
                except Exception as error:
                    row['status'], row['reason'] = 'FAIL', str(error)
                finally:
                    if client is not None:
                        client.close()
                    server.close()
            cases.append(row)
        for all_full in (False, True):
            cases.append(wire_budget_case(go, rust, output / ('wire-full-' if all_full else 'wire-reset-') / transport,
                                          transport, environment, timeout, all_full))
        for shared in (False, True):
            cases.append(aggregate_budget_case(go, rust, output / ('shared-aggregate' if shared else 'connection-aggregate') / transport,
                                               transport, environment, timeout, shared))
    after = {str(path): hashlib.sha256(path.read_bytes()).hexdigest() for path in (go, rust)}
    if hashes != after:
        cases.append({'status': 'FAIL', 'reason': 'endpoint binary changed during budget validation'})
    report = {'schemaVersion': 1, 'suite': 'budget-limits', 'cases': cases, 'binaries': hashes,
              'binaries_after': after, 'environment_overrides': env_overrides or {},
              'policy': {'messages': COUNT, 'payload_bytes': PAYLOAD_BYTES, 'probe_limit_ns': PROBE_LIMIT_NS,
                         'large_default_sample': {'messages': 2048, 'payload_bytes': 16384},
                         'limits': LIMITS, 'settle_seconds': 2},
              'scope': 'Go typed burst versus gated Go/Rust consumers; bounded receive counters, flushed RST terminal queue delay and Go error API; raw same-socket RST(6), recovery, and all-full late HEADER/RST; aggregate admission across streams and connections'}
    (output / 'report.json').write_text(json.dumps(report, ensure_ascii=False, indent=2) + '\n')
    return report
