#!/usr/bin/env python3
"""Capture a native Wayland window under real fractional output scaling.

App ui_scale is fixed at 1. Protocol preferred_scale and physical crop sizes are
recorded separately; this does not substitute an app font/zoom setting for DPR.
"""
import argparse
import hashlib
import importlib.util
import json
import re
import struct
import time
from pathlib import Path

p = argparse.ArgumentParser(description=__doc__)
p.add_argument('--binary', required=True, type=Path)
p.add_argument('--runner', required=True, type=Path)
p.add_argument('--output', required=True, type=Path)
p.add_argument('--scale', required=True, type=float, choices=[1.25, 1.5])
p.add_argument('--mode', choices=['block', 'chat'], default='chat')
a = p.parse_args()
spec = importlib.util.spec_from_file_location('native_smoke', a.runner)
m = importlib.util.module_from_spec(spec)
spec.loader.exec_module(m)
out = a.output.resolve()
out.mkdir(parents=True, exist_ok=False)
s = m.Smoke(a.binary.resolve(strict=True), 'wayland', out)
s.env.update(ICED_BACKEND='tiny-skia', JTERM_REFERENCE_FIXTURE='1', FROST_SMOKE_WAYLAND_DEBUG='1')
result = {'status': 'running', 'app_scale': 1, 'mode': a.mode, 'requested_display_scale': a.scale,
          'backend': 'native Iced Wayland / tiny-skia / headless Pixman',
          'binary_sha256': hashlib.sha256(a.binary.read_bytes()).hexdigest(),
          'logical_reference_size': [1356, 704], 'captures': [],
          'excluded': ['physical monitor rasterization', 'hardware GPU', 'original JPEG comparison']}
sidecar = a.binary.with_suffix('.source-manifest.json')
if sidecar.exists():
    result['binary_build_manifest'] = json.loads(sidecar.read_text())
    assert result['binary_sha256'] == result['binary_build_manifest']['binary_sha256']
try:
    s.start_desktop()
    s.run(['swaymsg', f'output * resolution 2400x1600 scale {a.scale}'])
    outputs = json.loads(s.run(['swaymsg', '-t', 'get_outputs', '-r'], capture_output=True, text=True).stdout)
    result['outputs'] = outputs
    assert outputs[0]['scale'] == a.scale, outputs
    s.config.write_text('shell = "/bin/bash"\nterminal_mode = "' + a.mode + '"\nrestore_session = false\njsh_update_check = "never"\ninitial_width = 1356\ninitial_height = 704\nui_scale = 1\nfont_family = "DejaVu Sans Mono"\nfont_size = 12\nchrome_font_size = 10\nshow_repo_strip = false\nbottom_bar = false\n')
    s.config.chmod(0o600)
    app = s.spawn([str(s.binary)], 'frost-display-scale')
    window_id = m.wait_for(s.app_window, 'native fractional-scale window')
    s.run(['swaymsg', f'[con_id={window_id}] floating enable, border none'])
    time.sleep(0.5)
    s.run(['swaymsg', f'[con_id={window_id}] resize set width 1356 px height 704 px, move position 20 px 20 px'])
    def find(node):
        if node.get('id') == window_id:
            return node
        for child in node.get('nodes', []) + node.get('floating_nodes', []):
            found = find(child)
            if found:
                return found
    def settled_rect():
        assert app.poll() is None, 'Native Frost exited'
        tree = json.loads(s.run(['swaymsg', '-t', 'get_tree', '-r'], capture_output=True, text=True).stdout)
        (out / 'sway-final-tree.json').write_text(json.dumps(tree, indent=2))
        rect = find(tree)['rect']
        return rect if [rect['width'], rect['height']] == [1356, 704] else None
    rect = m.wait_for(settled_rect, 'committed 1356 x 704 logical window')
    time.sleep(1)
    result['window_rect_logical'] = rect
    path = out / 'reference-display-scale.png'
    s.run(['grim', '-g', f"{rect['x']},{rect['y']} {rect['width']}x{rect['height']}", str(path)])
    width, height = struct.unpack('>II', path.read_bytes()[16:24])
    result['captures'].append({'file': path.name, 'physical_size': [width, height]})
    assert [width, height] == [round(1356 * a.scale), round(704 * a.scale)], (width, height)
    protocol = (out / 'frost-display-scale.log').read_text()
    preferred = [int(v) for v in re.findall(r'preferred_scale\((\d+)\)', protocol)]
    result['wayland_preferred_scale_120ths'] = preferred
    buffers = [int(v) for v in re.findall(r'set_buffer_scale\((\d+)\)', protocol)]
    result['wayland_surface_buffer_scales'] = buffers
    assert buffers, 'No native Wayland surface buffer scale recorded'
    result['winit_surface_dpr_from_protocol'] = (preferred[-1] / 120 if preferred else buffers[-1])
    result['fractional_scale_protocol_available'] = bool(preferred)
    result['display_scale_note'] = ('Fractional-scale-v1 event received' if preferred else
        'Installed Sway exposes integer wl_surface buffer scale; the compositor applies its actual fractional output transform')
    result['status'] = 'pass'
except Exception as error:
    result.update(status='fail', error=str(error))
    raise
finally:
    s.close()
    (out / 'result.json').write_text(json.dumps(result, indent=2))
