#!/usr/bin/env python3
"""Navigation observer regressions; real compositor/PTY execution is separate."""
import importlib.util
import tempfile
import json
import struct
from types import SimpleNamespace
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

# Intentional API navigation releases the API once. An already visible
# terminal must send no Escape, preserving the first shell byte.
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

# Fresh-launch smoke must reject the former background focus steal without
# silently repairing it with Escape. Preserve the observed frame as evidence.
for state, rejected in [(api, True), (terminal, False)]:
    with tempfile.TemporaryDirectory() as directory:
        smoke = module.Smoke(Path('/not-executed'), 'x11', Path(directory))
        keys = []
        def observe(label):
            smoke._frame_number = 1
            return 'startup', state
        smoke.observe_frame = observe
        smoke.key = keys.append
        try:
            smoke.select_terminal(require_terminal=True)
        except AssertionError as error:
            assert rejected and 'migration opened' in str(error)
        else:
            assert not rejected
        assert keys == []
        assert list(Path(directory).glob('terminal-owner-*.json'))
        smoke.close()

warning = rows('Closing Frost Eva disconnected shared body hidden Close anyway')
assert module.Smoke.close_button(warning) is not None
assert module.Smoke.close_button(rows('Closing Frost Work is still running Close anyway')) is None
assert module.Smoke.close_button(rows('Eva disconnected Close anyway')) is None
assert module.Smoke.close_button(warning + rows('Close anyway', line='2')) is None
# Crop coordinates are mapped back onto the original compositor frame. The
# observed dialog text is still mandatory; the crop is not click permission.
with tempfile.TemporaryDirectory() as directory:
    smoke = module.Smoke(Path('/not-executed'), 'wayland', Path(directory))
    header = b'\x89PNG\r\n\x1a\n' + b'\0' * 8 + struct.pack('>II', 1280, 900)
    smoke.screenshot = lambda name: (Path(directory) / (name + '.png')).write_bytes(header)
    columns = ['level', 'text', 'left', 'top', 'width', 'height']
    tsv = '\t'.join(columns) + '\n5\tObserved\t303\t606\t120\t30\n'
    commands = []
    def run(command, **kwargs):
        commands.append(command)
        return SimpleNamespace(stdout=tsv if command[0] == 'tesseract' else '')
    smoke.run = run
    name, observed = smoke.observe_frame('close-crop', enhanced=True)
    assert commands[0][2:5] == ['-crop', '768x270+256+315', '+repage']
    assert [observed[0][key] for key in ('left', 'top', 'width', 'height')] == ['357', '517', '40', '10']
    assert module.Smoke.close_button(observed) is None
    assert json.loads((Path(directory) / (name + '.ocr-transform.json')).read_text())['crop'] == [256, 315, 768, 270]
    smoke.close()

print('Native visible-owner navigation and exact disconnected-close observation passed')

# The recovery banner moves the column body. Absolute native overlays must
# reserve that same height, or the real recovery/preservation text is covered.
source = (Path(__file__).resolve().parents[1] / 'src/native_shell.rs').read_text()
workspace = ''.join(source.split('    pub(crate) fn reference_workspace', 1)[1].split())
for contract in ('y:layout.rail.y+recovery_height',
                 'height:(layout.rail.height-recovery_height).max(0.0)',
                 'top:layout.center.y+recovery_height'):
    assert contract in workspace, contract
