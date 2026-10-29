#!/usr/bin/env python3
"""Run actual Files follow callback regressions with Python 3 and rustc only.

Production functions are extracted from the current checkout on every invocation.
The companion Rust template supplies deterministic UI/transport boundaries. No
Cargo build, cached dependencies, GTK display, network, or SSH server is needed.
"""

from pathlib import Path
import hashlib
import json
import os
import re
import subprocess
import sys
import tempfile
import textwrap


ROOT = Path(__file__).resolve().parents[1]


def item(source, signature):
    """Extract a rustfmt-formatted braced item, preserving its body verbatim."""
    if source.count(signature) != 1:
        raise ValueError(f"expected one production item: {signature}")
    start = source.index(signature)
    line_start = source.rfind("\n", 0, start) + 1
    indentation = source[line_start:start]
    if indentation.strip():
        raise ValueError(f"production item must begin a source line: {signature}")
    opening = source.index("{", start)
    # An item's closing brace occupies its own line at its declaration's indent.
    # This avoids treating braces inside Rust strings or comments as delimiters.
    closing = re.search(r"^" + re.escape(indentation) + r"}[ \t]*$", source[opening:], re.M)
    if closing is None:
        raise ValueError(f"cannot find production item end: {signature}")
    return textwrap.dedent(source[line_start:opening + closing.end()])


def between(source, start_marker, end_marker):
    if source.count(start_marker) != 1 or source.count(end_marker) != 1:
        raise ValueError(f"production boundary changed: {start_marker}")
    start = source.index(start_marker)
    end = source.index(end_marker, start)
    return source[start:end].rstrip()


def drawer_decisions(source):
    """Keep the three actual reducer visibility assignments verbatim.

    Other reducer side effects (focus/tasks/PTY) stay native QA concerns; the
    fixture exercises these exact decisions against the real glass Layout.
    """
    cases = {
        "toggle": ("            Message::ReferenceInspectorToggle => {", "let show ="),
        "tab": ("            Message::ReferenceInspector(tab) => {", "self.reference_inspector_tab ="),
        "tasks": ("    fn set_task_panel_open(", "self.reference_inspector_open ="),
    }
    result = {}
    for name, (marker, first) in cases.items():
        start = source.index(marker)
        start = source.index(first, start)
        start = source.rfind("\n", 0, start) + 1
        stop = source.index("self.reference_inspector_drawer =", start)
        stop = source.index(";", stop) + 1
        statement = textwrap.dedent(source[start:stop])
        assert statement.count("self.reference_layout().inspector.is_none()") == 1, name
        assert "jterm_core::reference_shell::Layout" not in statement, name
        result[name] = statement
    return result


def build_harness():
    source = (ROOT / "src/main.rs").read_text(encoding="utf-8")
    shell = (ROOT / "src/native_shell.rs").read_text(encoding="utf-8")
    sidebar = (ROOT / "src/sidebar.rs").read_text(encoding="utf-8")
    remote = (ROOT / "src/remote_fs.rs").read_text(encoding="utf-8")
    template = (ROOT / "scripts/files_follow_harness.rs").read_text(encoding="utf-8")
    assert 'use crate::glass_profile::{self as shell, color};' in shell
    helpers = [
        "fn probe_sidebar_remote_follow(",
        "fn sidebar_remote_follow_tree_is_current(",
        "fn sidebar_remote_follow_context_is_current(",
        "fn next_sidebar_context_epoch(",
        "fn suppress_armed_sidebar_retry(",
    ]
    methods = [
        "fn reference_inspector_visible(",
        "fn files_surface_visible(",
        "fn cancel_sidebar_remote_follow(",
        "fn invalidate_sidebar_remote_follow_intent(",
        "fn active_session_changed_for_remote_follow(",
        "fn resolve_sidebar_remote_follow(",
    ]
    native_tests = [
        "fn combined_ssh_listing_has_no_second_navigation_completion(",
        "fn cancelled_ssh_follow_never_starts_home_or_listing(",
        "fn staged_ssh_follow_never_overrides_newer_file_tree_or_chrome_intent(",
    ]
    replacements = {
        "glass-tokens": (ROOT / "src/glass_shell_tokens.rs").read_text(encoding="utf-8"),
        "glass-layout": (ROOT / "src/glass_layout.rs").read_text(encoding="utf-8"),
        "inspector-tab": "#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]\n"
        + item(shell, "pub(crate) enum InspectorTab {"),
        "types": "\n\n".join(
            "#[derive(Clone, Debug)]\n" + item(source, name)
            for name in ["struct SidebarRemoteFollow {", "struct SidebarRemoteFollowResult {"]
        ),
        "helpers": "\n\n".join(item(source, name) for name in helpers),
        "methods": item(shell, "pub(crate) fn reference_layout(") + "\n\n"
        + "\n\n".join(item(source, name) for name in methods),
        "drawer-decisions": "\n\n".join(
            "fn fixture_drawer_" + name + "(&mut self" +
            (", tab: native_shell::InspectorTab" if name == "tab" else
             ", open: bool" if name == "tasks" else "") + ") {\n" + body + "\n}"
            for name, body in drawer_decisions(source).items()
        ),
        "sidebar-commit": item(sidebar, "pub fn commit_probed_location_listing("),
        "cancel-token": between(
            remote,
            "#[derive(Debug, Default)]\npub struct CancellationToken",
            "/// Shared state for one in-flight transfer:",
        ),
        "native-tests": item(source, "fn sidebar_remote_profile(") + "\n\n"
        + "\n\n".join("#[test]\n" + item(source, name) for name in native_tests),
    }
    for name, body in replacements.items():
        marker = f"// @files-follow:{name}"
        if template.count(marker) != 1:
            raise ValueError(f"expected one fixture marker: {marker}")
        template = template.replace(marker, body, 1)
    if "// @files-follow:" in template:
        raise ValueError("unexpanded production marker in Files follow fixture")
    return template


