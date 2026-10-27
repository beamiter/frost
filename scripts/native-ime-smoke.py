#!/usr/bin/env python3
"""Exercise actual compositor preedit/commit routing with a synthetic IME client."""
import argparse
import importlib.util
import json
from pathlib import Path
import shlex
import re
import time

p = argparse.ArgumentParser(description=__doc__)
p.add_argument('--runner', type=Path, required=True)
p.add_argument('--ime-client', type=Path, required=True)
p.add_argument('--binary', type=Path, required=True)
p.add_argument('--output', type=Path, required=True)
p.add_argument('--mode', choices=['block', 'chat'], default='block')
p.add_argument('--foreground', action='store_true', help='Compose through real foreground cat stdin')
p.add_argument('--multiline', action='store_true', help='Compose after four real quoted input lines')
a = p.parse_args()
if a.foreground and a.multiline:
    p.error('--foreground and --multiline are separate authority scenarios')
spec = importlib.util.spec_from_file_location('native_smoke', a.runner)
m = importlib.util.module_from_spec(spec)
spec.loader.exec_module(m)
out = a.output.resolve()
out.mkdir(parents=True, exist_ok=False)
s = m.Smoke(a.binary.resolve(strict=True), 'wayland', out)
s.env['FROST_SMOKE_WAYLAND_DEBUG'] = '1'
result = {'status': 'running', 'mechanism': 'real Sway input-method-v2 -> text-input-v3 -> winit IME',
          'scope': a.mode + (' foreground cat' if a.foreground else ' multiline' if a.multiline else '') + ' mode; preedit screenshot requires visual review',
          'renderer_environment': __import__('os').environ.get('ICED_BACKEND'),
          'binary_sha256': __import__('hashlib').sha256(a.binary.read_bytes()).hexdigest(),
          'excluded': ['full fcitx/IBus engine behavior', 'X11 XIM', 'physical keyboard IME toggles']}
sidecar = a.binary.with_suffix('.source-manifest.json')
if sidecar.exists():
    result['binary_build_manifest'] = json.loads(sidecar.read_text())
    assert result['binary_sha256'] == result['binary_build_manifest']['binary_sha256']
try:
    s.start_desktop()
    # Keep a keyboard on the headless seat while the IME composes. The ten-minute
    # hold exceeds the bounded test stages; tracked cleanup stops it before F24.
    s.spawn(["wtype", "-s", "600000", "-k", "F24"], "ime-seat-keyboard")
    time.sleep(0.5)
    control = out / 'ime-control'
    control.mkdir()
    ime = s.spawn([str(a.ime_client.resolve(strict=True)), str(control)], 'ime-client')
    m.wait_for((control / 'connected').exists, 'IME protocol connection')
    s.config.write_text('shell = "/bin/bash"\nterminal_mode = "' + a.mode + '"\nrestore_session = false\njsh_update_check = "never"\n')
    s.config.chmod(0o600)
    app = s.spawn([str(s.binary)], 'frost')
    m.wait_for(s.app_window, 'Frost IME fixture')
    time.sleep(2)
    s.command('ime-ready')
    committed = out / 'ime-committed.txt'
    if a.foreground:
        s.text("cat > " + shlex.quote(str(committed)))
        s.key('Return')
        time.sleep(0.5)
    else:
        s.text("printf '%s\\n' '")
    prefix = ''
    if a.multiline:
        for index in range(1, 5):
            line = 'native input row ' + str(index)
            s.text(line)
            s.key('Return')
            prefix += line + '\n'
    s.screenshot('ime-before-preedit')
    (control / 'start').touch()
    m.wait_for((control / 'preedit').exists, 'actual IME activation and preedit')
    time.sleep(0.5)
    s.screenshot('ime-preedit')
    rectangles = re.findall(r'set_cursor_rectangle\((-?\d+), (-?\d+), (\d+), (\d+)\)',
                            (out / 'frost.log').read_text())
    assert rectangles, 'No native text-input-v3 cursor rectangle'
    result['preedit_cursor_rectangle'] = [int(value) for value in rectangles[-1]]
    (control / 'commit').touch()
    m.wait_for((control / 'committed').exists, 'actual IME commit')
    time.sleep(0.5)
    s.screenshot('ime-commit-before-enter')
    if a.foreground:
        s.key('Return')
        s.key('d', ('ctrl',))
    else:
        s.text("' > " + shlex.quote(str(committed)))
        s.key('Return')
    m.wait_for(lambda: committed.exists() and committed.read_bytes() == (prefix + '你\n').encode(), 'shell receives exact committed Unicode')
    actual = committed.read_bytes()
    assert actual == (prefix + '你\n').encode(), actual.hex()
    result['committed_hex'] = actual.hex()
    s.command('ime-input-resumed')
    s.screenshot('ime-completed')
    (control / 'finish').touch()
    ime.wait(timeout=5)
    assert ime.returncode == 0
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
