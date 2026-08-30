#!/usr/bin/env bash
# Exercise the install/uninstall path contract without building or touching a
# real home directory. In addition to dry-run path coverage, this performs one
# real DESTDIR round trip from a prebuilt binary fixture.

set -Eeuo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
INSTALLER="${SCRIPT_DIR}/install.sh"
UNINSTALLER="${SCRIPT_DIR}/uninstall.sh"
TEST_ROOT="$(mktemp -d "${TMPDIR:-/tmp}/frost-install-paths.XXXXXX")"
TEST_HOME="${TEST_ROOT}/home"
TEST_PATH="/usr/bin:/bin"
unset XDG_CONFIG_HOME XDG_DATA_HOME

shopt -s nullglob
WORKFLOW_SOURCES=(
    "${SCRIPT_DIR}/workflows/"*.toml
    "${SCRIPT_DIR}/workflows/"*.yaml
    "${SCRIPT_DIR}/workflows/"*.yml
)
shopt -u nullglob
if ((${#WORKFLOW_SOURCES[@]} != 6)); then
    printf 'FAIL: expected six bundled workflow fixtures\n' >&2
    exit 1
fi

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

fail() {
    printf 'FAIL: %s\n' "$*" >&2
    exit 1
}

assert_regular_file() {
    local label="$1" path="$2"
    [[ -f "${path}" ]] || fail "${label} is not a regular file: ${path}"
}

assert_mode() {
    local label="$1" path="$2" expected="$3" actual
    actual="$(stat -c '%a' -- "${path}")"
    [[ "${actual}" == "${expected}" ]] \
        || fail "${label} mode was ${actual}, expected ${expected}: ${path}"
}

assert_absent() {
    local label="$1" path="$2"
    [[ ! -e "${path}" && ! -L "${path}" ]] \
        || fail "${label} was not removed: ${path}"
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

# With the default prefix, workflow assets follow the exact XDG user-data tier
# the runtime reads. An explicit packaging prefix deliberately remains
# prefix-relative; non-standard prefixes are exposed through XDG_DATA_DIRS.
custom_xdg_data="${TEST_ROOT}/custom-xdg-data"
custom_xdg_install="$(
    env HOME="${TEST_HOME}" PATH="${TEST_PATH}" DESTDIR= \
        XDG_DATA_HOME="${custom_xdg_data}" CARGO_TARGET_DIR= \
        "${INSTALLER}" --dry-run
)"
assert_contains "custom XDG workflow install" "${custom_xdg_install}" \
    "Installed workflow examples under ${custom_xdg_data}/frost/workflows"
explicit_xdg_install="$(
    env HOME="${TEST_HOME}" PATH="${TEST_PATH}" DESTDIR= \
        XDG_DATA_HOME="${custom_xdg_data}" CARGO_TARGET_DIR= \
        "${INSTALLER}" --dry-run --prefix "${TEST_ROOT}/xdg-explicit-prefix"
)"
assert_contains "explicit prefix owns workflow install" "${explicit_xdg_install}" \
    "Installed workflow examples under ${TEST_ROOT}/xdg-explicit-prefix/share/frost/workflows"
mkdir -p "${custom_xdg_data}/frost/workflows"
touch "${custom_xdg_data}/frost/workflows/git-feature.yaml"
custom_xdg_uninstall="$(
    env HOME="${TEST_HOME}" PATH="${TEST_PATH}" DESTDIR= \
        XDG_DATA_HOME="${custom_xdg_data}" \
        "${UNINSTALLER}" --dry-run
)"
assert_contains "custom XDG workflow uninstall" "${custom_xdg_uninstall}" \
    "${custom_xdg_data}/frost/workflows/git-feature.yaml"
for command in "${INSTALLER}" "${UNINSTALLER}"; do
    if env HOME="${TEST_HOME}" PATH="${TEST_PATH}" DESTDIR= \
        XDG_DATA_HOME=relative/data "${command}" --dry-run \
        >"${TEST_ROOT}/relative-xdg.log" 2>&1; then
        fail "${command} accepted a relative XDG_DATA_HOME"
    fi
    assert_contains "relative XDG diagnostic" \
        "$(<"${TEST_ROOT}/relative-xdg.log")" \
        "XDG_DATA_HOME must be an absolute path"
done

# The preservation handoff must match `dirs::config_dir`: only an absolute
# XDG_CONFIG_HOME wins. Quote the environment-derived value so a newline or
# terminal escape cannot forge another diagnostic line after uninstall.
custom_xdg_config="${TEST_ROOT}/custom config"
custom_config_uninstall="$(
    env HOME="${TEST_HOME}" PATH="${TEST_PATH}" DESTDIR= \
        XDG_CONFIG_HOME="${custom_xdg_config}" \
        "${UNINSTALLER}" --dry-run
)"
printf -v expected_config_display '%q' "${custom_xdg_config}/frost"
assert_contains "absolute XDG config handoff" "${custom_config_uninstall}" \
    "Preserved configuration and history under ${expected_config_display}"

relative_config_uninstall="$(
    env HOME="${TEST_HOME}" PATH="${TEST_PATH}" DESTDIR= \
        XDG_CONFIG_HOME=relative/config "${UNINSTALLER}" --dry-run
)"
printf -v expected_config_display '%q' "${TEST_HOME}/.config/frost"
assert_contains "relative XDG config fallback" "${relative_config_uninstall}" \
    "Preserved configuration and history under ${expected_config_display}"
[[ "${relative_config_uninstall}" != *relative/config* ]] \
    || fail "uninstaller reported a relative XDG_CONFIG_HOME ignored by frost"

hostile_xdg_config="${TEST_ROOT}/config"$'\n''forged uninstall message'
hostile_config_uninstall="$(
    env HOME="${TEST_HOME}" PATH="${TEST_PATH}" DESTDIR= \
        XDG_CONFIG_HOME="${hostile_xdg_config}" \
        "${UNINSTALLER}" --dry-run
)"
printf -v expected_config_display '%q' "${hostile_xdg_config}/frost"
assert_contains "quoted XDG config handoff" "${hostile_config_uninstall}" \
    "Preserved configuration and history under ${expected_config_display}"
[[ "${hostile_config_uninstall}" != *$'\nforged uninstall message'* ]] \
    || fail "XDG_CONFIG_HOME injected an uninstall diagnostic line"

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

# A release archive or distro packager can use the same installer without a
# Rust toolchain. Exercise actual copies and modes in a private staging root,
# and prove that launcher paths never leak DESTDIR into runtime metadata.
prebuilt_dir="${TEST_ROOT}/prebuilt"
prebuilt_binary="${prebuilt_dir}/frost"
roundtrip_stage="${TEST_ROOT}/roundtrip-stage"
roundtrip_prefix='/opt/frost release \dir $'
roundtrip_bin="${roundtrip_prefix}/bin"
roundtrip_share="${roundtrip_prefix}/share"
app_id="io.github.beamiter.frost"
mkdir -p "${prebuilt_dir}"
printf '#!/bin/sh\nprintf "frost release fixture\\n"\n' >"${prebuilt_binary}"
chmod 0600 "${prebuilt_binary}"

# PATH handoff is executable shell syntax, so quote it as one layer rather
# than nesting an arbitrary directory inside an `echo '...'` command. Exercise
# an apostrophe, dollar, relative PATH entry, and trailing empty PATH segment.
path_hint_bin="${TEST_ROOT}/shell hint '\$"
path_hint_output="$(
    env HOME="${TEST_HOME}" PATH="relative/bin:${TEST_PATH}:" DESTDIR= \
        "${INSTALLER}" --dry-run --binary "${prebuilt_binary}" \
        --bin-dir "${path_hint_bin}" --no-desktop
)"
printf -v expected_path_hint '%q' "${path_hint_bin}"
assert_contains "shell-safe PATH handoff" "${path_hint_output}" \
    "  export PATH=${expected_path_hint}:\"\$PATH\""
[[ "${path_hint_output}" != *"echo 'export PATH="* ]] \
    || fail "PATH handoff retained a breakable nested shell quote"

# Invoke Bash directly with an empty PATH. BASH_ENV supplies only the dirname
# helper needed before dry-run dependency checks; the handoff must remain
# nounset-safe and must not mistake an empty segment for the absolute bin dir.
empty_path_env="${TEST_ROOT}/empty-path-bash-env"
printf '%s\n' 'dirname() { /usr/bin/dirname "$@"; }' >"${empty_path_env}"
empty_path_output="$(
    env HOME="${TEST_HOME}" PATH= BASH_ENV="${empty_path_env}" DESTDIR= \
        /bin/bash "${INSTALLER}" --dry-run --binary "${prebuilt_binary}" \
        --bin-dir "${path_hint_bin}" --no-desktop
)"
assert_contains "empty PATH handoff" "${empty_path_output}" \
    "  export PATH=${expected_path_hint}:\"\$PATH\""

# Environment-derived executable diagnostics are inert even when PATH itself
# contains a newline-bearing directory.
shadow_dir="${TEST_ROOT}/shadow"$'\n''forged-shadow'
shadow_binary="${shadow_dir}/frost"
mkdir -p "${shadow_dir}"
printf '#!/bin/sh\nexit 0\n' >"${shadow_binary}"
chmod 0755 "${shadow_binary}"
shadow_output="$(
    env HOME="${TEST_HOME}" PATH="${shadow_dir}:${TEST_PATH}:" DESTDIR= \
        "${INSTALLER}" --dry-run --binary "${prebuilt_binary}" \
        --bin-dir "${path_hint_bin}" --no-desktop
)"
printf -v expected_shadow_display '%q' "${shadow_binary}"
assert_contains "quoted shadowing executable" "${shadow_output}" \
    "typing \`frost\` still runs ${expected_shadow_display}, an older copy"
[[ "${shadow_output}" != *$'\nforged-shadow/frost, an older copy'* ]] \
    || fail "shadowing executable injected an install diagnostic line"

hostile_prebuilt="${TEST_ROOT}/prebuilt"$'\n''forged-prebuilt-message'
hostile_prebuilt_output="$(
    env HOME="${TEST_HOME}" PATH="${TEST_PATH}" DESTDIR= \
        "${INSTALLER}" --dry-run --binary "${hostile_prebuilt}" --no-desktop
)"
printf -v expected_prebuilt_display '%q' "${hostile_prebuilt}"
assert_contains "quoted prebuilt diagnostic" "${hostile_prebuilt_output}" \
    "Using prebuilt frost binary: ${expected_prebuilt_display}"
[[ "${hostile_prebuilt_output}" != *$'\nforged-prebuilt-message'* ]] \
    || fail "prebuilt path injected an install diagnostic line"

