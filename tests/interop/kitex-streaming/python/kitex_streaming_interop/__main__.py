from __future__ import annotations

import argparse
import json
import os
from pathlib import Path
import sys

from . import SCHEMA_VERSION, __version__
from .idl import matrix
from .network import basic
from .codec import golden
from .grpc_payload import golden as grpc_payload_golden
from .grpc_network import basic as grpc_network_basic
from .grpc_lifecycle import lifecycle as grpc_lifecycle
from .grpc_boundaries import boundaries as grpc_boundaries
from .grpc_load import load as grpc_load
from .grpc_keepalive import keepalive as grpc_keepalive
from .wire import boundaries
from .idle import idle_unary
from .metainfo import multihop
from .cascade import cascading
from .status import status_errors
from .responses import response_boundaries
from .routing import service_routing
from .controls import controlled_lifecycle
from .lifecycle import lifecycle
from .shutdown import server_shutdown, shutdown_boundaries
from .budget import budget_limits
from .metadata_wire import metadata_wire
from .inherited import inherited_service
from .error_multihop import error_multihop


def suite_status(cases):
    statuses = {case['status'] for case in cases}
    for status in ('FAIL', 'BLOCKED', 'INCOMPLETE', 'SKIP'):
        if status in statuses:
            return status
    return 'PASS' if cases else 'INCOMPLETE'


def validated_environment(profile_config, selected_profile, inherited):
    """Validate direct fixture configuration without logging effective env values."""
    environment = profile_config.get('environment', {})
    if not isinstance(environment, dict):
        raise ValueError('profile environment must be a table')
    if selected_profile not in (None, 'direct') or environment.get('profile', 'direct') != 'direct':
        raise ValueError('only the direct env-profile is supported; real Mesh scenarios are deferred')
    isolation = environment.get('process_isolation', 'scenario-group')
    if isolation != 'scenario-group':
        raise ValueError('only process_isolation=scenario-group is supported')
    legacy_fresh = environment.get('fresh_process_per_case', False)
    if not isinstance(legacy_fresh, bool) or legacy_fresh:
        raise ValueError('fresh_process_per_case=true is unsupported; network suites intentionally reuse servers within scenario groups')
    overrides = environment.get('set', {})
    if not isinstance(overrides, dict) or any(
            not isinstance(key, str) or not key.strip() or '=' in key or '\0' in key
            or not isinstance(value, str) or '\0' in value
            for key, value in overrides.items()):
        raise ValueError('profile environment.set must contain nonempty string keys and string values')
    required = environment.get('required', [])
    if (not isinstance(required, list) or any(not isinstance(key, str) or not key.strip() or '=' in key or '\0' in key for key in required)
            or len(set(required)) != len(required)):
        raise ValueError('profile environment.required must be a list of unique nonempty variable names')
    effective = dict(inherited)
    effective.update(overrides)
    missing = [key for key in required if not isinstance(effective.get(key), str) or not effective[key].strip()]
    if missing:
        raise ValueError('missing or empty required environment variables: ' + ', '.join(missing))
    return overrides, {'profile': 'direct', 'process_isolation': isolation,
                       'required_names': required, 'override_names': sorted(overrides)}


