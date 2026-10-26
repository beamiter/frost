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

def block(source, marker):
    start = source.index(marker)
    opening = source.index('{', start)
    depth = 1
    end = opening + 1
    while depth:
        depth += (source[end] == '{') - (source[end] == '}')
        end += 1
    return source[start:end]

def function(source, name):
    return block(source, 'fn ' + name + '(')

# Verify real startup/live-config/zoom wiring, not an independent sample model.
assert 'Config::clamp_font_size(config.font_size)' in main
for name in ['adjust_font_size', 'reset_font_size']:
    assert 'self.apply_config()' in function(main, name)
apply = function(main, 'apply_config')
assert 'self.effective_font_size()' in apply and 'self.relayout()' in apply
# The window companion no longer reserves a status card in each pane. Keep
# native command headers, but admit zero organism rows at startup and after
# every font/config change; this contract is independent of line spacing.
for name, source in [('spawn_argv_env', function(main, 'spawn_argv_env')),
                     ('apply_config', apply)]:
    calls = re.findall(r'\.set_native_chrome\(([^)]*)\)', source)
    assert [re.sub(r'\s+', '', args) for args in calls] == ['true,0'], name
assert re.search(r'\.organism_status\(\s*None,', function(main, 'pane_view'))
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
# The lifecycle label now sizes to its actual text at the current cell width.
# Keep the full-size glyphs, matching bounds and narrow-pane admission coupled.
life_badge = block(terminal, 'let badge_w = presentation.life.chars().count() as f32 * cw;')
assert 'if detail_w > 14.0 * cw + 12.0 + badge_w {' in life_badge
assert 'presentation.life,' in life_badge
assert 'x: area.x + area.width - 12.0 - 3.0 * cw - badge_w,' in life_badge
assert 'width: badge_w,' in life_badge
assert re.search(r'\},\s*self\.mono,\s*self\.metrics\.font_size,', life_badge)
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
code += '\n' + function(main, 'companion_strip_height')
# Only the config/input accessors are fixtures. Execute the actual window
# geometry method, which excludes Block/Chat and sizes Traditional text.
code += '''
struct CompanionConfig {
    card_mode: bool,
    ascii_organism_enabled: bool,
    ascii_organism_expanded: bool,
    bottom_bar: bool,
}
impl CompanionConfig {
    fn is_card_mode(&self) -> bool { self.card_mode }
}
struct CompanionGeometry { config: CompanionConfig, font_size: f32 }
impl CompanionGeometry {
    fn effective_font_size(&self) -> f32 { self.font_size }
'''
code += function(main, 'organism_strip_height') + '\n}\n'
code += '''
#[test]
fn companion_geometry_keeps_card_rows_free_and_fits_traditional_text() {
    for font in [8.0, 14.0, 24.0, 36.0, 72.0] {
        for card_mode in [false, true] {
            for enabled in [false, true] {
                for expanded in [false, true] {
                    for bottom_bar in [false, true] {
                        let window = CompanionGeometry {
                            config: CompanionConfig {
                                card_mode,
                                ascii_organism_enabled: enabled,
                                ascii_organism_expanded: expanded,
                                bottom_bar,
                            },
                            font_size: font,
                        };
                        let height = window.organism_strip_height();
                        if card_mode || !enabled || !expanded || !bottom_bar {
                            assert_eq!(height, 0.0, "no extra pane or hidden companion rows");
                        } else {
                            // Three readable lines at Iced's full 1.3 line
                            // height, plus the real card's vertical padding.
                            assert!(height >= font * 1.3 * 3.0 + 16.0);
                            assert!(height >= companion_strip_height(true, true, true));
                        }
                    }
                }
            }
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