roundtrip_install="$(
    env HOME="${TEST_HOME}" PATH="${TEST_PATH}" DESTDIR="${roundtrip_stage}" \
        "${INSTALLER}" --binary "${prebuilt_binary}" --prefix "${roundtrip_prefix}" \
        2>&1
)"
assert_contains "prebuilt install selects fixture" "${roundtrip_install}" \
    "Using prebuilt frost binary: ${prebuilt_binary}"
assert_contains "prebuilt install skips staged cache refresh" "${roundtrip_install}" \
    "Staged install (DESTDIR set); skipping desktop cache refresh."

installed_binary="${roundtrip_stage}${roundtrip_bin}/frost"
installed_desktop="${roundtrip_stage}${roundtrip_share}/applications/${app_id}.desktop"
installed_metainfo="${roundtrip_stage}${roundtrip_share}/metainfo/${app_id}.metainfo.xml"
installed_svg="${roundtrip_stage}${roundtrip_share}/icons/hicolor/scalable/apps/${app_id}.svg"
installed_png_128="${roundtrip_stage}${roundtrip_share}/icons/hicolor/128x128/apps/${app_id}.png"
installed_png_256="${roundtrip_stage}${roundtrip_share}/icons/hicolor/256x256/apps/${app_id}.png"
installed_workflow_dir="${roundtrip_stage}${roundtrip_share}/frost/workflows"
installed_workflows=()
for source in "${WORKFLOW_SOURCES[@]}"; do
    installed_workflows+=("${installed_workflow_dir}/${source##*/}")
done

for installed_file in \
    "${installed_binary}" \
    "${installed_desktop}" \
    "${installed_metainfo}" \
    "${installed_svg}" \
    "${installed_png_128}" \
    "${installed_png_256}" \
    "${installed_workflows[@]}"; do
    assert_regular_file "staged install output" "${installed_file}"
done
cmp -- "${prebuilt_binary}" "${installed_binary}" \
    || fail "staged binary differs from the prebuilt input"
assert_mode "staged binary" "${installed_binary}" 755
for public_file in \
    "${installed_desktop}" \
    "${installed_metainfo}" \
    "${installed_svg}" \
    "${installed_png_128}" \
    "${installed_png_256}" \
    "${installed_workflows[@]}"; do
    assert_mode "staged public resource" "${public_file}" 644
done
for index in "${!WORKFLOW_SOURCES[@]}"; do
    cmp -- "${WORKFLOW_SOURCES[index]}" "${installed_workflows[index]}" \
        || fail "installed workflow differs from ${WORKFLOW_SOURCES[index]}"
done

expected_exec='Exec="/opt/frost release \\\\dir \\$/bin/frost"'
[[ "$(grep -Fxc "${expected_exec}" "${installed_desktop}")" == 2 ]] \
    || fail "desktop main/action Exec paths do not match the runtime binary"
expected_try_exec='TryExec=/opt/frost release \\dir $/bin/frost'
grep -Fxq "${expected_try_exec}" "${installed_desktop}" \
    || fail "desktop TryExec path does not match the runtime binary"
if grep -Fq "${roundtrip_stage}" "${installed_desktop}"; then
    fail "desktop entry leaked DESTDIR into a runtime path"
fi
if command -v desktop-file-validate >/dev/null 2>&1; then
    desktop-file-validate "${installed_desktop}"
fi

env HOME="${TEST_HOME}" PATH="${TEST_PATH}" DESTDIR="${roundtrip_stage}" \
    "${UNINSTALLER}" --prefix "${roundtrip_prefix}" >/dev/null
for removed_file in \
    "${installed_binary}" \
    "${installed_desktop}" \
    "${installed_metainfo}" \
    "${installed_svg}" \
    "${installed_png_128}" \
    "${installed_png_256}" \
    "${installed_workflows[@]}"; do
    assert_absent "staged uninstall target" "${removed_file}"
done
assert_absent "empty installed workflow directory" "${installed_workflow_dir}"
assert_regular_file "prebuilt source after uninstall" "${prebuilt_binary}"

symlink_victim="${TEST_ROOT}/must-not-change"
printf 'victim\n' >"${symlink_victim}"
mkdir -p "$(dirname -- "${installed_binary}")"
ln -s -- "${symlink_victim}" "${installed_binary}"
env HOME="${TEST_HOME}" PATH="${TEST_PATH}" DESTDIR="${roundtrip_stage}" \
    "${INSTALLER}" --binary "${prebuilt_binary}" --prefix "${roundtrip_prefix}" \
    --no-desktop >/dev/null
assert_regular_file "atomically replaced destination" "${installed_binary}"
for installed_workflow in "${installed_workflows[@]}"; do
    assert_regular_file "workflow retained by --no-desktop install" "${installed_workflow}"
done
[[ ! -L "${installed_binary}" ]] \
    || fail "binary install followed or retained the destination symlink"
[[ "$(<"${symlink_victim}")" == victim ]] \
    || fail "binary install overwrote the destination symlink target"
cmp -- "${prebuilt_binary}" "${installed_binary}" \
    || fail "atomically replaced binary differs from its source"
