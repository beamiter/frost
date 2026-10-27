#!/usr/bin/env python3
"""Run the migrated production layout test without the blocked renderer fixtures.

The existing test body and companion helper are extracted verbatim from main;
only include_str's relative path is resolved for the standalone source file.
"""
import argparse
import hashlib
import json
from pathlib import Path
import subprocess

p = argparse.ArgumentParser(description=__doc__)
p.add_argument('--deps', required=True, type=Path)
p.add_argument('--output', required=True, type=Path)
a = p.parse_args()
repo = Path(__file__).resolve().parents[1]
source = (repo / 'src/main.rs').read_text()
def function(name):
    begin = source.index('fn ' + name + '(')
    opening = source.index('{', begin)
    level = 1
    for end in range(opening + 1, len(source)):
        if source[end] == '{': level += 1
        elif source[end] == '}': level -= 1
        if level == 0: return source[begin:end + 1]
    raise ValueError('Unclosed function ' + name)
body = function('expanded_companion_reuses_one_snapshot_and_shared_geometry')
resolved = body.replace('include_str!("main.rs")', 'include_str!(' + json.dumps(str(repo / 'src/main.rs')) + ')')
test = '#[path = ' + json.dumps(str(repo / 'src/native_viewport.rs')) + ']\nmod native_viewport;\n'
test += function('companion_strip_height') + '\n#[test]\n' + resolved + '\n'
a.output.parent.mkdir(parents=True, exist_ok=True)
rust = a.output.with_suffix('.rs')
rust.write_text(test)
core = max(a.deps.glob('libjterm_core-*.rlib'), key=lambda path: path.stat().st_mtime)
subprocess.run(['rustc', '--edition=2021', '--test', str(rust), '-L', 'dependency=' + str(a.deps),
                '--extern', 'jterm_core=' + str(core), '-o', str(a.output)], check=True)
subprocess.run([str(a.output)], check=True)
a.output.with_suffix('.source.json').write_text(json.dumps({
    'production_main_sha256': hashlib.sha256((repo / 'src/main.rs').read_bytes()).hexdigest(),
    'extracted_test_sha256': hashlib.sha256(body.encode()).hexdigest(),
    'assertion_policy': 'Production test assertions unchanged; include_str path resolution only',
}, indent=2))
