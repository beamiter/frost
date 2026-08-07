#!/usr/bin/env bash
# Exercise the install/uninstall path contract without building or touching a
# real home directory.

set -Eeuo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
INSTALLER="${SCRIPT_DIR}/install.sh"
UNINSTALLER="${SCRIPT_DIR}/uninstall.sh"
TEST_ROOT="$(mktemp -d "${TMPDIR:-/tmp}/frost-install-paths.XXXXXX")"
TEST_HOME="${TEST_ROOT}/home"
TEST_PATH="/usr/bin:/bin"

trap 'rm -rf -- "${TEST_ROOT}"' EXIT
mkdir -p "${TEST_HOME}"

install_dry_run() {
    local destdir="$1"
    shift
    env HOME="${TEST_HOME}" PATH="${TEST_PATH}" DESTDIR="${destdir}" \
        CARGO_TARGET_DIR= "${INSTALLER}" --dry-run "$@"
}

uninstall_dry_run() {
    local destdir="$1"
    shift
    env HOME="${TEST_HOME}" PATH="${TEST_PATH}" DESTDIR="${destdir}" \
        "${UNINSTALLER}" --dry-run "$@"
}

assert_contains() {
    local label="$1" output="$2" expected="$3"
    if [[ "${output}" != *"${expected}"* ]]; then
        printf 'FAIL: %s did not contain %q\n%s\n' "${label}" "${expected}" "${output}" >&2
        exit 1
    fi
}

assert_same() {
    local label="$1" actual="$2" expected="$3"
    if [[ "${actual}" != "${expected}" ]]; then
        printf 'FAIL: %s differed between identical dry runs\n' "${label}" >&2
        exit 1
    fi
}

assert_install_uninstall_pair() {
    local label="$1" expected_binary="$2"
    shift 2
    local install_output uninstall_output expected_dir
    install_output="$(install_dry_run "" "$@")"
    mkdir -p "$(dirname -- "${expected_binary}")"
    touch "${expected_binary}"
    uninstall_output="$(uninstall_dry_run "" "$@")"
    expected_dir="${expected_binary%/frost}"

    assert_contains "${label} install" "${install_output}" \
        "Installed frost to ${expected_binary}"
    assert_contains "${label} uninstall target" "${uninstall_output}" \
        "${expected_binary}"
    assert_contains "${label} uninstall" "${uninstall_output}" \
        "Removed frost from ${expected_dir}"
}

default_install="$(install_dry_run "")"
assert_install_uninstall_pair \
    "default" "${TEST_HOME}/.local/bin/frost"
assert_same "default reinstall plan" "$(install_dry_run "")" "${default_install}"

custom_prefix="${TEST_ROOT}/prefix"
assert_install_uninstall_pair \
    "explicit prefix" "${custom_prefix}/bin/frost" \
    --prefix "${custom_prefix}"

custom_bin="${TEST_ROOT}/custom-bin"
assert_install_uninstall_pair \
    "explicit bin dir" "${custom_bin}/frost" \
    --bin-dir="${custom_bin}"

combined_prefix="${TEST_ROOT}/combined-prefix"
combined_bin="${TEST_ROOT}/combined-bin"
assert_install_uninstall_pair \
    "combined overrides" "${combined_bin}/frost" \
    --prefix="${combined_prefix}" --bin-dir "${combined_bin}"

# DESTDIR changes only where files are staged. The launcher must retain the
# runtime path, and uninstall must prepend the same staging root.
stage_root="${TEST_ROOT}/stage"
runtime_prefix="/opt/frost-contract"
runtime_bin="${runtime_prefix}/bin"
mkdir -p "${stage_root}${runtime_bin}"
touch "${stage_root}${runtime_bin}/frost"

stage_install="$(install_dry_run "${stage_root}/" --prefix "${runtime_prefix}")"
stage_uninstall="$(uninstall_dry_run "${stage_root}/" --prefix "${runtime_prefix}")"
assert_contains "DESTDIR staged install" "${stage_install}" \
    "Staged file: ${stage_root}${runtime_bin}/frost"
assert_contains "DESTDIR launcher" "${stage_install}" \
    "Exec=${runtime_bin}/frost"
assert_contains "DESTDIR staged uninstall" "${stage_uninstall}" \
    "${stage_root}${runtime_bin}/frost"
assert_contains "DESTDIR runtime summary" "${stage_uninstall}" \
    "Removed frost from ${runtime_bin}"

stage_custom_bin="/opt/frost-contract-libexec"
mkdir -p "${stage_root}${stage_custom_bin}"
touch "${stage_root}${stage_custom_bin}/frost"
stage_custom_install="$(
    install_dry_run "${stage_root}" \
        --prefix "${runtime_prefix}" --bin-dir "${stage_custom_bin}"
)"
stage_custom_uninstall="$(
    uninstall_dry_run "${stage_root}" \
        --prefix "${runtime_prefix}" --bin-dir "${stage_custom_bin}"
)"
assert_contains "DESTDIR custom-bin install" "${stage_custom_install}" \
    "Staged file: ${stage_root}${stage_custom_bin}/frost"
assert_contains "DESTDIR custom-bin launcher" "${stage_custom_install}" \
    "Exec=${stage_custom_bin}/frost"
assert_contains "DESTDIR custom-bin uninstall" "${stage_custom_uninstall}" \
    "${stage_root}${stage_custom_bin}/frost"

printf 'install/uninstall path contract: ok\n'
