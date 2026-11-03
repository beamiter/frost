#!/usr/bin/env python3
"""Isolated real X11/Vulkan/PTy drag-copy and device-loss recovery regression.

Requires Xvfb, xdotool, xclip, ImageMagick, Pillow, a C compiler and Vulkan
headers/loader. No live desktop, user clipboard or system suspend is used.
"""
import argparse
import importlib.util
import json
from pathlib import Path
import subprocess
import time

from PIL import Image

ROOT = Path(__file__).resolve().parents[1]
parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--binary', type=Path, required=True)
parser.add_argument('--output', type=Path, required=True)
args = parser.parse_args()
out = args.output.resolve()
out.mkdir(parents=True, exist_ok=False, mode=0o700)
spec = importlib.util.spec_from_file_location('native_smoke', ROOT / 'scripts/native-desktop-smoke.py')
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)
s = module.Smoke(args.binary.resolve(strict=True), 'x11', out)
result = {'status': 'running', 'modes': [], 'scope': 'native X11 drag/copy and simulated Vulkan device loss; physical lid/suspend not exercised'}


def focus_window():
    probe = subprocess.run(['xdotool', 'search', '--onlyvisible', '--classname',
                            'io.github.beamiter.frost'], env=s.env,
                           capture_output=True, text=True, timeout=3)
    if probe.returncode or not probe.stdout.strip():
        return None
    window = probe.stdout.splitlines()[-1]
    s.run(['xdotool', 'windowfocus', '--sync', window])
    return window


def marker_bounds(name, color=(231, 37, 213)):
    s.screenshot(name)
    im = Image.open(out / (name + '.png')).convert('RGB')
    points = [(x, y) for y in range(im.height) for x in range(im.width)
              if sum(abs(a - b) for a, b in zip(im.getpixel((x, y)), color)) < 12]
    assert len(points) > 100, 'Synthetic output marker is not painted'
    return min(x for x, _ in points), min(y for _, y in points), max(x for x, _ in points), max(y for _, y in points)


def drag_copy(bounds, label, expected=None):
    expected = MARKER if expected is None else expected
    left, top, right, bottom = bounds
    y = (top + bottom) // 2
    s.run(['xdotool', 'mousemove', str(left), str(y), 'mousedown', '1'])
    time.sleep(0.1)
    s.run(['xdotool', 'mousemove', str(right), str(y)])
    time.sleep(0.15)
    s.screenshot(label + '-held')
    s.run(['xdotool', 'mouseup', '1'])
    time.sleep(0.2)
    primary = s.run(['xclip', '-o', '-selection', 'primary'], capture_output=True).stdout
    s.key('c', ('ctrl', 'shift'))
    copied = s.run(['xclip', '-o', '-selection', 'clipboard'], capture_output=True).stdout
    assert primary == copied == expected.encode(), (label, primary, copied)
    s.screenshot(label + '-copied')


