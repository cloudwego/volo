"""Actual Go/Rust endpoint exchange; every PASS validates response content."""
from __future__ import annotations

import hashlib
import json
import os
import itertools
import tempfile
from pathlib import Path

from .process import Server, run


def result_event(log: Path) -> dict:
    text = log.read_text(errors='replace')
    # Go may pretty-print a result after framework log lines; Rust emits one line.
    decoder = json.JSONDecoder()
    for pos, char in enumerate(text):
        if char != '{':
            continue
        try:
            value, _ = decoder.raw_decode(text[pos:])
        except ValueError:
            continue
        if isinstance(value, dict) and value.get('event') == 'result':
            return value
    raise ValueError(f'no result JSON event in {log}')


def expected(mode: str, count: int, payload: str) -> list[dict]:
    if mode == 'client':
        return [{'value': payload if count else '', 'seq': count}]
    if mode in ('unary', 'explicit-unary'):
        return [{'value': payload, 'seq': count}]
    return [{'value': payload, 'seq': i} for i in range(count)]


def basic(*, go: Path, rust: Path, output: Path, transports: list[str],
          modes: list[str], timeout: float, directions: list[str], api: str = 'raw',
          pools: list[str] | None = None, profile: str = 'upstream',
          env_overrides: dict[str, str] | None = None, rust_api: str = 'typed',
          go_api: str = 'typed') -> dict:
    output.mkdir(parents=True, exist_ok=True)
    environment = os.environ.copy()
    environment.update(env_overrides or {})
    binaries = {str(p): hashlib.sha256(p.read_bytes()).hexdigest() for p in (go, rust)}
    cases = []
    for direction in directions:
        client_bin, server_bin = (go, rust) if direction == 'go-to-rust' else (rust, go)
        for transport in transports:
            if transport not in ('tcp', 'uds'):
                cases.append({'direction': direction, 'transport': transport, 'status': 'INCOMPLETE',
                              'reason': 'basic Go network suite only supports TCP/UDS; Rust ShmIPC has a separate gate'})
                continue
            directory = output / direction / transport
            directory.mkdir(parents=True, exist_ok=True)
            # Artifact paths can exceed sockaddr_un.sun_path. Own a short, isolated
            # socket directory independently of the caller's output directory.
            socket_directory = tempfile.TemporaryDirectory(prefix='tts-', dir='/tmp')
            socket = Path(socket_directory.name) / 'server.sock'
            addr = 'tcp://127.0.0.1:0' if transport == 'tcp' else 'unix://' + str(socket)
            server_command = [str(server_bin), '--role', 'server', '--addr', addr, '--service', 'Interop']
            if direction == 'rust-to-go' and go_api != 'typed':
                server_command += ['--api', go_api]
            if direction == 'go-to-rust' and rust_api != 'typed':
                server_command += ['--scenario', 'generic', '--rust-api', rust_api]
            server = Server(server_command, cwd=directory, log=directory / 'server.log',
                            env=environment)
            try:
                ready = server.start(timeout)
                if not ready.get('addr'):
                    raise ValueError('readiness event lacks actual bound address')
                if direction == 'rust-to-go' and ready.get('profile') != profile:
                    raise ValueError('Go endpoint profile does not match the requested profile')
                if direction == 'rust-to-go' and go_api != 'typed' and ready.get('api') != go_api:
                    raise ValueError('Go server did not use the requested generic API')
                if direction == 'go-to-rust' and rust_api != 'typed' and ready.get('rust_api') != rust_api:
                    raise ValueError('Rust server did not use the requested generic API')
                if direction == 'go-to-rust' and api == 'raw' and ready.get('raw_struct_validated') is not True:
                    raise ValueError('Rust raw server did not use the validated RawStruct API')
                for mode, pool in itertools.product(modes, pools or ['mux']):
                    for count in (0, 1, 8):
                        name = f'{mode}-{pool}-{count}'
                        log = directory / f'{name}.log'
                        payload = 'interop-中文-🦀'
                        command = [str(client_bin), '--role', 'client', '--addr', ready['addr'],
                                   '--service', 'Interop', '--mode', mode, '--messages', str(count),
                                   '--payload', payload]
                        if direction == 'go-to-rust' or api == 'typed':
                            command += ['--pool', pool]
                        if direction == 'rust-to-go' and rust_api != 'typed':
                            command += ['--api', rust_api]
                        if direction == 'go-to-rust' and go_api != 'typed':
                            command += ['--api', go_api]
                        executed = run(command, cwd=directory, timeout=timeout, log=log, env=environment)
                        row = {'name': name, 'direction': direction, 'transport': transport,
                               'api': api, 'rust_api': rust_api, 'go_api': go_api, 'configured_pool': pool, 'pool_reuse_validation': 'pending', 'execution': executed,
                               'ready': ready, 'expected': expected(mode, count, payload)}
                        try:
                            observed = result_event(log)
                            row['actual'] = observed
                            if executed['returncode'] != 0 or executed['timed_out']:
                                raise ValueError('client failed or timed out')
                            if not observed.get('ok') or observed.get('responses') != row['expected']:
                                raise ValueError('response/status differs from expected')
                            if direction == 'go-to-rust' and observed.get('profile') != profile:
                                raise ValueError('Go endpoint profile does not match the requested profile')
                            if direction == 'go-to-rust' and go_api != 'typed' and observed.get('api') != go_api:
                                raise ValueError('Go client did not use the requested generic API')
                            if direction == 'rust-to-go' and rust_api != 'typed' and observed.get('api') != rust_api:
                                raise ValueError('Rust endpoint did not use the requested generic API')
                            if direction == 'rust-to-go' and api == 'raw' and observed.get('raw_struct_validated') is not True:
                                raise ValueError('Rust raw client did not use the validated RawStruct API')
                            row['status'] = 'PASS'
                        except ValueError as error:
                            row['status'], row['reason'] = 'FAIL', str(error)
                        cases.append(row)
            except Exception as error:
                cases.append({'direction': direction, 'transport': transport, 'status': 'FAIL',
                              'reason': str(error), 'log': str(directory / 'server.log')})
            finally:
                server.close()
                # Remove only the socket owned by this runner, after the process exits.
                if socket.is_socket():
                    socket.unlink()
                socket_directory.cleanup()
    after = {str(p): hashlib.sha256(p.read_bytes()).hexdigest() for p in (go, rust)}
    if binaries != after:
        cases.append({'status': 'FAIL', 'reason': 'endpoint binaries changed during network suite'})
    report = {'schemaVersion': 1, 'suite': f'{api}-basic',
              'environment_overrides': env_overrides or {},
              'binaries': binaries, 'binaries_after': after,
              'cases': cases,
              'scope': f'{api} endpoint exchange; pool reuse/capacity, middleware and Rust direct ShmIPC are separate gates; real Mesh is outside this task'}
    (output / 'report.json').write_text(json.dumps(report, ensure_ascii=False, indent=2) + '\n')
    return report
