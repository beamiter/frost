#!/usr/bin/env python3
"""Guard frontend adapter wiring; behavioral/race tests live in jterm_core."""
from pathlib import Path
root = Path(__file__).resolve().parents[1]
source = (root / "src/vim9_ide.rs").read_text()
section = source[source.index("struct RenderSubscription"):source.index("/// Preserve the pre-existing")]
assert "watch_render()" in section
assert "updates.changed().await" in section
assert "interval_ms" not in section
assert "tokio::time::sleep" not in section
assert "editor.render_snapshot()" not in section
assert "accepts_render_entry(active_entry, entry_id, self.child_exited)" in source
print("render wake adapter wiring passed")
