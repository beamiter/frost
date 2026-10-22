#!/usr/bin/env python3
"""Check the committed font graph without downloading/building dependencies."""
from pathlib import Path
import sys
import tomllib

root = Path(__file__).resolve().parents[1]
lock = tomllib.loads((Path(sys.argv[1]) if len(sys.argv) > 1 else root / "Cargo.lock").read_text())
packages = lock["package"]
shapers = [p for p in packages if p["name"] == "cosmic-text"]
assert len(shapers) == 1, f"measurement/rendering shaper split: {[p['version'] for p in shapers]}"
shaper = shapers[0]
manifest = tomllib.loads((root / "Cargo.toml").read_text())
assert manifest["dependencies"]["cosmic-text"] == "=" + shaper["version"]
for name in ("frost", "iced_graphics", "iced_tiny_skia", "cryoglyph"):
    package, = [p for p in packages if p["name"] == name]
    edges = [d for d in package["dependencies"] if d.split()[0] == "cosmic-text"]
    assert edges in (["cosmic-text"], ["cosmic-text " + shaper["version"]]), (name, edges)
# Every lockfile edge must resolve uniquely, including after pruning old shapers.
for package in packages:
    for dependency in package.get("dependencies", []):
        parts = dependency.split()
        matches = [p for p in packages if p["name"] == parts[0]
                   and (len(parts) == 1 or p["version"] == parts[1])]
        assert len(matches) == 1, (package["name"], dependency)
print("font-stack lock graph OK: cosmic-text " + shaper["version"])
