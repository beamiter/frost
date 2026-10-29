#!/usr/bin/env python3
"""Headless production typography, sidebar virtualization and viewport budgets.

This uses rustc only, preserving the fmt CI job's no-native-SDK contract.
The production glass adapter, actual input row/cursor route and PTY dimension
guard are exercised; native renderer/input dispatch remain separate gates.
"""
import json
import hashlib
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
# Height delegates to the measured native budget. Actual fontsize authority is
# checked through the production getters/metrics/strip functions below, not by
# requiring a direct getter token in this particular wrapper.
term_height = function(main, 'term_height')
for route in ['native_viewport::raw_rect(', 'self.reference_layout()',
              'self.block_reading_bar_height()',
              'self.startup_recovery.banner_height(self.chrome_font_size())',
              'self.reference_input_metrics()',
              '.map(|metrics| metrics.input_height)']:
    assert route in term_height, route
# The retained original profile is test-only. Do not silently run its 80px
# viewport fixture while production consumes the glass joint budget.
assert re.search(r'#\[path = "glass_native_viewport.rs"\]\s*mod native_viewport;', main)
assert re.search(r'#\[cfg\(test\)\]\s*#\[path = "native_viewport.rs"\]\s*mod legacy_native_viewport;', main)
relayout = function(main, 'relayout')
assert relayout.count('self.refresh_glass_grid_dimensions()') == 1
assert relayout.index('self.refresh_glass_grid_dimensions()') < relayout.index('let mut targets')
assert 'self.relayout()' in function(main, 'activate_tab')
assert 'self.relayout()' in function(main, 'split')
settings = function(main, 'config_panel')
assert 'scrollable::Direction::Both' in settings
assert 'let buttons = buttons.wrap()' in settings

core_path = re.search(r'^jterm_core = \{[^\n]*\bpath = "([^"]+)"',
                      (ROOT / 'Cargo.toml').read_text(), re.MULTILINE)
assert core_path is not None, 'jterm_core must be the actual path dependency'
core = (ROOT / core_path.group(1)).resolve()
code = 'extern crate self as jterm_core;\n'
for name in ['reference_shell']:
    code += '#[path = ' + json.dumps(str(core / 'src' / (name + '.rs'))) + '] pub mod ' + name + ';\n'
code += '#[path = ' + json.dumps(str(core / 'src/chat_mode.rs')) + '] pub mod core_chat_mode;\n'
for name in ['ui_typography', 'glass_shell_tokens', 'glass_layout']:
    code += '#[path = ' + json.dumps(str(ROOT / 'src' / (name + '.rs'))) + '] mod ' + name + ';\n'
code += '#[path = ' + json.dumps(str(ROOT / 'src/glass_native_viewport.rs')) + '] mod native_viewport;\n'
# Only Layout is needed by the extracted production methods. Actual panel
# colours and desktop rendering are not stubbed as a passing visual test.
code += 'mod glass_profile { pub use crate::glass_layout::Layout; }\n'
sidebar = (ROOT / 'src/sidebar.rs').read_text()
for name in ['SIDEBAR_TREE_ROW_GAP', 'SIDEBAR_TREE_OVERSCAN']:
    code += re.search(r'(?:pub(?:\(crate\))? )?const ' + name + r': [^;]+;', sidebar).group() + '\n'
for name in ['sidebar_tree_window', 'sidebar_tree_reveal_offset']:
    code += function(sidebar, name) + '\n'
code += '#[test]\n' + function(sidebar, 'zoomed_tree_virtualization_and_keyboard_reveal_share_row_geometry')

# Bind the real App->viewport and Config->Metrics chains without native SDKs.
def braced_item(source, marker):
    start = source.index(marker)
    opening = source.index('{', start)
    depth, end = 1, opening + 1
    while depth:
        depth += (source[end] == '{') - (source[end] == '}')
        end += 1
    return source[start:end]


def constant(source, name):
    matches = re.findall(r'^(?:pub )?const ' + name + r':[^;]+;', source, re.MULTILINE)
    assert len(matches) == 1, name
    return matches[0]


