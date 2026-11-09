"""Paired local measurements; reuse the interop process owner and retain failures."""
import argparse
import hashlib
import json
from pathlib import Path
import resource
import statistics

from kitex_streaming_interop.process import run


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--baseline', type=Path, required=True)
    parser.add_argument('--candidate', type=Path, required=True)
    parser.add_argument('--out', type=Path, required=True)
    parser.add_argument('--seconds', type=int, default=3)
    parser.add_argument('--cpu-list', help='Linux taskset CPU list shared by both processes')
    parser.add_argument('--case-filter', help='comma separated transport-concurrency-payload identifiers')
    args = parser.parse_args()
    if not 1 <= args.seconds <= 120:
        parser.error('--seconds must be in 1..120')
    binaries = {'baseline': args.baseline.resolve(), 'candidate': args.candidate.resolve()}
    hashes = {key: hashlib.sha256(path.read_bytes()).hexdigest() for key, path in binaries.items()}
    output = args.out.resolve()
    output.mkdir(parents=True, exist_ok=True)
    cases = [(transport, concurrency, size) for transport in ('tcp', 'uds')
             for concurrency in (1, 32, 256) for size in (64, 1024, 65536, 1048576)]
    if args.case_filter:
        names = set(args.case_filter.split(','))
        cases = [case for case in cases if '{}-{}-{}'.format(*case) in names]
        if len(cases) != len(names):
            parser.error('unknown or duplicate case filter')
    rows = []
    for transport, concurrency, size in cases:
        name = '{}-{}-{}'.format(transport, concurrency, size)
        samples = {key: [] for key in binaries}
        for repetition in range(3):
            for key in (('baseline', 'candidate') if repetition != 1 else ('candidate', 'baseline')):
                log = output / (name + '-' + str(repetition) + '-' + key + '.log')
                command = [str(binaries[key]), '--transport', transport, '--concurrency', str(concurrency),
                           '--payload-bytes', str(size), '--seconds', str(args.seconds)]
                if args.cpu_list:
                    command = ['taskset', '-c', args.cpu_list] + command
                before = resource.getrusage(resource.RUSAGE_CHILDREN)
                executed = run(command, cwd=output, timeout=args.seconds + 30, log=log)
                after = resource.getrusage(resource.RUSAGE_CHILDREN)
                observed = None
                for line in log.read_text().splitlines():
                    try:
                        value = json.loads(line)
                    except ValueError:
                        continue
                    if isinstance(value, dict) and value.get('protocol') == 'grpc-protobuf':
                        observed = value
                sample = {'execution': executed, 'observed': observed,
                          'cpu_seconds': after.ru_utime + after.ru_stime - before.ru_utime - before.ru_stime}
                samples[key].append(sample)
                (output / (name + '-samples.json')).write_text(json.dumps(samples, indent=2) + '\n')
        valid = all(sample['execution']['returncode'] == 0 and not sample['execution']['timed_out']
                    and sample['observed'] and sample['observed']['status'] == 'PASS'
                    for values in samples.values() for sample in values)
        row = {'name': name, 'status': 'FAIL', 'samples': str(output / (name + '-samples.json'))}
        if valid:
            summaries = {}
            for key, values in samples.items():
                qps = [sample['observed']['qps'] for sample in values]
                p99 = [sample['observed']['latency_us']['p99'] for sample in values]
                summaries[key] = {'qps_median': statistics.median(qps), 'qps_range': [min(qps), max(qps)],
                                  'p99_us_median': statistics.median(p99), 'p99_us_range': [min(p99), max(p99)]}
            qps_ratio = summaries['candidate']['qps_median'] / summaries['baseline']['qps_median']
            p99_ratio = summaries['candidate']['p99_us_median'] / summaries['baseline']['p99_us_median']
            row.update(status='PASS' if qps_ratio >= 0.95 and p99_ratio <= 1.10 else 'FAIL',
                       summaries=summaries, qps_ratio=qps_ratio, p99_ratio=p99_ratio)
        rows.append(row)
        print(json.dumps(row), flush=True)
    after_hashes = {key: hashlib.sha256(path.read_bytes()).hexdigest() for key, path in binaries.items()}
    report = {'schemaVersion': 1, 'scope': 'paired public Protobuf identity/unary workload only',
              'status': 'PASS' if hashes == after_hashes and all(row['status'] == 'PASS' for row in rows) else 'FAIL',
              'thresholds': {'min_qps_ratio': 0.95, 'max_p99_ratio': 1.10}, 'seconds': args.seconds, 'cpu_list': args.cpu_list,
              'binaries': hashes, 'binaries_after': after_hashes, 'cases': rows}
    (output / 'report.json').write_text(json.dumps(report, indent=2) + '\n')
    return 0 if report['status'] == 'PASS' else 1


if __name__ == '__main__':
    raise SystemExit(main())
