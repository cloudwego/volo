"""Explicit raw-H2 boundaries; never present a constructed peer as Kitex."""
from __future__ import annotations

import gzip
import hashlib
import io
import json
from pathlib import Path
import struct
import tempfile

from .process import Server, run


def compress(payload):
    output = io.BytesIO()
    with gzip.GzipFile(fileobj=output, mode='wb', mtime=0) as stream:
        stream.write(payload)
    return output.getvalue()


def frame(payload, flag=0):
    return struct.pack('>BI', flag, len(payload)) + payload


def requests():
    stop = frame(b'\0')
    args = frame(bytes.fromhex('0c00010000'))
    base = '/api.Interop/'
    # Go's permissive cardinality is a documented difference, not a Rust requirement.
    rows = [
        ('unary-args-result', 'Unary', [args], 0, 0, 1),
        ('void-stop-response', 'Zero', [stop], 0, 0, 1),
        ('raw-bidi-empty-stop', 'Bidi', [stop], 0, 0, 1),
        ('prefix-every-byte', 'Bidi', [bytes([v]) for v in stop], 0, 0, 1),
        ('coalesced-messages', 'Bidi', [stop + stop], 0, 0, 2),
        ('client-empty-stream', 'Client', [], 0, 0, 1),
        ('client-two-messages', 'Client', [stop + stop], 0, 0, 1),
        ('unary-zero-requests', 'Unary', [], 3, 13, None),
        ('unary-two-requests', 'Unary', [args + args], 3, 0, None),
        ('server-two-requests', 'Server', [stop + stop], 0, 0, 3),
    ]
    result = []
    for name, method, data, rust, go, count in rows:
        result.append({'name': name, 'input': {'path': base + method, 'data': [v.hex() for v in data]},
                       'rust': str(rust), 'go': str(go), 'messages': count})
    for name, path, rust, go in [
        ('unknown-service', '/api.Missing/Bidi', '12', '20'),
        ('unknown-method', base + 'Missing', '12', '1'),
    ]:
        result.append({'name': name, 'input': {'path': path, 'data': [stop.hex()]}, 'rust': rust, 'go': go})
    echo = frame(bytes.fromhex('0b000100000002676f00')).hex()
    for name, path, method in [
        ('fallback-unknown-service', '/api.Missing/Bidi', 'Bidi'),
        ('fallback-unknown-method', base + 'Missing', 'Missing'),
        ('fallback-known-route', base + 'Bidi', None),
    ]:
        result.append({'name': name, 'input': {'path': path, 'data': [echo]},
                       'targets': ['go-fallback'], 'go-fallback': '0',
                       'fallback_method': method, 'messages': 1})
    for name, extra, expected, http_status in [
        ('zero-budget', {'headers': {'grpc-timeout': '0n'}}, '4', '200'),
        ('wrong-subtype', {'content_type': 'application/grpc+proto'}, '12', '200'),
        ('non-grpc-content-type', {'content_type': 'application/json'}, '12', '415'),
        ('missing-content-type', {'content_type': ''}, '12', '415'),
        ('wrong-http-method', {'method': 'GET'}, '12', '405'),
        ('invalid-frame-flag', {'data': [frame(b'\0', 2).hex()]}, '13', '200'),
        ('zero-byte-thrift-payload', {'data': [frame(b'').hex()]}, '13', '200'),
    ]:
        value = {'path': base + 'Bidi', 'data': [stop.hex()]}
        value.update(extra)
        result.append({'name': name, 'input': value, 'rust': expected, 'targets': ['rust'], 'http_status': http_status})
    result.append({'name': 'headers-only-empty-stream', 'input': {'path': base + 'Client', 'header_end_stream': True, 'data': []}, 'rust': '0', 'targets': ['rust'], 'messages': 1})
    # With HEADERS END_STREAM and no DATA, this Kitex version does not reply
    # before the raw peer's read deadline. Keep that observed difference in the
    # audit report; require prompt rejection from Rust, not the Go hang.
    result.append({'name': 'unary-header-end-stream', 'input': {'path': base + 'Unary', 'header_end_stream': True, 'data': []}, 'rust': '3', 'targets': ['rust']})
    result.append({'name': 'unary-open-request', 'input': {'path': base + 'Unary', 'data': [args.hex()], 'end_stream': False,
        'headers': {'grpc-timeout': '150m'}}, 'rust': '4', 'go': '0'})
    result.append({'name': 'server-open-request', 'input': {'path': base + 'Server', 'data': [stop.hex()], 'end_stream': False}, 'rust': '0', 'go': '0', 'messages': 3})
    # Exercise the real generated FastRead/Message decoder through gRPC, in
    # addition to the local checked-protocol tests. Field 99 is unknown to the
    # message, so skip paths must enforce the same length/depth boundaries.
    # Kitex serializes the Thrift protocol error type ID (7). Check the error
    # text as well, so a real gRPC permission denial cannot satisfy the oracle.
    for name, payload, rust, go_error in [
        ('binary-negative-length', b'\x0b\x00\x01\xff\xff\xff\xff\x00', '13', 'invalid data length'),
        ('binary-truncated-value', b'\x0b\x00\x01\x00\x00\x00\x08x\x00', '13', 'ReadBinary: buf too small'),
        ('unknown-list-negative-length', b'\x0f\x00\x63\x0a\xff\xff\xff\xff\x00', '13', 'invalid data length'),
        ('unknown-set-negative-length', b'\x0e\x00\x63\x0a\xff\xff\xff\xff\x00', '13', 'invalid data length'),
        ('unknown-map-negative-length', b'\x0d\x00\x63\x0b\x0a\xff\xff\xff\xff\x00', '13', 'invalid data length'),
        ('unknown-list-over-budget', b'\x0f\x00\x63\x0a\x7f\xff\xff\xff\x00', '8', 'buffer too short'),
        ('unknown-nesting-within-budget', b'\x0c\x00\x63' * 10 + b'\x00' * 11, '0', None),
        ('unknown-nesting-over-budget', b'\x0c\x00\x63' * 100 + b'\x00' * 101, '8', 'depth limit exceeded'),
        ('valid-struct-with-trailing-bytes', b'\x00suffix', '0', None),
    ]:
        result.append({'name': name, 'input': {'path': base + 'Bidi', 'data': [frame(payload).hex()]},
                       'rust': rust, 'go': '7' if go_error else '0', 'go_error': go_error,
                       'messages': 1 if not go_error else None})
    command = b'lifecycle|bare-context-deadline|after-raw'
    payload = b'\x0b\x00\x01' + struct.pack('>I', len(command)) + command + b'\0'
    result.append({'name': 'bare-handler-context-deadline', 'input': {'path': base + 'Bidi',
        'headers': {'grpc-timeout': '50m'}, 'data': [frame(payload).hex()]},
        'targets': ['go'], 'go': '13', 'messages': 1, 'data_before_error': True})
    return result


