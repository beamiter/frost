#!/usr/bin/env python3
"""Hosted, disposable-compositor acceptance of the exact native provider graph."""
import argparse
import hashlib
import importlib.util
import json
import os
from pathlib import Path
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
s.env.pop('WAYLAND_SOCKET', None)
result = {'status': 'running', 'cases': [], 'binary_sha256': hashlib.sha256(a.binary.read_bytes()).hexdigest()}
log = out / 'provider.log'
app = None
owners = []
owner_logs = []


def events(path=log, complete=False):
    if not path.exists():
        return []
    lines = path.read_text().splitlines(keepends=True)
    if complete and lines:
        assert lines[-1].endswith('\n'), 'truncated final event log: ' + str(path)
    return [json.loads(line) for line in lines if line.startswith('{') and line.endswith('\n')]


def wait(phase, ident=None, path=log, timeout=5):
    return m.wait_for(lambda: next((e for e in events(path) if e['phase'] == phase and (ident is None or e['id'] == ident)), None), phase + ' ' + str(ident), timeout)


def send(command):
    assert app.poll() is None, 'provider exited early'
    app.stdin.write(command + '\n')
    app.stdin.flush()


def own(name, payload=b'', scenario='finite', selection='clipboard'):
    control = out / (name + '-control')
    control.mkdir()
    body = out / (name + '-synthetic')
    body.write_bytes(payload)
    proc = s.spawn([str(a.owner.resolve()), str(control), scenario, '0', selection, str(body)], name)
    owners.append((proc, control))
    m.wait_for((control / 'ready').exists, name + ' ready')
    # Selection propagation; correctness is subsequently checked by request/completion evidence.
    time.sleep(.3)
    owner_logs.append(out / (name + '.log'))
    return owner_logs[-1]


def stop_owners():
    for proc, control in owners:
        (control / 'stop').touch()
        proc.wait(timeout=3)
        assert proc.returncode == 0
    owners.clear()


def case(name, **data):
    result['cases'].append(dict(name=name, status='pass', **data))


def fnv(data):
    n = 14695981039346656037
    for b in data:
        n = ((n ^ b) * 1099511628211) & ((1 << 64) - 1)
    return '%016x' % n


def read(ident, expected=None, primary=False, sync=False):
    send(('sync' if sync else 'read') + ' %d %s' % (ident, 'primary' if primary else 'clipboard'))
    if not sync:
        assert wait('admit', ident)['ok']
    r = wait('sync' if sync else 'done', ident)
    if expected is None:
        assert not r['ok'], r
    else:
        assert r['ok'] and r['len'] == len(expected) and r['digest'] == fnv(expected), r
    return r


def cpu_ticks():
    # /proc/stat comm may contain spaces; split only after its final closing parenthesis.
    fields = Path('/proc/%d/stat' % app.pid).read_text().rsplit(')', 1)[1].split()
    return int(fields[11]) + int(fields[12])

