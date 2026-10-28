#!/usr/bin/env python3
"""Headless production typography, sidebar virtualization and viewport budgets.

This uses rustc only, preserving the fmt CI job's no-native-SDK contract.
It does not certify native shell text zoom or archived footer geometry.
"""
import json
import os
from pathlib import Path
import re
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parents[1]


def function(source, name):
    start = re.search(r'fn ' + name + r'(?=[(<])', source).start()
    opening = source.index('{', start)
    depth, end = 1, opening + 1
    while depth:
        depth += (source[end] == '{') - (source[end] == '}')
        end += 1
    return source[start:end]


# Keep the real terminal metrics/config wiring checks, excluding unreachable
# presenters and their obsolete footer-height formula. Shell-font pixel
# behavior is separately verified against the real native shell.
main = (ROOT / 'src/main.rs').read_text()
assert 'Config::clamp_font_size(config.font_size)' in main
for name in ['adjust_font_size', 'reset_font_size']:
    assert 'self.apply_config()' in function(main, name)
apply = function(main, 'apply_config')
assert 'self.effective_font_size()' in apply and 'self.relayout()' in apply
for name, source in [('spawn_argv_env', function(main, 'spawn_argv_env')), ('apply_config', apply)]:
    calls = re.findall(r'\.set_native_chrome\(([^)]*)\)', source)
    assert [re.sub(r'\s+', '', args) for args in calls] == ['true,0'], name
assert re.search(r'\.organism_status\(\s*None,', function(main, 'pane_view'))
assert 'self.pane_header_height()' in function(main, 'relayout')
for name in ['pane_header', 'config_panel', 'agent_panel']:
    assert 'self.effective_font_size()' in function(main, name), name
for name in ['sidebar_files_view', 'sidebar_render_item', 'sidebar_tasks_view', 'task_card', 'sidebar_row_height']:
    source = function(main, name)
    assert 'self.chrome_font_size()' in source, name
    assert 'self.effective_font_size()' not in source, name
assert main.count('self.sidebar_row_height(),') >= 2
assert 'self.sidebar_row_height() + sidebar::SIDEBAR_TREE_ROW_GAP' in main
assert 'self.effective_font_size()' in function(main, 'term_height')
settings = function(main, 'config_panel')
assert 'scrollable::Direction::Both' in settings
assert 'let buttons = buttons.wrap()' in settings

core_path = re.search(r'^jterm_core = \{[^\n]*\bpath = "([^"]+)"',
                      (ROOT / 'Cargo.toml').read_text(), re.MULTILINE)
assert core_path is not None, 'jterm_core must be the actual path dependency'
core = (ROOT / core_path.group(1)).resolve()
code = 'extern crate self as jterm_core;\n'
for name in ['reference_shell', 'chat_mode']:
    code += '#[path = ' + json.dumps(str(core / 'src' / (name + '.rs'))) + '] pub mod ' + name + ';\n'
for name in ['ui_typography', 'native_viewport']:
    code += '#[path = ' + json.dumps(str(ROOT / 'src' / (name + '.rs'))) + '] mod ' + name + ';\n'
sidebar = (ROOT / 'src/sidebar.rs').read_text()
for name in ['SIDEBAR_TREE_ROW_GAP', 'SIDEBAR_TREE_OVERSCAN']:
    code += re.search(r'(?:pub(?:\(crate\))? )?const ' + name + r': [^;]+;', sidebar).group() + '\n'
for name in ['sidebar_tree_window', 'sidebar_tree_reveal_offset']:
    code += function(sidebar, name) + '\n'
code += '#[test]\n' + function(sidebar, 'zoomed_tree_virtualization_and_keyboard_reveal_share_row_geometry')
with tempfile.TemporaryDirectory(prefix='frost-font-sync-') as directory:
    source = Path(directory) / 'font_sync.rs'
    source.write_text(code)
    binary = Path(directory) / 'font_sync'
    env = os.environ.copy()
    env['RUSTUP_AUTO_INSTALL'] = '0'
    subprocess.run([os.environ.get('RUSTC', 'rustc'), '--edition=2021', '--test', str(source), '-o', str(binary)], env=env, check=True)
    subprocess.run([str(binary)], env=env, check=True)
print('actual typography, sidebar virtualization and native viewport contracts passed; native shell font pixels are a separate gate')
