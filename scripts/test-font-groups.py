#!/usr/bin/env python3
"""Headless production font routing/config tests; no window, PTY, or display server.

Only iced_core, the backend-free iced_widget layout widgets (no wgpu/tiny-skia
renderer) and serde/toml are built. Full Frost compilation/native QA are separate.
"""
import json
import os
from pathlib import Path
import re
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parents[1]
main = (ROOT / 'src/main.rs').read_text()
config = (ROOT / 'src/config.rs').read_text()


def function(source, name):
    start = re.search(r'fn ' + name + r'(?=[(<])', source).start()
    opening = source.index('{', start)
    depth, end = 1, opening + 1
    while depth:
        depth += (source[end] == '{') - (source[end] == '}')
        end += 1
    return source[start:end]


# Source contracts complement (not replace) the real Iced event tests.
view = function(main, 'view')
# Leaf terminal/editor widgets own the content font: a capturing scope around
# the whole body would resize the shell behind Vim, and overlays stacked over
# the body (settings, palette, search) must never zoom terminal content.
assert 'font_zoom::Group::Content, body' not in view
assert 'self.font_zoom_region(font_zoom::Group::Content, terminal)' in function(main, 'pane_view')
assert 'font_zoom::Group::Content' in function(main, 'pane_header')
assert 'self.font_zoom_region(font_zoom::Group::Content, self.block_reading_bar())' in view
assert view.index('font_zoom::Group::Content, self.block_reading_bar()') < view.index('if self.config_panel_open')
assert 'self.font_zoom_region(font_zoom::Group::Content, self.status_bar())' in view
assert 'self.font_zoom_region(font_zoom::Group::Chrome, self.sidebar_view())' in view
assert 'self.font_zoom_region(font_zoom::Group::Chrome, self.tab_bar())' in view
assert 'self.pane_view(self.active)' in view and 'self.render_tree(self.layout(), &[])' in view
router = main.split('            Message::FontWheel(group, delta) => {', 1)[1].split('            Message::SummaryActivate', 1)[0]
assert 'self.active' not in router and 'sidebar_files_focused' not in router
assert 'self.adjust_font_size(steps)' in router and 'self.set_chrome_font_size' in router
mouse = main.split('// Ctrl+wheel belongs exclusively', 1)[1].split('            MouseInput::ScrollTo', 1)[0]
assert 'self.adjust_font_size(' not in mouse
chrome = function(main, 'set_chrome_font_size')
assert 'self.metrics.grid_size' in chrome and 'self.relayout()' in chrome
assert 'self.config.font_size =' not in chrome and 'Metrics::' not in chrome
for method in ['adjust_font_size', 'reset_font_size']:
    assert 'self.config.set_content_font_size(' in function(main, method)
assert 'self.config.set_content_font_size(v)' in main
assert 'self.font_wheel.clear()' in main
assert 'self.font_zoom_enabled()' in function(main, 'font_zoom_region')
assert 'self.terminal_mouse_active()' in function(main, 'font_zoom_enabled')
assert 'if !self.font_zoom_enabled()' in router

# Use the actual serde fields, default values, normalization, setters and tests.
field_start = config.index('    #[serde(default = "default_font_size")]')
field_end = config.index('    #[serde(default = "default_font_family")]', field_start)
fields = config[field_start:field_end]
normalization = function(config, 'normalized').split('        self.line_spacing =', 1)[0] + '        self\n    }'
defaults = re.search(r'            font_size: default_font_size\(\),\n            chrome_font_size: [^\n]+', config).group()
code = '#![allow(dead_code)]\nextern crate self as iced;\npub use iced_core::*;\n'
code += 'pub mod advanced { pub use iced_core::{clipboard, image, layout, mouse, overlay, renderer, widget, Clipboard, Layout, Shell, Widget}; }\n'
# The per-pane routing test composes scopes in the production iced Row.
code += 'pub mod widget { pub use iced_core::widget::*; pub use iced_widget::Row; }\n'
for module in ['font_zoom', 'font_zoom_scope', 'ui_typography']:
    code += '#[path = ' + json.dumps(str(ROOT / 'src' / (module + '.rs'))) + '] mod ' + module + ';\n'