shopt -s nullglob
binary_temps=("${installed_binary}.install."*)
desktop_temps=("${roundtrip_stage}${roundtrip_share}/applications/.${app_id}.desktop.install."*)
shopt -u nullglob
(( ${#binary_temps[@]} == 0 )) || fail "binary install left temporary files"
(( ${#desktop_temps[@]} == 0 )) || fail "desktop install left temporary files"
custom_workflow="${installed_workflow_dir}/custom-user-workflow.yaml"
printf 'name: Custom\ncommand: echo custom\n' >"${custom_workflow}"
env HOME="${TEST_HOME}" PATH="${TEST_PATH}" DESTDIR="${roundtrip_stage}" \
    "${UNINSTALLER}" --prefix "${roundtrip_prefix}" >/dev/null
assert_regular_file "custom workflow preserved by uninstall" "${custom_workflow}"
for installed_workflow in "${installed_workflows[@]}"; do
    assert_absent "owned workflow removed beside custom file" "${installed_workflow}"
done

empty_prebuilt="${prebuilt_dir}/empty-frost"
: >"${empty_prebuilt}"
empty_stage="${TEST_ROOT}/empty-stage"
if env HOME="${TEST_HOME}" PATH="${TEST_PATH}" DESTDIR="${empty_stage}" \
    "${INSTALLER}" --binary "${empty_prebuilt}" --prefix /opt/empty-frost \
    --no-desktop >"${TEST_ROOT}/empty-prebuilt.log" 2>&1; then
    fail "installer accepted an empty prebuilt binary"
fi
assert_contains "empty prebuilt file diagnostic" \
    "$(<"${TEST_ROOT}/empty-prebuilt.log")" "prebuilt binary must not be empty"
assert_absent "empty prebuilt staged target" \
    "${empty_stage}/opt/empty-frost/bin/frost"

ancestor_stage="${TEST_ROOT}/ancestor-stage"
ancestor_victim="${TEST_ROOT}/ancestor-victim"
mkdir -p "${ancestor_stage}" "${ancestor_victim}"
ln -s -- "${ancestor_victim}" "${ancestor_stage}/opt"
if env HOME="${TEST_HOME}" PATH="${TEST_PATH}" DESTDIR="${ancestor_stage}" \
    "${INSTALLER}" --binary "${prebuilt_binary}" --prefix /opt/escaped-frost \
    --no-desktop >"${TEST_ROOT}/ancestor.log" 2>&1; then
    fail "installer accepted a symbolic-link ancestor below DESTDIR"
fi
assert_contains "staging ancestor diagnostic" "$(<"${TEST_ROOT}/ancestor.log")" \
    "symbolic-link ancestor"
[[ -z "$(find "${ancestor_victim}" -mindepth 1 -print -quit)" ]] \
    || fail "staging ancestor validation wrote outside DESTDIR"

# The workflow library adds a runtime-resource subtree below share. Preflight
# its complete directory chain before replacing the binary, so a package tree
# cannot redirect those copies through a pre-existing nested symlink.
workflow_link_stage="${TEST_ROOT}/workflow-link-stage"
workflow_link_prefix="/opt/frost-workflow-link"
workflow_link_victim="${TEST_ROOT}/workflow-link-victim"
workflow_link_binary="${workflow_link_stage}${workflow_link_prefix}/bin/frost"
mkdir -p "${workflow_link_stage}${workflow_link_prefix}/share" \
    "${workflow_link_victim}"
ln -s -- "${workflow_link_victim}" \
    "${workflow_link_stage}${workflow_link_prefix}/share/frost"
if env HOME="${TEST_HOME}" PATH="${TEST_PATH}" DESTDIR="${workflow_link_stage}" \
    "${INSTALLER}" --binary "${prebuilt_binary}" --prefix "${workflow_link_prefix}" \
    --no-desktop >"${TEST_ROOT}/workflow-link.log" 2>&1; then
    fail "installer followed a nested workflow-directory symlink"
fi
assert_contains "workflow ancestor diagnostic" \
    "$(<"${TEST_ROOT}/workflow-link.log")" \
    "staged install path contains a symbolic-link ancestor"
assert_absent "binary after workflow preflight failure" "${workflow_link_binary}"
[[ -z "$(find "${workflow_link_victim}" -mindepth 1 -print -quit)" ]] \
    || fail "workflow install escaped through a nested symlink"

# Desktop resources fan out below share. A symlink in the last icon branch
# must reject the complete plan before the binary or any earlier resource is
# replaced, rather than being discovered after a partial upgrade.
late_resource_stage="${TEST_ROOT}/late-resource-stage"
late_resource_prefix="/opt/frost-late-resource"
late_resource_victim="${TEST_ROOT}/late-resource-victim"
late_resource_binary="${late_resource_stage}${late_resource_prefix}/bin/frost"
late_resource_apps="${late_resource_stage}${late_resource_prefix}/share/icons/hicolor/256x256/apps"
mkdir -p "${late_resource_binary%/*}" "${late_resource_apps%/*}" \
    "${late_resource_victim}"
printf 'old frost before late resource rejection\n' >"${late_resource_binary}"
ln -s -- "${late_resource_victim}" "${late_resource_apps}"
if env HOME="${TEST_HOME}" PATH="${TEST_PATH}" \
    DESTDIR="${late_resource_stage}" "${INSTALLER}" \
    --binary "${prebuilt_binary}" --prefix "${late_resource_prefix}" \
    >"${TEST_ROOT}/late-resource.log" 2>&1; then
    fail "installer followed a late desktop-resource ancestor symlink"
fi
assert_contains "late resource ancestor diagnostic" \
    "$(<"${TEST_ROOT}/late-resource.log")" \
    "staged install path contains a symbolic-link ancestor"
[[ "$(<"${late_resource_binary}")" == \
    'old frost before late resource rejection' ]] \
    || fail "late resource preflight replaced the existing binary"
[[ -z "$(find "${late_resource_victim}" -mindepth 1 -print -quit)" ]] \
    || fail "late resource preflight wrote outside DESTDIR"

resource_stage="${TEST_ROOT}/resource-stage"
resource_prefix="/opt/frost-resource"
resource_victim="${TEST_ROOT}/resource-victim"
resource_metainfo="${resource_stage}${resource_prefix}/share/metainfo/${app_id}.metainfo.xml"
printf 'resource victim\n' >"${resource_victim}"
mkdir -p "$(dirname -- "${resource_metainfo}")"
ln -s -- "${resource_victim}" "${resource_metainfo}"
env HOME="${TEST_HOME}" PATH="${TEST_PATH}" DESTDIR="${resource_stage}" \
    "${INSTALLER}" --binary "${prebuilt_binary}" --prefix "${resource_prefix}" >/dev/null
assert_regular_file "atomically replaced metainfo" "${resource_metainfo}"
[[ ! -L "${resource_metainfo}" ]] || fail "metainfo destination remained a symlink"
[[ "$(<"${resource_victim}")" == "resource victim" ]] \
    || fail "metainfo install overwrote a symlink target"
env HOME="${TEST_HOME}" PATH="${TEST_PATH}" DESTDIR="${resource_stage}" \
    "${UNINSTALLER}" --prefix "${resource_prefix}" >/dev/null

# A failure while staging the last public resource must discard every queued
# temporary before any existing binary, workflow, or desktop asset changes.
stage_failure_tools="${TEST_ROOT}/stage-failure-tools"
stage_failure_root="${TEST_ROOT}/stage-failure-root"
stage_failure_prefix="/opt/frost-stage-failure"
stage_failure_binary="${stage_failure_root}${stage_failure_prefix}/bin/frost"
stage_failure_workflow="${stage_failure_root}${stage_failure_prefix}/share/frost/workflows/git-feature.yaml"
stage_failure_desktop="${stage_failure_root}${stage_failure_prefix}/share/applications/${app_id}.desktop"
stage_failure_icon="${stage_failure_root}${stage_failure_prefix}/share/icons/hicolor/128x128/apps/${app_id}.png"
mkdir -p "${stage_failure_tools}" "${stage_failure_binary%/*}" \
    "${stage_failure_workflow%/*}" "${stage_failure_desktop%/*}" \
    "${stage_failure_icon%/*}"
printf 'old staged frost\n' >"${stage_failure_binary}"
printf 'old staged workflow\n' >"${stage_failure_workflow}"
printf 'old staged desktop\n' >"${stage_failure_desktop}"
printf 'old staged icon\n' >"${stage_failure_icon}"
# shellcheck disable=SC2016
printf '%s\n' \
    '#!/bin/sh' \
    'set -eu' \
    'for argument do' \
    '    case "${argument}" in *io.github.beamiter.frost-256.png) exit 73 ;; esac' \
    'done' \
    'exec /usr/bin/install "$@"' \
    >"${stage_failure_tools}/install"
chmod 0755 "${stage_failure_tools}/install"
if env HOME="${TEST_HOME}" PATH="${stage_failure_tools}:${TEST_PATH}" \
    DESTDIR="${stage_failure_root}" "${INSTALLER}" \
    --binary "${prebuilt_binary}" --prefix "${stage_failure_prefix}" \
    >"${TEST_ROOT}/stage-failure.log" 2>&1; then
    fail "installer accepted a failure while staging the final icon"
fi
assert_contains "late staging failure diagnostic" \
    "$(<"${TEST_ROOT}/stage-failure.log")" \
    "cannot stage ${stage_failure_root}${stage_failure_prefix}/share/icons/hicolor/256x256/apps/${app_id}.png"
[[ "$(<"${stage_failure_binary}")" == 'old staged frost' ]] \
    || fail "late staging failure replaced the existing binary"
[[ "$(<"${stage_failure_workflow}")" == 'old staged workflow' ]] \
    || fail "late staging failure replaced an existing workflow"
[[ "$(<"${stage_failure_desktop}")" == 'old staged desktop' ]] \
    || fail "late staging failure replaced the existing desktop entry"
[[ "$(<"${stage_failure_icon}")" == 'old staged icon' ]] \
    || fail "late staging failure replaced an earlier icon"
[[ -z "$(find "${stage_failure_root}" -type f -name '*.install.*' -print -quit)" ]] \
    || fail "late staging failure left an install temporary"

# Fail the executable's final rename after every resource has published. The
# exit trap must restore the full old generation in reverse order, including
# content/mode, and remove both staging files and rollback backups.
rollback_tools="${TEST_ROOT}/rollback-tools"
rollback_stage="${TEST_ROOT}/rollback-stage"
rollback_prefix="/opt/frost-rollback"
rollback_state="${TEST_ROOT}/rollback-mv-count"
mkdir -p "${rollback_tools}"
env HOME="${TEST_HOME}" PATH="${TEST_PATH}" DESTDIR="${rollback_stage}" \
    "${INSTALLER}" --binary "${prebuilt_binary}" \
    --prefix "${rollback_prefix}" >/dev/null
rollback_share="${rollback_stage}${rollback_prefix}/share"
rollback_targets=(
    "${rollback_share}/applications/${app_id}.desktop"
    "${rollback_share}/metainfo/${app_id}.metainfo.xml"
    "${rollback_share}/icons/hicolor/scalable/apps/${app_id}.svg"
    "${rollback_share}/icons/hicolor/128x128/apps/${app_id}.png"
    "${rollback_share}/icons/hicolor/256x256/apps/${app_id}.png"
)
for source in "${WORKFLOW_SOURCES[@]}"; do
    rollback_targets+=(
        "${rollback_share}/frost/workflows/${source##*/}"
    )
done
rollback_targets+=("${rollback_stage}${rollback_prefix}/bin/frost")
for index in "${!rollback_targets[@]}"; do
    printf 'old rollback target %s\n' "${index}" >"${rollback_targets[index]}"
    chmod 0600 "${rollback_targets[index]}"
done
rollback_absent_target="${rollback_targets[4]}"
rm -f -- "${rollback_absent_target}"
rollback_symlink_target="${rollback_targets[0]}"
rollback_symlink_value='../../missing-old-desktop'
rm -f -- "${rollback_symlink_target}"
ln -s -- "${rollback_symlink_value}" "${rollback_symlink_target}"
rollback_xattr_target="${rollback_targets[1]}"
rollback_xattr_checked=0
if command -v setfattr >/dev/null 2>&1 \
    && command -v getfattr >/dev/null 2>&1; then
    setfattr -n user.frost.rollback -v 'old-xattr' "${rollback_xattr_target}"
    rollback_xattr_checked=1
fi
rollback_identities=()
for index in "${!rollback_targets[@]}"; do
    if [[ "${rollback_targets[index]}" == "${rollback_absent_target}" ]]; then
        rollback_identities+=("")
    else
        rollback_identities+=("$(stat -c '%d:%i:%u:%g' -- \
            "${rollback_targets[index]}")")
    fi
done
# shellcheck disable=SC2016
printf '%s\n' \
    '#!/bin/sh' \
    'set -eu' \
    'state=${FROST_TEST_MV_STATE:?}' \
    'count=0' \
    '[ ! -f "${state}" ] || read -r count <"${state}"' \
    'count=$((count + 1))' \
    'printf "%s\n" "${count}" >"${state}"' \
    '[ "${count}" -ne 12 ] || exit 74' \
    'exec /usr/bin/mv "$@"' \
    >"${rollback_tools}/mv"
chmod 0755 "${rollback_tools}/mv"
if env HOME="${TEST_HOME}" PATH="${rollback_tools}:${TEST_PATH}" \
    FROST_TEST_MV_STATE="${rollback_state}" DESTDIR="${rollback_stage}" \
    "${INSTALLER}" --binary "${prebuilt_binary}" \
    --prefix "${rollback_prefix}" >"${TEST_ROOT}/rollback.log" 2>&1; then
    fail "installer accepted a final executable rename failure"
fi
assert_contains "publish rollback diagnostic" "$(<"${TEST_ROOT}/rollback.log")" \
    "cannot atomically replace ${rollback_stage}${rollback_prefix}/bin/frost"
for index in "${!rollback_targets[@]}"; do
    if [[ "${rollback_targets[index]}" == "${rollback_absent_target}" ]]; then
        assert_absent "new target removed by publish rollback" \
            "${rollback_targets[index]}"
        continue
    fi
    [[ "$(stat -c '%d:%i:%u:%g' -- "${rollback_targets[index]}")" == \
        "${rollback_identities[index]}" ]] \
        || fail "publish rollback changed inode ownership for ${rollback_targets[index]}"
    if [[ "${rollback_targets[index]}" == "${rollback_symlink_target}" ]]; then
        [[ -L "${rollback_symlink_target}" ]] \
            || fail "publish rollback did not restore a dangling symlink"
        [[ "$(readlink -- "${rollback_symlink_target}")" == \
            "${rollback_symlink_value}" ]] \
            || fail "publish rollback changed a dangling symlink target"
        continue
    fi
    [[ "$(<"${rollback_targets[index]}")" == "old rollback target ${index}" ]] \
        || fail "publish rollback changed target ${rollback_targets[index]}"
    assert_mode "publish rollback target" "${rollback_targets[index]}" 600
done
if ((rollback_xattr_checked == 1)); then
    [[ "$(getfattr --only-values -n user.frost.rollback -- \
        "${rollback_xattr_target}" 2>/dev/null)" == 'old-xattr' ]] \
        || fail "publish rollback changed a target xattr"
fi
[[ -z "$(find "${rollback_stage}" \
    \( -name '*.install.*' -o -name '*.rollback.*' \) -print -quit)" ]] \
    || fail "publish rollback left a temporary or backup"

# Final targets may be regular files, symlinks (atomically replaced), or
# absent. Reject special files and directories across the complete set before
# the first rollback hardlink; in particular, never open or read a FIFO.
special_backup_tools="${TEST_ROOT}/special-backup-tools"
mkdir -p "${special_backup_tools}"
# shellcheck disable=SC2016
printf '%s\n' \
    '#!/bin/sh' \
    'set -eu' \
    ': >"${FROST_TEST_LN_MARKER:?}"' \
    'exec /usr/bin/ln "$@"' \
    >"${special_backup_tools}/ln"
chmod 0755 "${special_backup_tools}/ln"
special_target_kinds=(directory fifo)
socket_probe="${TEST_ROOT}/socket-probe"
if command -v python3 >/dev/null 2>&1 \
    && python3 -c \
        'import socket, sys; s = socket.socket(socket.AF_UNIX); s.bind(sys.argv[1]); s.close()' \
        "${socket_probe}" 2>/dev/null; then
    rm -f -- "${socket_probe}"
    special_target_kinds+=(socket)
fi
device_probe="${TEST_ROOT}/device-probe"
if mknod "${device_probe}" c 1 3 2>/dev/null; then
    rm -f -- "${device_probe}"
    special_target_kinds+=(device)
fi
for special_kind in "${special_target_kinds[@]}"; do
    special_stage="${TEST_ROOT}/special-${special_kind}-stage"
    special_prefix="/opt/frost-special-${special_kind}"
    special_binary="${special_stage}${special_prefix}/bin/frost"
    special_marker="${TEST_ROOT}/special-${special_kind}-ln-called"
    env HOME="${TEST_HOME}" PATH="${TEST_PATH}" DESTDIR="${special_stage}" \
        "${INSTALLER}" --binary "${prebuilt_binary}" \
        --prefix "${special_prefix}" --no-desktop >/dev/null
    rm -f -- "${special_binary}"
    case "${special_kind}" in
        directory) mkdir "${special_binary}" ;;
        fifo) mkfifo "${special_binary}" ;;
        socket)
            python3 -c \
                'import socket, sys; s = socket.socket(socket.AF_UNIX); s.bind(sys.argv[1]); s.close()' \
                "${special_binary}"
            ;;
        device) mknod "${special_binary}" c 1 3 ;;
    esac
    if env HOME="${TEST_HOME}" \
        PATH="${special_backup_tools}:${TEST_PATH}" \
        FROST_TEST_LN_MARKER="${special_marker}" DESTDIR="${special_stage}" \
        "${INSTALLER}" --binary "${prebuilt_binary}" \
        --prefix "${special_prefix}" --no-desktop \
        >"${TEST_ROOT}/special-${special_kind}.log" 2>&1; then
        fail "installer accepted a ${special_kind} final target"
    fi
    assert_contains "${special_kind} target diagnostic" \
        "$(<"${TEST_ROOT}/special-${special_kind}.log")" \
        "install destination is not a regular file or symlink: ${special_binary}"
    assert_absent "backup attempt before ${special_kind} rejection" \
        "${special_marker}"
    case "${special_kind}" in
        directory) [[ -d "${special_binary}" ]] ;;
        fifo) [[ -p "${special_binary}" ]] ;;
        socket) [[ -S "${special_binary}" ]] ;;
        device) [[ -c "${special_binary}" ]] ;;
    esac || fail "${special_kind} target changed during rejection"
    [[ -z "$(find "${special_stage}" \
        \( -name '*.install.*' -o -name '*.rollback.*' \) -print -quit)" ]] \
        || fail "${special_kind} rejection left a temporary or backup"
