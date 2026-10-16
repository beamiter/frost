#!/usr/bin/env python3
"""Execute production OSC52 callback/owner gates with synthetic clipboard/PTY sinks.

No OS clipboard or PTY is opened. Terminal encoding and platform scheduling are
explicit test doubles; native cargo tests separately exercise real encoding.
"""
import os
from pathlib import Path
import subprocess
import tempfile

root = Path(__file__).resolve().parents[1]
source = (root / 'src/main.rs').read_text().split('#[cfg(test)]\nmod tests {')[0]
terminal = (root / 'src/terminal.rs').read_text()

def item(source, marker):
    start = source.index(marker)
    brace = source.index('{', start)
    depth, end = 1, brace + 1
    while depth:
        depth += (source[end] == '{') - (source[end] == '}')
        end += 1
    return source[start:end]

helpers = '\n'.join(item(source, 'fn ' + name + '(') for name in (
    'defer_osc52_refusal', 'complete_osc52_read'))
methods = '\n'.join(item(source, 'fn ' + name + '(') for name in (
    'host_clipboard_read_blocked_for', 'try_begin_host_clipboard_read',
    'finish_host_clipboard_read', 'release_host_clipboard_read', 'session_by_identity'))
callback = item(source, 'Message::Osc52Query(id, fd, terminator, content) =>')
cap = source.split('const MAX_CLIPBOARD_RESPONSE_BYTES:', 1)[1].split(';', 1)[0]
query = item(terminal, 'if data == "?"')
# Bind the actual async message producer and close path to the tested gates.
assert '.map(move |c| Message::Osc52Query(id, fd, terminator, c))' in source
close = item(source, 'fn close_session(')
assert 'self.sessions.remove(index)' in close
assert 'self.release_host_clipboard_read(closed_id, closed_fd)' in close
assert 'self.next_id =' not in source  # runtime IDs never reset/reuse
assert source.count('self.next_id += 1;') == 8
for body in (helpers, methods, callback, query):
    assert 'log::' not in body and 'debug_log!' not in body
fixture = (root / 'scripts/osc52_ownership_harness.rs').read_text()
fixture = fixture.replace('// HELPERS', helpers).replace('// METHODS', methods)
fixture = fixture.replace('// CALLBACK', callback).replace('// QUERY', query)
fixture = fixture.replace('// CAP', 'const MAX_CLIPBOARD_RESPONSE_BYTES:' + cap + ';')
variants = [('current', fixture),
    ('fd-only regression', fixture.replace('session.id == id && session.master_fd == fd', 'session.master_fd == fd')),
    ('revocation regression', fixture.replace('if allow_clipboard_read && content.len()', 'if content.len()')),
    ('size regression', fixture.replace('content.len() <= MAX_CLIPBOARD_RESPONSE_BYTES', 'true')),
    ('owner-release regression', fixture.replace('owner_id == id && owner_fd == fd', 'true'))]
with tempfile.TemporaryDirectory(prefix='frost-osc52-') as temp:
    path = Path(temp)
    for name, code in variants:
        (path / 'test.rs').write_text(code)
        subprocess.run([os.environ.get('RUSTC', 'rustc'), '--edition=2021', '--test',
                        str(path / 'test.rs'), '-o', str(path / 'tests')], check=True)
        result = subprocess.run([str(path / 'tests')], capture_output=True, text=True)
        print(name + ':\n' + result.stdout)
        assert (result.returncode == 0) == (name == 'current'), result.stderr
print('Production OSC52 ownership checks passed; four deliberate regressions rejected.')
