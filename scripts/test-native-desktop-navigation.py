#!/usr/bin/env python3
"""Navigation observer regressions; real compositor/PTY execution is separate."""
import importlib.util
import tempfile
from pathlib import Path
from unittest.mock import patch

spec = importlib.util.spec_from_file_location('smoke', Path(__file__).with_name('native-desktop-smoke.py'))
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)


def rows(text, line='1'):
    return [dict(level='5', text=word, left=str(index * 50), top='20',
                 width='45', height='15', page_num='1', block_num='1',
                 par_num='1', line_num=line) for index, word in enumerate(text.split())]


api = rows('New chat API conversation AI Chats not configured')
terminal = rows('Workspace / frost Block session Local API CONVERSATIONS New chat')
assert module.Smoke.surface_state(api) == 'api'
assert module.Smoke.surface_state(terminal) == 'terminal'
assert module.Smoke.surface_state(rows('Workspace / frost Chat session Local')) == 'terminal'
assert module.Smoke.surface_state(rows('API CONVERSATIONS')) is None
assert module.Smoke.surface_state(rows('Workspace / frost')) is None
assert module.Smoke.surface_state(api + terminal) is None

# Startup releases the API once. A subsequent launch/mode whose saved view is
# already terminal must send no Escape, preserving the first shell byte.
for states, expected in [([api, terminal], ['Escape']), ([terminal], [])]:
    with tempfile.TemporaryDirectory() as directory:
        smoke = module.Smoke(Path('/not-executed'), 'x11', Path(directory))
        keys = []
        iterator = iter(states)
        def observe(label):
            smoke._frame_number = getattr(smoke, '_frame_number', 0) + 1
            return str(smoke._frame_number), next(iterator)
        smoke.observe_frame = observe
        smoke.key = keys.append
        smoke.select_terminal()
        assert keys == expected, keys
        smoke.close()

warning = rows('Closing Frost Eva disconnected shared body hidden Close anyway')
assert module.Smoke.close_button(warning) is not None
assert module.Smoke.close_button(rows('Closing Frost Work is still running Close anyway')) is None
assert module.Smoke.close_button(rows('Eva disconnected Close anyway')) is None
assert module.Smoke.close_button(warning + rows('Close anyway', line='2')) is None
print('Native visible-owner navigation and exact disconnected-close observation passed')

# The recovery banner moves the column body. Absolute native overlays must
# reserve that same height, or the real recovery/preservation text is covered.
source = (Path(__file__).resolve().parents[1] / 'src/native_shell.rs').read_text()
workspace = ''.join(source.split('    pub(crate) fn reference_workspace', 1)[1].split())
for contract in ('y:layout.rail.y+recovery_height',
                 'height:(layout.rail.height-recovery_height).max(0.0)',
                 'top:layout.center.y+recovery_height'):
    assert contract in workspace, contract