MARKER = 'SELECT_TARGET_0123456789_ABCDEFGHIJKLMNOPQRSTUVWXYZ'
try:
    # Let Xvfb choose an unused display; a window manager is unnecessary for
    # native input/rendering and direct XSetInputFocus on this private server.
    xvfb = s.spawn(['Xvfb', '-displayfd', '1', '-screen', '0', '1280x900x24',
                    '-nolisten', 'tcp'], 'xvfb')
    display = module.wait_for(lambda: next((line for line in (out / 'xvfb.log').read_text().splitlines()
                                           if line.isdecimal()), None), 'private X display')
    s.env['DISPLAY'] = ':' + display
    s.env['JTERM_WORKSPACE_STATE_DIR'] = str(out / 'workspace')
    s.env['RUST_LOG'] = 'warn,iced_winit=info'
    proxy = out / 'vulkan-proxy'
    proxy.mkdir()
    s.run(['cc', '-shared', '-fPIC', '-Wall', '-Wextra', '-Werror',
           str(ROOT / 'scripts/native-vulkan-fault.c'), '-ldl', '-o', str(proxy / 'libvulkan.so.1')])
    s.env['LD_LIBRARY_PATH'] = str(proxy) + (':' + s.env['LD_LIBRARY_PATH'] if s.env.get('LD_LIBRARY_PATH') else '')
    trigger = out / 'lose-device'
    s.env['FROST_TEST_VULKAN_FAULT'] = str(trigger)
    for mode in ['block', 'chat', 'normal']:
        s.config.write_text(f'shell = "/bin/bash"\nterminal_mode = "{mode}"\nrestore_session = false\n'
                            'eva_companion_enabled = false\nascii_organism_enabled = false\n'
                            'agent_workspace_enabled = false\ninitial_width = 1100\ninitial_height = 760\n'
                            'font_family = "DejaVu Sans Mono"\nfont_size = 14\n')
        s.config.chmod(0o600)
        app = s.spawn([str(s.binary)], mode)
        window = module.wait_for(focus_window, mode + ' window')
        time.sleep(1)
        # The default workspace opens its API conversation. With navigation
        # collapsed above, the first native terminal is the row at this slot.
        s.run(['xdotool', 'mousemove', '110', '126', 'click', '1'])
        if mode == 'block':
            s.text('echo HEADER_DRAG')
            s.key('Return')
            time.sleep(0.3)
            drag_copy((479, 174, 503, 182), 'completed-command', 'echo')
            s.run(['xdotool', 'mousemove', '600', '649', 'click', '1'])
        identity = s.command(mode + '-before')
        s.text(r"printf '\033[38;2;231;37;213m" + MARKER + r"\033[0m\n'")
        s.key('Return')
        time.sleep(0.5)
        bounds = marker_bounds(mode + '-before')
        drag_copy(bounds, mode + '-selection')
        if mode == 'block':
            # Loss occurs in the real Vulkan acquire path. WGPU marks this
            # device lost; the host has to replace its compositor/resources.
            for cycle in range(3):
                # Drag-copy deliberately parks the history reader. Use its
                # public Reading pill to follow new output before the next run.
                s.run(['xdotool', 'mousemove', '600', '649', 'click', '1'])
                time.sleep(0.2)
                s.run(['xdotool', 'mousemove', '800', '67', 'click', '1'])
                time.sleep(0.3)
                trigger.touch()
                s.run(['xdotool', 'windowsize', window, str(1102 + cycle * 2), '760'])
                module.wait_for(lambda: not trigger.exists(), 'device loss injection')
                log = out / (mode + '.log')
                module.wait_for(lambda: log.read_text().count('Graphics device and window surfaces recovered') == cycle + 1,
                                'graphics recovery')
                time.sleep(0.5)
                assert app.poll() is None, 'Recovery exited the terminal process'
                assert s.command(f'recovered-{cycle}') == identity, 'Recovery replaced the shell/PTY'
                color = [(37, 213, 87), (213, 127, 37), (37, 153, 231)][cycle]
                s.text(r"printf '\033[38;2;" + ';'.join(map(str, color)) + 'm' + MARKER + r"\033[0m\n'")
                s.key('Return')
                time.sleep(0.3)
                # Each cycle uses a new color, so stale pre-loss pixels cannot
                # satisfy the paint or drag-copy assertion.
                recovered_bounds = marker_bounds('recovered-' + str(cycle), color)
                drag_copy(recovered_bounds, 'recovered-selection-' + str(cycle))
            result['recovery_cycles'] = 3
            s.text('DRAFT_TARGET')
            drag_copy((479, 645, 573, 653), 'current-draft', 'DRAFT_TARGET')
            s.key('c', ('ctrl',))
        result['modes'].append(mode)
        app.terminate()
        app.wait(timeout=10)
    result['status'] = 'pass'
except Exception as error:
    result.update(status='fail', error=str(error))
    try:
        s.screenshot('failure')
    except Exception:
        pass
    raise
finally:
    s.close()
    (out / 'result.json').write_text(json.dumps(result, indent=2))
