#!/usr/bin/env python3
"""Run a standalone Cargo workspace without inheriting the checkout's patches.

Cargo discovers .cargo/config.toml from its working directory, even when
--manifest-path points outside it. Preserve Cargo's normal user configuration
and lock enforcement while excluding the host checkout's integration patches.
"""
import argparse
from pathlib import Path
import subprocess
import tempfile


def run_cargo(manifest, arguments, **kwargs):
    manifest = Path(manifest).resolve(strict=True)
    if not arguments or arguments[0].startswith('-'):
        raise ValueError('A Cargo subcommand is required')
    command = ['cargo', arguments[0], '--manifest-path', str(manifest), *arguments[1:]]
    with tempfile.TemporaryDirectory(prefix='frost-isolated-cargo-') as working_directory:
        return subprocess.run(command, cwd=working_directory, **kwargs)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--manifest-path', type=Path, required=True)
    parser.add_argument('cargo_arguments', nargs=argparse.REMAINDER)
    arguments = parser.parse_args()
    if not arguments.cargo_arguments:
        parser.error('A Cargo subcommand is required')
    return run_cargo(arguments.manifest_path, arguments.cargo_arguments).returncode


if __name__ == '__main__':
    raise SystemExit(main())