def main():
    harness = build_harness()
    env = os.environ.copy()
    evidence = env.get("FROST_FILES_FOLLOW_EVIDENCE")
    evidence = Path(evidence) if evidence else None
    if evidence:
        evidence.mkdir(parents=True, exist_ok=False)
        (evidence / "actual-files-follow-harness.rs").write_text(harness, encoding="utf-8")
    # Mutate temporary extracted source only. Each candidate must compile and
    # then be rejected by the unchanged full callback/geometry test suite.
    mutants = [
        ("legacy-1078-inspector-breakpoint",
         "pub const INSPECTOR_DRAWER_BREAKPOINT: f32 = 1180.0;",
         "pub const INSPECTOR_DRAWER_BREAKPOINT: f32 = 1078.0;"),
        ("ignore-real-narrow-drawer",
         "self.reference_layout().inspector.is_some() || self.reference_inspector_drawer",
         "self.reference_layout().inspector.is_some()"),
        ("context-tab-inherits-files-ownership",
         "        && self.reference_inspector_tab == native_shell::InspectorTab::Files\n",
         ""),
        ("accept-new-active-session-with-same-ssh-argv",
         "session.id == pending.session_id && !session.managed_remote",
         "!session.managed_remote"),
        ("left-navigation-substitutes-right-files-surface",
         "        self.files_surface_visible(),\n",
         "        self.sidebar_open,\n"),
    ]
    for name, original in drawer_decisions((ROOT / "src/main.rs").read_text()).items():
        mutants.append(("legacy-drawer-decision-" + name, original,
                        original.replace("self.reference_layout().inspector.is_none()",
                                         "self.win_size.width < 1078.0")))
    assert all(harness.count(original) == 1 for _, original, _ in mutants)
    receipts = []
    with tempfile.TemporaryDirectory(prefix="frost-files-follow-") as directory:
        directory = Path(directory)
        source = directory / "files_follow_harness.rs"
        binary = directory / "files_follow_tests"
        source.write_text(harness, encoding="utf-8")
        command = [
                env.get("RUSTC", "rustc"), "--edition=2021", "--test", "-D", "warnings",
                "-C", "debuginfo=0", str(source), "-o", str(binary),
            ]
        subprocess.run(
            command,
            check=True,
            env=env,
        )
        subprocess.run(
            [str(binary), "--test-threads=1", *sys.argv[1:]],
            check=True,
            env=env,
        )
        for name, original, replacement in mutants:
            source.write_text(harness.replace(original, replacement, 1), encoding="utf-8")
            subprocess.run(command, check=True, env=env)
            result = subprocess.run([str(binary), "--test-threads=1"], env=env,
                                    capture_output=True, text=True)
            assert result.returncode != 0 and "test result: FAILED" in result.stdout, name
            receipts.append(dict(name=name, compiled=True, unchanged_test_suite_rejected=True,
                                 exit_code=result.returncode,
                                 original_item_sha256=hashlib.sha256(original.encode()).hexdigest()))
            if evidence:
                (evidence / (name + ".stdout")).write_text(result.stdout)
                (evidence / (name + ".stderr")).write_text(result.stderr)
            print("negative Files follow contract rejected: " + name)
    if evidence:
        (evidence / "negative-results.json").write_text(json.dumps(receipts, indent=2) + "\n")


if __name__ == "__main__":
    try:
        main()
    except (OSError, ValueError) as error:
        print(f"Files follow regression setup failed: {error}", file=sys.stderr)
        sys.exit(1)
    except subprocess.CalledProcessError as error:
        sys.exit(error.returncode or 1)
