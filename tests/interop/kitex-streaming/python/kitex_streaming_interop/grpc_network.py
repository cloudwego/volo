"""Direct TCP/UDS Thrift-over-gRPC interoperability, using the shared runner."""
from __future__ import annotations

import hashlib
import json
import tempfile
from pathlib import Path

from .process import Server, run


EXPECTED = {
    'unary-args-result', 'explicit-unary', 'zero-void-stop',
    'multiple-parameters-field-ids', 'container-result',
    'multiple-declared-exceptions', 'single-declared-void-exception',
    'client-stream-zero-one-many', 'server-stream-and-inherited-path',
    'bidi-first-response-before-half-close', 'biz-status-unicode-json',
}


def basic(*, go: Path, rust: Path, output: Path, timeout: float,
          transports: list[str], direction: str = 'both', env=None, compression='identity', legacy=False,
          metadata_values=False) -> dict:
    output.mkdir(parents=True, exist_ok=True)
    rows = []
    hashes = {'go': hashlib.sha256(go.read_bytes()).hexdigest(), 'rust': hashlib.sha256(rust.read_bytes()).hexdigest()}
    if compression not in ('identity', 'gzip'): raise ValueError('invalid compression')
    directions = ['go-to-rust', 'rust-to-go'] if direction == 'both' else [direction]
    for transport in transports:
        if transport not in ('tcp', 'uds'):
            raise ValueError('gRPC direct fixture supports tcp/uds only')
        for selected in directions:
            if selected not in ('go-to-rust', 'rust-to-go'):
                raise ValueError('unknown gRPC direction: ' + selected)
            work = output / (transport + '-' + selected)
            work.mkdir(parents=True, exist_ok=True)
            server, client = (rust, go) if selected == 'go-to-rust' else (go, rust)
            socket_dir = tempfile.TemporaryDirectory(prefix='grpc-')
            address = 'tcp://127.0.0.1:0' if transport == 'tcp' else 'unix://' + str(Path(socket_dir.name) / 'rpc.sock')
            row = {'transport': transport, 'direction': selected, 'status': 'FAIL'}
            server_role = 'grpc-thrift-legacy-server' if legacy and selected == 'rust-to-go' else 'grpc-thrift-server'
            client_role = 'grpc-thrift-legacy-client' if legacy and selected == 'go-to-rust' else 'grpc-thrift-client'
            if metadata_values:
                client_role = 'grpc-thrift-metadata-values-client'
                if selected == 'rust-to-go':
                    server_role = 'grpc-thrift-metadata-values-server'
            process = Server([str(server), '--role', server_role, '--addr', address, '--compression', compression],
                            cwd=work, log=work / 'server.log', env=env)
            try:
                ready = process.start(timeout=min(timeout, 30))
                actual = ready.get('addr', '')
                if not actual.startswith('tcp://' if transport == 'tcp' else 'unix://'):
                    raise ValueError('readiness transport differs from requested transport')
                result = run([str(client), '--role', client_role, '--addr', actual,
                              '--report', str(work / 'client.json'), '--compression', compression], cwd=work, timeout=timeout,
                             log=work / 'client.log', env=env)
                row['command'] = result
                if result['returncode'] != 0 or result['timed_out']:
                    raise ValueError('client process failed or timed out')
                report = json.loads((work / 'client.json').read_text())
                cases = report.get('cases', [])
                expected = {'ascii-multi-binary-header-trailer'} if metadata_values else EXPECTED
                if report.get('status') != 'PASS' or set(cases) != expected or len(cases) != len(expected):
                    raise ValueError('client did not verify the complete generated API matrix')
                if legacy and selected == 'go-to-rust' and report.get('legacy') is not True:
                    raise ValueError('Go did not use the requested legacy generated API')
                row.update(status='PASS', cases=cases, server_ready=ready,
                           go_api='legacy.StreamClient (metadata.MD)' if metadata_values else 'legacy.Client+StreamClient' if legacy else 'streamx.Client')
            except (OSError, ValueError, RuntimeError) as error:
                row['error'] = str(error)
            finally:
                process.close()
                socket_dir.cleanup()
            rows.append(row)
    after = {'go': hashlib.sha256(go.read_bytes()).hexdigest(), 'rust': hashlib.sha256(rust.read_bytes()).hexdigest()}
    if after != hashes: rows.append({'status': 'FAIL', 'error': 'endpoint binary changed during suite'})
    report = {'schemaVersion': 1, 'suite': 'grpc-thrift-metadata-values' if metadata_values else 'grpc-thrift-legacy' if legacy else 'grpc-thrift-gzip' if compression == 'gzip' else 'grpc-thrift-basic', 'protocol': 'grpc-thrift',
              'status': 'PASS' if rows and all(row['status'] == 'PASS' for row in rows) else 'FAIL',
              'scope': 'direct generated API interoperability; lifecycle/boundaries remain separate gates',
              'cases': rows, 'binaries': hashes, 'binaries_after': after, 'compression': compression}
    (output / 'report.json').write_text(json.dumps(report, indent=2) + '\n')
    return report