done

# Removing a pre-rename launcher is migration hygiene, not part of committing
# the new generation. A cleanup failure must warn without converting an
# otherwise complete install into a false failure or skipping its summary.
legacy_cleanup_tools="${TEST_ROOT}/legacy-cleanup-tools"
legacy_cleanup_stage="${TEST_ROOT}/legacy-cleanup-stage"
legacy_cleanup_prefix="/opt/frost-legacy-cleanup"
legacy_cleanup_share="${legacy_cleanup_stage}${legacy_cleanup_prefix}/share"
legacy_cleanup_entry="${legacy_cleanup_share}/applications/io.github.beamiter.jterm3.desktop"
mkdir -p "${legacy_cleanup_tools}" "${legacy_cleanup_entry%/*}"
printf 'legacy launcher retained after warning\n' >"${legacy_cleanup_entry}"
# shellcheck disable=SC2016
printf '%s\n' \
    '#!/bin/sh' \
    'set -eu' \
    'for argument do' \
    '    case "${argument}" in *io.github.beamiter.jterm3.desktop) exit 75 ;; esac' \
    'done' \
    'exec /usr/bin/rm "$@"' \
    >"${legacy_cleanup_tools}/rm"
chmod 0755 "${legacy_cleanup_tools}/rm"
legacy_cleanup_output="$(
    env HOME="${TEST_HOME}" PATH="${legacy_cleanup_tools}:${TEST_PATH}" \
        DESTDIR="${legacy_cleanup_stage}" "${INSTALLER}" \
        --binary "${prebuilt_binary}" --prefix "${legacy_cleanup_prefix}" 2>&1
)"
assert_contains "legacy cleanup warning" "${legacy_cleanup_output}" \
    "warning: could not remove legacy launcher (non-fatal): ${legacy_cleanup_entry}"
assert_contains "legacy cleanup success summary" "${legacy_cleanup_output}" \
    "Installed frost to ${legacy_cleanup_prefix}/bin/frost"
assert_regular_file "binary after legacy cleanup warning" \
    "${legacy_cleanup_stage}${legacy_cleanup_prefix}/bin/frost"
[[ "$(<"${legacy_cleanup_entry}")" == \
    'legacy launcher retained after warning' ]] \
    || fail "failed legacy cleanup changed its launcher"
assert_regular_file "desktop entry after legacy cleanup warning" \
    "${legacy_cleanup_share}/applications/${app_id}.desktop"
[[ -z "$(find "${legacy_cleanup_stage}" \
    \( -name '*.install.*' -o -name '*.rollback.*' \) -print -quit)" ]] \
    || fail "legacy cleanup warning left a temporary or backup"

interrupt_tools="${TEST_ROOT}/interrupt-tools"
interrupt_stage="${TEST_ROOT}/interrupt-stage"
interrupt_prefix="/opt/frost-interrupt"
interrupt_binary="${interrupt_stage}${interrupt_prefix}/bin/frost"
mkdir -p "${interrupt_tools}" "$(dirname -- "${interrupt_binary}")"
printf 'old interrupt frost\n' >"${interrupt_binary}"
# shellcheck disable=SC2016
printf '%s\n' \
    '#!/bin/sh' \
    'set -eu' \
    'last=""' \
    'for argument do last="${argument}"; done' \
    '/usr/bin/install "$@"' \
    'case "${last}" in *.install.*) kill -TERM "${PPID}" ;; esac' \
    >"${interrupt_tools}/install"
chmod 0755 "${interrupt_tools}/install"
if {
    env HOME="${TEST_HOME}" PATH="${interrupt_tools}:${TEST_PATH}" \
        DESTDIR="${interrupt_stage}" "${INSTALLER}" --binary "${prebuilt_binary}" \
        --prefix "${interrupt_prefix}" --no-desktop
} >"${TEST_ROOT}/interrupt.log" 2>&1; then
    fail "interrupted installer unexpectedly succeeded"
fi
[[ "$(<"${interrupt_binary}")" == 'old interrupt frost' ]] \
    || fail "pre-rename interruption replaced the old binary"