def responses():
    stop = frame(b'\0')
    success = frame(bytes.fromhex('0c00000000'))
    result = []

    def add(name, mode='bidi', data=None, code=0, go=None, headers=None, trailers=None,
            messages=None, headers_hex=None, **extra):
        value = {'content_type': 'application/grpc+thrift', 'headers': headers or {},
                 'data': [item.hex() for item in (data or [])],
                 'trailers': {'grpc-status': '0'} if trailers is None else trailers}
        if headers_hex:
            value['headers_hex'] = headers_hex
        result.append(dict(name=name, mode=mode, input=value, rust=code,
                           go=go, messages=messages, **extra))

    add('unary-success', 'unary', [success], go=0, messages=1)
    add('void-stop', 'void', [stop], go=0, messages=1)
    add('client-single-raw-response', 'client', [stop], go=0, messages=1)
    add('unary-missing-response', 'unary', code=13)
    add('void-missing-response', 'void', code=13)
    add('unary-extra-response', 'unary', [success + success], code=13, go=2)
    add('client-extra-response', 'client', [stop + stop], code=13, go=2)
    add('bidi-empty-stop', data=[stop], go=0, messages=1)
    add('bidi-empty-stream', go=0, messages=0)
    add('header-ok-before-error-trailer', code=13, go=13, headers={'grpc-status': '0'}, trailers={'grpc-status': '13'})
    add('coalesced-responses', data=[stop + stop], go=0, messages=2)
    add('prefix-split-every-byte', data=[bytes([v]) for v in stop], go=0, messages=1)
    add('error-after-unary-value', 'unary', [success], code=4, go=4, trailers={'grpc-status': '4', 'grpc-message': 'expired'})
    add('peer-unknown-service-code', code=2, go=20, trailers={'grpc-status': '20'}, raw_code='20')
    add('peer-unknown-method-code', code=1, go=1, trailers={'grpc-status': '1'}, raw_code='1')
    add('zero-byte-payload', data=[frame(b'')], code=13)
    add('invalid-flag', data=[frame(b'\0', 2)], code=13)
    add('compressed-without-encoding', data=[frame(b'\0', 1)], code=13)
    add('unknown-encoding', data=[stop], code=12, headers={'grpc-encoding': 'unknown'})
    add('declared-size-over-budget', data=[struct.pack('>BI', 0, 16 * 1024 * 1024 + 1)], code=8)
    add('truncated-prefix', data=[b'\0\0'], code=13)
    add('truncated-body', data=[struct.pack('>BI', 0, 3) + b'\0'], code=13)
    add('missing-terminal-status', data=[stop], code=13, trailers={})
    add('bad-base64-header', code=13, headers={'grpc-status': '13', 'grpc-status-details-bin': '@@@'}, trailers={})
    add('bad-base64-final-trailer', code=13, trailers={'grpc-status': '0', 'grpc-status-details-bin': '@@@'})
    for name, encoded in [('padded', 'AQID/w=='), ('unpadded', 'AQID/w')]:
        add('details-' + name, code=13, trailers={'grpc-status': '13', 'grpc-status-details-bin': encoded}, details='010203ff')
    add('gzip-message', data=[frame(compress(b'\0'), 1)], go=0, messages=1, headers={'grpc-encoding': 'gzip'})
    add('gzip-corrupt', data=[frame(b'bad-gzip', 1)], code=13, headers={'grpc-encoding': 'gzip'})
    add('gzip-decompression-budget', data=[frame(compress(b'\0' * (16 * 1024 * 1024 + 1)), 1)], code=8, headers={'grpc-encoding': 'gzip'})
    extra = {'中文"\\': '值\n\t\0'}
    add('biz-unicode-json', code=13, headers={'grpc-status': '13', 'grpc-message': '%E5%A4%B1%E8%B4%A5', 'biz-status': '409', 'biz-extra': json.dumps(extra, ensure_ascii=False)}, trailers={}, biz=extra)
    add('biz-invalid-json', code=13, headers={'grpc-status': '13', 'biz-status': '409', 'biz-extra': '{'}, trailers={}, biz_error=True)
    add('biz-invalid-utf8', code=13, headers={'grpc-status': '13', 'biz-status': '409'},
        headers_hex={'biz-extra': b'{"key":"\xff"}'.hex()}, trailers={}, raw_code='13', biz_error=True)
    return result


