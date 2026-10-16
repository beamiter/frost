#!/usr/bin/env python3
"""Create an isolated, source-identical Smithay lib-test workspace; never edit vendor."""
import argparse
import hashlib
import json
from pathlib import Path
import re
import shutil
import subprocess
import tomllib

p = argparse.ArgumentParser(description=__doc__)
p.add_argument('--root', type=Path, default=Path(__file__).resolve().parent.parent)
p.add_argument('--output', type=Path, required=True)
a = p.parse_args()
root, output = a.root.resolve(strict=True), a.output.resolve()
assert not output.is_relative_to(root), 'test workspace must be outside the checkout'
assert not output.exists(), 'refuse to overwrite an existing workspace'
subprocess.run(['python3', str(root / 'scripts/verify-provider-sources.py')], check=True)
source = root / 'vendor/smithay-clipboard'
original = (source / 'Cargo.toml').read_text()
before = tomllib.loads(original)
expected_dev = {'sctk': {'version': '0.20.0', 'features': ['calloop', 'xkbcommon'], 'default-features': False, 'package': 'smithay-client-toolkit'}}
assert before['dev-dependencies'] == expected_dev, 'upstream dev-dependency contract changed'
# Delete precisely the one reviewed example-only section, retaining all normal
# dependency declarations, package metadata, feature definitions and test bodies.
modified, count = re.subn(r'(?ms)^\[dev-dependencies\.sctk\]\n.*?(?=^\[|\Z)', '', original)
assert count == 1
expected = dict(before)
del expected['dev-dependencies']
assert tomllib.loads(modified) == expected
lock = root / 'scripts/provider-acceptance/smithay-tests.lock'
def identities(path):
    return {(x['name'], x['version'], x.get('source'), x.get('checksum')) for x in tomllib.loads(path.read_text())['package']}
assert identities(lock) <= identities(root / 'Cargo.lock'), 'isolated lock introduced a non-production dependency'
output.mkdir(parents=True)
shutil.copytree(source, output / 'smithay-clipboard')
(output / 'smithay-clipboard/Cargo.toml').write_text(modified)
# JSON strings are valid TOML basic strings, including absolute paths.
(output / 'Cargo.toml').write_text('[workspace]\nmembers = ["smithay-clipboard"]\nresolver = "2"\n\n[patch.crates-io]\nwayland-client = { path = ' + json.dumps(str(root / 'vendor/wayland-client')) + ' }\n')
shutil.copyfile(lock, output / 'Cargo.lock')
files = []
for path in sorted(source.rglob('*')):
    if not path.is_file() or path.name == 'Cargo.toml':
        continue
    relative = path.relative_to(source)
    data = path.read_bytes()
    assert (output / 'smithay-clipboard' / relative).read_bytes() == data
    files.append({'path': str(relative), 'sha256': hashlib.sha256(data).hexdigest()})
report = {'status': 'pass', 'files': files, 'original_manifest_sha256': hashlib.sha256(original.encode()).hexdigest(), 'test_manifest_sha256': hashlib.sha256(modified.encode()).hexdigest(), 'omitted_example_only_dev_dependencies': expected_dev, 'normal_manifest_semantics': 'unchanged', 'lock_sha256': hashlib.sha256(lock.read_bytes()).hexdigest()}
# Check actual provider-specific feature resolution, not merely declarations.
# Other host-only consumers may legitimately add features to shared libc/rustix.
host = next(line.split(': ', 1)[1] for line in subprocess.check_output(['rustc', '-vV'], text=True).splitlines() if line.startswith('host: '))
def metadata(manifest):
    return json.loads(subprocess.check_output(['cargo', 'metadata', '--locked', '--filter-platform', host, '--format-version', '1', '--manifest-path', str(manifest)], text=True))
def features(data):
    packages = {p['id']: (p['name'], p['version']) for p in data['packages']}
    return {packages[n['id']]: sorted(n['features']) for n in data['resolve']['nodes']}
isolated = features(metadata(output / 'Cargo.toml'))
production = features(metadata(root / 'scripts/provider-acceptance/Cargo.toml'))
keys = [('smithay-clipboard', '0.7.3'), ('smithay-client-toolkit', '0.20.0'), ('wayland-client', '0.31.15'), ('wayland-backend', '0.3.17')]
for key in keys:
    assert isolated[key] == production[key], ('provider feature drift', key, isolated[key], production[key])
report['matched_provider_features'] = [{'package': name, 'version': version, 'features': isolated[(name, version)]} for name, version in keys]
report['shared_dependency_feature_differences'] = [{'package': key[0], 'version': key[1], 'isolated': value, 'host': production.get(key)} for key, value in isolated.items() if value != production.get(key)]
(output / 'source-identity.json').write_text(json.dumps(report, indent=2)+'\n')
print(json.dumps({'status': 'pass', 'workspace': str(output), 'source_files_verified': len(files), 'normal_manifest_semantics': 'unchanged'}))
