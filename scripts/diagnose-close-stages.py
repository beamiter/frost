"""Bounded, isolated Wayland close diagnostics; no production semantics changed.

Each trial uses one click, the existing 20-second close deadline and real
compositor input. Required environment: FROST_SMOKE_POINTER and, for the
negative control, FROST_STAGE_HELD_POINTER (native-pointer.c built separately
with only the 50ms pressed interval changed to 3000ms). `cancel` is a negative control: cancel/reopen during a held
click must retire the old gesture. A pointer `done` is only a compositor ack.
"""
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import sys
import threading
import time
from types import SimpleNamespace

ROOT = Path(os.environ.get('FROST_STAGE_SOURCE', str(Path(__file__).resolve().parents[1])))
sys.path.insert(0, str(ROOT / 'scripts'))

def load(name, path):
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module

recovery = load('recovery', ROOT / 'scripts/native-recovery-report.py')
smoke_module = load('smoke', ROOT / 'scripts/native-desktop-smoke.py')
variant, output, binary = sys.argv[1], Path(sys.argv[2]).resolve(), Path(sys.argv[3]).resolve()
assert variant in ('reuse', 'early', 'late', 'cancel')
output.mkdir(parents=True, exist_ok=False)
pointer = Path(os.environ['FROST_SMOKE_POINTER']).resolve()
args = SimpleNamespace(binary=binary, backend='wayland', output=output, pointer=pointer)
result, stages = {}, []
case = recovery.Case(smoke_module, args, 'excess_sessions', result, time.monotonic() + 240)
smoke = case.smoke
smoke.env['FROST_SMOKE_WAYLAND_DEBUG'] = '1'
smoke.env['FROST_SMOKE_POINTER'] = str(pointer if variant != 'cancel' else Path(os.environ['FROST_STAGE_HELD_POINTER']))
trace_path = case.out / 'frost-excess_sessions.log'
threads, errors = [], []

def mark(stage, **fields):
    stages.append(dict(stage=stage, monotonic=time.monotonic(), **fields))
    (output / 'stages.json').write_text(json.dumps(stages, indent=2))

original_popen = subprocess.Popen

def tracked_popen(command, *a, **kw):
    proc = original_popen(command, *a, **kw)
    if command and str(command[0]).endswith(('native-pointer', 'held-pointer')):
        mark('pointer_created', pid=proc.pid, executable=str(command[0]))
    return proc
subprocess.Popen = tracked_popen
original_reply = smoke.pointer_reply

def reply(proc, expected):
    value = original_reply(proc, expected)
    mark('pointer_compositor_ack', pid=proc.pid, ack=expected, application_delivery_proven=False)
    return value
smoke.pointer_reply = reply
original_frame = smoke.observe_frame

def frame(*a, **kw):
    name, rows = original_frame(*a, **kw)
    button = smoke.close_button(rows)
    if button:
        mark('close_button_observed', frame=name, center=list(button))
    return name, rows
smoke.observe_frame = frame
original_run = smoke.run

def run(command, **kw):
    if command and command[0] == 'swaymsg' and command[-1] == 'kill':
        mark('close_request_sent')
    return original_run(command, **kw)
smoke.run = run

original_close = smoke.close_app

def close(app):
    mark('close_attempt_started', app_pid=app.pid, variant=variant)
    offset = len(trace_path.read_text())
    if variant == 'reuse':
        smoke._close_pointer = case.pointer
        mark('pointer_reused', pid=case.pointer.pid)
    elif variant == 'early':
        log = open(smoke.out / 'close-pointer.log', 'w')
        smoke.logs.append(log)
        proc = subprocess.Popen([str(pointer)], env=smoke.env, stdin=subprocess.PIPE,
                                stdout=subprocess.PIPE, stderr=log, bufsize=0)
        smoke.processes.append(proc)
        smoke._close_pointer = proc
        smoke.pointer_reply(proc, 'ready')
        mark('second_pointer_ready_before_close_request', pid=proc.pid)
    elif variant == 'cancel':
        def await_trace(fragment, timeout=10):
            deadline = time.monotonic() + timeout
            while time.monotonic() < deadline:
                if fragment in trace_path.read_text()[offset:]:
                    return
                time.sleep(.01)
            raise AssertionError('Missing application trace: ' + fragment)
        def transition():
            try:
                await_trace('state: Pressed, button: Left')
                mark('application_press_observed_before_state_change')
                smoke.key('Escape')
                await_trace('keep_window_open current=')
                mark('application_cancel_observed')
                smoke.run(['swaymsg', '[app_id="io.github.beamiter.frost"]', 'kill'])
                await_trace('request_guarded_close current=2')
                mark('application_reopen_observed')
            except BaseException as error:
                errors.append(repr(error))
        thread = threading.Thread(target=transition)
        threads.append(thread)
        thread.start()
    try:
        value = original_close(app)  # Exactly one click and the unchanged wait(timeout=20).
        mark('process_exited', returncode=app.returncode, original_times=value)
        assert variant != 'cancel', 'Old held gesture closed a new dialog'
        return value
    except subprocess.TimeoutExpired:
        mark('process_exit_timeout', timeout_seconds=20, still_alive=app.poll() is None)
        raise
smoke.close_app = close

try:
    case.execute()
    trace = trace_path.read_text()
    trace = trace[trace.index('native CloseRequested'):]
    assert trace.count('state: Pressed, button: Left') == 1
    assert trace.count('state: Released, button: Left') == 1
    assert trace.count('ForceClose accepted') == 1
    assert 'before iced::exit()' in trace
    assert any(stage['stage'] == 'process_exited' and stage['returncode'] == 0 for stage in stages)
    result['single_click_application_delivery_verified'] = True
except BaseException as error:
    result.update(status='fail', error=repr(error))
    smoke.screenshot('failure-before-cleanup')
    if variant == 'cancel' and isinstance(error, subprocess.TimeoutExpired):
        for thread in threads:
            thread.join(1)
        suffix = trace_path.read_text()[trace_path.read_text().find('native CloseRequested'):]
        assert not errors, errors
        assert 'keep_window_open current=1' in suffix
        assert 'request_guarded_close current=2' in suffix
        assert suffix.count('state: Pressed, button: Left') == 1
        assert suffix.count('state: Released, button: Left') == 1
        assert suffix.index('request_guarded_close current=2') < suffix.index('state: Released, button: Left')
        assert 'ForceClose accepted' not in suffix
        assert 'before iced::exit()' not in suffix
        assert case.app.poll() is None
        result.update(status='pass-negative-control', expected_timeout=True, single_click_application_delivery_verified=True)
    else:
        raise
finally:
    for thread in threads:
        thread.join(1)
    result.update(variant=variant, stages=stages, transition_errors=errors)
    if trace_path.exists():
        result['application_stages'] = [line for line in trace_path.read_text().splitlines() if '[close-diag' in line]
    case.cleanup()
    (output / 'result.json').write_text(json.dumps(result, indent=2))
    print(json.dumps({k: result.get(k) for k in ('variant', 'status', 'error', 'elapsed_seconds')}))