def message_count(data):
    data = bytes.fromhex(data)
    count = 0
    while data:
        if len(data) < 5:
            raise ValueError('incomplete response prefix')
        flag, length = struct.unpack('>BI', data[:5])
        if flag not in (0, 1) or len(data) < 5 + length:
            raise ValueError('invalid response framing')
        data = data[5 + length:]
        count += 1
    return count


def boundaries(*, go: Path, rust: Path, output: Path, timeout: float,
               transports: list[str], direction='both', env=None, case_filter=None) -> dict:
    output.mkdir(parents=True, exist_ok=True)
    hashes = {'go': hashlib.sha256(go.read_bytes()).hexdigest(), 'rust': hashlib.sha256(rust.read_bytes()).hexdigest()}
    rows = []
    request_cases, response_cases = requests(), responses()
    if case_filter:
        selected = set(case_filter.split(','))
        valid = {'request/' + case['name'] for case in request_cases} | {'response/' + case['name'] for case in response_cases}
        if selected - valid:
            raise ValueError('unknown boundary case: ' + ','.join(sorted(selected - valid)))
        request_cases = [case for case in request_cases if 'request/' + case['name'] in selected]
        response_cases = [case for case in response_cases if 'response/' + case['name'] in selected]
    for transport in transports:
        if transport not in ('tcp', 'uds'):
            raise ValueError('raw gRPC fixtures support tcp/uds only')
        for kind, binary, role in [
            ('rust', rust, 'grpc-thrift-server'),
            ('go', go, 'grpc-thrift-server'),
            ('go-fallback', go, 'grpc-thrift-fallback-server'),
        ]:
            cases = [case for case in request_cases if kind in case.get('targets', ('rust', 'go'))]
            if not cases and kind == 'go-fallback':
                continue
            directory = output / (transport + '-' + kind + '-request')
            directory.mkdir(parents=True, exist_ok=True)
            with tempfile.TemporaryDirectory(prefix='grpc-') as sockets:
                address = 'tcp://127.0.0.1:0' if transport == 'tcp' else 'unix://' + sockets + '/rpc.sock'
                server_command = [str(binary), '--role', role, '--addr', address]
                server = Server(server_command, cwd=directory, log=directory / 'server.log', env=env)
                try:
                    ready = server.start(timeout)
                    for case in cases:
                        work = directory / case['name']
                        work.mkdir(parents=True, exist_ok=True)
                        value = {'content_type': 'application/grpc+thrift', 'end_stream': True}
                        value.update(case['input'])
                        source = work / 'input.json'
                        source.write_text(json.dumps(value))
                        command = [str(go), '--role', 'grpc-thrift-wire', '--addr', ready['addr'], '--input', str(source), '--report', str(work / 'actual.json')]
                        row = {'name': case['name'], 'transport': transport, 'target': kind, 'peer': 'raw-h2-client', 'status': 'FAIL', 'input': value, 'expected_code': case[kind]}
                        if 'fallback_method' in case:
                            row.update(server_command=server_command, expected_fallback_method=case['fallback_method'])
                        try:
                            executed = run(command, cwd=work, timeout=timeout, log=work / 'client.log', env=env)
                            if executed['returncode'] or executed['timed_out']:
                                raise ValueError('raw wire client failed')
                            row['command'] = executed
                            actual = json.loads((work / 'actual.json').read_text())
                            row['actual'] = actual
                            terminal = actual['trailers'].get('grpc-status', actual['headers'].get('grpc-status'))
                            if terminal != case[kind] or actual['reset']:
                                raise ValueError('request boundary status differs: ' + str(terminal))
                            if kind == 'go' and case.get('go_error'):
                                message = actual['trailers'].get('grpc-message', actual['headers'].get('grpc-message', ''))
                                if case['go_error'] not in message:
                                    raise ValueError('Go protocol error reason differs: ' + message)
                            if case[kind] != '0' and actual['data'] and not case.get('data_before_error'):
                                raise ValueError('rejected request produced success DATA')
                            if actual['headers'].get(':status') != case.get('http_status', '200'):
                                raise ValueError('HTTP status differs')
                            if case.get('messages') is not None and message_count(actual['data']) != case['messages']:
                                raise ValueError('response cardinality differs')
                            if 'fallback_method' in case:
                                if actual['headers'].get('x-interop-fallback-method') != case['fallback_method']:
                                    raise ValueError('unexpected Kitex fallback route')
                                if actual['data'] != value['data'][0]:
                                    raise ValueError('fallback fixture did not echo the Thrift payload')
                            row.update(status='PASS', actual=actual, command=executed)
                        except (OSError, ValueError, RuntimeError) as error:
                            row['error'] = str(error)
                        rows.append(row)
                except (OSError, ValueError, RuntimeError) as error:
                    rows.append({'status': 'FAIL', 'target': kind, 'transport': transport, 'error': str(error)})
                finally:
                    server.close()
            for case in response_cases:
                if case.get(kind) is None:
                    continue
                work = output / (transport + '-' + kind + '-response') / case['name']
                work.mkdir(parents=True, exist_ok=True)
                source = work / 'input.json'
                source.write_text(json.dumps(case['input'], ensure_ascii=False))
                with tempfile.TemporaryDirectory(prefix='grpc-') as sockets:
                    address = 'tcp://127.0.0.1:0' if transport == 'tcp' else 'unix://' + sockets + '/rpc.sock'
                    server = Server([str(go), '--role', 'grpc-thrift-raw-response-server', '--addr', address, '--input', str(source)], cwd=work, log=work / 'server.log', env=env)
                    row = {'name': case['name'], 'transport': transport, 'target': kind, 'peer': 'raw-h2-server', 'status': 'FAIL', 'input': case['input'], 'expected_code': case[kind]}
                    try:
                        ready = server.start(timeout)
                        command = [str(binary), '--role', 'grpc-thrift-response-client', '--addr', ready['addr'], '--mode', case['mode'], '--report', str(work / 'actual.json')]
                        executed = run(command, cwd=work, timeout=timeout, log=work / 'client.log', env=env)
                        if executed['returncode'] or executed['timed_out']:
                            raise ValueError('response probe failed')
                        row['command'] = executed
                        actual = json.loads((work / 'actual.json').read_text())
                        row['actual'] = actual
                        if actual.get('status') != 'OBSERVED' or actual.get('code') != case[kind]:
                            raise ValueError('response boundary status differs: ' + str(actual.get('code')))
                        if case.get('messages') is not None and actual['messages'] != case['messages']:
                            raise ValueError('response cardinality differs')
                        if kind == 'rust':
                            if 'raw_code' in case and actual['raw_code'] != case['raw_code']:
                                raise ValueError('peer status provenance was lost')
                            if 'details' in case and actual['details_hex'] != case['details']:
                                raise ValueError('opaque error details differ')
                            if 'biz' in case and ((actual.get('biz') or {}).get('code') != 409 or (actual.get('biz') or {}).get('extra') != case['biz']):
                                raise ValueError('business JSON differs')
                            if case.get('biz_error') and not actual['biz_error']:
                                raise ValueError('invalid business JSON was not reported')
                        row.update(status='PASS', actual=actual, command=executed)
                    except (OSError, ValueError, RuntimeError) as error:
                        row['error'] = str(error)
                    finally:
                        server.close()
                    rows.append(row)
    after = {'go': hashlib.sha256(go.read_bytes()).hexdigest(), 'rust': hashlib.sha256(rust.read_bytes()).hexdigest()}
    if after != hashes:
        rows.append({'status': 'FAIL', 'error': 'endpoint binary changed during suite'})
    report = {'schemaVersion': 1, 'suite': 'grpc-thrift-boundaries', 'status': 'PASS' if rows and all(row['status'] == 'PASS' for row in rows) else 'FAIL', 'cases': rows, 'binaries': hashes, 'binaries_after': after, 'scope': 'raw H2 request/response boundaries; Go reference behavior separately asserted; Rust-only limits are enhancements'}
    (output / 'report.json').write_text(json.dumps(report, ensure_ascii=False, indent=2) + '\n')
    return report
