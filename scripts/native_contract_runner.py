"""Run current Frost contracts in the real debug test target.

The native layout entry point uses the runtime crate and actual native widgets.
The separate font scripts retain their backend-free CI build budget.
They do not certify native pixel typography or replace display-backed QA.
"""
import os
from pathlib import Path
import subprocess

ROOT = Path(__file__).resolve().parents[1]


def run(filters):
    cargo = os.environ.get('CARGO', 'cargo')
    env = os.environ.copy()
    env['RUSTUP_AUTO_INSTALL'] = '0'
    for name in filters:
        subprocess.run([
            cargo, 'test', '--manifest-path', str(ROOT / 'Cargo.toml'),
            '--bin', 'frost', '--all-features', '--locked', '--offline',
            '--jobs', '2', name, '--', '--test-threads=2',
        ], env=env, check=True)
