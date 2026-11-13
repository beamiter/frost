#!/usr/bin/env python3
"""Exercise exact host mouse adapter and pinned engine without desktop services.

Only TerminalState storage/constructor is a fixture here. Parser/lifecycle tests
remain in the full application suite; this is not native pointer acceptance.
"""
import json
import os
from pathlib import Path
import subprocess
import tempfile
import tomllib

ROOT = Path(__file__).resolve().parents[1]
source = (ROOT / 'src/terminal.rs').read_text()


def item(marker):
    assert source.count(marker) == 1, marker
    start = source.index(marker)
    opening = source.index('{', start)
    depth, end = 1, opening + 1
    while depth:
        depth += (source[end] == '{') - (source[end] == '}')
        end += 1
    return source[start:end]


code = '#![allow(dead_code)]\n#[derive(Default)]\n' + item('struct TerminalModes {')
code += '\n' + item('impl TerminalModes {')
code += '\nstruct TerminalState { modes: TerminalModes }\nimpl TerminalState {\n'
code += 'fn new(_: usize, _: usize) -> Self { Self { modes: TerminalModes::default() } }\n'
for marker in ['fn mouse_wire_modes(', 'pub fn get_mouse_report(', 'pub fn get_mouse_release_report(']:
    code += item(marker) + '\n'
code += '}\n#[cfg(test)]\n#[path = ' + json.dumps(str(ROOT / 'src/terminal/mouse_reporting_tests.rs')) + '] mod mouse_reporting_tests;\n'
manifest = (ROOT / 'Cargo.toml').read_text()
dependency = next(line for line in manifest.splitlines() if line.startswith('jterm_engine = '))
lock = tomllib.loads((ROOT / 'Cargo.lock').read_text())
smallvec = [p['version'] for p in lock['package'] if p['name'] == 'smallvec']
assert len(smallvec) == 1
dependency += '\nsmallvec = "=' + smallvec[0] + '"'
with tempfile.TemporaryDirectory(prefix='frost-mouse-') as temp:
    path = Path(temp)
    (path / 'src').mkdir()
    (path / 'src/lib.rs').write_text(code)
    (path / 'Cargo.toml').write_text('[package]\nname="frost-host-mouse-test"\nversion="0.0.0"\nedition="2021"\n[dependencies]\n' + dependency + '\n')
    command = [os.environ.get('CARGO', 'cargo'), 'test', '--manifest-path', str(path / 'Cargo.toml')]
    subprocess.run(command, check=True)
    subprocess.run([command[0], 'clippy', '--manifest-path', str(path / 'Cargo.toml'), '--all-targets', '--', '-D', 'warnings'], check=True)
    # Prove unchanged independent expectations reject lost encoding projection.
    mutant = code.replace('[1000, 1002, 1003, 1005, 1006, 1015]', '[1000, 1002, 1003, 1005, 1015]')
    assert mutant != code
    (path / 'src/lib.rs').write_text(mutant)
    result = subprocess.run(command, capture_output=True, text=True)
    assert result.returncode != 0 and 'test result: FAILED' in result.stdout, (result.stdout, result.stderr)
    print('Compiled missing-SGR projection mutant rejected by unchanged wire expectations')
print('Host adapter wire tests passed; no parser, window or pointer provider was used.')