code += 'mod config { use serde::{Serialize, Deserialize};\n#[derive(Debug, Serialize, Deserialize)]\npub struct Config {\n' + fields + '}\n'
code += 'impl Default for Config { fn default() -> Self { Self {\n' + defaults + '\n} } }\n'
code += function(config, 'default_font_size') + '\n' + function(config, 'finite_clamp') + '\n'
code += 'impl Config {\n' + '\n'.join(function(config, name) for name in ['from_toml', 'effective_chrome_font_size', 'set_content_font_size', 'clamp_font_size']) + '\n' + normalization + '\n}\n'
code += '#[cfg(test)] mod tests {\n'
for test in ['legacy_font_groups_split_before_first_edit_and_survive_reload',
             'font_groups_clamp_independently_and_direct_legacy_setter_freezes_fallback']:
    code += '#[test]\n' + function(config, test) + '\n'
code += '} }\n'
# STATUS_BAR_H is jterm_core::bottom_bar::BAR_HEIGHT. Read it from the core
# source this build actually uses (the path dependency: the sibling checkout or
# the bundle CI materializes), so unrelated core upgrades never require editing
# this font-routing regression fixture and a changed height is still exercised.
core_path = re.search(
    r'^jterm_core = \{[^\n]*\bpath = "([^"]+)"', (ROOT / 'Cargo.toml').read_text(), re.MULTILINE,
)
assert core_path is not None, 'jterm_core must be a path dependency'
bottom_bar = (ROOT / core_path.group(1) / 'src/bottom_bar.rs').read_text()
bar_height = re.search(r'^pub const BAR_HEIGHT: f32 = ([0-9.]+);', bottom_bar, re.MULTILINE)
assert bar_height is not None, 'jterm_core bottom_bar::BAR_HEIGHT must stay a literal f32'
for constant in ['TAB_BAR_H', 'TAB_BAR_PADDING', 'STATUS_BAR_H']:
    code += re.search(r'const ' + constant + r': f32 = [^;]+;', main).group().replace(
        'jterm_core::bottom_bar::BAR_HEIGHT', bar_height.group(1)) + '\n'
code += function(main, 'chrome_height') + '\n' + function(main, 'terminal_height_after_chrome')
code += '''
#[test]
fn independent_fonts_reserve_actual_chrome_in_all_content_modes() {
    // Mode-independent layout shared by Block, Chat, VTE, and split panes.
    for chrome in [8.0, 14.0, 36.0, 72.0] {
        for content in [8.0, 14.0, 36.0, 72.0] {
            for bottom in [false, true] {
                for height in [0.0, 100.0, 800.0] {
                    let actual = terminal_height_after_chrome(height, content, chrome, bottom, 64.0, 88.0);
                    let top = ui_typography::row_height(chrome, TAB_BAR_H, TAB_BAR_PADDING);
                    let status = if bottom { ui_typography::row_height(content, STATUS_BAR_H, 4.0) } else { 0.0 };
                    assert_eq!(actual, (height - top - status - 64.0 - 88.0).max(0.0));
                }
            }
        }
    }
}
'''
with tempfile.TemporaryDirectory(prefix='frost-font-groups-') as directory:
    harness = Path(directory)
    (harness / 'src').mkdir()
    (harness / 'src/lib.rs').write_text(code)
    (harness / 'Cargo.toml').write_text('''[package]
name = "frost-font-groups-regression"
version = "0.0.0"
edition = "2021"
[dependencies]
iced_core = "=0.14.0"
iced_widget = { version = "=0.14.2", default-features = false }
serde = { version = "1", features = ["derive"] }
toml = "1.1"
''')
    subprocess.run([os.environ.get('CARGO', 'cargo'), 'test', '--manifest-path', str(harness / 'Cargo.toml')], check=True)
print('font group wiring, production Iced event routing, serde reload and geometry passed')