shopt -s nullglob
interrupt_temps=("${interrupt_binary}.install."*)
shopt -u nullglob
(( ${#interrupt_temps[@]} == 0 )) \
    || fail "pre-rename interruption left a binary temporary"

prebuilt_symlink="${prebuilt_dir}/frost-link"
ln -s -- "${prebuilt_binary}" "${prebuilt_symlink}"
if env HOME="${TEST_HOME}" PATH="${TEST_PATH}" DESTDIR="${roundtrip_stage}" \
    "${INSTALLER}" --binary "${prebuilt_symlink}" --prefix "${roundtrip_prefix}" \
    >"${TEST_ROOT}/symlink-binary.log" 2>&1; then
    fail "installer accepted a symlinked prebuilt binary"
fi
assert_contains "symlinked prebuilt diagnostic" \
    "$(<"${TEST_ROOT}/symlink-binary.log")" \
    "prebuilt binary must not be a symbolic link: ${prebuilt_symlink}"
assert_regular_file "symlink target after rejection" "${prebuilt_binary}"

missing_binary="${TEST_ROOT}/does-not-exist/frost"
if env HOME="${TEST_HOME}" PATH="${TEST_PATH}" DESTDIR="${roundtrip_stage}" \
    "${INSTALLER}" --binary "${missing_binary}" --prefix "${roundtrip_prefix}" \
    >"${TEST_ROOT}/missing-binary.log" 2>&1; then
    fail "installer accepted a missing prebuilt binary"
fi
assert_contains "missing prebuilt diagnostic" \
    "$(<"${TEST_ROOT}/missing-binary.log")" \
    "prebuilt binary is not a regular file: ${missing_binary}"

if install_dry_run "" --binary= >"${TEST_ROOT}/empty-binary.log" 2>&1; then
    fail "installer accepted an empty --binary path"
fi
assert_contains "empty prebuilt diagnostic" \
    "$(<"${TEST_ROOT}/empty-binary.log")" \
    "--binary must not be empty"

if install_dry_run "" --prefix '/opt/frost=invalid' \
    >"${TEST_ROOT}/invalid-desktop-path.log" 2>&1; then
    fail "installer accepted '=' in a desktop executable path"
fi
assert_contains "invalid desktop executable diagnostic" \
    "$(<"${TEST_ROOT}/invalid-desktop-path.log")" \
    "desktop executable path must not contain '='"

if install_dry_run "" --prefix '/opt/frost%invalid' \
    >"${TEST_ROOT}/percent-desktop-path.log" 2>&1; then
    fail "installer accepted '%' in a desktop executable path"
fi
assert_contains "invalid percent executable diagnostic" \
    "$(<"${TEST_ROOT}/percent-desktop-path.log")" \
    "desktop executable path must not contain '%'"

invalid_stage="${TEST_ROOT}/invalid-desktop-stage"
invalid_prefix='/opt/frost=invalid'
sentinel_binary="${invalid_stage}${invalid_prefix}/bin/frost"
mkdir -p "$(dirname -- "${sentinel_binary}")"
printf 'old frost\n' >"${sentinel_binary}"
if env HOME="${TEST_HOME}" PATH="${TEST_PATH}" DESTDIR="${invalid_stage}" \
    "${INSTALLER}" --binary "${prebuilt_binary}" --prefix "${invalid_prefix}" \
    >"${TEST_ROOT}/desktop-preflight.log" 2>&1; then
    fail "installer accepted an invalid desktop executable path"
fi
[[ "$(<"${sentinel_binary}")" == 'old frost' ]] \
    || fail "desktop preflight failure replaced the old binary"

for bad_path in '/opt/frost/../escape' '/opt/frost/'$'bad\npath'; do
    if install_dry_run "${stage_root}" --prefix "${bad_path}" \
        >"${TEST_ROOT}/bad-install-path.log" 2>&1; then
        fail "installer accepted unsafe prefix ${bad_path@Q}"
    fi
    if uninstall_dry_run "${stage_root}" --prefix "${bad_path}" \
        >"${TEST_ROOT}/bad-uninstall-path.log" 2>&1; then
        fail "uninstaller accepted unsafe prefix ${bad_path@Q}"
    fi
done
assert_contains "parent path diagnostic" \
    "$(install_dry_run "${stage_root}" --prefix '/opt/frost/../escape' 2>&1 || true)" \
    "--prefix must not contain '..' path components"

if install_dry_run "${stage_root}/../escape" --prefix /opt/frost \
    >"${TEST_ROOT}/bad-destdir.log" 2>&1; then
    fail "installer accepted a DESTDIR with a parent component"
fi
assert_contains "DESTDIR parent diagnostic" "$(<"${TEST_ROOT}/bad-destdir.log")" \
    "DESTDIR must not contain '..' path components"

for command in install_dry_run uninstall_dry_run; do
    if "${command}" "" --bin-dir= >"${TEST_ROOT}/empty-bin.log" 2>&1; then
        fail "${command} accepted an empty --bin-dir"
    fi
    assert_contains "empty bin diagnostic" "$(<"${TEST_ROOT}/empty-bin.log")" \
        "--bin-dir must not be empty"
done

root_stage_install="$(install_dry_run / --prefix /opt/frost-root)"
assert_contains "root DESTDIR cache policy" "${root_stage_install}" \
    "Staged install (DESTDIR set); skipping desktop cache refresh."
assert_contains "root DESTDIR summary" "${root_stage_install}" \
    "Staged file: /opt/frost-root/bin/frost"

portable_path='/opt/./霜 terminal'
assert_contains "Unicode path accepted" \
    "$(install_dry_run "${stage_root}" --prefix "${portable_path}")" \
    "Installed frost to ${portable_path}/bin/frost"

# Uninstall owns regular files and final symlink entries, not whatever special
# object may have replaced one of those paths. Put the unexpected object after
# a valid binary in the removal plan and prove preflight calls no rm at all.
uninstall_special_tools="${TEST_ROOT}/uninstall-special-tools"
mkdir -p "${uninstall_special_tools}"
# shellcheck disable=SC2016
printf '%s\n' \
    '#!/bin/sh' \
    'set -eu' \
    ': >"${FROST_TEST_RM_MARKER:?}"' \
    'exec /usr/bin/rm "$@"' \
    >"${uninstall_special_tools}/rm"
chmod 0755 "${uninstall_special_tools}/rm"
for special_kind in "${special_target_kinds[@]}"; do
    uninstall_special_stage="${TEST_ROOT}/uninstall-special-${special_kind}-stage"
    uninstall_special_prefix="/opt/frost-uninstall-special-${special_kind}"
    uninstall_special_binary="${uninstall_special_stage}${uninstall_special_prefix}/bin/frost"
    uninstall_special_target="${uninstall_special_stage}${uninstall_special_prefix}/share/applications/${app_id}.desktop"
    uninstall_special_marker="${TEST_ROOT}/uninstall-special-${special_kind}-rm-called"
    mkdir -p "${uninstall_special_binary%/*}" \
        "${uninstall_special_target%/*}"
    printf 'installed frost before special target rejection\n' \
        >"${uninstall_special_binary}"
    case "${special_kind}" in
        directory) mkdir "${uninstall_special_target}" ;;
        fifo) mkfifo "${uninstall_special_target}" ;;
        socket)
            python3 -c \
                'import socket, sys; s = socket.socket(socket.AF_UNIX); s.bind(sys.argv[1]); s.close()' \
                "${uninstall_special_target}"
            ;;
        device) mknod "${uninstall_special_target}" c 1 3 ;;
    esac
    if env HOME="${TEST_HOME}" \
        PATH="${uninstall_special_tools}:${TEST_PATH}" \
        FROST_TEST_RM_MARKER="${uninstall_special_marker}" \
        DESTDIR="${uninstall_special_stage}" "${UNINSTALLER}" \
        --prefix "${uninstall_special_prefix}" \
        >"${TEST_ROOT}/uninstall-special-${special_kind}.log" 2>&1; then
        fail "uninstaller accepted a ${special_kind} file target"
    fi
    assert_contains "${special_kind} uninstall target diagnostic" \
        "$(<"${TEST_ROOT}/uninstall-special-${special_kind}.log")" \
        "uninstall target is not a regular file or symlink: ${uninstall_special_target}"
    assert_absent "rm call before ${special_kind} uninstall rejection" \
        "${uninstall_special_marker}"
    [[ "$(<"${uninstall_special_binary}")" == \
        'installed frost before special target rejection' ]] \
        || fail "${special_kind} preflight changed the installed binary"
    case "${special_kind}" in
        directory) [[ -d "${uninstall_special_target}" ]] ;;
        fifo) [[ -p "${uninstall_special_target}" ]] ;;
        socket) [[ -S "${uninstall_special_target}" ]] ;;
        device) [[ -c "${uninstall_special_target}" ]] ;;
    esac || fail "${special_kind} uninstall target changed during rejection"
done

# The one cleanup path is intentionally narrower than file targets: only a
# real directory may reach non-recursive rmdir. Reject every replacement type
# during whole-plan preflight, before the earlier binary can be unlinked.
cleanup_target_kinds=(regular fifo symlink)
for special_kind in "${special_target_kinds[@]}"; do
    case "${special_kind}" in
        socket|device) cleanup_target_kinds+=("${special_kind}") ;;
    esac
done
for cleanup_kind in "${cleanup_target_kinds[@]}"; do
    cleanup_stage="${TEST_ROOT}/uninstall-cleanup-${cleanup_kind}-stage"
    cleanup_prefix="/opt/frost-uninstall-cleanup-${cleanup_kind}"
    cleanup_binary="${cleanup_stage}${cleanup_prefix}/bin/frost"
    cleanup_target="${cleanup_stage}${cleanup_prefix}/share/frost/workflows"
    cleanup_marker="${TEST_ROOT}/uninstall-cleanup-${cleanup_kind}-rm-called"
    cleanup_victim="${TEST_ROOT}/uninstall-cleanup-${cleanup_kind}-victim"
    mkdir -p "${cleanup_binary%/*}" "${cleanup_target%/*}"
    printf 'installed frost before cleanup target rejection\n' \
        >"${cleanup_binary}"
    case "${cleanup_kind}" in
        regular) printf 'not a cleanup directory\n' >"${cleanup_target}" ;;
        fifo) mkfifo "${cleanup_target}" ;;
        symlink)
            mkdir -p "${cleanup_victim}"
            ln -s -- "${cleanup_victim}" "${cleanup_target}"
            ;;
        socket)
            python3 -c \
                'import socket, sys; s = socket.socket(socket.AF_UNIX); s.bind(sys.argv[1]); s.close()' \
                "${cleanup_target}"
            ;;
        device) mknod "${cleanup_target}" c 1 3 ;;
    esac
    if env HOME="${TEST_HOME}" \
        PATH="${uninstall_special_tools}:${TEST_PATH}" \
        FROST_TEST_RM_MARKER="${cleanup_marker}" DESTDIR="${cleanup_stage}" \
        "${UNINSTALLER}" --prefix "${cleanup_prefix}" \
        >"${TEST_ROOT}/uninstall-cleanup-${cleanup_kind}.log" 2>&1; then
        fail "uninstaller accepted a ${cleanup_kind} cleanup target"
    fi
    assert_contains "${cleanup_kind} cleanup target diagnostic" \
        "$(<"${TEST_ROOT}/uninstall-cleanup-${cleanup_kind}.log")" \
        "uninstall cleanup target is not a directory: ${cleanup_target}"
    assert_absent "rm call before ${cleanup_kind} cleanup rejection" \
        "${cleanup_marker}"
    [[ "$(<"${cleanup_binary}")" == \
        'installed frost before cleanup target rejection' ]] \
        || fail "${cleanup_kind} cleanup preflight changed the binary"
    case "${cleanup_kind}" in
        regular) [[ -f "${cleanup_target}" && ! -L "${cleanup_target}" ]] ;;
        fifo) [[ -p "${cleanup_target}" ]] ;;
        symlink) [[ -L "${cleanup_target}" ]] ;;
        socket) [[ -S "${cleanup_target}" ]] ;;
        device) [[ -c "${cleanup_target}" ]] ;;
    esac || fail "${cleanup_kind} cleanup target changed during rejection"
    if [[ "${cleanup_kind}" == symlink ]]; then
        [[ -z "$(find "${cleanup_victim}" -mindepth 1 -print -quit)" ]] \
            || fail "cleanup symlink rejection touched its referent"
    fi
done

# A final symlink is an entry the installer owns: unlink it without following
# its target, including when the target lives outside the staged prefix.
uninstall_symlink_stage="${TEST_ROOT}/uninstall-final-symlink-stage"
uninstall_symlink_prefix="/opt/frost-uninstall-final-symlink"
uninstall_symlink_path="${uninstall_symlink_stage}${uninstall_symlink_prefix}/bin/frost"
uninstall_symlink_victim="${TEST_ROOT}/uninstall-final-symlink-victim"
mkdir -p "${uninstall_symlink_path%/*}"
printf 'outside final symlink target\n' >"${uninstall_symlink_victim}"
ln -s -- "${uninstall_symlink_victim}" "${uninstall_symlink_path}"
env HOME="${TEST_HOME}" PATH="${TEST_PATH}" \
    DESTDIR="${uninstall_symlink_stage}" "${UNINSTALLER}" \
    --prefix "${uninstall_symlink_prefix}" >/dev/null
assert_absent "final uninstall symlink" "${uninstall_symlink_path}"
[[ "$(<"${uninstall_symlink_victim}")" == 'outside final symlink target' ]] \
    || fail "uninstaller followed a final symlink"

# File removal is a reversible rename phase. Fail the third quarantine rename
# before it executes and require both earlier targets to return in reverse
# order with their exact inode identity, while unused reservations disappear.
uninstall_rollback_tools="${TEST_ROOT}/uninstall-rollback-tools"
uninstall_rollback_stage="${TEST_ROOT}/uninstall-rollback-stage"
uninstall_rollback_prefix="/opt/frost-uninstall-rollback"
uninstall_rollback_state="${TEST_ROOT}/uninstall-rollback-mv-state"
uninstall_rollback_share="${uninstall_rollback_stage}${uninstall_rollback_prefix}/share"
uninstall_rollback_targets=(
    "${uninstall_rollback_stage}${uninstall_rollback_prefix}/bin/frost"
    "${uninstall_rollback_share}/applications/${app_id}.desktop"
    "${uninstall_rollback_share}/metainfo/${app_id}.metainfo.xml"
)
mkdir -p "${uninstall_rollback_tools}"
uninstall_rollback_identities=()
for index in "${!uninstall_rollback_targets[@]}"; do
    mkdir -p "${uninstall_rollback_targets[index]%/*}"
    printf 'old uninstall rollback target %s\n' "${index}" \
        >"${uninstall_rollback_targets[index]}"
    chmod 0600 "${uninstall_rollback_targets[index]}"
    uninstall_rollback_identities+=("$(stat -c '%d:%i:%u:%g:%a' -- \
        "${uninstall_rollback_targets[index]}")")
