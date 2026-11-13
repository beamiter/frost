#!/usr/bin/env python3
"""Hash exactly one test executable reported by a successful Cargo JSON build.

Do not infer Cargo's internal target directory layout or collect stale binaries.
The expected manifest and target bind evidence to the reviewed provider fixture.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path


def record(messages, manifest, target):
    expected = Path(manifest).resolve(strict=True)
    artifacts = set()
    finished = []
    for line in messages.splitlines():
        message = json.loads(line)
        if message.get('reason') == 'build-finished':
            finished.append(message.get('success'))
        if (message.get('reason') == 'compiler-artifact'
                and message.get('profile', {}).get('test') is True
                and message.get('target', {}).get('name') == target
                and message.get('target', {}).get('kind') == ['lib']
                and Path(message.get('manifest_path', '')).resolve() == expected
                and message.get('executable')):
            artifacts.add(Path(message['executable']).resolve(strict=True))
    if finished != [True] or len(artifacts) != 1:
        raise ValueError('Expected one executable from one successful Cargo test build')
    binary = artifacts.pop()
    if not binary.is_file() or not os.access(binary, os.X_OK):
        raise ValueError('Reported test artifact is not an executable file')
    # sha256sum's unescaped output format cannot represent newlines/backslashes.
    if any(c in str(binary) for c in '\n\r\\'):
        raise ValueError('Unsupported checksum path')
    with binary.open('rb') as source:
        digest = hashlib.file_digest(source, 'sha256').hexdigest()
    return f'{digest}  {binary}\n'


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--messages', type=Path, required=True)
    parser.add_argument('--manifest', type=Path, required=True)
    parser.add_argument('--target', required=True)
    args = parser.parse_args()
    print(record(args.messages.read_text(), args.manifest, args.target), end='')


if __name__ == '__main__':
    main()
