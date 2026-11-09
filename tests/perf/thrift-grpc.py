"""Real Thrift gRPC workload matrix; process ownership comes from the interop runner.

All samples are capability measurements. Protobuf regression is a separate
paired workload, and the 600-second resource gate is a separate suite.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import resource
import signal
import statistics
import tempfile

from kitex_streaming_interop.process import Server, run


def save(path, value):
    temporary = path.with_suffix(path.suffix + '.tmp')
    temporary.write_text(json.dumps(value, indent=2) + '\n')
    temporary.replace(path)


def main():
    def interrupted(signum, _frame):
        # Unwind through run()/Server cleanup before an outer time limit kills
        # the runner. Checkpoints remain resumable, never marked successful.
        raise SystemExit(128 + signum)
    signal.signal(signal.SIGTERM, interrupted)
    parser = argparse.ArgumentParser()
    parser.add_argument('--go-bin', type=Path, required=True)
    parser.add_argument('--rust-bin', type=Path, required=True)
    parser.add_argument('--out', type=Path, required=True)
    parser.add_argument('--seconds', type=int, default=3)
    parser.add_argument('--cpu-list', help='Linux taskset CPU list for client and server')
    parser.add_argument('--directions', default='rust-to-rust,rust-to-go,go-to-rust')
    parser.add_argument('--hooks', choices=('off', 'on'), default='off', help='Rust client-only Noop hook registration')
    parser.add_argument('--observability', choices=('off', 'on'), default='off', help='downstream Rust client exporter toggle; record runtime separately')
    parser.add_argument('--phase-latency', choices=('off', 'on'), default='off', help='instrument client header/first/last/terminal callbacks')
    parser.add_argument('--server-max-streams', type=int, help='diagnostic Rust server H2 stream limit, default unchanged')
    parser.add_argument('--case-filter', help='comma-separated transport-mode-concurrency-payload-compression identifiers')
    parser.add_argument('--resume', action='store_true', help='continue the same matrix without rerunning completed samples, including failures')
    args = parser.parse_args()
    if not 1 <= args.seconds <= 120:
        parser.error('--seconds must be in 1..120')
    directions = args.directions.split(',')
    if not directions or len(set(directions)) != len(directions) or any(value not in ('rust-to-rust', 'rust-to-go', 'go-to-rust') for value in directions):
        parser.error('invalid directions')
    if args.server_max_streams is not None and (not 1 <= args.server_max_streams <= 4294967295 or 'rust-to-go' in directions):
        parser.error('--server-max-streams requires a positive u32 limit and a Rust server')
    binaries = {'go': args.go_bin.resolve(), 'rust': args.rust_bin.resolve()}
    prefix = ['taskset', '-c', args.cpu_list] if args.cpu_list else []
    hashes = {key: hashlib.sha256(value.read_bytes()).hexdigest() for key, value in binaries.items()}
    output = args.out.resolve()
    output.mkdir(parents=True, exist_ok=True)
    configurations = [(transport, mode, concurrency, payload, compression)
                      for transport in ('tcp', 'uds') for mode in ('unary', 'client', 'server', 'bidi')
                      for concurrency in (1, 32, 256) for payload in (64, 1024, 65536, 1048576)
                      for compression in ('identity', 'gzip')]
    def name(values):
        return '-'.join(str(value) for value in values)
    if args.case_filter:
        requested = args.case_filter.split(',')
        selected = set(requested)
        configurations = [values for values in configurations if name(values) in selected]
        if len(configurations) != len(selected) or len(selected) != len(requested):
            parser.error('unknown or duplicate case-filter')
    env = dict(os.environ, GOPKG_DEBUG_ADDR='127.0.0.1:0')
    rows = []
    report = {'schemaVersion': 1, 'scope': 'new capability; short RPCs, client hooks/exporters only; no protobuf comparison or long-stream claim',
              'seconds': args.seconds, 'cpu_list': args.cpu_list, 'hooks': args.hooks, 'observability': args.observability,
              'phase_latency': args.phase_latency,
              'server_max_streams': args.server_max_streams,
              'latency_sampling': 'latest 8192 per worker; CPU includes warmup and server lifetime',
              'binaries': hashes, 'cases': rows, 'status': 'RUNNING'}
    report_path = output / 'report.json'
    names = [direction + '-' + name(configuration) for configuration in configurations for direction in directions]
    if args.resume:
        if not report_path.is_file():
            parser.error('--resume requires an existing report.json')
        previous = json.loads(report_path.read_text())
        for key in ('schemaVersion', 'scope', 'seconds', 'cpu_list', 'hooks', 'observability', 'server_max_streams', 'binaries', 'phase_latency'):
            if previous.get(key, 'off' if key == 'phase_latency' else None) != report[key]:
                parser.error('--resume differs from the recorded ' + key)
        if previous.get('binaries_after', hashes) != hashes:
            parser.error('--resume cannot reuse samples whose binaries changed during execution')
        rows.extend(previous['cases'])
        if [row['name'] for row in rows] != names[:len(rows)]:
            parser.error('--resume requires the same ordered cases')
    elif report_path.exists():
        parser.error('output already contains a report; use a new directory or --resume')
    save(report_path, report)
    cursor = 0
    for configuration in configurations:
        transport, mode, concurrency, payload, compression = configuration
        for direction in directions:
            case_name = direction + '-' + name(configuration)
            if cursor < len(rows):
                cursor += 1
                continue
            samples = []
            samples_path = output / case_name / 'samples.json'
            if args.resume and samples_path.is_file():
                samples = json.loads(samples_path.read_text())
                if len(samples) > 3 or any(sample.get('status') not in ('PASS', 'FAIL') for sample in samples):
                    parser.error('invalid saved samples for ' + case_name)
            for repetition in range(len(samples), 3):
                directory = output / case_name / str(repetition)
                directory.mkdir(parents=True, exist_ok=True)
                sample = {'status': 'FAIL'}
                peer = None
                before = resource.getrusage(resource.RUSAGE_CHILDREN)
                with tempfile.TemporaryDirectory(prefix='grpc-p-') as sockets:
                    try:
                        server = binaries['go' if direction == 'rust-to-go' else 'rust']
                        address = 'tcp://127.0.0.1:0' if transport == 'tcp' else 'unix://' + sockets + '/p.sock'
                        server_command = prefix + [str(server), '--role', 'grpc-thrift-server', '--addr', address,
                                                   '--compression', compression]
                        if args.server_max_streams is not None:
                            server_command += ['--server-max-streams', str(args.server_max_streams)]
                        peer = Server(server_command, cwd=directory, log=directory / 'server.log', env=env)
                        ready = peer.start(timeout=15)
                        if args.server_max_streams is not None and ready.get('max_concurrent_streams') != args.server_max_streams:
                            raise ValueError('Rust server did not select the diagnostic stream limit')
                        language = 'go' if direction == 'go-to-rust' else 'rust'
                        command = [str(binaries[language]), '--role', 'grpc-thrift-benchmark-client',
                                   '--addr', ready['addr'], '--mode', mode, '--concurrency', str(concurrency),
                                   '--payload-bytes', str(payload), '--duration-secs', str(args.seconds),
                                   '--compression', compression, '--report', str(directory / 'client.json')]
                        if language == 'rust':
                            command += ['--hooks', args.hooks, '--observability', args.observability]
                        if args.phase_latency == 'on':
                            command += ['--phase-latency', 'on']
                        command = prefix + command
                        executed = run(command, cwd=directory, timeout=args.seconds + 30, log=directory / 'client.log', env=env)
                        sample['execution'] = executed
                        if executed['returncode'] != 0 or executed['timed_out']:
                            raise RuntimeError('benchmark client did not finish')
                        observed = json.loads((directory / 'client.json').read_text())
                        sample['observed'] = observed
                        if observed['status'] != 'PASS' or observed['errors'] or observed['calls'] == 0:
                            raise RuntimeError('benchmark RPCs failed')
                        if observed['duration_secs'] < args.seconds:
                            raise RuntimeError('measurement ended early')
                        if args.phase_latency == 'on':
                            phases = observed.get('phase_latencies', {})
                            if (phases.get('finished') != observed['calls'] or phases.get('errors') != 0
                                    or phases.get('invalid_order') != 0 or phases.get('missing_callbacks', 0) != 0
                                    or not phases.get('sample_count')):
                                raise RuntimeError('phase timing terminal counts differ')
                            for phase in ('header_us', 'first_message_us', 'last_message_us', 'finished_us'):
                                if phases[phase]['count'] != phases['sample_count']:
                                    raise RuntimeError('phase timing missed a callback: ' + phase)
                        sample['status'] = 'PASS'
                    except (OSError, ValueError, RuntimeError, KeyError) as error:
                        sample['error'] = repr(error)
                    finally:
                        if peer is not None:
                            peer.close()
                after = resource.getrusage(resource.RUSAGE_CHILDREN)
                sample['client_and_server_cpu_secs'] = after.ru_utime + after.ru_stime - before.ru_utime - before.ru_stime
                samples.append(sample)
                save(samples_path, samples)
            row = {'name': case_name, 'status': 'PASS' if all(value['status'] == 'PASS' for value in samples) else 'FAIL'}
            if row['status'] == 'PASS':
                qps = [value['observed']['qps'] for value in samples]
                p99 = [value['observed']['latency_us']['p99'] for value in samples]
                row.update(qps_median=statistics.median(qps), qps_range=[min(qps), max(qps)],
                           p99_us_median=statistics.median(p99), p99_us_range=[min(p99), max(p99)])
                if args.phase_latency == 'on':
                    row['phase_latencies_us'] = {}
                    for phase in ('header_us', 'first_message_us', 'last_message_us', 'finished_us'):
                        p50 = [sample['observed']['phase_latencies'][phase]['p50'] for sample in samples]
                        p99 = [sample['observed']['phase_latencies'][phase]['p99'] for sample in samples]
                        row['phase_latencies_us'][phase] = {
                            'p50_median': statistics.median(p50),
                            'p99_median': statistics.median(p99),
                            'p99_range': [min(p99), max(p99)],
                        }
            rows.append(row)
            cursor += 1
            print(json.dumps(row), flush=True)
            save(report_path, report)
    after_hashes = {key: hashlib.sha256(value.read_bytes()).hexdigest() for key, value in binaries.items()}
    report.update(status='PASS' if hashes == after_hashes and all(value['status'] == 'PASS' for value in rows) else 'FAIL', binaries_after=after_hashes)
    save(report_path, report)
    return 0 if report['status'] == 'PASS' else 1


if __name__ == '__main__':
    raise SystemExit(main())
