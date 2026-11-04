#!/usr/bin/env python3
"""Exercise the actual parser, projection, input metrics and PTY sizing together.

Uses exact release dependency fingerprints, like test-selection-admission.py.
Only window/config ownership is a fixture. Production method bodies are copied
verbatim; no native window, network connection or remote command is started.
"""
import json
import os
from pathlib import Path
import re
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parents[1]


def item(source, marker):
    start = source.index(marker)
    opening = source.index("{", start)
    depth, end = 1, opening + 1
    while depth:
        depth += (source[end] == "{") - (source[end] == "}")
        end += 1
    return source[start:end]


deps = ROOT / "target/release/deps"
fingerprints = sorted((deps.parent / ".fingerprint").glob("frost-*/bin-frost.json"),
                      key=lambda path: path.stat().st_mtime, reverse=True)
assert fingerprints, "Build Frost release with --locked first"
app = json.loads(fingerprints[0].read_text())
libraries = []
for _, name, _, identity in app["deps"]:
    matches = []
    for library in deps.glob("lib" + name + "-*.rlib"):
        suffix = library.stem.split("-")[-1]
        for fp in (deps.parent / ".fingerprint").glob("*-" + suffix + "/lib-" + name + ".json"):
            checksum = fp.with_suffix("")
            if (checksum.exists() and json.loads(fp.read_text()).get("rustc") == app["rustc"]
                    and int.from_bytes(bytes.fromhex(checksum.read_text()), "little") == identity):
                matches.append(library)
    assert len(matches) == 1, f"Missing or ambiguous production dependency: {name}"
    libraries += ["--extern", name + "=" + str(matches[0])]

terminal = (ROOT / "src/terminal.rs").read_text()
view = (ROOT / "src/terminal_view.rs").read_text()
surface = (ROOT / "src/native_input_surface.rs").read_text()
native_shell = (ROOT / "src/native_shell.rs").read_text()
modules = ["ansi", "block_mode", "chat_mode", "debug", "fold_preview", "immersive_command",
           "kitty_graphics", "link", "organism_activity", "organism_watch", "review_text", "search",
           "glass_shell_tokens", "glass_layout"]
code = "#![allow(dead_code)]\npub(crate) use jterm_core::char_width;\n"
for module in modules:
    code += f"#[path = {json.dumps(str(ROOT / 'src' / (module + '.rs')))}] mod {module};\n"
code += "use terminal::clamp_terminal_dimensions;\n"
for name in ["SCROLLBAR_WIDTH", "BLOCK_GUTTER_WIDTH"]:
    code += re.search(r"^(?:pub )?const " + name + r": [^;]+;", view, re.MULTILINE).group() + "\n"
code += "mod terminal_view { pub const SCROLLBAR_WIDTH: f32 = super::SCROLLBAR_WIDTH; }\n"
code += "#[derive(Clone, Copy, Debug)]\n" + item(view, "pub struct Metrics {")
code += "impl Metrics {\n" + "\n".join(item(view, marker) for marker in [
    "pub fn new(", "fn block_gutter_width(", "pub fn grid_size("]) + "\n}\n"
code += "mod native_input_surface {\n"
code += "pub const INPUT_STRIP_HEIGHT: f32 = crate::glass_layout::INPUT_STRIP_HEIGHT;\n"
code += "#[derive(Clone, Copy, Debug, PartialEq)]\n"
code += item(surface, "pub(crate) struct DockMetrics {") + "\n"
code += item(surface, "pub(crate) fn dock_metrics(") + "\n}\n"
code += "#[derive(Clone, Copy, Debug)]\n" + item(native_shell, "pub(crate) struct InputDockHold {") + "\n"
input_methods = "\n".join(item(native_shell, marker) for marker in [
    "pub(crate) fn reference_input_metrics(", "fn held_input_metrics(",
    "fn input_dock_hold_eligible(", "pub(crate) fn sync_input_dock_hold(",
    "pub(crate) fn expire_input_dock_hold(", "fn live_input_metrics("])
code += (ROOT / "scripts/input-geometry-harness.rs").read_text().replace(
    "//!", "//").replace("// @input-metrics@", input_methods)

with tempfile.TemporaryDirectory(prefix="frost-input-geometry-") as directory:
    out = Path(directory)
    (out / "terminal.rs").write_text(terminal.replace("mod chat_projection;",
        "#[path = " + json.dumps(str(ROOT / "src/terminal/chat_projection.rs")) + "] mod chat_projection;", 1))
    code += "\n#[path = " + json.dumps(str(out / "terminal.rs")) + "] mod terminal;\n"
    source = out / "harness.rs"
    source.write_text(code)
    binary = out / "input-geometry"
    command = [os.environ.get("RUSTC", "rustc"), "--edition=2021", "-C", "opt-level=0",
               "-C", "debug-assertions=on", "-L", "dependency=" + str(deps), *libraries,
               str(source), "-o", str(binary)]
    subprocess.run(command, check=True)
    subprocess.run([str(binary)], check=True)
    # Removing only the prospective-size check must bring back the feedback
    # loop with the same parser, resize code, fixtures and assertions.
    guard = "(rows.saturating_add(2) < native_rows).then_some(metrics)"
    assert code.count(guard) == 1
    source.write_text(code.replace(guard, "{ let _ = native_rows; Some(metrics) }"))
    subprocess.run(command, check=True)
    failed = subprocess.run([str(binary)], capture_output=True, text=True)
    assert failed.returncode != 0 and "input/PTY resize oscillation" in failed.stderr, failed.stderr
    print("PASS: removing the fix reproduces the input/PTY resize oscillation")
    # Removing only the composer-row floor must bring back the one-row bash
    # prompt loop: one row on the last PTY row, two rows one row higher.
    floor = "\n            .max(terminal::CHAT_MIN_INPUT_ROWS);"
    assert code.count(floor) == 1
    source.write_text(code.replace(floor, ";"))
    subprocess.run(command, check=True)
    failed = subprocess.run([str(binary)], capture_output=True, text=True)
    assert failed.returncode != 0 and "input/PTY resize oscillation" in failed.stderr, failed.stderr
    print("PASS: removing the row floor reproduces the one-row prompt oscillation")
    # Without the hold, every Enter collapses the input and resizes the PTY.
    hold = "self.live_input_metrics()\n            .or_else(|| self.held_input_metrics())"
    assert code.count(hold) == 1
    source.write_text(code.replace(hold, "self.live_input_metrics()"))
    subprocess.run(command, check=True)
    failed = subprocess.run([str(binary)], capture_output=True, text=True)
    assert failed.returncode != 0 and "command start resized the PTY" in failed.stderr, failed.stderr
    print("PASS: removing the hold brings back the per-command input collapse")
