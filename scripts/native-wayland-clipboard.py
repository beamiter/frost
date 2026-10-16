#!/usr/bin/env python3
"""Actual Frost/compositor clipboard transfers with synthetic bounded owners."""
import argparse
import importlib.util
import json
from pathlib import Path
import shlex
import subprocess
import time

p = argparse.ArgumentParser(description=__doc__)
for name in ('runner', 'binary', 'owner', 'output'):
    p.add_argument('--' + name, type=Path, required=True)
a = p.parse_args()
spec = importlib.util.spec_from_file_location('native_smoke', a.runner)
m = importlib.util.module_from_spec(spec)
spec.loader.exec_module(m)
out = a.output.resolve()
out.mkdir(parents=True, exist_ok=False)
s = m.Smoke(a.binary.resolve(strict=True), 'wayland', out)
# libwayland may prefer an inherited display FD over WAYLAND_DISPLAY.
s.env.pop('WAYLAND_SOCKET', None)
result = {'status': 'running', 'cases': [], 'excluded': ['allocation accounting', 'eight concurrent provider admissions', 'PRIMARY', 'idle owned-source write', 'legacy synchronous widget read']}


# A public Bash readline binding observes the real foreground shell buffer.
# It does not insert, execute, consume, or repair clipboard text.
line_capture = out / 'readline-bytes.bin'
s.env['FROST_SMOKE_LINE_CAPTURE'] = str(line_capture)
with (Path(s.env['HOME']) / '.bash_profile').open('a') as profile:
    profile.write(r"""
__frost_capture_line() {
    local __frost_capture_trace=0
    [[ $- == *x* ]] && __frost_capture_trace=1
    set +x
    printf %s "$READLINE_LINE" > "$FROST_SMOKE_LINE_CAPTURE.tmp"
    mv "$FROST_SMOKE_LINE_CAPTURE.tmp" "$FROST_SMOKE_LINE_CAPTURE"
    if (( __frost_capture_trace )); then set -x; fi
}
bind -x '"\C-x\C-g":__frost_capture_line'
""")


def exact_pending_line(expected, label):
    deadline = time.monotonic() + 5
    attempts = 0
    observed = b''
    while time.monotonic() < deadline:
        # Retry only this read-only observation chord, never paste or Return.
        line_capture.unlink(missing_ok=True)
        remaining = deadline - time.monotonic()
        if remaining <= 0:
            break
        subprocess.run(['wtype', '-s', '250', '-M', 'ctrl', '-k', 'x', '-k', 'g', '-m', 'ctrl', '-s', '50'],
                       env=s.env, check=True, timeout=remaining)
        attempts += 1
        until = min(deadline, time.monotonic() + 0.3)
        while not line_capture.exists() and time.monotonic() < until:
            time.sleep(0.01)
        if line_capture.exists():
            observed = line_capture.read_bytes()
            (out / (label + '.bin')).write_bytes(observed)
            if observed == expected:
                (out / (label + '.json')).write_text(json.dumps({'exact_bytes': len(observed), 'observation_chords': attempts}))
                s.screenshot(label)
                return
    raise AssertionError({'pending_line_expected_hex': expected.hex(), 'observed_hex': observed.hex()})



def events(log, complete=False):
    lines = log.read_text().splitlines(keepends=True)
    if lines and not lines[-1].endswith('\n'):
        assert not complete, 'incomplete final owner event'
        lines.pop()  # A live writer may be between write and newline.
    return [json.loads(line) for line in lines if line.startswith('{')]


def wait_event(log, phase, timeout=5):
    return m.wait_for(lambda: next((e for e in events(log) if e['phase'] == phase), None), 'owner ' + phase, timeout)


def owner(case, scenario, payload, delay=0):
    control = out / (case + '-control')
    control.mkdir()
    body = out / (case + '-synthetic-payload')
    body.write_bytes(payload)
    proc = s.spawn([str(a.owner.resolve(strict=True)), str(control), scenario, str(delay), 'clipboard', str(body)], case + '-owner')
    m.wait_for((control / 'ready').exists, case + ' owner ready')
    return proc, control, out / (case + '-owner.log')


def command(text, filename, timeout=3):
    marker = out / filename
    s.text(text)
    s.key('Return')
    m.wait_for(marker.exists, 'exact shell command ' + filename, timeout)
    assert marker.read_bytes() == b'ok', marker.read_bytes().hex()