try:
    s.start_desktop()
    s.spawn(['wtype', '-s', '600000', '-k', 'F24'], 'provider-seat-keyboard')
    stream = log.open('w')
    s.logs.append(stream)
    app = subprocess.Popen([str(a.binary.resolve())], env=s.env, stdin=subprocess.PIPE, stdout=stream, stderr=stream, text=True)
    s.processes.append(app)
    wait('focused')
    s.screenshot('real-provider-host')
    for base in (10, 30):
        cliplog = own('batch-%d-clipboard' % base, scenario='stalled')
        primarylog = own('batch-%d-primary' % base, scenario='stalled', selection='primary')
        start = time.monotonic()
        send('batch %d' % base)
        admissions = [wait('admit', base+i) for i in range(9)]
        assert all(e['ok'] for e in admissions[:8]), admissions
        assert not admissions[8]['ok'] and admissions[8]['kind'] == 'WouldBlock', admissions
        assert admissions[-1]['ms'] - admissions[0]['ms'] < 100, 'admission blocked GUI callback'
        wait('request', 4, path=cliplog)
        wait('request', 4, path=primarylog)
        send('cancel')
        completions = [wait('done', base+i) for i in range(8)]
        assert all(not e['ok'] for e in completions), completions
        assert time.monotonic() - start < 2, 'cancellation waited for deadline'
        time.sleep(.1)  # callback-return permit release precedes the next user-event batch
        assert not any(e['phase'] == 'done' and e['id'] == base+8 for e in events())
        for ownerlog in (cliplog, primarylog):
            for ident in range(1, 5):
                request = wait('request', ident, path=ownerlog)
                closed = wait('reader-closed', ident, path=ownerlog)
                assert closed['monotonic'] - request['monotonic'] < 2, 'cancel leaked offered pipe until timeout'
        case('admission-cancel-reuse-%d' % base, admissions=admissions, completions=completions)
        stop_owners()
    for ident, name, payload, expected, primary in [
        (50, 'healthy-after-cancel', b'healthy', b'healthy', False),
        (51, 'exact-cap', b'x' * (1024*1024), b'x' * (1024*1024), False),
        (52, 'lossy-expanded-over-cap', b'\xff' * 349526, None, False),
        (53, 'normalization', b'a\r\nb\rc', b'a\nb\nc', False),
        (54, 'primary', b'primary', b'primary', True),
        (55, 'cap-plus-one', b'x' * (1024*1024+1), None, False),
        (56, 'lossy-within-cap', b'\xff', '\ufffd'.encode(), False),
    ]:
        ownerlog = own(name, payload, selection='primary' if primary else 'clipboard')
        completion = read(ident, expected, primary)
        wait('request', path=ownerlog)
        case(name, completion=completion)
        stop_owners()
    # Owned selection service during a truly quiet host: no stdin/user events between
    # write ACK and external requests; winit ControlFlow::Wait remains in effect.
    send('write')
    wait('write')
    time.sleep(.5)
    before=cpu_ticks(); start=time.monotonic(); time.sleep(3)
    elapsed=time.monotonic()-start
    after=cpu_ticks(); hz=os.sysconf('SC_CLK_TCK')
    cpu=(after-before)/hz/elapsed
    assert cpu < .10, 'quiet provider consumed >=10% of one CPU'
    latencies=[]
    for primary in (False, True):
        start=time.monotonic()
        cmd=['wl-paste','--no-newline'] + (['--primary'] if primary else [])
        got=subprocess.run(cmd,env=s.env,stdout=subprocess.PIPE,stderr=subprocess.PIPE,check=True,timeout=2).stdout
        latency=time.monotonic()-start
        assert got == (b'owned-primary' if primary else b'owned-clipboard')
        assert latency < .75, '250ms drain + 500ms hosted scheduler tolerance exceeded'
        latencies.append(latency)
    case('idle-owned-selections', quiet_seconds=elapsed, cpu_ticks_before=before, cpu_ticks_after=after, ticks_per_second=hz, cpu_fraction=cpu, service_seconds=latencies)
    for ident, primary in ((60,False),(61,True)):
        ownerlog = own('sync-%d' % ident, b'synchronous', selection='primary' if primary else 'clipboard')
        case('sync-%d' % ident, completion=read(ident,b'synchronous',primary,sync=True))
        wait('request', path=ownerlog)
        stop_owners()
    ownerlog = own('sync-stalled', scenario='stalled')
    start = time.monotonic()
    completion = read(62,sync=True)
    elapsed=time.monotonic()-start
    assert 2.5 <= elapsed < 4, elapsed
    request = wait('request', path=ownerlog)
    case('sync-stalled-bounded', completion=completion, seconds=elapsed)
    start=time.monotonic(); send('quit'); app.wait(timeout=2)
    assert app.returncode == 0 and time.monotonic()-start < 1.5
    wait('exit')
    case('sync-immediate-drop',seconds=time.monotonic()-start)
    closed = wait('reader-closed', path=ownerlog)
    assert 2.5 <= closed['monotonic'] - request['monotonic'] < 4
    stop_owners()
    # Separate fresh real host covers teardown with an active offered pipe.
    focused_before=sum(e['phase']=='focused' for e in events())
    app = subprocess.Popen([str(a.binary.resolve())], env=s.env, stdin=subprocess.PIPE, stdout=stream, stderr=stream, text=True)
    s.processes.append(app)
    m.wait_for(lambda: sum(e['phase']=='focused' for e in events()) > focused_before, 'second real host focus')
    ownerlog=own('pending-drop',scenario='stalled')
    send('read 70 clipboard'); assert wait('admit',70)['ok']; wait('request',path=ownerlog)
    start=time.monotonic(); send('quit'); app.wait(timeout=2)
    assert app.returncode == 0
    assert time.monotonic()-start < 1.5
    assert sum(e['phase']=='exit' for e in events()) == 2
    wait('reader-closed',path=ownerlog)
    case('pending-drop',seconds=time.monotonic()-start)
    stop_owners()
    # Every admitted non-teardown request completes exactly once, never rejected IDs.
    final=events()
    for e in [e for e in final if e['phase']=='admit' and e['id']!=70]:
        count=sum(x['phase']=='done' and x['id']==e['id'] for x in final)
        assert count == int(e['ok']), (e,count)
    result['status']='pass'
except Exception as error:
    result.update(status='fail',error=str(error))
    raise
finally:
    try:
        s.close()
    finally:
        evidence_error = None
        try:
            result['timeline'] = events(complete=True)
            result['owner_events'] = {p.name: events(p, complete=True) for p in owner_logs}
        except Exception as error:
            evidence_error = error
            result.update(status='fail', evidence_error=str(error))
        (out/'result.json').write_text(json.dumps(result,indent=2)+'\n')
        if evidence_error:
            raise evidence_error
