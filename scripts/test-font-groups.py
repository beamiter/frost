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
config = (ROOT / 'src/config.rs').read_text()


def function(source, name):
    start = re.search(r'fn ' + name + r'(?=[(<])', source).start()
    opening = source.index('{', start)
    depth, end = 1, opening + 1
    while depth:
        depth += (source[end] == '{') - (source[end] == '}')
        end += 1
    return source[start:end]


# Retain wiring checks for production paths that still exist. Retired
# tab/status/sidebar presenter scopes cannot stand in for native shell zoom.
main = (ROOT / 'src/main.rs').read_text()
assert 'self.font_zoom_region(font_zoom::Group::Content, terminal)' in function(main, 'pane_view')
assert 'font_zoom::Group::Content' in function(main, 'pane_header')
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

# Native shell font sizing is a separate display-backed gate. The executable
# tests use actual Config setters and Iced scope/event code below.
# Use the actual serde fields, default values, normalization, setters and tests.
field_start = config.index('    #[serde(default = "default_font_size")]')
field_end = config.index('    #[serde(default = "default_font_family")]', field_start)
fields = config[field_start:field_end]
normalization = function(config, 'normalized').split('        self.line_spacing =', 1)[0] + '        self\n    }'
defaults = re.search(r'            font_size: default_font_size\(\),\n            chrome_font_size: [^\n]+', config).group()
code = 'extern crate self as iced;\npub use iced_core::*;\n'
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
    env = os.environ.copy()
    env['RUSTUP_AUTO_INSTALL'] = '0'
    cargo = os.environ.get('CARGO', 'cargo')
    manifest = str(harness / 'Cargo.toml')
    # CI may populate its normal crate cache; local offline gates explicitly
    # set CARGO_NET_OFFLINE=true. No native SDK or renderer is requested.
    subprocess.run([cargo, 'generate-lockfile', '--manifest-path', manifest], env=env, check=True)
    subprocess.run([cargo, 'test', '--locked', '--jobs', '2', '--manifest-path', manifest], env=env, check=True)
print('production font configuration/reload and Iced event-scope contracts passed; native shell font pixels are a separate gate')
