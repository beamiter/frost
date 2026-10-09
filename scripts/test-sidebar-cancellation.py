#!/usr/bin/env python3
"""Run the actual Files worker with a deterministic cancellation-race backend.

Only the filesystem boundary is replaced. Production worker code is extracted
verbatim so this catches a late destination delete without requiring SSH/GTK.
"""
from pathlib import Path
import subprocess
import tempfile

root = Path(__file__).resolve().parent.parent
source = (root / "src/main.rs").read_text()

def item(name):
    start = source.index(name)
    brace = source.index("{", start)
    depth = 1
    end = brace + 1
    while depth:
        depth += (source[end] == "{") - (source[end] == "}")
        end += 1
    return source[start:end]

production = "\n".join(item(name) for name in (
    "struct DropItem {", "enum SidebarOp {", "struct SidebarPathRemap {",
    "struct SidebarWorkerOutcome {", "fn run_sidebar_op(",
))
# Include the former helper while running against a pre-fix checkout too.
if "fn cleanup_after_cancel(" in source:
    production += "\n" + item("fn cleanup_after_cancel(")
harness = (root / "scripts/sidebar_cancellation_harness.rs").read_text()
with tempfile.TemporaryDirectory(prefix="frost-sidebar-cancellation-") as directory:
    path = Path(directory)
    (path / "test.rs").write_text(harness + "\n" + production)
    subprocess.run(["rustc", "--edition=2021", "--test", str(path / "test.rs"),
                    "-C", "debuginfo=0", "-o", str(path / "tests")], check=True)
    subprocess.run([str(path / "tests"), "--nocapture"], check=True)