done
# shellcheck disable=SC2016
printf '%s\n' \
    '#!/bin/sh' \
    'set -eu' \
    'state=${FROST_TEST_UNINSTALL_MV_STATE:?}' \
    'count=0' \
    '[ ! -f "${state}" ] || read -r count <"${state}"' \
    'count=$((count + 1))' \
    'printf "%s\n" "${count}" >"${state}"' \
    '[ "${count}" -ne 3 ] || exit 76' \
    'if [ "${count}" -eq 4 ]; then /usr/bin/mv "$@"; exit 77; fi' \
    'exec /usr/bin/mv "$@"' \
    >"${uninstall_rollback_tools}/mv"
chmod 0755 "${uninstall_rollback_tools}/mv"
if env HOME="${TEST_HOME}" \
    PATH="${uninstall_rollback_tools}:${TEST_PATH}" \
    FROST_TEST_UNINSTALL_MV_STATE="${uninstall_rollback_state}" \
    DESTDIR="${uninstall_rollback_stage}" "${UNINSTALLER}" \
    --prefix "${uninstall_rollback_prefix}" \
    >"${TEST_ROOT}/uninstall-rollback.log" 2>&1; then
    fail "uninstaller accepted a quarantine rename failure"
fi
assert_contains "uninstall rollback diagnostic" \
    "$(<"${TEST_ROOT}/uninstall-rollback.log")" \
    "cannot quarantine uninstall target ${uninstall_rollback_targets[2]}"
[[ "$(<"${TEST_ROOT}/uninstall-rollback.log")" != \
    *"rollback failed for ${uninstall_rollback_targets[1]}"* ]] \
    || fail "post-restore mv failure was reported as an unrestored target"
for index in "${!uninstall_rollback_targets[@]}"; do
    [[ "$(<"${uninstall_rollback_targets[index]}")" == \
        "old uninstall rollback target ${index}" ]] \
        || fail "uninstall rollback changed target ${uninstall_rollback_targets[index]}"
    [[ "$(stat -c '%d:%i:%u:%g:%a' -- \
        "${uninstall_rollback_targets[index]}")" == \
        "${uninstall_rollback_identities[index]}" ]] \
        || fail "uninstall rollback changed inode metadata for ${uninstall_rollback_targets[index]}"
done
[[ -z "$(find "${uninstall_rollback_stage}" \
    -name '*.uninstall.*' -print -quit)" ]] \
    || fail "uninstall rollback left a quarantine reservation"

# A catchable signal can land after rename(2) succeeds but before Bash records
# that success. The in-flight state is reconciled by inode, then restored.
uninstall_interrupt_tools="${TEST_ROOT}/uninstall-interrupt-tools"
uninstall_interrupt_stage="${TEST_ROOT}/uninstall-interrupt-stage"
uninstall_interrupt_prefix="/opt/frost-uninstall-interrupt"
uninstall_interrupt_binary="${uninstall_interrupt_stage}${uninstall_interrupt_prefix}/bin/frost"
uninstall_interrupt_marker="${TEST_ROOT}/uninstall-interrupt-moved"
mkdir -p "${uninstall_interrupt_tools}" "${uninstall_interrupt_binary%/*}"
printf 'binary restored after uninstall interrupt\n' \
    >"${uninstall_interrupt_binary}"
uninstall_interrupt_identity="$(stat -c '%d:%i:%u:%g:%a' -- \
    "${uninstall_interrupt_binary}")"
# shellcheck disable=SC2016
printf '%s\n' \
    '#!/bin/sh' \
    'set -eu' \
    'if [ ! -e "${FROST_TEST_UNINSTALL_INTERRUPT_MARKER:?}" ]; then' \
    '    /usr/bin/mv "$@"' \
    '    : >"${FROST_TEST_UNINSTALL_INTERRUPT_MARKER}"' \
    '    kill -TERM "${PPID}"' \
    '    exit 0' \
    'fi' \
    'exec /usr/bin/mv "$@"' \
    >"${uninstall_interrupt_tools}/mv"
chmod 0755 "${uninstall_interrupt_tools}/mv"
if env HOME="${TEST_HOME}" \
    PATH="${uninstall_interrupt_tools}:${TEST_PATH}" \
    FROST_TEST_UNINSTALL_INTERRUPT_MARKER="${uninstall_interrupt_marker}" \
    DESTDIR="${uninstall_interrupt_stage}" "${UNINSTALLER}" \
    --prefix "${uninstall_interrupt_prefix}" \
    >"${TEST_ROOT}/uninstall-interrupt.log" 2>&1; then
    fail "uninstaller ignored a catchable interrupt during quarantine"
fi
[[ -e "${uninstall_interrupt_marker}" ]] \
    || fail "uninstall interrupt did not occur after rename"
[[ "$(<"${uninstall_interrupt_binary}")" == \
    'binary restored after uninstall interrupt' ]] \
    || fail "uninstall interrupt rollback changed the binary"
[[ "$(stat -c '%d:%i:%u:%g:%a' -- "${uninstall_interrupt_binary}")" == \
    "${uninstall_interrupt_identity}" ]] \
    || fail "uninstall interrupt rollback changed binary inode metadata"
[[ -z "$(find "${uninstall_interrupt_stage}" \
    -name '*.uninstall.*' -print -quit)" ]] \
    || fail "uninstall interrupt rollback left a quarantine"

# Reservation names are untrusted again after mktemp returns. After the first
# target moves, replace the later desktop reservation with a symlink. Its
# recorded placeholder inode must reject the second rename; rollback restores
# the binary while cleanup refuses to unlink the substituted entry or referent.
uninstall_reservation_tools="${TEST_ROOT}/uninstall-reservation-tools"
uninstall_reservation_stage="${TEST_ROOT}/uninstall-reservation-stage"
uninstall_reservation_prefix="/opt/frost-uninstall-reservation"
uninstall_reservation_binary="${uninstall_reservation_stage}${uninstall_reservation_prefix}/bin/frost"
uninstall_reservation_app_dir="${uninstall_reservation_stage}${uninstall_reservation_prefix}/share/applications"
uninstall_reservation_desktop="${uninstall_reservation_app_dir}/${app_id}.desktop"
uninstall_reservation_marker="${TEST_ROOT}/uninstall-reservation-replaced"
uninstall_reservation_victim="${TEST_ROOT}/uninstall-reservation-victim"
uninstall_reservation_saved="${TEST_ROOT}/uninstall-reservation-original-placeholder"
mkdir -p "${uninstall_reservation_tools}" \
    "${uninstall_reservation_binary%/*}" "${uninstall_reservation_app_dir}" \
    "${uninstall_reservation_victim}"
printf 'binary restored after reservation replacement\n' \
    >"${uninstall_reservation_binary}"
printf 'desktop preserved after reservation replacement\n' \
    >"${uninstall_reservation_desktop}"
uninstall_reservation_binary_identity="$(stat -c '%d:%i:%u:%g:%a' -- \
    "${uninstall_reservation_binary}")"
uninstall_reservation_desktop_identity="$(stat -c '%d:%i:%u:%g:%a' -- \
    "${uninstall_reservation_desktop}")"
# shellcheck disable=SC2016
printf '%s\n' \
    '#!/bin/sh' \
    'set -eu' \
    'if [ ! -e "${FROST_TEST_UNINSTALL_RESERVATION_MARKER:?}" ]; then' \
    '    /usr/bin/mv "$@"' \
    '    : >"${FROST_TEST_UNINSTALL_RESERVATION_MARKER}"' \
    '    found=' \
    '    for candidate in "${FROST_TEST_UNINSTALL_RESERVATION_DIR:?}"/.io.github.beamiter.frost.desktop.uninstall.*; do' \
    '        [ -e "${candidate}" ] || continue' \
    '        found=${candidate}' \
    '        break' \
    '    done' \
    '    [ -n "${found}" ]' \
    '    /usr/bin/mv "${found}" "${FROST_TEST_UNINSTALL_RESERVATION_SAVED:?}"' \
    '    /usr/bin/ln -s -- "${FROST_TEST_UNINSTALL_RESERVATION_VICTIM:?}" "${found}"' \
    '    exit 0' \
    'fi' \
    'exec /usr/bin/mv "$@"' \
    >"${uninstall_reservation_tools}/mv"
chmod 0755 "${uninstall_reservation_tools}/mv"
if env HOME="${TEST_HOME}" \
    PATH="${uninstall_reservation_tools}:${TEST_PATH}" \
    FROST_TEST_UNINSTALL_RESERVATION_MARKER="${uninstall_reservation_marker}" \
    FROST_TEST_UNINSTALL_RESERVATION_DIR="${uninstall_reservation_app_dir}" \
    FROST_TEST_UNINSTALL_RESERVATION_SAVED="${uninstall_reservation_saved}" \
    FROST_TEST_UNINSTALL_RESERVATION_VICTIM="${uninstall_reservation_victim}" \
    DESTDIR="${uninstall_reservation_stage}" "${UNINSTALLER}" \
    --prefix "${uninstall_reservation_prefix}" \
    >"${TEST_ROOT}/uninstall-reservation.log" 2>&1; then
    fail "uninstaller accepted a replaced quarantine reservation"