terminal = (ROOT / 'src/terminal.rs').read_text()
view = (ROOT / 'src/terminal_view.rs').read_text()
config = (ROOT / 'src/config.rs').read_text()
native_shell = (ROOT / 'src/native_shell.rs').read_text()
input_surface = (ROOT / 'src/native_input_surface.rs').read_text()
profile = (ROOT / 'src/glass_profile.rs').read_text()
presenter = function(native_shell, 'reference_terminal_surface')
assert presenter.count('return terminal;') == 1
assert presenter.index('return terminal;') < presenter.index('macro_rules! direct')
assert presenter.count('return direct!();') == 5
assert 'crate::native_input_surface::surface(' in presenter
assert 'tree.children[0].diff(self.terminal.as_widget())' in function(input_surface, 'diff')
assert 'self.dock.map_or(0.0, |dock| dock.input_height)' in function(input_surface, 'input_height')
tools = function(native_shell, 'reference_tools')
assert 'self.reference_layout().composer.width' in tools
for requirement in ['COMPOSER_SECONDARY_MIN_WIDTH', 'COMPOSER_TERMINAL_MIN_AVAILABLE',
                    'COMPOSER_MODEL_MIN_AVAILABLE']:
    assert requirement in tools
native_chat = (ROOT / 'src/chat_mode.rs').read_text()
code += ('mod chat_mode { pub use crate::core_chat_mode::*;\n'
         '#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]\n'
         + braced_item(native_chat, 'pub enum ChatRole {') + '\n}\n')
metrics_new = braced_item(view, 'pub fn new(')
chrome_getter = function(main, 'chrome_font_size')
metrics_start = apply.index('self.metrics = Metrics::with_font(')
metrics_assignment = apply[metrics_start:apply.index(';', metrics_start) + 1]
replacements = {
    'toolbar-constants': '\n'.join(constant(profile, name) for name in [
        'COMPOSER_CONTROL_LEFT', 'COMPOSER_CONTROL_RIGHT', 'COMPOSER_PLUS_WIDTH',
        'COMPOSER_ATTACH_WIDTH', 'COMPOSER_IMAGE_WIDTH', 'COMPOSER_CODE_WIDTH',
        'COMPOSER_TERMINAL_WIDTH', 'COMPOSER_TOOLS_WIDTH', 'COMPOSER_SEND_WIDTH',
        'COMPOSER_MODEL_MIN_WIDTH', 'METADATA_GAP', 'COMPOSER_TERMINAL_MIN_AVAILABLE',
        'COMPOSER_MODEL_MIN_AVAILABLE', 'COMPOSER_SECONDARY_MIN_WIDTH']),
    'constants': '\n'.join([constant(view, name) for name in ['SCROLLBAR_WIDTH', 'BLOCK_GUTTER_WIDTH']]
                           + [constant(terminal, name) for name in ['MAX_TERMINAL_COLS', 'MAX_TERMINAL_ROWS']]
                           + [constant(main, 'PANE_HEADER_H')]),
    'clamp': function(terminal, 'clamp_terminal_dimensions'),
    'metrics-type': '#[derive(Clone, Copy, Debug)]\n' + braced_item(view, 'pub struct Metrics {'),
    'metrics-methods': '\n'.join(braced_item(view, marker) for marker in
                                ['pub fn new(', 'pub fn with_font(', 'fn block_gutter_width(', 'pub fn grid_size(']),
    'config-functions': function(config, 'default_font_size') + '\n' + function(config, 'finite_clamp'),
    'config-methods': '\n'.join(function(config, name) for name in
                               ['effective_chrome_font_size', 'set_content_font_size', 'clamp_font_size']),
    'banner': function(main, 'banner_height'),
    'apply-metrics': metrics_assignment,
    'dock-metrics': ('pub(crate) const INPUT_STRIP_HEIGHT: f32 = crate::native_viewport::INPUT_STRIP_HEIGHT;\n'
                     + braced_item(input_surface, 'pub(crate) struct DockMetrics {') + '\n'
                     + braced_item(input_surface, 'pub(crate) fn dock_metrics(')),
    'app-methods': '\n'.join(function(source, name) for source, name in
                            [(main, 'effective_font_size'), (main, 'chrome_font_size'),
                             (native_shell, 'reference_layout'),
                             (native_shell, 'reference_input_metrics'),
                             (native_shell, 'reference_native_budget'),
                             (main, 'refresh_glass_grid_dimensions'),
                             (main, 'block_reading_bar_height'), (main, 'term_width'),
                             (main, 'term_height'), (main, 'pane_header_height')]),
}
budget = (ROOT / 'scripts/font_viewport_budget_harness.rs').read_text()
for name, value in replacements.items():
    token = '@' + name + '@'
    assert budget.count(token) == 1, name
    budget = budget.replace(token, value)
