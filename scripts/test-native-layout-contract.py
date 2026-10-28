#!/usr/bin/env python3
"""Run actual native body and viewport contracts in the debug Frost target.

No source extraction, archived presenter or arbitrary newest-rlib selection.
--deps is retained as a deprecated command-line compatibility argument; Cargo
selects the actual locked runtime dependency graph instead.
"""
import argparse
import hashlib
import json
from pathlib import Path
import subprocess

from native_contract_runner import ROOT, run

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--deps', type=Path, help='Deprecated: dependencies are resolved by locked Cargo')
parser.add_argument('--output', type=Path, help='Optional evidence report prefix')
args = parser.parse_args()
paths = ['src/main.rs', 'src/native_companion.rs', 'src/native_companion_tests.rs',
         'src/native_viewport.rs', 'src/terminal_view.rs']
before = {path: hashlib.sha256((ROOT / path).read_bytes()).hexdigest() for path in paths}
run(['native_companion_contracts', 'native_viewport::tests'])
after = {path: hashlib.sha256((ROOT / path).read_bytes()).hexdigest() for path in paths}
assert before == after, 'Production contract source changed during the test run'
if args.output:
    report = args.output.with_suffix('.source.json')
    report.parent.mkdir(parents=True, exist_ok=True)
    report.write_text(json.dumps({
        'source_sha256': after,
        'source_head': subprocess.check_output(['git', 'rev-parse', 'HEAD'], cwd=ROOT, text=True).strip(),
        'assertion_policy': 'Actual TermWidget owner/clip/event and native allocator assertions; debug all-features target',
        'native_pixel_font_zoom': 'separate gate; not certified by this component runner',
    }, indent=2) + '\n')
print('actual native body and viewport contracts passed')
