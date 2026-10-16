#!/usr/bin/env python3
"""Exercise actual compositor preedit/commit routing with a synthetic IME client."""
import argparse
import importlib.util
import json
from pathlib import Path
import shlex
import time

p = argparse.ArgumentParser(description=__doc__)
p.add_argument('--runner', type=Path, required=True)
p.add_argument('--ime-client', type=Path, required=True)
p.add_argument('--binary', type=Path, required=True)
p.add_argument('--output', type=Path, required=True)
a = p.parse_args()
spec = importlib.util.spec_from_file_location('native_smoke', a.runner)
m = importlib.util.module_from_spec(spec)
spec.loader.exec_module(m)
out = a.output.resolve()
out.mkdir(parents=True, exist_ok=False)
s = m.Smoke(a.binary.resolve(strict=True), 'wayland', out)
result = {'status': 'running', 'mechanism': 'real Sway input-method-v2 -> text-input-v3 -> winit IME',
          'scope': 'normal mode only; preedit screenshot requires visual review',
          'excluded': ['full fcitx/IBus engine behavior', 'X11 XIM', 'physical keyboard IME toggles']}
try:
    s.start_desktop()
    control = out / 'ime-control'
    control.mkdir()
    ime = s.spawn([str(a.ime_client.resolve(strict=True)), str(control)], 'ime-client')
    m.wait_for((control / 'connected').exists, 'IME protocol connection')
    s.config.write_text('shell = "/bin/bash"\nterminal_mode = "normal"\nrestore_session = false\n')
    app = s.spawn([str(s.binary)], 'frost')
    m.wait_for(s.app_window, 'Frost IME fixture')
    time.sleep(2)
    s.command('ime-ready')
    committed = out / 'ime-committed.txt'
    s.text("printf '%s\\n' '")
    s.screenshot('ime-before-preedit')
    (control / 'start').touch()
    m.wait_for((control / 'preedit').exists, 'actual IME activation and preedit')
    time.sleep(0.5)
    s.screenshot('ime-preedit')
    (control / 'commit').touch()
    m.wait_for((control / 'committed').exists, 'actual IME commit')
    time.sleep(0.5)
    s.screenshot('ime-commit-before-enter')
    s.text("' > " + shlex.quote(str(committed)))
    s.key('Return')
    m.wait_for(committed.exists, 'shell receives committed Unicode')
    actual = committed.read_bytes()
    assert actual == '你\n'.encode(), actual.hex()
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
