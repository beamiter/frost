#!/usr/bin/env python3
"""Real Find widget compatibility, idle clipboard serving, and PRIMARY input."""
import argparse
import csv
import importlib.util
import io
import json
import os
from pathlib import Path
import select
import shlex
import subprocess
import time

p = argparse.ArgumentParser(description=__doc__)
for name in ('runner', 'binary', 'owner', 'pointer', 'output'):
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
result = {'status': 'running', 'cases': [], 'scope': 'actual Find text input, terminal selection/copy and middle-button PRIMARY',
          'idle_service_budget_seconds': {'poll_interval': 0.25, 'scheduler_slack': 0.5, 'total': 0.75}}


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



def reply(expected):
    deadline = time.monotonic() + 5
    data = bytearray()
    while not data.endswith(b'\n'):
        remaining = deadline - time.monotonic()
        assert remaining > 0 and select.select([pointer.stdout], [], [], remaining)[0], 'pointer timeout'
        chunk = os.read(pointer.stdout.fileno(), 1)
        assert chunk, 'pointer EOF'
        data.extend(chunk)
        assert len(data) <= 64
    assert data == (expected + '\n').encode(), data


def click(x, y, kind='click'):
    pointer.stdin.write(f'{kind} {int(x)} {int(y)} 1280 900\n'.encode())
    pointer.stdin.flush()
    reply('done')


def events(log, complete=False):
    lines = log.read_text().splitlines(keepends=True)
    if lines and not lines[-1].endswith('\n'):
        assert not complete, 'incomplete final owner event'
        lines.pop()  # A live writer may be between write and newline.
    return [json.loads(line) for line in lines if line.startswith('{')]


def owner(label, payload, selection='clipboard', scenario='finite', delay=0):
    control = out / (label + '-control')
    control.mkdir()
    body = out / (label + '-synthetic-payload')
    body.write_bytes(payload)
    proc = s.spawn([str(a.owner.resolve(strict=True)), str(control), scenario, str(delay), selection, str(body)], label + '-owner')
    m.wait_for((control / 'ready').exists, label + ' owner')
    return proc, control, out / (label + '-owner.log')


def stop(proc, control):
    (control / 'stop').touch()
    proc.wait(timeout=5)
    assert proc.returncode == 0


def words(label):
    s.screenshot(label)
    tsv = s.run(['tesseract', str(out / (label + '.png')), 'stdout', '--psm', '11', 'tsv'], text=True, capture_output=True).stdout
    (out / (label + '.tsv')).write_text(tsv)
    return [row for row in csv.DictReader(io.StringIO(tsv), delimiter='\t', quoting=csv.QUOTE_NONE) if row['text'].strip()]


def exact_output_word(rows, expected):
    # OCR may split one rendered word into adjacent boxes. Match only an entire
    # exact output line, never a prompt/command substring or approximate text.
    lines = {}
    for row in rows:
        if row['level'] == '5':
            key = tuple(row[k] for k in ('page_num', 'block_num', 'par_num', 'line_num'))
            lines.setdefault(key, []).append(row)
    found = []
    for line in lines.values():
        line.sort(key=lambda row: int(row['left']))
        if ''.join(row['text'] for row in line) != expected:
            continue
        if any(int(right['left']) - (int(left['left']) + int(left['width'])) > 12
               for left, right in zip(line, line[1:])):
            continue
        x = min(int(row['left']) for row in line)
        y = min(int(row['top']) for row in line)
        found.append({'left': x, 'top': y,
                      'width': max(int(row['left']) + int(row['width']) for row in line) - x,
                      'height': max(int(row['top']) + int(row['height']) for row in line) - y})
    return found


def external(expected, primary=False):
    # No Frost input/events are injected between idle wait and external request.
    time.sleep(1)
    start = time.monotonic()
    args = ['wl-paste', '--no-newline'] + (['--primary'] if primary else [])
    received = s.run(args, capture_output=True).stdout
    elapsed = time.monotonic() - start
    assert received == expected, {'length': len(received)}
    # Candidate worker polls every 250 ms; allow 500 ms hosted scheduling slack.
    assert elapsed < 0.75, elapsed
    return elapsed

