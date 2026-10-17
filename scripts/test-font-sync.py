#!/usr/bin/env python3
"""Dependency-free font/layout regression; extracts the actual production helpers."""
import os
from pathlib import Path
import re
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parents[1]
main = (ROOT / 'src/main.rs').read_text()
sidebar = (ROOT / 'src/sidebar.rs').read_text()

def function(source, name):
    start = source.index('fn ' + name + '(')
    opening = source.index('{', start)
    depth = 1
    end = opening + 1
    while depth:
        depth += (source[end] == '{') - (source[end] == '}')
        end += 1
    return source[start:end]

# Verify real startup/live-config/zoom wiring, not an independent sample model.
assert 'Config::clamp_font_size(config.font_size)' in main
for name in ['adjust_font_size', 'reset_font_size']:
    assert 'self.apply_config()' in function(main, name)
apply = function(main, 'apply_config')
assert 'self.effective_font_size()' in apply and 'self.relayout()' in apply
assert 'self.pane_header_height()' in function(main, 'relayout')
for name in ['pane_header', 'status_bar', 'config_panel', 'agent_panel']:
    # Generic functions have lifetimes before their parentheses.
    source = re.split(r'fn ' + name + r'(?=[(<])', main, maxsplit=1)[1].split('\n    fn ', 1)[0]
    assert 'self.effective_font_size()' in source, name
for name in ['tab_bar', 'top_bar_with_close', 'sidebar_view', 'sidebar_files_view',
             'sidebar_tabs_view', 'sidebar_render_item', 'sidebar_tasks_view', 'task_card',
             'pane_to_tab_drop_hint', 'tab_bar_height', 'sidebar_row_height']:
    source = re.split(r'fn ' + name + r'(?=[(<])', main, maxsplit=1)[1].split('\n    fn ', 1)[0]
    assert 'self.chrome_font_size()' in source, name
    assert 'self.effective_font_size()' not in source, name
for size in ['status_bar_height', 'pane_header_height']:
    assert 'self.effective_font_size()' in function(main, size), size
assert main.count('self.sidebar_row_height(),') >= 2
assert 'self.sidebar_row_height() + sidebar::SIDEBAR_TREE_ROW_GAP' in main
assert 'self.effective_font_size()' in function(main, 'term_height')
assert 'chrome_height(font_size, chrome_font_size, bottom_bar)' in function(main, 'terminal_height_after_chrome')

terminal = (ROOT / 'src/terminal_view.rs').read_text()
# The only reduced scales left in native chrome belong to the status icon,
# chevron, compact glyph and decorative sprite, never readable labels.
assert terminal.count('self.metrics.font_size * 0.72,') == 1
assert terminal.count('self.metrics.font_size * 0.82,') == 3
assert 'self.metrics.font_size * 0.9,' not in terminal
assert 'Pixels((font_size * 0.9)' not in terminal
assert 'let badge_w = 19.0 * cw;' in terminal
assert 'column![nav_row.wrap(), action_row.wrap()]' in main
assert 'container(header.wrap())' in main
settings = main.split('    fn config_panel', 1)[1].split('    fn theme_editor_view', 1)[0]
assert 'scrollable::Direction::Both' in settings
assert 'let buttons = buttons.wrap()' in settings

code = (ROOT / 'src/ui_typography.rs').read_text()
code += '\nconst SIDEBAR_TREE_ROW_GAP: f32 = 1.0;\nconst SIDEBAR_TREE_OVERSCAN: usize = 8;\n'
for name in ['sidebar_tree_window', 'sidebar_tree_reveal_offset']:
    code += '\n' + function(sidebar, name)
code += '\n#[test]\n' + function(sidebar, 'zoomed_tree_virtualization_and_keyboard_reveal_share_row_geometry')
code += '\n' + function(main, 'native_status_rows')
code += '''
#[test]
fn expanded_status_reserves_three_full_size_text_lines() {
    for font in [8.0, 14.0, 24.0, 36.0, 72.0] {
        for spacing in [0.8, 1.0, 1.5, 3.0] {
            let cell_h = font * 1.2 * spacing;
            let rows = native_status_rows(true, true, cell_h);
            let available_text_height = (rows as f32 * cell_h - 6.0 - 22.0) / 3.0;
            assert!(available_text_height >= cell_h - 0.001);
            assert_eq!(native_status_rows(true, false, cell_h), 1);
            assert_eq!(native_status_rows(false, true, cell_h), 0);
        }
    }
}
'''
with tempfile.TemporaryDirectory(prefix='frost-font-sync-') as directory:
    source = Path(directory) / 'font_sync.rs'
    source.write_text(code)
    binary = Path(directory) / 'font_sync'
    subprocess.run([os.environ.get('RUSTC', 'rustc'), '--edition=2021', '--test', str(source), '-o', str(binary)], check=True)
    subprocess.run([str(binary)], check=True)
print('font synchronization wiring and production geometry checks passed')
