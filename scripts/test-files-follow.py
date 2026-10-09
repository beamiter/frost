#!/usr/bin/env python3
"""Run actual Files follow callback regressions with Python 3 and rustc only.

Production functions are extracted from the current checkout on every invocation.
The companion Rust template supplies deterministic UI/transport boundaries. No
Cargo build, cached dependencies, GTK display, network, or SSH server is needed.
"""

from pathlib import Path
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


def build_harness():
    source = (ROOT / "src/main.rs").read_text(encoding="utf-8")
    sidebar = (ROOT / "src/sidebar.rs").read_text(encoding="utf-8")
    remote = (ROOT / "src/remote_fs.rs").read_text(encoding="utf-8")
    template = (ROOT / "scripts/files_follow_harness.rs").read_text(encoding="utf-8")
    helpers = [
        "fn probe_sidebar_remote_follow(",
        "fn sidebar_remote_follow_tree_is_current(",
        "fn sidebar_remote_follow_context_is_current(",
        "fn next_sidebar_context_epoch(",
        "fn suppress_armed_sidebar_retry(",
    ]
    methods = [
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
        "types": "\n\n".join(
            "#[derive(Clone, Debug)]\n" + item(source, name)
            for name in ["struct SidebarRemoteFollow {", "struct SidebarRemoteFollowResult {"]
        ),
        "helpers": "\n\n".join(item(source, name) for name in helpers),
        "methods": "\n\n".join(item(source, name) for name in methods),
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
    with tempfile.TemporaryDirectory(prefix="frost-files-follow-") as directory:
        directory = Path(directory)
        source = directory / "files_follow_harness.rs"
        binary = directory / "files_follow_tests"
        source.write_text(harness, encoding="utf-8")
        subprocess.run(
            [
                "rustc", "--edition=2021", "--test", "-D", "warnings",
                "-C", "debuginfo=0", str(source), "-o", str(binary),
            ],
            check=True,
            env=env,
        )
        subprocess.run(
            [str(binary), "--test-threads=1", *sys.argv[1:]],
            check=True,
            env=env,
        )


if __name__ == "__main__":
    try:
        main()
    except (OSError, ValueError) as error:
        print(f"Files follow regression setup failed: {error}", file=sys.stderr)
        sys.exit(1)
    except subprocess.CalledProcessError as error:
        sys.exit(error.returncode or 1)
