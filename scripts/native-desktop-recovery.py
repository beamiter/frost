#!/usr/bin/env python3
"""Second-stage real desktop acceptance: graceful restart and headless scaling.
Imports the reviewed basic runner. No app-only test mode or direct UI messages.
"""
import argparse
import importlib.util
import json
from pathlib import Path
import shlex
import time

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--runner', type=Path, required=True)
parser.add_argument('--binary', type=Path, required=True)
parser.add_argument('--backend', choices=['x11', 'wayland'], required=True)
parser.add_argument('--output', type=Path, required=True)
args = parser.parse_args()
spec = importlib.util.spec_from_file_location('native_smoke', args.runner)
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)
out = args.output.resolve()
out.mkdir(parents=True, exist_ok=False)
smoke = module.Smoke(args.binary.resolve(strict=True), args.backend, out)
result = {'backend': args.backend, 'status': 'running', 'assertions': []}

def execute(command, marker):
    smoke.text(command)
    smoke.key('Return')
    return smoke.command(marker)

def launch(label):
    proc = smoke.spawn([str(smoke.binary)], label)
    module.wait_for(smoke.app_window, 'application ' + label)
    time.sleep(2)
    assert proc.poll() is None
    return proc

def close(proc):
    if args.backend == 'x11':
        smoke.key('F4', ('alt',))
    else:
        smoke.run(['swaymsg', '[app_id="io.github.beamiter.frost"]', 'kill'])
    proc.wait(timeout=20)
    assert proc.returncode == 0, proc.returncode

def read_pwd(label):
    path = out / (label + '-pwd.txt')
    identity = execute('printf "%s\\n" "$PWD" > ' + shlex.quote(str(path)), label + '-pwd-recorded')
    return path.read_text().strip(), identity

try:
    smoke.start_desktop()
    smoke.config.write_text('shell = "/bin/bash"\nterminal_mode = "normal"\nrestore_session = true\ninitial_width = 1100\ninitial_height = 760\n')
    dirs = [out / 'first-cwd', out / 'second-cwd']
    for directory in dirs:
        directory.mkdir()
    first = launch('before-restart')
    first_identity = execute('cd ' + shlex.quote(str(dirs[0])), 'before-first')
    smoke.key('t', ('ctrl', 'shift'))
    time.sleep(1)
    second_identity = execute('cd ' + shlex.quote(str(dirs[1])), 'before-second')
    assert first_identity != second_identity
    smoke.screenshot('before-restart')
    close(first)
    snapshot_path = smoke.config.parent / 'session_history.json'
    snapshot = json.loads(snapshot_path.read_text())
    (out / 'saved-snapshot.json').write_text(json.dumps(snapshot, indent=2))
    assert [s['cwd'] for s in snapshot['sessions']] == [str(p) for p in dirs], snapshot
    assert len(snapshot['tabs']) == 2 and snapshot['active_tab'] == 1, snapshot
    second = launch('after-restart')
    active_cwd, restored_second = read_pwd('restored-active')
    assert active_cwd == str(dirs[1])
    smoke.key('Page_Up', ('ctrl',))
    first_cwd, restored_first = read_pwd('restored-first')
    assert first_cwd == str(dirs[0])
    old_pids = {first_identity.split(':', 1)[0], second_identity.split(':', 1)[0]}
    new_pids = {restored_first.split(':', 1)[0], restored_second.split(':', 1)[0]}
    assert len(old_pids) == len(new_pids) == 2 and old_pids.isdisjoint(new_pids)
    assert restored_first.split(':', 1)[1] != restored_second.split(':', 1)[1]
    # PTY device names may legitimately be recycled across a graceful restart.
    result['shell_identities'] = {'before': [first_identity, second_identity], 'after': [restored_first, restored_second]}
    result['assertions'].append('Two tabs, active tab, and actual shell cwd restored after graceful installed-binary restart')
    smoke.screenshot('after-restart')
    if args.backend == 'wayland':
        dimensions = []
        for index, scale in enumerate((1, 1.5, 2, 1)):
            smoke.run(['swaymsg', 'output', 'HEADLESS-1', 'scale', str(scale)])
            time.sleep(2)
            record = out / ('scale-' + str(index) + '-stty.txt')
            identity = execute('stty size > ' + shlex.quote(str(record)), 'scale-' + str(index))
            assert identity == restored_first, 'Scaling changed the active session'
            rows, cols = map(int, record.read_text().split())
            assert rows > 0 and cols > 0
            dimensions.append({'scale': scale, 'rows': rows, 'cols': cols})
            smoke.screenshot('scale-' + str(index))
            outputs = smoke.run(['swaymsg', '-t', 'get_outputs', '-r'], text=True, capture_output=True)
            (out / ('scale-' + str(index) + '-outputs.json')).write_text(outputs.stdout)
            assert any(o['name'] == 'HEADLESS-1' and o['scale'] == scale for o in json.loads(outputs.stdout))
        assert dimensions[0]['rows'] == dimensions[3]['rows'] and dimensions[0]['cols'] == dimensions[3]['cols'], dimensions
        assert dimensions[1]['rows'] < dimensions[0]['rows'] and dimensions[2]['rows'] < dimensions[1]['rows'], dimensions
        assert dimensions[1]['cols'] < dimensions[0]['cols'] and dimensions[2]['cols'] < dimensions[1]['cols'], dimensions
        result['scaling'] = dimensions
        result['assertions'].append('Headless Wayland scale 1/1.5/2/1: positive shrinking PTY geometry and exact round-trip grid')
    close(second)
    result['status'] = 'pass'
except Exception as error:
    result.update(status='fail', error=str(error))
    try:
        smoke.screenshot('failure')
    except Exception:
        pass
    raise
finally:
    smoke.close()
    (out / 'result.json').write_text(json.dumps(result, indent=2))
