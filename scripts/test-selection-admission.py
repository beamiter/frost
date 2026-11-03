#!/usr/bin/env python3
"""Run production selection admission and a failing pre-fix control.

Uses the exact locked release dependency identities in Frost's Cargo fingerprint.
Compiles the actual terminal modules without cfg(test), avoiding unrelated legacy
GUI fixtures. No parser, planner, selection or storage implementation is mocked.
"""
import argparse
import json
import os
from pathlib import Path
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parents[1]
parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--deps', type=Path, default=ROOT / 'target/release/deps')
args = parser.parse_args()
deps = args.deps.resolve()
fingerprints = sorted((deps.parent / '.fingerprint').glob('frost-*/bin-frost.json'),
                      key=lambda path: path.stat().st_mtime, reverse=True)
assert fingerprints, 'Build Frost release with --locked first'
app = json.loads(fingerprints[0].read_text())
libraries = []
for _, name, _, identity in app['deps']:
    matches = []
    for library in deps.glob('lib' + name + '-*.rlib'):
        suffix = library.stem.split('-')[-1]
        for fp in (deps.parent / '.fingerprint').glob('*-' + suffix + '/lib-' + name + '.json'):
            checksum = fp.with_suffix('')
            if (checksum.exists() and json.loads(fp.read_text()).get('rustc') == app['rustc']
                    and int.from_bytes(bytes.fromhex(checksum.read_text()), 'little') == identity):
                matches.append(library)
    assert len(matches) == 1, f'Missing or ambiguous production dependency: {name}; rebuild release'
    libraries += ['--extern', name + '=' + str(matches[0])]

with tempfile.TemporaryDirectory(prefix='frost-selection-admission-') as tmp:
    out = Path(tmp)
    source = ROOT / 'scripts/selection-admission-harness.rs'
    terminal = (ROOT / 'src/terminal.rs').read_text()
    # Rust's #[path] root changes implicit submodule lookup. Redirect only the
    # module declaration, keeping all production types and method bodies exact.
    fixed_terminal = terminal.replace('mod chat_projection;',
        '#[path = ' + json.dumps(str(ROOT / 'src/terminal/chat_projection.rs')) + '] mod chat_projection;', 1)
    (out / 'terminal.rs').write_text(fixed_terminal)
    harness = source.read_text().replace('../src/', str(ROOT / 'src') + '/')
    harness = harness.replace(str(ROOT / 'src/terminal.rs'), str(out / 'terminal.rs'))
    (out / 'harness.rs').write_text(harness)
    command = [os.environ.get('RUSTC', 'rustc'), '--edition=2021', '-C', 'opt-level=0',
               '-C', 'debug-assertions=off', '-L', 'dependency=' + str(deps), *libraries]
    subprocess.run([*command, str(out / 'harness.rs'), '-o', str(out / 'selection')], check=True)
    subprocess.run([str(out / 'selection')], check=True)

    # Roll back only the bug fix in a temporary source copy. The same parser,
    # assertions and dependency graph must now reject the legitimate Block drag.
    (out / 'terminal').mkdir()
    (out / 'terminal.rs').write_text(terminal.replace('mod chat_projection;',
        '#[path = ' + json.dumps(str(out / 'terminal/chat_projection.rs')) + '] mod chat_projection;', 1))
    chat = (ROOT / 'src/terminal/chat_projection.rs').read_text()
    fix = '        policy.command_bubbles = key.command_bubbles;\n'
    assert chat.count(fix) == 1
    (out / 'terminal/chat_projection.rs').write_text(chat.replace(fix, ''))
    subprocess.run([*command, str(out / 'harness.rs'), '-o', str(out / 'control')], check=True)
    failed = subprocess.run([str(out / 'control')], capture_output=True, text=True)
    assert failed.returncode != 0 and 'drag, bubbles=false' in failed.stderr, failed.stderr
    print('PASS: removing the fix reproduces the Block drag failure')