fi
mapfile -t uninstall_reservation_links < <(
    find "${uninstall_reservation_app_dir}" -maxdepth 1 \
        -name '.io.github.beamiter.frost.desktop.uninstall.*' -print
)
(( ${#uninstall_reservation_links[@]} == 1 )) \
    || fail "reservation replacement did not leave exactly one refused entry"
uninstall_reservation_link="${uninstall_reservation_links[0]}"
assert_contains "changed reservation diagnostic" \
    "$(<"${TEST_ROOT}/uninstall-reservation.log")" \
    "uninstall quarantine reservation changed after preflight: ${uninstall_reservation_link}"
assert_contains "changed reservation cleanup warning" \
    "$(<"${TEST_ROOT}/uninstall-reservation.log")" \
    "refusing to remove changed unused quarantine ${uninstall_reservation_link}"
[[ -L "${uninstall_reservation_link}" ]] \
    || fail "changed quarantine reservation was unlinked"
[[ "$(readlink -- "${uninstall_reservation_link}")" == \
    "${uninstall_reservation_victim}" ]] \
    || fail "changed quarantine reservation link target changed"
[[ -z "$(find "${uninstall_reservation_victim}" -mindepth 1 -print -quit)" ]] \
    || fail "changed quarantine reservation referent was touched"
[[ "$(stat -c '%d:%i:%u:%g:%a' -- "${uninstall_reservation_binary}")" == \
    "${uninstall_reservation_binary_identity}" ]] \
    || fail "reservation replacement rollback changed binary inode metadata"
[[ "$(stat -c '%d:%i:%u:%g:%a' -- "${uninstall_reservation_desktop}")" == \
    "${uninstall_reservation_desktop_identity}" ]] \
    || fail "reservation replacement changed desktop inode metadata"

# Purge also owns only the inode that quarantine received from the target.
# Move that inode aside and substitute a symlink after mv reports success. The
# committed target stays absent, but purge must neither call rm on the changed
# name nor advertise the symlink as a recovery copy of the original.
uninstall_changed_purge_tools="${TEST_ROOT}/uninstall-changed-purge-tools"
uninstall_changed_purge_stage="${TEST_ROOT}/uninstall-changed-purge-stage"
uninstall_changed_purge_prefix="/opt/frost-uninstall-changed-purge"
uninstall_changed_purge_binary="${uninstall_changed_purge_stage}${uninstall_changed_purge_prefix}/bin/frost"
uninstall_changed_purge_displaced="${TEST_ROOT}/uninstall-changed-purge-original"
uninstall_changed_purge_victim="${TEST_ROOT}/uninstall-changed-purge-victim"
uninstall_changed_purge_rm_marker="${TEST_ROOT}/uninstall-changed-purge-rm-called"
mkdir -p "${uninstall_changed_purge_tools}" \
    "${uninstall_changed_purge_binary%/*}" "${uninstall_changed_purge_victim}"
printf 'original inode displaced before purge\n' \
    >"${uninstall_changed_purge_binary}"
uninstall_changed_purge_identity="$(stat -c '%d:%i:%u:%g:%a' -- \
    "${uninstall_changed_purge_binary}")"
# shellcheck disable=SC2016
printf '%s\n' \
    '#!/bin/sh' \
    'set -eu' \
    '/usr/bin/mv "$@"' \
    'last=' \
    'for last do :; done' \
    '/usr/bin/mv "${last}" "${FROST_TEST_UNINSTALL_CHANGED_PURGE_DISPLACED:?}"' \
    '/usr/bin/ln -s -- "${FROST_TEST_UNINSTALL_CHANGED_PURGE_VICTIM:?}" "${last}"' \
    >"${uninstall_changed_purge_tools}/mv"
# shellcheck disable=SC2016
printf '%s\n' \
    '#!/bin/sh' \
    'set -eu' \
    ': >"${FROST_TEST_UNINSTALL_CHANGED_PURGE_RM_MARKER:?}"' \
    'exec /usr/bin/rm "$@"' \
    >"${uninstall_changed_purge_tools}/rm"
chmod 0755 "${uninstall_changed_purge_tools}/mv" \
    "${uninstall_changed_purge_tools}/rm"
uninstall_changed_purge_output="$(
    env HOME="${TEST_HOME}" \
        PATH="${uninstall_changed_purge_tools}:${TEST_PATH}" \
        FROST_TEST_UNINSTALL_CHANGED_PURGE_DISPLACED="${uninstall_changed_purge_displaced}" \
        FROST_TEST_UNINSTALL_CHANGED_PURGE_VICTIM="${uninstall_changed_purge_victim}" \
        FROST_TEST_UNINSTALL_CHANGED_PURGE_RM_MARKER="${uninstall_changed_purge_rm_marker}" \
        DESTDIR="${uninstall_changed_purge_stage}" "${UNINSTALLER}" \
        --prefix "${uninstall_changed_purge_prefix}" 2>&1
)"
assert_absent "binary after changed committed quarantine" \
    "${uninstall_changed_purge_binary}"
mapfile -t uninstall_changed_purge_links < <(
    find "${uninstall_changed_purge_binary%/*}" -maxdepth 1 \
        -name '.frost.uninstall.*' -print
)
(( ${#uninstall_changed_purge_links[@]} == 1 )) \
    || fail "changed purge quarantine did not remain named"
uninstall_changed_purge_link="${uninstall_changed_purge_links[0]}"
[[ -L "${uninstall_changed_purge_link}" ]] \
    || fail "purge removed a changed quarantine symlink"
[[ "$(readlink -- "${uninstall_changed_purge_link}")" == \
    "${uninstall_changed_purge_victim}" ]] \
    || fail "changed purge quarantine link target changed"
assert_absent "rm call for changed purge quarantine" \
    "${uninstall_changed_purge_rm_marker}"
[[ "$(stat -c '%d:%i:%u:%g:%a' -- \
    "${uninstall_changed_purge_displaced}")" == \
    "${uninstall_changed_purge_identity}" ]] \
    || fail "changed purge lost the displaced original inode"
[[ -z "$(find "${uninstall_changed_purge_victim}" -mindepth 1 -print -quit)" ]] \
    || fail "changed purge followed the substitute symlink"
assert_contains "changed purge warning" "${uninstall_changed_purge_output}" \
    "refusing to purge changed quarantine for ${uninstall_changed_purge_binary}; unexpected entry retained at ${uninstall_changed_purge_link}"
[[ "${uninstall_changed_purge_output}" != \
    *"recovery after inspecting destination:"* ]] \
    || fail "changed quarantine was falsely advertised as the original recovery inode"
assert_contains "changed purge success summary" "${uninstall_changed_purge_output}" \
    "Removed frost from ${uninstall_changed_purge_prefix}/bin"

# A wrapper can also return failure after the requested unlink completed. Trust
# the observed absence: there is no recovery inode to retain or advertise.
uninstall_post_rm_tools="${TEST_ROOT}/uninstall-post-rm-tools"
uninstall_post_rm_stage="${TEST_ROOT}/uninstall-post-rm-stage"
uninstall_post_rm_prefix="/opt/frost-uninstall-post-rm"
uninstall_post_rm_binary="${uninstall_post_rm_stage}${uninstall_post_rm_prefix}/bin/frost"
mkdir -p "${uninstall_post_rm_tools}" "${uninstall_post_rm_binary%/*}"
printf 'purged before rm reports failure\n' >"${uninstall_post_rm_binary}"
# shellcheck disable=SC2016
printf '%s\n' \
    '#!/bin/sh' \
    'set -eu' \
    '/usr/bin/rm "$@"' \
    'exit 79' \
    >"${uninstall_post_rm_tools}/rm"
chmod 0755 "${uninstall_post_rm_tools}/rm"
uninstall_post_rm_output="$(
    env HOME="${TEST_HOME}" PATH="${uninstall_post_rm_tools}:${TEST_PATH}" \
        DESTDIR="${uninstall_post_rm_stage}" "${UNINSTALLER}" \
        --prefix "${uninstall_post_rm_prefix}" 2>&1
)"
assert_absent "binary after post-unlink rm failure" \
    "${uninstall_post_rm_binary}"
[[ -z "$(find "${uninstall_post_rm_stage}" \
    -name '*.uninstall.*' -print -quit)" ]] \
    || fail "post-unlink rm failure falsely retained a quarantine"
assert_contains "post-unlink rm warning" "${uninstall_post_rm_output}" \
    "purge reported failure after removing ${uninstall_post_rm_binary}"
[[ "${uninstall_post_rm_output}" != \
    *"recovery after inspecting destination:"* ]] \
    || fail "post-unlink rm failure printed a nonexistent recovery path"
assert_contains "post-unlink rm success summary" "${uninstall_post_rm_output}" \
    "Removed frost from ${uninstall_post_rm_prefix}/bin"

# Once every owned name has moved, uninstall is committed. A purge failure
# cannot honestly put that generation back; retain the exact inode under its
# named quarantine, report a copy-safe recovery command, and still report the
# target as uninstalled.
uninstall_purge_tools="${TEST_ROOT}/uninstall-purge-tools"
uninstall_purge_stage="${TEST_ROOT}/uninstall-purge-stage"
uninstall_purge_prefix="/opt/frost-uninstall-purge"
uninstall_purge_binary="${uninstall_purge_stage}${uninstall_purge_prefix}/bin/frost"
mkdir -p "${uninstall_purge_tools}" "${uninstall_purge_binary%/*}"
printf 'committed uninstall quarantine\n' >"${uninstall_purge_binary}"
uninstall_purge_identity="$(stat -c '%d:%i:%u:%g:%a' -- \
    "${uninstall_purge_binary}")"
# shellcheck disable=SC2016
printf '%s\n' \
    '#!/bin/sh' \
    'set -eu' \
    'for argument do' \
    '    case "${argument}" in *.uninstall.*) exit 77 ;; esac' \
    'done' \
    'exec /usr/bin/rm "$@"' \
    >"${uninstall_purge_tools}/rm"
chmod 0755 "${uninstall_purge_tools}/rm"
uninstall_purge_output="$(
    env HOME="${TEST_HOME}" PATH="${uninstall_purge_tools}:${TEST_PATH}" \
        DESTDIR="${uninstall_purge_stage}" "${UNINSTALLER}" \
        --prefix "${uninstall_purge_prefix}" 2>&1
)"
assert_absent "binary after committed uninstall" "${uninstall_purge_binary}"
mapfile -t uninstall_purge_quarantines < <(
    find "${uninstall_purge_binary%/*}" -maxdepth 1 \
        -name '.frost.uninstall.*' -print
)
(( ${#uninstall_purge_quarantines[@]} == 1 )) \
    || fail "purge failure did not retain exactly one quarantine"
uninstall_purge_quarantine="${uninstall_purge_quarantines[0]}"
[[ "$(<"${uninstall_purge_quarantine}")" == \
    'committed uninstall quarantine' ]] \
    || fail "purge failure changed retained quarantine content"
[[ "$(stat -c '%d:%i:%u:%g:%a' -- "${uninstall_purge_quarantine}")" == \
    "${uninstall_purge_identity}" ]] \
    || fail "purge failure changed retained quarantine inode metadata"
assert_contains "purge failure warning" "${uninstall_purge_output}" \
    "quarantine retained at ${uninstall_purge_quarantine}"
assert_contains "purge recovery command" "${uninstall_purge_output}" \
    "recovery after inspecting destination: mv -fT -- ${uninstall_purge_quarantine} ${uninstall_purge_binary}"
assert_contains "purge failure success summary" "${uninstall_purge_output}" \
    "Removed frost from ${uninstall_purge_prefix}/bin"

# Empty-directory cleanup is also post-commit and non-recursive. Its failure
# may leave the harmless directory, but must not reverse the committed file
# removal or turn the truthful uninstall summary into an error exit.
uninstall_rmdir_tools="${TEST_ROOT}/uninstall-rmdir-tools"
uninstall_rmdir_stage="${TEST_ROOT}/uninstall-rmdir-stage"
uninstall_rmdir_prefix="/opt/frost-uninstall-rmdir"
uninstall_rmdir_binary="${uninstall_rmdir_stage}${uninstall_rmdir_prefix}/bin/frost"
uninstall_rmdir_target="${uninstall_rmdir_stage}${uninstall_rmdir_prefix}/share/frost/workflows"
mkdir -p "${uninstall_rmdir_tools}" "${uninstall_rmdir_binary%/*}" \
    "${uninstall_rmdir_target}"
printf 'binary before non-fatal rmdir failure\n' >"${uninstall_rmdir_binary}"
printf '%s\n' '#!/bin/sh' 'exit 78' >"${uninstall_rmdir_tools}/rmdir"
chmod 0755 "${uninstall_rmdir_tools}/rmdir"
uninstall_rmdir_output="$(
    env HOME="${TEST_HOME}" PATH="${uninstall_rmdir_tools}:${TEST_PATH}" \
        DESTDIR="${uninstall_rmdir_stage}" "${UNINSTALLER}" \
        --prefix "${uninstall_rmdir_prefix}" 2>&1
)"
assert_absent "binary after non-fatal rmdir failure" "${uninstall_rmdir_binary}"
[[ -d "${uninstall_rmdir_target}" ]] \
    || fail "failed rmdir unexpectedly removed its cleanup directory"
assert_contains "non-fatal rmdir warning" "${uninstall_rmdir_output}" \
    "could not remove empty cleanup directory ${uninstall_rmdir_target} (non-fatal)"
assert_contains "rmdir failure success summary" "${uninstall_rmdir_output}" \
    "Removed frost from ${uninstall_rmdir_prefix}/bin"
[[ -z "$(find "${uninstall_rmdir_stage}" \
    -name '*.uninstall.*' -print -quit)" ]] \
    || fail "non-fatal rmdir failure left a quarantine"

uninstall_link_stage="${TEST_ROOT}/uninstall-link-stage"
uninstall_link_victim="${TEST_ROOT}/uninstall-link-victim"
uninstall_link_prefix="/opt/frost-uninstall-link"
mkdir -p "${uninstall_link_stage}" \
    "${uninstall_link_victim}/frost-uninstall-link/bin"
printf 'outside frost\n' \
    >"${uninstall_link_victim}/frost-uninstall-link/bin/frost"
ln -s -- "${uninstall_link_victim}" "${uninstall_link_stage}/opt"
if env HOME="${TEST_HOME}" PATH="${TEST_PATH}" \
    DESTDIR="${uninstall_link_stage}" "${UNINSTALLER}" \
    --prefix "${uninstall_link_prefix}" >"${TEST_ROOT}/uninstall-link.log" 2>&1; then
    fail "uninstaller followed a symbolic-link ancestor below DESTDIR"
fi
assert_contains "uninstall ancestor diagnostic" \
    "$(<"${TEST_ROOT}/uninstall-link.log")" \
    "staged uninstall path contains a symbolic-link ancestor"
[[ "$(<"${uninstall_link_victim}/frost-uninstall-link/bin/frost")" == \
    'outside frost' ]] || fail "uninstaller removed a file outside DESTDIR"

# A late unsafe target must reject the whole plan before an earlier safe binary
# is removed. The old per-target validation discovered this share symlink only
# after deleting PREFIX/bin/frost.
late_link_stage="${TEST_ROOT}/late-uninstall-link-stage"
late_link_victim="${TEST_ROOT}/late-uninstall-link-victim"
late_link_prefix="/opt/frost-late-uninstall-link"
late_link_binary="${late_link_stage}${late_link_prefix}/bin/frost"
late_link_rm_marker="${TEST_ROOT}/late-uninstall-link-rm-called"
mkdir -p "${late_link_binary%/*}" "${late_link_victim}"
printf 'installed frost before rejected uninstall\n' >"${late_link_binary}"
ln -s -- "${late_link_victim}" \
    "${late_link_stage}${late_link_prefix}/share"
if env HOME="${TEST_HOME}" \
    PATH="${uninstall_special_tools}:${TEST_PATH}" \
    FROST_TEST_RM_MARKER="${late_link_rm_marker}" \
    DESTDIR="${late_link_stage}" "${UNINSTALLER}" \
    --prefix "${late_link_prefix}" >"${TEST_ROOT}/late-uninstall-link.log" 2>&1; then
    fail "uninstaller accepted a late symbolic-link ancestor below DESTDIR"
fi
assert_contains "late uninstall ancestor diagnostic" \
    "$(<"${TEST_ROOT}/late-uninstall-link.log")" \
    "staged uninstall path contains a symbolic-link ancestor"
assert_regular_file "binary preserved after whole-plan rejection" \
    "${late_link_binary}"
assert_absent "rm call before late ancestor rejection" "${late_link_rm_marker}"
[[ "$(<"${late_link_binary}")" == \
    'installed frost before rejected uninstall' ]] \
    || fail "late uninstall preflight changed the existing binary"
[[ -z "$(find "${late_link_victim}" -mindepth 1 -print -quit)" ]] \
    || fail "late uninstall preflight removed outside DESTDIR"

# A point-in-time preflight can become stale. Replace the later share ancestor
# only after the binary has moved to quarantine; the use-point check must abort
# and the transaction must restore that exact binary rather than leave a
# partial uninstall or follow the new link.
uninstall_race_tools="${TEST_ROOT}/uninstall-race-tools"
uninstall_race_stage="${TEST_ROOT}/uninstall-race-stage"
uninstall_race_prefix="/opt/frost-uninstall-race"
uninstall_race_binary="${uninstall_race_stage}${uninstall_race_prefix}/bin/frost"
uninstall_race_share="${uninstall_race_stage}${uninstall_race_prefix}/share"
uninstall_race_victim="${TEST_ROOT}/uninstall-race-victim"
uninstall_race_marker="${TEST_ROOT}/uninstall-race-moved"
mkdir -p "${uninstall_race_tools}" "${uninstall_race_binary%/*}" \
    "${uninstall_race_share}" "${uninstall_race_victim}"
printf 'binary restored after ancestor replacement\n' >"${uninstall_race_binary}"
uninstall_race_identity="$(stat -c '%d:%i:%u:%g:%a' -- \
    "${uninstall_race_binary}")"
# shellcheck disable=SC2016
printf '%s\n' \
    '#!/bin/sh' \
    'set -eu' \
    'if [ ! -e "${FROST_TEST_UNINSTALL_RACE_MARKER:?}" ]; then' \
    '    /usr/bin/mv "$@"' \
    '    : >"${FROST_TEST_UNINSTALL_RACE_MARKER}"' \
    '    /usr/bin/rmdir "${FROST_TEST_UNINSTALL_RACE_ANCESTOR:?}"' \
    '    /usr/bin/ln -s -- "${FROST_TEST_UNINSTALL_RACE_VICTIM:?}" "${FROST_TEST_UNINSTALL_RACE_ANCESTOR}"' \
    '    exit 0' \
    'fi' \
    'exec /usr/bin/mv "$@"' \
    >"${uninstall_race_tools}/mv"
chmod 0755 "${uninstall_race_tools}/mv"
if env HOME="${TEST_HOME}" PATH="${uninstall_race_tools}:${TEST_PATH}" \
    FROST_TEST_UNINSTALL_RACE_MARKER="${uninstall_race_marker}" \
    FROST_TEST_UNINSTALL_RACE_ANCESTOR="${uninstall_race_share}" \
    FROST_TEST_UNINSTALL_RACE_VICTIM="${uninstall_race_victim}" \
    DESTDIR="${uninstall_race_stage}" "${UNINSTALLER}" \
    --prefix "${uninstall_race_prefix}" \
    >"${TEST_ROOT}/uninstall-race.log" 2>&1; then
    fail "uninstaller accepted an ancestor replacement during quarantine"
fi
assert_contains "uninstall use-point ancestor diagnostic" \
    "$(<"${TEST_ROOT}/uninstall-race.log")" \
    "staged uninstall path contains a symbolic-link ancestor: ${uninstall_race_share}"
[[ "$(<"${uninstall_race_binary}")" == \
    'binary restored after ancestor replacement' ]] \
    || fail "ancestor replacement rollback changed the binary"
[[ "$(stat -c '%d:%i:%u:%g:%a' -- "${uninstall_race_binary}")" == \
    "${uninstall_race_identity}" ]] \
    || fail "ancestor replacement rollback changed binary inode metadata"
[[ -L "${uninstall_race_share}" ]] \
    || fail "test ancestor replacement did not occur"
[[ -z "$(find "${uninstall_race_victim}" -mindepth 1 -print -quit)" ]] \
    || fail "uninstaller followed a replaced ancestor"
[[ -z "$(find "${uninstall_race_stage}" \
    -name '*.uninstall.*' -print -quit)" ]] \
    || fail "ancestor replacement rollback left a quarantine"

# Normalize `link/.` and repeated-separator DESTDIR spellings before walking
# the complete existing root chain. Neither install nor uninstall may reach
# the directory behind such a root symlink.
root_link="${TEST_ROOT}/destdir-root-link"
root_victim="${TEST_ROOT}/destdir-root-victim"
root_prefix="/opt/frost-destdir-root"
root_binary="${root_victim}${root_prefix}/bin/frost"
mkdir -p "${root_victim}"
ln -s -- "${root_victim}" "${root_link}"
if env HOME="${TEST_HOME}" PATH="${TEST_PATH}" DESTDIR="${root_link}/." \
    "${INSTALLER}" --binary "${prebuilt_binary}" --prefix "${root_prefix}" \
    --no-desktop >"${TEST_ROOT}/root-link-install.log" 2>&1; then
    fail "installer accepted a symlinked DESTDIR root disguised with /."
fi
assert_contains "symlinked DESTDIR install diagnostic" \
    "$(<"${TEST_ROOT}/root-link-install.log")" \
    "DESTDIR path contains a symbolic-link component"
[[ -z "$(find "${root_victim}" -mindepth 1 -print -quit)" ]] \
    || fail "symlinked DESTDIR install wrote outside its staging boundary"

mkdir -p "$(dirname -- "${root_binary}")"
printf 'outside root frost\n' >"${root_binary}"
if env HOME="${TEST_HOME}" PATH="${TEST_PATH}" DESTDIR="${root_link}//" \
    "${UNINSTALLER}" --prefix "${root_prefix}" \
    >"${TEST_ROOT}/root-link-uninstall.log" 2>&1; then
    fail "uninstaller accepted a symlinked DESTDIR root with trailing separators"
fi
assert_contains "symlinked DESTDIR uninstall diagnostic" \
    "$(<"${TEST_ROOT}/root-link-uninstall.log")" \
    "DESTDIR path contains a symbolic-link component"
[[ "$(<"${root_binary}")" == 'outside root frost' ]] \
    || fail "symlinked DESTDIR uninstall removed an outside binary"

printf 'install/uninstall path contract: ok\n'
