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


def item(marker, text=source):
    start = text.index(marker)
    opening = text.index('{', start)
    depth, end = 1, opening + 1
    while depth:
        depth += (text[end] == '{') - (text[end] == '}')
        end += 1
    return text[start:end]


code = '#![allow(dead_code)]\nuse smallvec::SmallVec;\n'
# Use the same engine cell types as the live terminal, never a test-only copy.
shared_start = source.index('pub use jterm_engine::{')
code += source[shared_start:source.index(';', shared_start) + 1] + '\n'
code += 'use std::{borrow::Cow, collections::{BTreeMap, BTreeSet, HashMap, VecDeque}, sync::atomic::{AtomicU64, Ordering}};\n'
code += '#[path = ' + json.dumps(str(ROOT / 'src/terminal/visible_rows.rs')) + '] mod visible_rows; use visible_rows::{VisibleRows, RowView};\n'
code += item('thread_local!') + '\n'
code += 'const MAX_TERMINAL_COLS: usize = 1024; const MAX_TERMINAL_ROWS: usize = 512;\n'
types = between('fn fresh_raw_row_id() -> RawRowId {', '#[derive(Clone, Debug, PartialEq, Eq)]\nstruct ProjectedViewportCacheKey')
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
# Paint metadata is the only Chat dependency of these kernels. Extract the
# actual declarations independently of nearby comments or unrelated controls.
code += '\nmod chat_mode {\n'
for marker in ['pub enum ChatRole {', 'pub struct ChatRow {']:
    declaration = item(marker, chat)
    start = chat.rfind('#[derive', 0, chat.index(marker))
    assert start >= 0, marker
    code += chat[start:chat.index(marker)] + declaration + '\n'
code += '}\n'
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
    library = path / 'src/lib.rs'
    library.write_text(code)
    (path / 'Cargo.toml').write_text('''[package]
name = "frost-projection-properties"
version = "0.0.0"
edition = "2021"
[dependencies]
smallvec = "=1.15.2"
[profile.test]
opt-level = 1
'''.replace('[dependencies]\n', '[dependencies]\n' + next(line for line in (ROOT / 'Cargo.toml').read_text().splitlines() if line.startswith('jterm_engine = ')) + '\n'))
    command = [os.environ.get('CARGO', 'cargo'), 'test', '--manifest-path', str(path / 'Cargo.toml'), '-j', '1', '--', '--nocapture']
    subprocess.run(command, check=True)
    # The independent tagged-cell/text oracle must reject real kernel regressions,
    # not just compile the extracted declarations. Mutate only the temporary crate;
    # production source, properties, corpus and expected values stay untouched.
    mutants = [
        ('lost-source-column',
         'col: self.raw_col_start + cell.col - self.view_col_start,',
         'col: self.raw_col_start,'),
        ('copied-wide-continuation',
         'if !cell.flags.wide_continuation() {\n                        result.push(cell.character);',
         'if true {\n                        result.push(cell.character);'),
    ]
    for name, original, mutated in mutants:
        assert code.count(original) == 1 and original != mutated, name
        library.write_text(code.replace(original, mutated, 1))
        env = os.environ.copy()
        env['FROST_PROJECTION_CASE'] = '0'
        env['FROST_PROJECTION_STEPS'] = '16'
        result = subprocess.run(command[:-2] + [
            'projection_properties::seeded_projection_ten_thousand_sequences',
            '--', '--exact', '--nocapture'], env=env, capture_output=True, text=True)
        assert result.returncode != 0 and 'test result: FAILED' in result.stdout, (
            name, result.stdout, result.stderr)
        assert 'seeded_projection_ten_thousand_sequences ... FAILED' in result.stdout, name
        print('Compiled projection kernel mutant rejected by unchanged oracle: ' + name)
print('Real projection kernel properties passed; no parser, window or clipboard provider was used.')