try:
    s.start_desktop()
    # Keep keyboard capability stable across asynchronous clipboard delivery.
    s.spawn(['wtype', '-s', '600000', '-k', 'F24'], 'clipboard-seat-keyboard')
    cases = [('small', 'finite', b"printf ok > small-result", 0),
             ('delayed', 'finite', b'', 2000),
             ('stalled', 'stalled', b'', 0),
             ('oversized', 'finite', b'X' * (1024 * 1024 + 1), 0),
             ('continuous', 'continuous', b'', 0),
             ('revoked', 'finite', b'REVOKED_POISON', 2000),
             ('teardown', 'stalled', b'', 0)]
    for case, scenario, payload, delay in cases:
        entry = {'name': case, 'status': 'running'}
        result['cases'].append(entry)
        s.config.write_text('shell = "/bin/bash"\nterminal_mode = "normal"\nrestore_session = false\n')
        app = s.spawn([str(s.binary)], case + '-frost')
        m.wait_for(s.app_window, case + ' window')
        time.sleep(1)
        s.text('cd ' + shlex.quote(str(out)))
        s.key('Return')
        identity = s.command(case + '-ready')
        proc, control, log = owner(case, scenario, payload, delay)
        started = time.monotonic()
        s.key('v', ('ctrl', 'shift'))
        request = wait_event(log, 'request')
        if case == 'small':
            wait_event(log, 'complete')
            exact_pending_line(payload, 'small-pasted-before-enter')
            s.key('Return')
            m.wait_for((out / 'small-result').exists, 'small payload executed')
            assert (out / 'small-result').read_bytes() == b'ok'
            wait_event(log, 'complete')
        elif case == 'teardown':
            closing = time.monotonic()
            s.run(['swaymsg', '[app_id="io.github.beamiter.frost"]', 'kill'])
            app.wait(timeout=5)
            assert app.returncode == 0
            entry['window_close_seconds'] = time.monotonic() - closing
            assert entry['window_close_seconds'] < 1.5, 'window close waited for transfer timeout'
            closed = wait_event(log, 'reader-closed')
            entry['request_to_pipe_close_seconds'] = closed['monotonic'] - request['monotonic']
            assert entry['request_to_pipe_close_seconds'] < 3, 'pipe closed only at read deadline'
        else:
            if case == 'revoked':
                (control / 'revoke').touch()
                revoke_ack = wait_event(log, 'revoked')
            # This short command must execute before a delayed owner responds.
            marker = case + '-responsive'
            command('printf ok > ' + marker, marker)
            observed = time.monotonic()
            entry['request_to_shell_seconds'] = observed - request['monotonic']
            if case in ('delayed', 'stalled', 'revoked'):
                assert observed - request['monotonic'] < 1.5, entry
            if case == 'delayed':
                complete = wait_event(log, 'complete')
                assert complete['monotonic'] - request['monotonic'] >= 2
                assert observed < complete['monotonic'], 'input completed only after clipboard'
            elif case == 'oversized':
                m.wait_for(lambda: any(e['phase'] in ('complete', 'reader-closed') for e in events(log)), 'oversize transfer ends', 5)
                time.sleep(0.5)
            else:
                closed = wait_event(log, 'reader-closed')
                assert closed['monotonic'] - request['monotonic'] < 5, closed
                if case == 'revoked':
                    assert closed['monotonic'] < request['monotonic'] + 2, 'old pipe survived until payload/deadline'
                    assert closed['monotonic'] - revoke_ack['monotonic'] < 1, 'revocation did not promptly close old pipe'
                if case == 'continuous':
                    assert closed['bytes'] <= 1024 * 1024 + 262144, closed
            # No rejected/revoked prefix may leak into a later shell command.
            marker = case + '-clean'
            command('printf ok > ' + marker, marker)
        entry['events'] = events(log)
        entry['elapsed_seconds'] = time.monotonic() - started
        if case != 'teardown':
            assert s.command(case + '-identity') == identity
            s.screenshot(case + '-completed')
            s.run(['swaymsg', '[app_id="io.github.beamiter.frost"]', 'kill'])
            app.wait(timeout=5)
            assert app.returncode == 0
        (control / 'stop').touch()
        proc.wait(timeout=5)
        assert proc.returncode == 0
        entry['status'] = 'pass'
    result['status'] = 'pass'
except Exception as error:
    result.update(status='fail', error=str(error))
    try:
        s.screenshot('failure')
    except Exception:
        pass
    raise
finally:
    try:
        s.close()
    finally:
        evidence_error = None
        try:
            result['owner_events'] = {path.name: events(path, complete=True) for path in out.glob('*-owner.log')}
        except Exception as log_error:
            evidence_error = log_error
            result.update(status='fail', owner_log_error=str(log_error))
        (out / 'result.json').write_text(json.dumps(result, indent=2))
        if evidence_error is not None:
            raise RuntimeError('Owner evidence parse failed after cleanup') from evidence_error
