"""Actual fixed-version Kitex generation and generated-code compile oracle."""
from __future__ import annotations

import hashlib
import json
import os
from pathlib import Path

from .process import run


BASE = '''namespace go matrix
struct Message { 1: string value }
exception Failure { 1: string message }
typedef Message Alias
'''


def cases() -> dict[str, str]:
    result = {}
    for mode in (None, "unary", "client", "server", "bidirectional", "invalid", ""):
        ann = '' if mode is None else f' (streaming.mode="{mode}")'
        result['mode_' + ('none' if mode is None else mode or 'empty')] = BASE + (
            'service Test { Message Call(1: Message request)' + ann + ' }\n')
    ann = ' (streaming.mode="bidirectional")'
    signatures = {
        'empty_args': 'Message Call()',
        'multiple_args': 'Message Call(1: Message a, 2: Message b)',
        'void': 'void Call(1: Message a)',
        'oneway': 'oneway void Call(1: Message a)',
        'throws': 'Message Call(1: Message a) throws (1: Failure failure)',
        'scalar_arg': 'Message Call(1: i64 a)',
        'scalar_return': 'i64 Call(1: Message a)',
        'list_arg': 'Message Call(1: list<Message> a)',
        'list_return': 'list<Message> Call(1: Message a)',
        'map_arg': 'Message Call(1: map<string,Message> a)',
        'map_return': 'map<string,Message> Call(1: Message a)',
        'set_arg': 'Message Call(1: set<i32> a)',
        'set_return': 'set<i32> Call(1: Message a)',
        'typedef': 'Alias Call(1: Alias a)',
    }
    for name, sig in signatures.items():
        result[name] = BASE + 'service Test { ' + sig + ann + ' }\n'
    for name, tail in [('duplicate_same', '"client", streaming.mode="client"'),
                       ('duplicate_different', '"client", streaming.mode="server"')]:
        result[name] = BASE + ('service Test { Message Call(1: Message a) '
                              f'(streaming.mode={tail})' + ' }\n')
    result['duplicate_keys'] = BASE + ('service Test { Message Call(1: Message a) '
        '(streaming.mode="client", streaming.mode="server") }\n')
    for name, sig in [('unary_empty_args', 'Message Call()'),
                      ('unary_multiple_args', 'Message Call(1: Message a, 2: Message b)'),
                      ('unary_throws', 'Message Call(1: Message a) throws (1: Failure f)')]:
        result[name] = BASE + 'service Test { ' + sig + ' (streaming.mode="unary") }\n'
    result['mixed'] = BASE + '''service Test {
        Message Plain(1: Message a)
        Message Unary(1: Message a) (streaming.mode="unary")
        Message Client(1: Message a) (streaming.mode="client")
        Message Server(1: Message a) (streaming.mode="server")
        Message Bidi(1: Message a) (streaming.mode="bidirectional")
    }
    service Child extends Test { Message Extra(1: Message a) }
    '''
    result['all_services_throws'] = BASE + '''service Good { Message Plain(1: Message a) }
    service ThrowsService {
        Message Fail(1: Message a) throws (1: Failure f) (streaming.mode="client")
    }
    '''
    return result


def matrix(*, tool: Path, source_root: Path, output: Path,
           timeout: float, env: dict[str, str], compile_generated: bool = True) -> dict:
    output.mkdir(parents=True, exist_ok=True)
    all_cases = cases()
    for p in sorted(source_root.glob('idl/*.thrift')):
        all_cases['external_' + p.stem] = p.read_text()
    rows = []
    for name, source in all_cases.items():
        for style in ('streamx', 'legacy'):
            directory = output / name / style
            directory.mkdir(parents=True, exist_ok=True)
            idl = directory / 'input.thrift'
            idl.write_text(source)
            # Include fixtures retain their original include names.
            for p in sorted(source_root.glob('idl/*.thrift')):
                (directory / p.name).write_bytes(p.read_bytes())
            module = f'github.com/cloudwego/volo/interop/matrix/{name}/{style}'
            (directory / 'go.mod').write_text(
                f'module {module}\n\ngo 1.20\n\nrequire github.com/cloudwego/kitex v0.16.4\n')
            command = [str(tool), '-module', module]
            if style == 'streamx':
                command.append('-streamx')
            command.append('input.thrift')
            generated = run(command, cwd=directory, timeout=timeout,
                            log=directory / 'generate.log', env=env)
            compiled = None
            if generated['returncode'] == 0 and compile_generated:
                compiled = run(['go', 'test', '-buildvcs=false', '-mod=mod', './kitex_gen/...'], cwd=directory,
                               timeout=timeout, log=directory / 'compile.log', env=env)
            files = {str(p.relative_to(directory)): hashlib.sha256(p.read_bytes()).hexdigest()
                     for p in sorted((directory / 'kitex_gen').rglob('*.go'))}
            rows.append({'name': name, 'style': style,
                         'idl_sha256': hashlib.sha256(source.encode()).hexdigest(),
                         'generation': generated, 'compile': compiled, 'files': files,
                         'outcome': 'timeout' if generated['timed_out'] or (compiled and compiled['timed_out'])
                         else 'rejected' if generated['returncode'] != 0
                         else 'compile_failure' if compiled and compiled['returncode'] != 0
                         else 'accepted' if compiled else 'generated_unverified'})
    expected_path = Path(__file__).resolve().parent / 'data/idl-expected.json'
    known = json.loads(expected_path.read_text()) if expected_path.is_file() else {'cases': []}
    expected = {(c['name'], c['style']): c for c in known['cases']}
    for row in rows:
        baseline = expected.get((row['name'], row['style']))
        row['expected'] = baseline
        row['status'] = ('INCOMPLETE' if not baseline or baseline['idl_sha256'] != row['idl_sha256']
                         or row['outcome'] == 'generated_unverified'
                         else 'PASS' if row['outcome'] == baseline['outcome'] else 'FAIL')
        if baseline and baseline.get('diagnostic_contains'):
            command = row['compile'] if row['outcome'] == 'compile_failure' else row['generation']
            if not command or baseline['diagnostic_contains'] not in Path(command['log']).read_text(errors='replace'):
                row['status'] = 'FAIL'
                row['reason'] = 'failure does not match the reviewed compiler diagnostic'
    report = {'schemaVersion': 1, 'kitex_module_version': 'v0.16.4', 'suite': 'idl-matrix',
              'tool_sha256': hashlib.sha256(tool.read_bytes()).hexdigest(), 'cases': rows}
    (output / 'report.json').write_text(json.dumps(report, indent=2) + '\n')
    return report