def main() -> int:
    parser = argparse.ArgumentParser(description='Kitex streaming interoperability runner')
    parser.add_argument('--version', action='version', version=__version__)
    parser.add_argument('--profile', default='upstream')
    parser.add_argument('--suite', default='all')
    parser.add_argument('--case-filter', help='grpc-thrift-boundaries: request/name or response/name, comma separated')
    parser.add_argument('--out', type=Path, required=True)
    parser.add_argument('--idl-root', type=Path, default=Path('tests/interop/kitex-codec'))
    parser.add_argument('--fixture-root', type=Path)
    parser.add_argument('--kitex-bin', type=Path, default=os.environ.get('TTSTREAM_KITEX_BIN'))
    parser.add_argument('--go-bin', type=Path, default=os.environ.get('TTSTREAM_GO_BIN'))
    parser.add_argument('--rust-bin', type=Path, default=os.environ.get('TTSTREAM_RUST_BIN'))
    parser.add_argument('--rust-bytes', type=Path)
    parser.add_argument('--timeout', type=float, default=180)
    parser.add_argument('--idle-seconds', type=float, default=300)
    parser.add_argument('--load-seconds', type=int, default=10)
    parser.add_argument('--load-concurrency', type=int, default=16)
    parser.add_argument('--load-payload-bytes', type=int, default=128)
    parser.add_argument('--load-cooldown-seconds', type=int, default=0)
    parser.add_argument('--load-compression', choices=['identity', 'gzip'], default='identity')
    parser.add_argument('--regenerate', action='store_true')
    parser.add_argument('--generation-only', action='store_true', help='record unverified generation; never counts as full acceptance')
    parser.add_argument('--protocol', default='ttstream')
    parser.add_argument('--direction', default='both')
    parser.add_argument('--mode', default='all')
    parser.add_argument('--transport', default='tcp,uds')
    parser.add_argument('--pool', default='short,long,mux')
    parser.add_argument('--rust-api', choices=['typed', 'binary', 'json', 'map'], default='typed')
    parser.add_argument('--go-api', choices=['typed', 'binary'], default='typed')
    parser.add_argument('--reference-only', action='store_true', help='error-status/response-boundaries: run Go reference endpoints only')
    parser.add_argument('--topology', default='all', help='multihop A/B/C roles: g=Go, r=Rust')
    parser.add_argument('--metainfo-concurrency', type=int, default=1)
    parser.add_argument('--env-profile', choices=['direct'], help='direct only; real Mesh scenarios are deferred')
    args = parser.parse_args()
    if args.timeout <= 0:
        parser.error('--timeout must be positive')
    args.out = args.out.resolve()
    args.out.mkdir(parents=True, exist_ok=True)
    results = []
    profile_config = {}
    if args.profile == 'upstream':
        profile_name = 'upstream'
    elif Path(args.profile).is_file():
        try:
            import tomllib
        except ImportError:
            import tomli as tomllib
        with Path(args.profile).open('rb') as profile_file:
            profile_config = tomllib.load(profile_file)
            profile_name = profile_config['name']
    else:
        parser.error('non-upstream --profile must be an existing profile.toml')
    if args.protocol not in ('ttstream', 'grpc-thrift'):
        parser.error('--protocol must be ttstream or grpc-thrift')
    selected_suites = args.suite.split(',')
    if args.protocol == 'grpc-thrift' and args.suite == 'all':
        selected_suites = ['grpc-thrift-codec', 'grpc-thrift-basic', 'grpc-thrift-gzip', 'grpc-thrift-legacy', 'grpc-thrift-metadata-values', 'grpc-thrift-boundaries', 'grpc-thrift-lifecycle', 'grpc-thrift-multihop']
    if any(suite.startswith('grpc-thrift-') != (args.protocol == 'grpc-thrift') for suite in selected_suites):
        parser.error('suite and --protocol differ; gRPC suites cannot run TTStream scenarios')
    try:
        env_overrides, environment_contract = validated_environment(profile_config, args.env_profile, os.environ)
    except ValueError as error:
        parser.error(str(error))
    child_environment = os.environ.copy()
    child_environment.update(env_overrides)
    for suite in selected_suites:
        if suite == 'idl-matrix':
            if not args.kitex_bin or not args.kitex_bin.is_file():
                results.append({'suite': suite, 'status': 'BLOCKED', 'reason': '--kitex-bin must identify the fixed-version built generator'})
                continue
            report = matrix(tool=args.kitex_bin.resolve(), source_root=args.idl_root.resolve(),
                            output=args.out / suite, timeout=args.timeout, env=child_environment,
                            compile_generated=not args.generation_only)
            statuses = {c['status'] for c in report['cases']}
            results.append({'suite': suite, 'status': 'FAIL' if 'FAIL' in statuses else 'INCOMPLETE' if 'INCOMPLETE' in statuses else 'PASS',
                            'report': str(args.out / suite / 'report.json'), 'cases': len(report['cases']),
                            'meaning': 'fixed generator/compile outcome regression; Rust alignment is a separate gate'})
        elif suite in ('grpc-thrift-basic', 'grpc-thrift-gzip', 'grpc-thrift-legacy', 'grpc-thrift-metadata-values', 'grpc-thrift-lifecycle', 'grpc-thrift-multihop', 'grpc-thrift-boundaries', 'grpc-thrift-load', 'grpc-thrift-keepalive'):
            if not args.go_bin or not args.rust_bin or not args.go_bin.is_file() or not args.rust_bin.is_file():
                results.append({'suite': suite, 'status': 'BLOCKED', 'reason': 'built --go-bin and --rust-bin are required'})
                continue
            function = {
                'grpc-thrift-basic': grpc_network_basic,
                'grpc-thrift-gzip': grpc_network_basic,
                'grpc-thrift-legacy': grpc_network_basic,
                'grpc-thrift-metadata-values': grpc_network_basic,
                'grpc-thrift-lifecycle': grpc_lifecycle,
                'grpc-thrift-multihop': grpc_lifecycle,
                'grpc-thrift-boundaries': grpc_boundaries,
                'grpc-thrift-load': grpc_load,
                'grpc-thrift-keepalive': grpc_keepalive,
            }[suite]
            grpc_options = {}
            if suite == 'grpc-thrift-legacy':
                grpc_options['legacy'] = True
            elif suite == 'grpc-thrift-metadata-values':
                grpc_options['metadata_values'] = True
            elif suite == 'grpc-thrift-gzip':
                grpc_options['compression'] = 'gzip'
            elif suite == 'grpc-thrift-multihop':
                grpc_options['multihop'] = True
            elif suite == 'grpc-thrift-boundaries':
                grpc_options['case_filter'] = args.case_filter
            elif suite == 'grpc-thrift-load':
                grpc_options.update(duration=args.load_seconds, concurrency=args.load_concurrency,
                                    payload_bytes=args.load_payload_bytes, cooldown=args.load_cooldown_seconds,
                                    compression=args.load_compression)
            report = function(go=args.go_bin.resolve(), rust=args.rust_bin.resolve(),
                                        output=(args.out / suite).resolve(), timeout=args.timeout,
                                        transports=args.transport.split(','), direction=args.direction,
                                        env=child_environment, **grpc_options)
            results.append({'suite': suite, 'status': report['status'], 'cases': len(report['cases']),
                            'report': str(args.out / suite / 'report.json')})
        elif suite in ('codec-golden', 'grpc-thrift-codec'):
            if not args.go_bin or not args.go_bin.is_file():
                results.append({'suite': suite, 'status': 'BLOCKED', 'reason': '--go-bin oracle must exist'})
                continue
            golden_function = grpc_payload_golden if suite == 'grpc-thrift-codec' else golden
            results.append(golden_function(oracle=args.go_bin.resolve(), fixture_root=(args.fixture_root or args.idl_root).resolve(),
                                  output=args.out / suite, regenerate=args.regenerate,
                                  rust_bytes=args.rust_bytes.resolve() if args.rust_bytes else None, timeout=args.timeout,
                                  env=child_environment))
        elif suite in ('wire-boundaries', 'idle-unary', 'metainfo', 'cascade', 'error-status', 'response-boundaries', 'service-routing', 'drain-disconnect', 'timeout-policy', 'server-shutdown', 'budget-limits', 'shutdown-boundaries', 'metadata-wire', 'inherited-service', 'error-multihop'):
            if not args.go_bin or not args.rust_bin or not args.go_bin.is_file() or not args.rust_bin.is_file():
                results.append({'suite': suite, 'status': 'BLOCKED', 'reason': '--go-bin and --rust-bin must exist'})
                continue
            suite_function = {'wire-boundaries': boundaries, 'idle-unary': idle_unary,
                              'metainfo': multihop, 'cascade': cascading, 'error-status': status_errors,
                              'response-boundaries': response_boundaries, 'service-routing': service_routing,
                              'drain-disconnect': controlled_lifecycle, 'timeout-policy': lifecycle,
                              'server-shutdown': server_shutdown, 'budget-limits': budget_limits,
                              'shutdown-boundaries': shutdown_boundaries, 'metadata-wire': metadata_wire,
                              'inherited-service': inherited_service, 'error-multihop': error_multihop}[suite]
            options = {}
            if suite in ('error-status', 'response-boundaries'):
                options['reference_only'] = args.reference_only
            if suite == 'response-boundaries':
                if args.reference_only and args.rust_api != 'typed':
                    parser.error('reference-only has no Rust generic endpoint')
                options['rust_api'] = args.rust_api
            if suite == 'idle-unary':
                if args.idle_seconds <= 0 or args.timeout <= 2 * args.idle_seconds:
                    parser.error('idle-unary requires positive --idle-seconds and --timeout greater than twice that interval')
                options['idle_seconds'] = args.idle_seconds
            if suite == 'metainfo':
                if not 1 <= args.metainfo_concurrency <= 32:
                    parser.error('--metainfo-concurrency must be in 1..32')
                options['concurrency'] = args.metainfo_concurrency
            if suite in ('metainfo', 'cascade', 'error-multihop'):
                options['topologies'] = (['ggg', 'grg', 'ggr', 'grr', 'rgg', 'rrg', 'rgr', 'rrr']
                                         if args.topology == 'all' else args.topology.split(','))
            report = suite_function(go=args.go_bin.resolve(), rust=args.rust_bin.resolve(), output=args.out / suite,
                                    transports=args.transport.split(','), timeout=args.timeout,
                                    profile=profile_name, env_overrides=env_overrides, **options)
            results.append({'suite': suite, 'status': suite_status(report['cases']),
                            'report': str(args.out / suite / 'report.json'), 'cases': len(report['cases'])})
        elif suite in ('raw-basic', 'typed-basic'):
            if not args.go_bin or not args.rust_bin or not args.go_bin.is_file() or not args.rust_bin.is_file():
                results.append({'suite': suite, 'status': 'BLOCKED', 'reason': '--go-bin and --rust-bin must exist'})
                continue
            modes = (['unary', 'explicit-unary', 'client', 'server', 'bidi'] if suite == 'typed-basic'
                     else ['client', 'server', 'bidi']) if args.mode == 'all' else args.mode.split(',')
            if args.go_api == 'binary' and any(mode not in ('client', 'server', 'bidi') for mode in modes):
                parser.error('--go-api binary requires --mode client,server,bidi or a subset')
            directions = ['go-to-rust', 'rust-to-go'] if args.direction == 'both' else args.direction.split(',')
            if any(d not in ('go-to-rust', 'rust-to-go') for d in directions):
                parser.error('invalid direction')
            report = basic(go=args.go_bin.resolve(), rust=args.rust_bin.resolve(), output=args.out / suite,
                           transports=args.transport.split(','), modes=modes, timeout=args.timeout,
                           directions=directions, api='typed' if suite == 'typed-basic' else 'raw',
                           pools=args.pool.split(',') if suite == 'typed-basic' else ['mux'], profile=profile_name,
                           env_overrides=env_overrides, rust_api=args.rust_api, go_api=args.go_api)
            results.append({'suite': suite, 'status': suite_status(report['cases']),
                            'report': str(args.out / suite / 'report.json'), 'cases': len(report['cases'])})
        else:
            results.append({'suite': suite, 'status': 'INCOMPLETE', 'reason': 'suite is not implemented yet'})
    report = {'schemaVersion': SCHEMA_VERSION, 'tool_version': __version__, 'profile': args.profile, 'protocol': args.protocol,
              'command': sys.argv, 'environment_contract': environment_contract, 'results': results}
    (args.out / 'summary.json').write_text(json.dumps(report, indent=2) + '\n')
    print(json.dumps(report))
    return 0 if all(r['status'] == 'PASS' for r in results) else 1


if __name__ == '__main__':
    raise SystemExit(main())
