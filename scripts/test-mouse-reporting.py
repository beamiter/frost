#!/usr/bin/env python3
"""Exercise exact host mouse adapter and pinned engine without desktop services.

TerminalState's non-wire state is a fixture. Mode admission and report methods
come from the host, and protocol modes/encoding from the pinned engine.
Parser, cursor/scroll scheduling and screen lifecycle are covered in the full
application suite; this is not native pointer acceptance.
"""
import json
import os
from pathlib import Path
import subprocess
import tempfile
import tomllib

ROOT = Path(__file__).resolve().parents[1]
source = (ROOT / 'src/terminal.rs').read_text()


def item(marker, source=source):
    assert source.count(marker) == 1, marker
    start = source.index(marker)
    opening = source.index('{', start)
    depth, end = 1, opening + 1
    while depth:
        depth += (source[end] == '{') - (source[end] == '}')
        end += 1
    return source[start:end]


# The full host method admits private and ANSI modes. In this display-free
# fixture only drop the alternate-screen lifecycle, which does not supply
# wire-mode state: retain its private-mode insertion and test unrelated modes.
set_mode = item('fn set_mode(')
alt_lifecycle = item('47 | 1047 | 1049 => {', set_mode)
alt_insert = 'self.protocol.modes_mut().insert(mode);'
assert alt_lifecycle.rstrip().endswith(alt_insert + '\n            }')
assert set_mode.count(alt_lifecycle) == 1
set_mode = set_mode.replace(alt_lifecycle, f'47 | 1047 | 1049 => {{ {alt_insert} }}')

code = """#![allow(dead_code)]
use jterm_engine::protocol_state::{ProtocolState, TerminalModes};

struct FixtureScrollGuard;
impl FixtureScrollGuard {
    fn revoke(&mut self) {}
}

struct FixtureCursorControls<'a>(&'a mut bool);
impl FixtureCursorControls<'_> {
    fn set_origin_mode(&mut self, enabled: bool) {
        *self.0 = enabled;
    }
}

struct TerminalState {
    protocol: ProtocolState,
    insert_mode: bool,
    origin_mode: bool,
    projection_scroll_guard: FixtureScrollGuard,
    sync_output_active: bool,
    sync_output_start: Option<std::time::Instant>,
}

impl TerminalState {
    fn new(_: usize, _: usize) -> Self {
        Self {
            protocol: ProtocolState::with_modes(Self::initial_protocol_modes()),
            insert_mode: false,
            origin_mode: false,
            projection_scroll_guard: FixtureScrollGuard,
            sync_output_active: false,
            sync_output_start: None,
        }
    }
    fn save_cursor(&mut self) {}
    fn cursor_controls(&mut self) -> FixtureCursorControls<'_> {
        FixtureCursorControls(&mut self.origin_mode)
    }
}
impl TerminalState {
"""
code += set_mode + '\n'
for marker in ['fn initial_protocol_modes(', 'pub fn get_mouse_report(', 'pub fn get_mouse_release_report(']:
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
    # Prove unchanged independent expectations reject lost SGR admission.
    sgr_arm = item('1006 => {\n                // SGR mouse reporting format')
    assert code.count(sgr_arm) == 1
    mutant = code.replace(sgr_arm, '1006 => {}')
    (path / 'src/lib.rs').write_text(mutant)
    result = subprocess.run(command, capture_output=True, text=True)
    assert result.returncode != 0 and 'test result: FAILED' in result.stdout, (result.stdout, result.stderr)
    assert 'test mouse_reporting_tests::shared_mouse_all_mode_combinations_preserve_encoding_precedence ... FAILED' in result.stdout
    print('Compiled missing-SGR admission mutant rejected by unchanged wire expectations')
print('Host adapter wire tests passed; no parser, window or pointer provider was used.')
