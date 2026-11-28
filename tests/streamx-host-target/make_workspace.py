"""Generate a fresh cross-crate consumer with the actual StreamX workspace CLI."""
import argparse
import json
import subprocess
from pathlib import Path

parser = argparse.ArgumentParser()
parser.add_argument('protocol', choices=['thrift', 'protobuf'])
parser.add_argument('output', type=Path)
parser.add_argument('--generator', type=Path, required=True)
args = parser.parse_args()
fixture = Path(__file__).resolve().parent
repo = fixture.parents[1]
output = args.output.resolve()
if output.exists():
    parser.error('use a fresh output directory')
(output / 'consumer/src').mkdir(parents=True)
dependencies = '\n'.join('{} = {{ path = {}{} }}'.format(
    name, json.dumps(str(repo / name)),
    ', features = ["ttstream"]' if name == 'volo-thrift' else '')
    for name in ['volo', 'volo-thrift', 'volo-grpc', 'volo-streamx'])
(output / 'Cargo.toml').write_text(
    '[workspace]\nmembers = ["consumer"]\nresolver = "3"\n'
    '[workspace.dependencies]\npilota = "0.14.0"\n' + dependencies + '\n')
sources = [('api.thrift', 'api_types'), ('parent.thrift', 'parent_types')] if args.protocol == 'thrift' else [
    ('api.proto', 'api_types'), ('common.proto', 'common_types')]
config = dict(common_crate_name='shared', touch_all=True, split_generated_files=True,
              services=[dict(idl=dict(source='local', path=str(fixture / 'idl' / source),
                                     includes=[str(fixture / 'idl')]),
                             codegen_option=dict(config=dict(crate_name=name))) for source, name in sources])
(output / 'volo.workspace.yml').write_text(json.dumps(config, indent=2) + '\n')
consumer_deps = '\n'.join('{} = {{ path = "../{}" }}'.format(name, name) for _, name in sources)
(output / 'consumer/Cargo.toml').write_text(
    '[package]\nname = "boundary-workspace-consumer"\nversion = "0.0.0"\nedition = "2024"\npublish = false\n'
    '[dependencies]\nvolo-streamx.workspace = true\n' + consumer_deps + '\n')
(output / 'consumer/src/lib.rs').write_bytes((fixture / ('workspace_' + args.protocol + '.rs')).read_bytes())
subprocess.check_call([str(args.generator.resolve()), args.protocol], cwd=str(output))
print(output)
