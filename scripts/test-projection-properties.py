#!/usr/bin/env python3
"""Run real projection kernels without compiling/starting the desktop shell.

Extracts production types, compression, planning, materialization, mapping and
pure selected-text copying verbatim. Only TerminalState storage/constructor is a
fixture: no parser, cache admission, application mode routing or provider exists
here. The same Rust property module also runs in the normal Frost test binary.
"""
import json
import os
from pathlib import Path
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parents[1]
source = (ROOT / 'src/terminal.rs').read_text()


def between(start, end):
    return source[source.index(start):source.index(end)]


def item(marker):
    start = source.index(marker)
    opening = source.index('{', start)
    depth, end = 1, opening + 1
    while depth:
        depth += (source[end] == '{') - (source[end] == '}')
        end += 1
    return source[start:end]


code = '#![allow(dead_code)]\nuse smallvec::SmallVec;\n'
code += 'use std::{borrow::Cow, collections::{BTreeMap, BTreeSet, HashMap, VecDeque}, sync::atomic::{AtomicU64, Ordering}};\n'
code += item('thread_local!') + '\n'
code += 'const MAX_TERMINAL_COLS: usize = 1024; const MAX_TERMINAL_ROWS: usize = 512;\n'
types = between('#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]\npub struct RawRowId', '#[derive(Clone, Debug, PartialEq, Eq)]\nstruct ProjectedViewportCacheKey')
# The native canvas suite needs the full TerminalState/parser. Its registration
# sits between production viewport items but belongs to the normal app tests.
canvas_tests = '#[cfg(test)]\n#[path = "terminal/canvas_tests.rs"]\nmod canvas_tests;\n'
assert types.count(canvas_tests) == 1
code += types.replace(canvas_tests, '')
code += '#[derive(Clone, Copy, Debug, PartialEq, Eq)]\n' + item('enum ProjectedTopAnchor')
# These contiguous types include the pure copy helper and selection anchors.
start = source.index('#[derive(Debug)]\nstruct ProjectedLine')
end = source.index('\n}', source.index('impl ProjectedSelectionEndpoint {', start)) + 2
code += source[start:end]
chat = (ROOT / 'src/chat_mode.rs').read_text()
start = chat.rfind('#[derive', 0, chat.index('pub enum ChatRole'))
end = chat.index('/// Short commands', start)
code += '\nmod chat_mode {\n' + chat[start:end] + '\n}\n'
code += '''
struct TerminalState { scrollback: VecDeque<ScrollbackLine>, grid: TerminalGrid }
impl TerminalState {
    fn new(cols: usize, rows: usize) -> Self {
        Self { scrollback: VecDeque::new(), grid: TerminalGrid::new(rows, cols) }
    }
'''
for method in ['identity_projection_plan', 'materialize_projection_plan', 'materialize_projection_plan_with_margin', 'projection_indexes']:
    code += item('fn ' + method + '(') + '\n'
code += '}\n'
code += '#[path = ' + json.dumps(str(ROOT / 'src/terminal/projection_properties.rs')) + '] mod projection_properties;\n'
# Verify the production caller still retains the original admission guards.
caller = item('fn copy_projected_selection(')
assert 'projection_plan_key_matches_current_source(plan_key)' in caller
assert 'plan.plan_revision != selection.plan_revision' in caller
assert 'copy_projected_cells(plan, selection, &self.scrollback, &self.grid)' in caller
with tempfile.TemporaryDirectory(prefix='frost-projection-') as temp:
    path = Path(temp)
    (path / 'src').mkdir()
    (path / 'src/lib.rs').write_text(code)
    (path / 'Cargo.toml').write_text('''[package]
name = "frost-projection-properties"
version = "0.0.0"
edition = "2021"
[dependencies]
smallvec = "=1.15.2"
[profile.test]
opt-level = 1
''')
    command = [os.environ.get('CARGO', 'cargo'), 'test', '--manifest-path', str(path / 'Cargo.toml'), '-j', '1', '--', '--nocapture']
    subprocess.run(command, check=True)
print('Real projection kernel properties passed; no parser, window or clipboard provider was used.')