try:
    s.start_desktop()
    s.spawn(['wtype', '-s', '600000', '-k', 'F24'], 'ui-seat-keyboard')
    log = open(out / 'pointer.log', 'w')
    s.logs.append(log)
    pointer = subprocess.Popen([str(a.pointer.resolve(strict=True))], env=s.env, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=log, bufsize=0)
    s.processes.append(pointer)
    reply('ready')
    s.config.write_text('shell = "/bin/bash"\nterminal_mode = "normal"\nrestore_session = false\n')
    app = s.spawn([str(s.binary)], 'frost')
    m.wait_for(s.app_window, 'clipboard UI window')
    time.sleep(1)
    s.text('cd ' + shlex.quote(str(out)))
    s.key('Return')
    identity = s.command('ui-ready')
    # The Find wrapper delegates all events to unmodified iced TextInput, whose
    # Ctrl+V invokes core Clipboard.read synchronously. Terminal paste is separate.
    proc, control, log = owner('widget', b'FROSTWIDGET', delay=200)
    s.key('f', ('ctrl', 'shift'))
    s.key('v', ('ctrl',))
    m.wait_for(lambda: any(e['phase'] == 'complete' for e in events(log)), 'widget clipboard transfer')
    time.sleep(0.5)
    found = [w for w in words('widget-pasted') if w['text'] == 'FROSTWIDGET']
    assert len(found) == 1, found
    # Remove the original source before copying: it must not satisfy the idle read.
    stop(proc, control)
    s.key('a', ('ctrl',))
    s.key('c', ('ctrl',))
    latency = external(b'FROSTWIDGET')
    result['cases'].append({'name': 'legacy-widget-paste-and-idle-copy', 'status': 'pass', 'external_read_seconds': latency, 'events': events(log)})
    s.key('Escape')
    assert s.command('widget-resumed') == identity
    # Selecting a real terminal word publishes PRIMARY; copying it publishes
    # CLIPBOARD. Independent wl-paste requests both while no UI input is sent.
    s.text("printf 'FROSTPRIMARY\\n'")
    s.key('Return')
    time.sleep(0.5)
    found = exact_output_word(words('primary-output'), 'FROSTPRIMARY')
    assert len(found) == 1, found
    w = found[0]
    x, y = int(w['left']) + int(w['width']) / 2, int(w['top']) + int(w['height']) / 2
    click(x, y)
    click(x, y)
    primary_latency = external(b'FROSTPRIMARY', primary=True)
    s.key('c', ('ctrl', 'shift'))
    clipboard_latency = external(b'FROSTPRIMARY')
    result['cases'].append({'name': 'idle-terminal-primary-and-copy', 'status': 'pass', 'primary_seconds': primary_latency, 'clipboard_seconds': clipboard_latency})
    proc, control, log = owner('primary-read', b'printf ok > primary-result', selection='primary')
    click(600, 600, 'middle')
    m.wait_for(lambda: any(e['phase'] == 'complete' for e in events(log)), 'PRIMARY transfer')
    exact_pending_line(b'printf ok > primary-result', 'primary-pasted-before-enter')
    s.key('Return')
    m.wait_for((out / 'primary-result').exists, 'PRIMARY pasted command executed')
    assert (out / 'primary-result').read_bytes() == b'ok'
    assert s.command('primary-resumed') == identity
    result['cases'].append({'name': 'primary-middle-paste', 'status': 'pass', 'events': events(log)})
    stop(proc, control)
    # Legacy synchronous compatibility is bounded, not claimed nonblocking.
    proc, control, log = owner('widget-stalled', b'', scenario='stalled')
    s.key('f', ('ctrl', 'shift'))
    s.key('a', ('ctrl',))
    s.key('BackSpace')
    s.key('v', ('ctrl',))
    m.wait_for(lambda: any(e['phase'] == 'reader-closed' for e in events(log)), 'bounded synchronous stalled read', 5)
    trace = events(log)
    request = next(e for e in trace if e['phase'] == 'request')
    closed = next(e for e in trace if e['phase'] == 'reader-closed')
    assert closed['monotonic'] - request['monotonic'] < 5
    s.screenshot('widget-stalled-recovered')
    start = time.monotonic()
    s.run(['swaymsg', '[app_id="io.github.beamiter.frost"]', 'kill'])
    app.wait(timeout=5)
    assert app.returncode == 0
    result['cases'].append({'name': 'legacy-stall-then-teardown', 'status': 'pass', 'window_close_seconds': time.monotonic() - start, 'events': trace})
    stop(proc, control)
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
