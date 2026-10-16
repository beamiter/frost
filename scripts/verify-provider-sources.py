#!/usr/bin/env python3
"""Fail closed if the acceptance fixture no longer tests the reviewed providers."""
import hashlib
import json
from pathlib import Path
import tomllib
root = Path(__file__).resolve().parent.parent
manifest = json.loads((root / 'scripts/provider-acceptance/source-manifest.json').read_text())
for record in manifest['files']:
    path = root / record['path']
    assert hashlib.sha256(path.read_bytes()).hexdigest() == record['sha256'], record['path']
# Fixture dependencies are an exact subset of the production locked package identities.
def identities(path):
    return {(p['name'], p['version'], p.get('source'), p.get('checksum')) for p in tomllib.loads(path.read_text())['package']}
extra = identities(root/'scripts/provider-acceptance/Cargo.lock') - identities(root/'Cargo.lock')
assert extra == {('frost-provider-acceptance', '0.0.0', None, None)}, extra
print(json.dumps({'status':'pass','source_files':len(manifest['files']),'lock_identity':'production subset plus fixture only'}))