code += '\n' + budget
code += ('\n#[cfg(test)] mod input_surface_geometry {\n'
         'use crate::iced::Rectangle;\n'
         'use crate::native_input_surface::dock_metrics;\n'
         + function(input_surface, 'clips') + '\n')
for name in ['no_dock_allocates_no_editor_or_phantom_floating_gap',
             'two_default_rows_fit_the_reference_strip_without_losing_a_row',
             'a_short_prompt_keeps_its_existing_padding',
             'two_row_float_boundaries_keep_both_cells_and_caret_inside_the_clip',
             'larger_physical_cells_grow_instead_of_being_clipped',
             'multiline_and_capped_drafts_keep_the_original_budget_and_last_row']:
    code += '#[test]\n' + function(input_surface, name) + '\n'
code += '}\n'
# These temporary mutants must COMPILE and then fail the executable numerical
# contract. No production source or expected assertion is changed on disk.
assert metrics_new.count('font_size * 1.2 * line_spacing') == 1
assert term_height.count('self.block_reading_bar_height()') == 1
assert chrome_getter.count('self.config.effective_chrome_font_size()') == 1
assert metrics_assignment.count('self.effective_font_size()') == 1
input_metrics = function(native_shell, 'reference_input_metrics')
assert input_metrics.count('            rows,') == 1
assert term_height.count('.map(|metrics| metrics.input_height)') == 1
mutants = [
    ('fixed-content-cell-height', metrics_new,
     metrics_new.replace('font_size * 1.2 * line_spacing', '14.0 * 1.2 * line_spacing')),
    ('fixed-reading-strip-height', term_height,
     term_height.replace('self.block_reading_bar_height()',
                         'if self.block_reading_bar_visible() { 36.0 } else { 0.0 }')),
    ('metrics-caller-coupled-to-chrome', metrics_assignment,
     metrics_assignment.replace('self.effective_font_size()', 'self.chrome_font_size()')),
    ('chrome-coupled-to-content', chrome_getter,
     chrome_getter.replace('self.config.effective_chrome_font_size()', 'self.effective_font_size()')),
    ('fixed-44px-input-in-pty-height', term_height,
     term_height.replace('.map(|metrics| metrics.input_height)',
                         '.map(|_| native_viewport::INPUT_STRIP_HEIGHT)')),
    ('discard-real-multiline-input-rows', input_metrics,
     input_metrics.replace('            rows,', '            1,')),
]
assert all(code.count(original) == 1 for _, original, _ in mutants)
with tempfile.TemporaryDirectory(prefix='frost-font-sync-') as directory:
    source = Path(directory) / 'font_sync.rs'
    source.write_text(code)
    binary = Path(directory) / 'font_sync'
    env = os.environ.copy()
    env['RUSTUP_AUTO_INSTALL'] = '0'
    subprocess.run([os.environ.get('RUSTC', 'rustc'), '--edition=2021', '--test', str(source), '-o', str(binary)], env=env, check=True)
    subprocess.run([str(binary)], env=env, check=True)
    evidence = os.environ.get('FROST_FONT_SYNC_EVIDENCE')
    evidence = Path(evidence) if evidence else None
    if evidence:
        evidence.mkdir(parents=True, exist_ok=True)
        (evidence / 'actual-font-contract.rs').write_text(code)
    receipts = []
    for name, original, mutated in mutants:
        source.write_text(code.replace(original, mutated, 1))
        subprocess.run([os.environ.get('RUSTC', 'rustc'), '--edition=2021', '--test', str(source),
                        '-o', str(binary)], env=env, check=True)
        result = subprocess.run([str(binary), 'font_viewport_budget::', '--nocapture'],
                                env=env, capture_output=True, text=True)
        assert result.returncode != 0 and 'test result: FAILED' in result.stdout, name
        receipts.append({'name': name, 'compiled': True, 'numerical_gate_rejected': True,
                         'exit_code': result.returncode,
                         'original_item_sha256': hashlib.sha256(original.encode()).hexdigest()})
        if evidence:
            (evidence / (name + '.stdout')).write_text(result.stdout)
            (evidence / (name + '.stderr')).write_text(result.stderr)
        print('negative font-budget contract rejected: ' + name)
    if evidence:
        (evidence / 'negative-results.json').write_text(json.dumps(receipts, indent=2) + '\n')
print('actual typography, sidebar virtualization and native viewport contracts passed; native shell font pixels are a separate gate')
