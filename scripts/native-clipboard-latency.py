#!/usr/bin/env python3
"""Characterize production X11 clipboard/UI latency with synthetic owners.
Always reports measured values. Passing this harness is not latency acceptance.
"""
import argparse
import importlib.util
import json
from pathlib import Path
import shlex
import time

p = argparse.ArgumentParser(description=__doc__)
p.add_argument('--runner', type=Path, required=True)
p.add_argument('--owner', type=Path, required=True)
p.add_argument('--binary', type=Path, required=True)
p.add_argument('--output', type=Path, required=True)
a = p.parse_args()
spec = importlib.util.spec_from_file_location('native_smoke', a.runner)
m = importlib.util.module_from_spec(spec)
spec.loader.exec_module(m)
out = a.output.resolve()
out.mkdir(parents=True, exist_ok=False)
s = m.Smoke(a.binary.resolve(strict=True), 'x11', out)
result = {'status': 'running', 'measurements': [], 'latency_acceptance': 'not established'}
try:
    s.start_desktop()
    s.config.write_text('shell = "/bin/bash"\nterminal_mode = "normal"\nrestore_session = false\n')
    app = s.spawn([str(s.binary)], 'frost')
    m.wait_for(s.app_window, 'Frost clipboard fixture')
    time.sleep(2)
    s.command('ready')
    for index, delay in enumerate((0, 2)):
        ready, log = out / f'owner-{index}.ready', out / f'owner-{index}.jsonl'
        owner = s.spawn(['python3', str(a.owner.resolve()), '--delay', str(delay), '--ready', str(ready), '--log', str(log)], f'owner-{index}')
        m.wait_for(ready.exists, 'synthetic clipboard owner')
        marker = out / f'input-{index}.txt'
        # Read completes with an empty string, so queued input remains a standalone command.
        start = time.monotonic()
        s.key('v', ('ctrl', 'shift'))
        injection = time.monotonic()
        command = "printf 'received\\n' > " + shlex.quote(str(marker))
        s.run(['xdotool', 'type', '--clearmodifiers', '--delay', '0', '--', command])
        s.key('Return')
        m.wait_for(marker.exists, 'input after clipboard request', timeout=10)
        observed = time.monotonic()
        assert marker.read_text() == 'received\n'
        events = [json.loads(line) for line in log.read_text().splitlines()]
        replies = [e for e in events if e.get('phase') == 'reply' and e.get('accepted') and e.get('text_transfer') and start <= e.get('started_monotonic', 0) <= observed and e.get('completed_monotonic', 0) - e.get('started_monotonic', 0) >= delay]
        assert replies, events
        for reply in replies:
            assert any(request.get('phase') == 'request' and request.get('request_id') == reply['request_id'] and request.get('text_transfer') for request in events)
        result['measurements'].append({'provider_delay_seconds': delay, 'trigger_to_shell_seconds': observed - start,
                                       'queued_input_to_shell_seconds': observed - injection, 'events': events})
        s.screenshot(f'clipboard-{index}')
        owner.terminate()
        owner.wait(timeout=5)
    result['status'] = 'characterized'
    result['slow_minus_fast_seconds'] = result['measurements'][1]['queued_input_to_shell_seconds'] - result['measurements'][0]['queued_input_to_shell_seconds']
except Exception as error:
    result.update(status='failed', error=str(error))
    result['provider_events_on_failure'] = {
        path.name: [json.loads(line) for line in path.read_text().splitlines()]
        for path in out.glob('owner-*.jsonl')
    }
    try:
        s.screenshot('clipboard-failure')
    except Exception as screenshot_error:
        result['screenshot_error'] = str(screenshot_error)
    raise
finally:
    s.close()
    (out / 'result.json').write_text(json.dumps(result, indent=2))
