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

assert_before() {
    local label="$1" output="$2" first="$3" second="$4"
    case "${output}" in
        *"${first}"*"${second}"*) ;;
        *) fail "${label} did not report ${first@Q} before ${second@Q}" ;;
    esac
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

extract_symlink_recovery_command() {
    local label="$1" log="$2" line command="" count=0
    local prefix='frost install: recovery command (symlink contents not displayed): '
    while IFS= read -r line; do
        case "${line}" in
            "${prefix}"*)
                count=$((count + 1))
                command="${line#"${prefix}"}"
                ;;
        esac
    done <"${log}"
    ((count == 1)) \
        || fail "${label} emitted ${count} recovery commands instead of one"
    printf '%s' "${command}"
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
    'exec /usr/bin/cat "$@"' \
    >"${stage_failure_tools}/cat"
chmod 0755 "${stage_failure_tools}/cat"
if env HOME="${TEST_HOME}" PATH="${stage_failure_tools}:${TEST_PATH}" \
    DESTDIR="${stage_failure_root}" "${INSTALLER}" \
    --binary "${prebuilt_binary}" --prefix "${stage_failure_prefix}" \
    >"${TEST_ROOT}/stage-failure.log" 2>&1; then
    fail "installer accepted a failure while staging the final icon"
fi
assert_contains "late staging failure diagnostic" \
    "$(<"${TEST_ROOT}/stage-failure.log")" \
    "cannot copy staged content for ${stage_failure_root}${stage_failure_prefix}/share/icons/hicolor/256x256/apps/${app_id}.png"
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

# The core install transaction binds each physical destination directory before
# it creates a staging name. Replacing the logical workflow directory from
# inside mktemp must create and clean the temporary only in the displaced inode;
# the replacement symlink and its referent remain untouched.
install_stage_parent_tools="${TEST_ROOT}/install-stage-parent-tools"
install_stage_parent_stage="${TEST_ROOT}/install-stage-parent-stage"
install_stage_parent_prefix="/opt/frost-install-stage-parent"
install_stage_parent_dir="${install_stage_parent_stage}${install_stage_parent_prefix}/share/frost/workflows"
install_stage_parent_displaced="${TEST_ROOT}/install-stage-parent-original-workflows"
install_stage_parent_victim="${TEST_ROOT}/install-stage-parent-victim"
install_stage_parent_marker="${TEST_ROOT}/install-stage-parent-replaced"
install_stage_parent_name="${WORKFLOW_SOURCES[0]##*/}"
install_stage_parent_target="${install_stage_parent_dir}/${install_stage_parent_name}"
install_stage_parent_binary="${install_stage_parent_stage}${install_stage_parent_prefix}/bin/frost"
mkdir -p "${install_stage_parent_tools}" "${install_stage_parent_dir}" \
    "${install_stage_parent_victim}"
printf 'original workflow before staging parent swap\n' \
    >"${install_stage_parent_target}"
printf 'outside workflow staging sentinel\n' \
    >"${install_stage_parent_victim}/${install_stage_parent_name}"
install_stage_parent_identity="$(stat -c '%d:%i:%u:%g:%a' -- \
    "${install_stage_parent_target}")"
# shellcheck disable=SC2016
printf '%s\n' \
    '#!/bin/sh' \
    'set -eu' \
    'if [ ! -e "${FROST_TEST_INSTALL_STAGE_PARENT_MARKER:?}" ]; then' \
    '    : >"${FROST_TEST_INSTALL_STAGE_PARENT_MARKER}"' \
    '    /usr/bin/mv "${FROST_TEST_INSTALL_STAGE_PARENT_DIR:?}" "${FROST_TEST_INSTALL_STAGE_PARENT_DISPLACED:?}"' \
    '    /usr/bin/ln -s -- "${FROST_TEST_INSTALL_STAGE_PARENT_VICTIM:?}" "${FROST_TEST_INSTALL_STAGE_PARENT_DIR}"' \
    'fi' \
    'exec /usr/bin/mktemp "$@"' \
    >"${install_stage_parent_tools}/mktemp"
chmod 0755 "${install_stage_parent_tools}/mktemp"
if env HOME="${TEST_HOME}" PATH="${install_stage_parent_tools}:${TEST_PATH}" \
    FROST_TEST_INSTALL_STAGE_PARENT_MARKER="${install_stage_parent_marker}" \
    FROST_TEST_INSTALL_STAGE_PARENT_DIR="${install_stage_parent_dir}" \
    FROST_TEST_INSTALL_STAGE_PARENT_DISPLACED="${install_stage_parent_displaced}" \
    FROST_TEST_INSTALL_STAGE_PARENT_VICTIM="${install_stage_parent_victim}" \
    DESTDIR="${install_stage_parent_stage}" "${INSTALLER}" \
    --binary "${prebuilt_binary}" --prefix "${install_stage_parent_prefix}" \
    --no-desktop >"${TEST_ROOT}/install-stage-parent.log" 2>&1; then
    fail "installer accepted a parent replacement during staging mktemp"
fi
assert_contains "install staging parent diagnostic" \
    "$(<"${TEST_ROOT}/install-stage-parent.log")" \
    "install destination directory changed while staging: ${install_stage_parent_dir}"
[[ -L "${install_stage_parent_dir}" ]] \
    || fail "install staging mktemp did not replace its logical parent"
[[ "$(<"${install_stage_parent_victim}/${install_stage_parent_name}")" == \
    'outside workflow staging sentinel' ]] \
    || fail "install staging mktemp touched the replacement parent referent"
[[ "$(<"${install_stage_parent_displaced}/${install_stage_parent_name}")" == \
    'original workflow before staging parent swap' ]] \
    || fail "install staging failure changed the original workflow"
[[ "$(stat -c '%d:%i:%u:%g:%a' -- \
    "${install_stage_parent_displaced}/${install_stage_parent_name}")" == \
    "${install_stage_parent_identity}" ]] \
    || fail "install staging failure changed original workflow inode metadata"
[[ -z "$(find "${install_stage_parent_displaced}" \
    \( -name '*.install.*' -o -name '*.rollback.*' \) -print -quit)" ]] \
    || fail "bound staging cleanup left an artifact in the displaced parent"
assert_absent "binary after staging parent swap" "${install_stage_parent_binary}"

# Backup reservations and snapshots use the same bound directory. Swap the
# logical parent from inside ln: the backup is made beside the original inode,
# then the use-point identity check aborts. Cleanup now fails closed on that
# directory identity change and retains the exact backup without touching an
# identically named file behind the new symlink.
install_backup_parent_tools="${TEST_ROOT}/install-backup-parent-tools"
install_backup_parent_stage="${TEST_ROOT}/install-backup-parent-stage"
install_backup_parent_prefix="/opt/frost-install-backup-parent"
install_backup_parent_dir="${install_backup_parent_stage}${install_backup_parent_prefix}/share/frost/workflows"
install_backup_parent_displaced="${TEST_ROOT}/install-backup-parent-original-workflows"
install_backup_parent_victim="${TEST_ROOT}/install-backup-parent-victim"
install_backup_parent_name="${WORKFLOW_SOURCES[0]##*/}"
install_backup_parent_target="${install_backup_parent_dir}/${install_backup_parent_name}"
install_backup_parent_binary="${install_backup_parent_stage}${install_backup_parent_prefix}/bin/frost"
mkdir -p "${install_backup_parent_tools}" "${install_backup_parent_dir}" \
    "${install_backup_parent_victim}"
printf 'original workflow before backup parent swap\n' \
    >"${install_backup_parent_target}"
printf 'outside workflow backup sentinel\n' \
    >"${install_backup_parent_victim}/${install_backup_parent_name}"
install_backup_parent_identity="$(stat -c '%d:%i:%u:%g:%a' -- \
    "${install_backup_parent_target}")"
# shellcheck disable=SC2016
printf '%s\n' \
    '#!/bin/sh' \
    'set -eu' \
    '/usr/bin/mv "${FROST_TEST_INSTALL_BACKUP_PARENT_DIR:?}" "${FROST_TEST_INSTALL_BACKUP_PARENT_DISPLACED:?}"' \
    '/usr/bin/ln -s -- "${FROST_TEST_INSTALL_BACKUP_PARENT_VICTIM:?}" "${FROST_TEST_INSTALL_BACKUP_PARENT_DIR}"' \
    'exec /usr/bin/ln "$@"' \
    >"${install_backup_parent_tools}/ln"
chmod 0755 "${install_backup_parent_tools}/ln"
if env HOME="${TEST_HOME}" PATH="${install_backup_parent_tools}:${TEST_PATH}" \
    FROST_TEST_INSTALL_BACKUP_PARENT_DIR="${install_backup_parent_dir}" \
    FROST_TEST_INSTALL_BACKUP_PARENT_DISPLACED="${install_backup_parent_displaced}" \
    FROST_TEST_INSTALL_BACKUP_PARENT_VICTIM="${install_backup_parent_victim}" \
    DESTDIR="${install_backup_parent_stage}" "${INSTALLER}" \
    --binary "${prebuilt_binary}" --prefix "${install_backup_parent_prefix}" \
    --no-desktop >"${TEST_ROOT}/install-backup-parent.log" 2>&1; then
    fail "installer accepted a parent replacement during backup ln"
fi
assert_contains "install backup parent diagnostic" \
    "$(<"${TEST_ROOT}/install-backup-parent.log")" \
    "install destination directory changed while backing up: ${install_backup_parent_dir}"
[[ -L "${install_backup_parent_dir}" ]] \
    || fail "install backup ln did not replace its logical parent"
[[ "$(<"${install_backup_parent_victim}/${install_backup_parent_name}")" == \
    'outside workflow backup sentinel' ]] \
    || fail "install backup ln touched the replacement parent referent"
[[ "$(<"${install_backup_parent_displaced}/${install_backup_parent_name}")" == \
    'original workflow before backup parent swap' ]] \
    || fail "install backup failure changed the original workflow"
[[ "$(stat -c '%d:%i:%u:%g:%a' -- \
    "${install_backup_parent_displaced}/${install_backup_parent_name}")" == \
    "${install_backup_parent_identity}" ]] \
    || fail "install backup failure changed original workflow inode metadata"
install_backup_parent_recovery="$(find \
    "${install_backup_parent_displaced}" -maxdepth 1 \
    -name ".${install_backup_parent_name}.rollback.??????" -print -quit)"
[[ -n "${install_backup_parent_recovery}" \
    && -f "${install_backup_parent_recovery}" ]] \
    || fail "bound backup failure lost the exact rollback backup"
[[ "$(stat -c '%d:%i:%u:%g:%a' -- \
    "${install_backup_parent_recovery}")" == \
    "${install_backup_parent_identity}" ]] \
    || fail "bound backup failure changed the retained rollback identity"
[[ -z "$(find "${install_backup_parent_displaced}" \
    -name '*.install.*' -print -quit)" ]] \
    || fail "bound backup failure left a temporary"
assert_contains "install backup parent cleanup diagnostic" \
    "$(<"${TEST_ROOT}/install-backup-parent.log")" \
    "skipped rollback backup cleanup because destination directory identity changed (non-fatal): ${install_backup_parent_dir}"
assert_absent "binary after backup parent swap" "${install_backup_parent_binary}"

# Swap the binary directory inside its final publish rename. The bound rename
# lands in the displaced directory, the post-rename identity check fails, and
# rollback removes that newly introduced inode there—not the same name exposed
# by the replacement symlink.
install_publish_parent_tools="${TEST_ROOT}/install-publish-parent-tools"
install_publish_parent_stage="${TEST_ROOT}/install-publish-parent-stage"
install_publish_parent_prefix="/opt/frost-install-publish-parent"
install_publish_parent_dir="${install_publish_parent_stage}${install_publish_parent_prefix}/bin"
install_publish_parent_displaced="${TEST_ROOT}/install-publish-parent-original-bin"
install_publish_parent_victim="${TEST_ROOT}/install-publish-parent-victim"
mkdir -p "${install_publish_parent_tools}" \
    "${install_publish_parent_victim}"
printf 'outside binary publish sentinel\n' \
    >"${install_publish_parent_victim}/frost"
printf 'outside rollback-name sentinel\n' \
    >"${install_publish_parent_victim}/.frost.rollback.outside"
# shellcheck disable=SC2016
printf '%s\n' \
    '#!/bin/sh' \
    'set -eu' \
    'last=' \
    'for argument do last=${argument}; done' \
    'case "${last}" in' \
    '    /proc/self/fd/*/frost)' \
    '        /usr/bin/mv "${FROST_TEST_INSTALL_PUBLISH_PARENT_DIR:?}" "${FROST_TEST_INSTALL_PUBLISH_PARENT_DISPLACED:?}"' \
    '        /usr/bin/ln -s -- "${FROST_TEST_INSTALL_PUBLISH_PARENT_VICTIM:?}" "${FROST_TEST_INSTALL_PUBLISH_PARENT_DIR}"' \
    '        ;;' \
    'esac' \
    'exec /usr/bin/mv "$@"' \
    >"${install_publish_parent_tools}/mv"
chmod 0755 "${install_publish_parent_tools}/mv"
if env HOME="${TEST_HOME}" PATH="${install_publish_parent_tools}:${TEST_PATH}" \
    FROST_TEST_INSTALL_PUBLISH_PARENT_DIR="${install_publish_parent_dir}" \
    FROST_TEST_INSTALL_PUBLISH_PARENT_DISPLACED="${install_publish_parent_displaced}" \
    FROST_TEST_INSTALL_PUBLISH_PARENT_VICTIM="${install_publish_parent_victim}" \
    DESTDIR="${install_publish_parent_stage}" "${INSTALLER}" \
    --binary "${prebuilt_binary}" --prefix "${install_publish_parent_prefix}" \
    --no-desktop >"${TEST_ROOT}/install-publish-parent.log" 2>&1; then
    fail "installer accepted a parent replacement during publish mv"
fi
assert_contains "install publish parent diagnostic" \
    "$(<"${TEST_ROOT}/install-publish-parent.log")" \
    "install destination directory changed during publish: ${install_publish_parent_dir}"
[[ -L "${install_publish_parent_dir}" ]] \
    || fail "install publish mv did not replace its logical parent"
[[ "$(<"${install_publish_parent_victim}/frost")" == \
    'outside binary publish sentinel' ]] \
    || fail "bound install rollback removed the replacement binary"
[[ "$(<"${install_publish_parent_victim}/.frost.rollback.outside")" == \
    'outside rollback-name sentinel' ]] \
    || fail "bound install rollback touched an outside rollback name"
assert_absent "rolled-back binary in displaced parent" \
    "${install_publish_parent_displaced}/frost"
[[ -z "$(find "${install_publish_parent_displaced}" \
    \( -name '*.install.*' -o -name '*.rollback.*' \) -print -quit)" ]] \
    || fail "bound publish rollback left an artifact in the displaced parent"

# A successful generation commit owns its rollback backup until cleanup. Swap
# the logical bin directory from the cleanup rm itself: removal stays bound to
# the old inode, installation still succeeds, and neither the retained binary
# nor the replacement referent is polluted by a stale backup.
install_cleanup_parent_tools="${TEST_ROOT}/install-cleanup-parent-tools"
install_cleanup_parent_stage="${TEST_ROOT}/install-cleanup-parent-stage"
install_cleanup_parent_prefix="/opt/frost-install-cleanup-parent"
install_cleanup_parent_dir="${install_cleanup_parent_stage}${install_cleanup_parent_prefix}/bin"
install_cleanup_parent_displaced="${TEST_ROOT}/install-cleanup-parent-original-bin"
install_cleanup_parent_victim="${TEST_ROOT}/install-cleanup-parent-victim"
install_cleanup_parent_binary="${install_cleanup_parent_dir}/frost"
install_cleanup_parent_state="${TEST_ROOT}/install-cleanup-parent-rm-count"
mkdir -p "${install_cleanup_parent_tools}" "${install_cleanup_parent_dir}" \
    "${install_cleanup_parent_victim}"
printf 'old binary before bound backup cleanup\n' \
    >"${install_cleanup_parent_binary}"
printf 'outside binary cleanup sentinel\n' \
    >"${install_cleanup_parent_victim}/frost"
printf 'outside cleanup-name sentinel\n' \
    >"${install_cleanup_parent_victim}/.frost.rollback.outside"
# shellcheck disable=SC2016
printf '%s\n' \
    '#!/bin/sh' \
    'set -eu' \
    'last=' \
    'for argument do last=${argument}; done' \
    'case "${last}" in' \
    '    /proc/self/fd/*/.frost.rollback.*)' \
    '        state=${FROST_TEST_INSTALL_CLEANUP_PARENT_STATE:?}' \
    '        count=0' \
    '        [ ! -f "${state}" ] || read -r count <"${state}"' \
    '        count=$((count + 1))' \
    '        printf "%s\n" "${count}" >"${state}"' \
    '        if [ "${count}" -eq 2 ]; then' \
    '            /usr/bin/mv "${FROST_TEST_INSTALL_CLEANUP_PARENT_DIR:?}" "${FROST_TEST_INSTALL_CLEANUP_PARENT_DISPLACED:?}"' \
    '            /usr/bin/ln -s -- "${FROST_TEST_INSTALL_CLEANUP_PARENT_VICTIM:?}" "${FROST_TEST_INSTALL_CLEANUP_PARENT_DIR}"' \
    '        fi' \
    '        ;;' \
    'esac' \
    'exec /usr/bin/rm "$@"' \
    >"${install_cleanup_parent_tools}/rm"
chmod 0755 "${install_cleanup_parent_tools}/rm"
install_cleanup_parent_output="$(
    env HOME="${TEST_HOME}" PATH="${install_cleanup_parent_tools}:${TEST_PATH}" \
        FROST_TEST_INSTALL_CLEANUP_PARENT_STATE="${install_cleanup_parent_state}" \
        FROST_TEST_INSTALL_CLEANUP_PARENT_DIR="${install_cleanup_parent_dir}" \
        FROST_TEST_INSTALL_CLEANUP_PARENT_DISPLACED="${install_cleanup_parent_displaced}" \
        FROST_TEST_INSTALL_CLEANUP_PARENT_VICTIM="${install_cleanup_parent_victim}" \
        DESTDIR="${install_cleanup_parent_stage}" "${INSTALLER}" \
        --binary "${prebuilt_binary}" --prefix "${install_cleanup_parent_prefix}" \
        --no-desktop 2>&1
)"
[[ -L "${install_cleanup_parent_dir}" ]] \
    || fail "install backup cleanup did not replace its logical parent"
cmp -- "${prebuilt_binary}" "${install_cleanup_parent_displaced}/frost" \
    || fail "successful bound cleanup did not retain the installed binary"
[[ "$(<"${install_cleanup_parent_victim}/frost")" == \
    'outside binary cleanup sentinel' ]] \
    || fail "successful bound cleanup touched the replacement binary"
[[ "$(<"${install_cleanup_parent_victim}/.frost.rollback.outside")" == \
    'outside cleanup-name sentinel' ]] \
    || fail "successful bound cleanup touched an outside backup name"
[[ -z "$(find "${install_cleanup_parent_displaced}" \
    \( -name '*.install.*' -o -name '*.rollback.*' \) -print -quit)" ]] \
    || fail "successful install left a backup in the displaced parent"
assert_contains "install backup cleanup parent warning" \
    "${install_cleanup_parent_output}" \
    "destination directory changed during bound artifact cleanup (non-fatal): ${install_cleanup_parent_dir}"
assert_contains "install backup cleanup success summary" \
    "${install_cleanup_parent_output}" \
    "Installed frost to ${install_cleanup_parent_prefix}/bin/frost"

# Once the rollback reservation has been unlinked, its name must be absent
# before it can be reused. Inject a symlink after rm returns successfully: the
# installer must abort, mark that name unowned, and leave the substitute for an
# operator rather than overwrite it with ln/cp or delete it from the exit trap.
install_reservation_aba_tools="${TEST_ROOT}/install-reservation-aba-tools"
install_reservation_aba_stage="${TEST_ROOT}/install-reservation-aba-stage"
install_reservation_aba_prefix="/opt/frost-install-reservation-aba"
install_reservation_aba_binary="${install_reservation_aba_stage}${install_reservation_aba_prefix}/bin/frost"
install_reservation_aba_victim="${TEST_ROOT}/install-reservation-aba-victim"
install_reservation_aba_path_log="${TEST_ROOT}/install-reservation-aba-path"
mkdir -p "${install_reservation_aba_tools}" \
    "${install_reservation_aba_binary%/*}"
printf 'old binary before reservation ABA\n' \
    >"${install_reservation_aba_binary}"
printf 'reservation ABA victim sentinel\n' \
    >"${install_reservation_aba_victim}"
install_reservation_aba_identity="$(stat -c '%d:%i:%u:%g:%a' -- \
    "${install_reservation_aba_binary}")"
# shellcheck disable=SC2016
printf '%s\n' \
    '#!/bin/sh' \
    'set -eu' \
    'last=' \
    'for argument do last=${argument}; done' \
    'case "${last}" in' \
    '    /proc/self/fd/*/.frost.rollback.*)' \
    '        /usr/bin/rm "$@"' \
    '        parent=$(/usr/bin/readlink -- "${last%/*}")' \
    '        printf "%s/%s\n" "${parent}" "${last##*/}" >"${FROST_TEST_INSTALL_RESERVATION_ABA_PATH_LOG:?}"' \
    '        /usr/bin/ln -s -- "${FROST_TEST_INSTALL_RESERVATION_ABA_VICTIM:?}" "${last}"' \
    '        exit 0' \
    '        ;;' \
    'esac' \
    'exec /usr/bin/rm "$@"' \
    >"${install_reservation_aba_tools}/rm"
chmod 0755 "${install_reservation_aba_tools}/rm"
if env HOME="${TEST_HOME}" PATH="${install_reservation_aba_tools}:${TEST_PATH}" \
    FROST_TEST_INSTALL_RESERVATION_ABA_PATH_LOG="${install_reservation_aba_path_log}" \
    FROST_TEST_INSTALL_RESERVATION_ABA_VICTIM="${install_reservation_aba_victim}" \
    DESTDIR="${install_reservation_aba_stage}" "${INSTALLER}" \
    --binary "${prebuilt_binary}" --prefix "${install_reservation_aba_prefix}" \
    --no-desktop >"${TEST_ROOT}/install-reservation-aba.log" 2>&1; then
    fail "installer reused a replaced rollback reservation name"
fi
install_reservation_aba_path="$(<"${install_reservation_aba_path_log}")"
assert_contains "rollback reservation ABA diagnostic" \
    "$(<"${TEST_ROOT}/install-reservation-aba.log")" \
    "rollback reservation name changed while removing it beside ${install_reservation_aba_binary}"
[[ -L "${install_reservation_aba_path}" ]] \
    || fail "installer deleted the rollback reservation substitute"
[[ "$(readlink -- "${install_reservation_aba_path}")" == \
    "${install_reservation_aba_victim}" ]] \
    || fail "installer changed the rollback reservation substitute"
[[ "$(<"${install_reservation_aba_victim}")" == \
    'reservation ABA victim sentinel' ]] \
    || fail "installer followed the rollback reservation substitute"
[[ "$(stat -c '%d:%i:%u:%g:%a' -- \
    "${install_reservation_aba_binary}")" == \
    "${install_reservation_aba_identity}" ]] \
    || fail "rollback reservation ABA changed the original binary inode"
[[ "$(<"${install_reservation_aba_binary}")" == \
    'old binary before reservation ABA' ]] \
    || fail "rollback reservation ABA changed the original binary"

# A successful hardlink has a known identity: it must be the original target's
# inode. Replace that link before ln returns zero and prove the unexpected name
# is neither accepted as a backup nor removed by cleanup.
install_backup_aba_tools="${TEST_ROOT}/install-backup-aba-tools"
install_backup_aba_stage="${TEST_ROOT}/install-backup-aba-stage"
install_backup_aba_prefix="/opt/frost-install-backup-aba"
install_backup_aba_binary="${install_backup_aba_stage}${install_backup_aba_prefix}/bin/frost"
install_backup_aba_victim="${TEST_ROOT}/install-backup-aba-victim"
install_backup_aba_path_log="${TEST_ROOT}/install-backup-aba-path"
mkdir -p "${install_backup_aba_tools}" "${install_backup_aba_binary%/*}"
printf 'old binary before backup ABA\n' >"${install_backup_aba_binary}"
printf 'backup ABA victim sentinel\n' >"${install_backup_aba_victim}"
install_backup_aba_identity="$(stat -c '%d:%i:%u:%g:%a' -- \
    "${install_backup_aba_binary}")"
# shellcheck disable=SC2016
printf '%s\n' \
    '#!/bin/sh' \
    'set -eu' \
    '/usr/bin/ln "$@"' \
    'last=' \
    'for argument do last=${argument}; done' \
    'parent=$(/usr/bin/readlink -- "${last%/*}")' \
    'printf "%s/%s\n" "${parent}" "${last##*/}" >"${FROST_TEST_INSTALL_BACKUP_ABA_PATH_LOG:?}"' \
    '/usr/bin/rm -f -- "${last}"' \
    '/usr/bin/ln -s -- "${FROST_TEST_INSTALL_BACKUP_ABA_VICTIM:?}" "${last}"' \
    'exit 0' \
    >"${install_backup_aba_tools}/ln"
chmod 0755 "${install_backup_aba_tools}/ln"
if env HOME="${TEST_HOME}" PATH="${install_backup_aba_tools}:${TEST_PATH}" \
    FROST_TEST_INSTALL_BACKUP_ABA_PATH_LOG="${install_backup_aba_path_log}" \
    FROST_TEST_INSTALL_BACKUP_ABA_VICTIM="${install_backup_aba_victim}" \
    DESTDIR="${install_backup_aba_stage}" "${INSTALLER}" \
    --binary "${prebuilt_binary}" --prefix "${install_backup_aba_prefix}" \
    --no-desktop >"${TEST_ROOT}/install-backup-aba.log" 2>&1; then
    fail "installer accepted a replaced rollback backup name"
fi
install_backup_aba_path="$(<"${install_backup_aba_path_log}")"
assert_contains "rollback backup ABA diagnostic" \
    "$(<"${TEST_ROOT}/install-backup-aba.log")" \
    "rollback backup name was replaced while linking ${install_backup_aba_binary}"
[[ -L "${install_backup_aba_path}" ]] \
    || fail "installer deleted the rollback backup substitute"
[[ "$(readlink -- "${install_backup_aba_path}")" == \
    "${install_backup_aba_victim}" ]] \
    || fail "installer changed the rollback backup substitute"
[[ "$(<"${install_backup_aba_victim}")" == \
    'backup ABA victim sentinel' ]] \
    || fail "installer followed the rollback backup substitute"
[[ "$(stat -c '%d:%i:%u:%g:%a' -- "${install_backup_aba_binary}")" == \
    "${install_backup_aba_identity}" ]] \
    || fail "rollback backup ABA changed the original binary inode"
[[ "$(<"${install_backup_aba_binary}")" == \
    'old binary before backup ABA' ]] \
    || fail "rollback backup ABA changed the original binary"

# Staged bytes are written through an already-open temporary inode. Replace its
# logical name from inside cat before copying: the installer must revoke
# ownership, leave the substitute untouched, and never publish it.
install_stage_aba_tools="${TEST_ROOT}/install-stage-aba-tools"
install_stage_aba_stage="${TEST_ROOT}/install-stage-aba-stage"
install_stage_aba_prefix="/opt/frost-install-stage-aba"
install_stage_aba_binary="${install_stage_aba_stage}${install_stage_aba_prefix}/bin/frost"
install_stage_aba_victim="${TEST_ROOT}/install-stage-aba-victim"
install_stage_aba_path_log="${TEST_ROOT}/install-stage-aba-path"
mkdir -p "${install_stage_aba_tools}" "${install_stage_aba_binary%/*}"
printf 'old binary before staged temporary ABA\n' \
    >"${install_stage_aba_binary}"
printf 'staged temporary ABA victim sentinel\n' \
    >"${install_stage_aba_victim}"
install_stage_aba_identity="$(stat -c '%d:%i:%u:%g:%a' -- \
    "${install_stage_aba_binary}")"
# shellcheck disable=SC2016
printf '%s\n' \
    '#!/bin/sh' \
    'set -eu' \
    'temporary=' \
    'for candidate in /proc/self/fd/*; do' \
    '    target=$(/usr/bin/readlink -- "${candidate}" 2>/dev/null || :)' \
    '    case "${target}" in *.install.*) temporary=${target}; break ;; esac' \
    'done' \
    ': "${temporary:?missing inherited staged temporary fd}"' \
    'printf "%s\n" "${temporary}" >"${FROST_TEST_INSTALL_STAGE_ABA_PATH_LOG:?}"' \
    '/usr/bin/rm -f -- "${temporary}"' \
    '/usr/bin/ln -s -- "${FROST_TEST_INSTALL_STAGE_ABA_VICTIM:?}" "${temporary}"' \
    '/usr/bin/cat "$@"' \
    'exit 0' \
    >"${install_stage_aba_tools}/cat"
chmod 0755 "${install_stage_aba_tools}/cat"
if env HOME="${TEST_HOME}" PATH="${install_stage_aba_tools}:${TEST_PATH}" \
    FROST_TEST_INSTALL_STAGE_ABA_PATH_LOG="${install_stage_aba_path_log}" \
    FROST_TEST_INSTALL_STAGE_ABA_VICTIM="${install_stage_aba_victim}" \
    DESTDIR="${install_stage_aba_stage}" "${INSTALLER}" \
    --binary "${prebuilt_binary}" --prefix "${install_stage_aba_prefix}" \
    --no-desktop >"${TEST_ROOT}/install-stage-aba.log" 2>&1; then
    fail "installer accepted a replaced staged temporary name"
fi
install_stage_aba_path="$(<"${install_stage_aba_path_log}")"
assert_contains "staged temporary ABA diagnostic" \
    "$(<"${TEST_ROOT}/install-stage-aba.log")" \
    "install temporary changed while copying"
assert_contains "staged temporary ABA cleanup warning" \
    "$(<"${TEST_ROOT}/install-stage-aba.log")" \
    "refusing to remove changed temporary ${install_stage_aba_path}"
[[ -L "${install_stage_aba_path}" ]] \
    || fail "installer deleted the staged temporary substitute"
[[ "$(readlink -- "${install_stage_aba_path}")" == \
    "${install_stage_aba_victim}" ]] \
    || fail "installer changed the staged temporary substitute"
[[ "$(<"${install_stage_aba_victim}")" == \
    'staged temporary ABA victim sentinel' ]] \
    || fail "installer followed the staged temporary substitute"
[[ "$(stat -c '%d:%i:%u:%g:%a' -- "${install_stage_aba_binary}")" == \
    "${install_stage_aba_identity}" ]] \
    || fail "staged temporary ABA changed the old binary inode"
[[ "$(<"${install_stage_aba_binary}")" == \
    'old binary before staged temporary ABA' ]] \
    || fail "staged temporary ABA changed the old binary"

# A regular copy fallback also owns an already-open reservation inode. Force
# hardlink failure, copy into that descriptor, then replace the bound name from
# inside cp. The installer must retain the substitute and leave the source
# target byte-for-byte and inode-for-inode unchanged.
install_fallback_aba_tools="${TEST_ROOT}/install-fallback-aba-tools"
install_fallback_aba_stage="${TEST_ROOT}/install-fallback-aba-stage"
install_fallback_aba_prefix="/opt/frost-install-fallback-aba"
install_fallback_aba_binary="${install_fallback_aba_stage}${install_fallback_aba_prefix}/bin/frost"
install_fallback_aba_victim="${TEST_ROOT}/install-fallback-aba-victim"
install_fallback_aba_path_log="${TEST_ROOT}/install-fallback-aba-path"
mkdir -p "${install_fallback_aba_tools}" \
    "${install_fallback_aba_binary%/*}"
printf 'old binary before fallback ABA\n' \
    >"${install_fallback_aba_binary}"
printf 'fallback ABA victim sentinel\n' \
    >"${install_fallback_aba_victim}"
install_fallback_aba_identity="$(stat -c '%d:%i:%u:%g:%a' -- \
    "${install_fallback_aba_binary}")"
# shellcheck disable=SC2016
printf '%s\n' \
    '#!/bin/sh' \
    'set -eu' \
    'exit 88' \
    >"${install_fallback_aba_tools}/ln"
# shellcheck disable=SC2016
printf '%s\n' \
    '#!/bin/sh' \
    'set -eu' \
    '/usr/bin/cp "$@"' \
    'last=' \
    'for argument do last=${argument}; done' \
    'backup=$(/usr/bin/readlink -- "${last}")' \
    'printf "%s\n" "${backup}" >"${FROST_TEST_INSTALL_FALLBACK_ABA_PATH_LOG:?}"' \
    '/usr/bin/rm -f -- "${backup}"' \
    '/usr/bin/ln -s -- "${FROST_TEST_INSTALL_FALLBACK_ABA_VICTIM:?}" "${backup}"' \
    'exit 0' \
    >"${install_fallback_aba_tools}/cp"
chmod 0755 "${install_fallback_aba_tools}/ln" \
    "${install_fallback_aba_tools}/cp"
if env HOME="${TEST_HOME}" PATH="${install_fallback_aba_tools}:${TEST_PATH}" \
    FROST_TEST_INSTALL_FALLBACK_ABA_PATH_LOG="${install_fallback_aba_path_log}" \
    FROST_TEST_INSTALL_FALLBACK_ABA_VICTIM="${install_fallback_aba_victim}" \
    DESTDIR="${install_fallback_aba_stage}" "${INSTALLER}" \
    --binary "${prebuilt_binary}" --prefix "${install_fallback_aba_prefix}" \
    --no-desktop >"${TEST_ROOT}/install-fallback-aba.log" 2>&1; then
    fail "installer accepted a replaced fallback backup name"
fi
install_fallback_aba_path="$(<"${install_fallback_aba_path_log}")"
assert_contains "fallback backup ABA diagnostic" \
    "$(<"${TEST_ROOT}/install-fallback-aba.log")" \
    "fallback rollback backup name was replaced while copying ${install_fallback_aba_binary}"
assert_contains "fallback backup ABA cleanup warning" \
    "$(<"${TEST_ROOT}/install-fallback-aba.log")" \
    "refusing to remove changed rollback backup ${install_fallback_aba_path}"
[[ -L "${install_fallback_aba_path}" ]] \
    || fail "installer deleted the fallback backup substitute"
[[ "$(readlink -- "${install_fallback_aba_path}")" == \
    "${install_fallback_aba_victim}" ]] \
    || fail "installer changed the fallback backup substitute"
[[ "$(<"${install_fallback_aba_victim}")" == \
    'fallback ABA victim sentinel' ]] \
    || fail "installer followed the fallback backup substitute"
[[ "$(stat -c '%d:%i:%u:%g:%a' -- \
    "${install_fallback_aba_binary}")" == \
    "${install_fallback_aba_identity}" ]] \
    || fail "fallback backup ABA changed the original binary inode"
[[ "$(<"${install_fallback_aba_binary}")" == \
    'old binary before fallback ABA' ]] \
    || fail "fallback backup ABA changed the original binary"

# Select a readable prebuilt fixture on another device from the DESTDIR. A
# post-action cat failure must reconcile the descriptor-pinned bytes and mode,
# proving staging never relies on a same-filesystem rename or inode reuse.
cross_device_prebuilt=
install_cross_device_stage="${TEST_ROOT}/install-cross-device-stage"
install_cross_device_prefix="/opt/frost-install-cross-device"
mkdir -p "${install_cross_device_stage}"
install_cross_device_dev="$(stat -c '%d' -- "${install_cross_device_stage}")"
for candidate in "${SCRIPT_DIR}/../Cargo.toml" /etc/hostname /bin/true; do
    if [[ -f "${candidate}" && -r "${candidate}" && -s "${candidate}" \
        && "$(stat -c '%d' -- "${candidate}")" != \
            "${install_cross_device_dev}" ]]; then
        cross_device_prebuilt="${candidate}"
        break
    fi
done
[[ -n "${cross_device_prebuilt}" ]] \
    || fail "no cross-device prebuilt fixture is available"
install_cross_device_tools="${TEST_ROOT}/install-cross-device-tools"
install_cross_device_cat_marker="${TEST_ROOT}/install-cross-device-cat"
install_cross_device_binary="${install_cross_device_stage}${install_cross_device_prefix}/bin/frost"
mkdir -p "${install_cross_device_tools}"
# shellcheck disable=SC2016
printf '%s\n' \
    '#!/bin/sh' \
    'set -eu' \
    '/usr/bin/cat "$@"' \
    ': >"${FROST_TEST_INSTALL_CROSS_DEVICE_CAT_MARKER:?}"' \
    'exit 89' \
    >"${install_cross_device_tools}/cat"
chmod 0755 "${install_cross_device_tools}/cat"
install_cross_device_output="$(
    env HOME="${TEST_HOME}" PATH="${install_cross_device_tools}:${TEST_PATH}" \
        FROST_TEST_INSTALL_CROSS_DEVICE_CAT_MARKER="${install_cross_device_cat_marker}" \
        DESTDIR="${install_cross_device_stage}" "${INSTALLER}" \
        --binary "${cross_device_prebuilt}" \
        --prefix "${install_cross_device_prefix}" --no-desktop 2>&1
)"
assert_regular_file "cross-device post-action cat marker" \
    "${install_cross_device_cat_marker}"
cmp -- "${cross_device_prebuilt}" "${install_cross_device_binary}" \
    || fail "cross-device descriptor copy changed the installed bytes"
assert_mode "cross-device descriptor copy" \
    "${install_cross_device_binary}" 755
assert_contains "cross-device install success summary" \
    "${install_cross_device_output}" \
    "Installed frost to ${install_cross_device_prefix}/bin/frost"
[[ -z "$(find "${install_cross_device_stage}" \
    \( -name '*.install.*' -o -name '*.rollback.*' \) -print -quit)" ]] \
    || fail "cross-device descriptor copy left a temporary or backup"

# Exercise the complete regular fallback path: every hardlink fails, cp copies
# the first existing workflow and then returns non-zero, and the next publish
# rename fails. Rollback must restore the copied bytes/mode while removing all
# transaction-owned artifacts.
install_fallback_restore_tools="${TEST_ROOT}/install-fallback-restore-tools"
install_fallback_restore_stage="${TEST_ROOT}/install-fallback-restore-stage"
install_fallback_restore_prefix="/opt/frost-install-fallback-restore"
install_fallback_restore_workflow_dir="${install_fallback_restore_stage}${install_fallback_restore_prefix}/share/frost/workflows"
install_fallback_restore_first="${install_fallback_restore_workflow_dir}/${WORKFLOW_SOURCES[0]##*/}"
install_fallback_restore_second="${install_fallback_restore_workflow_dir}/${WORKFLOW_SOURCES[1]##*/}"
install_fallback_restore_ln_marker="${TEST_ROOT}/install-fallback-restore-ln"
install_fallback_restore_cp_marker="${TEST_ROOT}/install-fallback-restore-cp"
install_fallback_restore_mv_marker="${TEST_ROOT}/install-fallback-restore-mv"
install_fallback_restore_mv_state="${TEST_ROOT}/install-fallback-restore-mv-state"
mkdir -p "${install_fallback_restore_tools}" \
    "${install_fallback_restore_workflow_dir}"
printf 'old workflow before copy fallback rollback\n' \
    >"${install_fallback_restore_first}"
chmod 0613 "${install_fallback_restore_first}"
# shellcheck disable=SC2016
printf '%s\n' \
    '#!/bin/sh' \
    'set -eu' \
    ': >"${FROST_TEST_INSTALL_FALLBACK_RESTORE_LN_MARKER:?}"' \
    'exit 90' \
    >"${install_fallback_restore_tools}/ln"
# shellcheck disable=SC2016
printf '%s\n' \
    '#!/bin/sh' \
    'set -eu' \
    '/usr/bin/cp "$@"' \
    ': >"${FROST_TEST_INSTALL_FALLBACK_RESTORE_CP_MARKER:?}"' \
    'exit 91' \
    >"${install_fallback_restore_tools}/cp"
# shellcheck disable=SC2016
printf '%s\n' \
    '#!/bin/sh' \
    'set -eu' \
    'previous=' \
    'last=' \
    'for argument do previous=${last}; last=${argument}; done' \
    'case "${previous}" in' \
    '    /proc/self/fd/*/*.install.*)' \
    '        state=${FROST_TEST_INSTALL_FALLBACK_RESTORE_MV_STATE:?}' \
    '        count=0' \
    '        [ ! -f "${state}" ] || read -r count <"${state}"' \
    '        count=$((count + 1))' \
    '        printf "%s\n" "${count}" >"${state}"' \
    '        if [ "${count}" -eq 2 ]; then' \
    '            : >"${FROST_TEST_INSTALL_FALLBACK_RESTORE_MV_MARKER:?}"' \
    '            exit 92' \
    '        fi' \
    '        ;;' \
    'esac' \
    'exec /usr/bin/mv "$@"' \
    >"${install_fallback_restore_tools}/mv"
chmod 0755 "${install_fallback_restore_tools}/ln" \
    "${install_fallback_restore_tools}/cp" \
    "${install_fallback_restore_tools}/mv"
if env HOME="${TEST_HOME}" \
    PATH="${install_fallback_restore_tools}:${TEST_PATH}" \
    FROST_TEST_INSTALL_FALLBACK_RESTORE_LN_MARKER="${install_fallback_restore_ln_marker}" \
    FROST_TEST_INSTALL_FALLBACK_RESTORE_CP_MARKER="${install_fallback_restore_cp_marker}" \
    FROST_TEST_INSTALL_FALLBACK_RESTORE_MV_MARKER="${install_fallback_restore_mv_marker}" \
    FROST_TEST_INSTALL_FALLBACK_RESTORE_MV_STATE="${install_fallback_restore_mv_state}" \
    DESTDIR="${install_fallback_restore_stage}" "${INSTALLER}" \
    --binary "${cross_device_prebuilt}" \
    --prefix "${install_fallback_restore_prefix}" --no-desktop \
    >"${TEST_ROOT}/install-fallback-restore.log" 2>&1; then
    fail "installer ignored a publish failure after copy fallback"
fi
assert_regular_file "forced hardlink failure marker" \
    "${install_fallback_restore_ln_marker}"
assert_regular_file "completed fallback copy marker" \
    "${install_fallback_restore_cp_marker}"
assert_regular_file "forced publish failure marker" \
    "${install_fallback_restore_mv_marker}"
[[ "$(<"${install_fallback_restore_first}")" == \
    'old workflow before copy fallback rollback' ]] \
    || fail "copy fallback rollback changed the old workflow bytes"
assert_mode "copy fallback rollback" "${install_fallback_restore_first}" 613
assert_absent "failed second workflow publish" \
    "${install_fallback_restore_second}"
assert_absent "binary after pre-commit fallback rollback" \
    "${install_fallback_restore_stage}${install_fallback_restore_prefix}/bin/frost"
assert_contains "copy fallback publish failure diagnostic" \
    "$(<"${TEST_ROOT}/install-fallback-restore.log")" \
    "cannot atomically replace ${install_fallback_restore_second}"
[[ -z "$(find "${install_fallback_restore_stage}" \
    \( -name '*.install.*' -o -name '*.rollback.*' \) -print -quit)" ]] \
    || fail "copy fallback rollback left a temporary or backup"

# A symlink fallback has no descriptor that Bash can open without following
# its referent. Force the primary hardlink to fail, let cp create the exact
# link object but return non-zero, and do the same after ln creates a second
# hardlink pin. A later publish failure must restore link text and ownership
# metadata without touching the referent or leaking either owned name.
install_symlink_fallback_tools="${TEST_ROOT}/install-symlink-fallback-tools"
install_symlink_fallback_stage="${TEST_ROOT}/install-symlink-fallback-stage"
install_symlink_fallback_prefix="/opt/frost-install-symlink-fallback"
install_symlink_fallback_workflow_dir="${install_symlink_fallback_stage}${install_symlink_fallback_prefix}/share/frost/workflows"
install_symlink_fallback_first="${install_symlink_fallback_workflow_dir}/${WORKFLOW_SOURCES[0]##*/}"
install_symlink_fallback_second="${install_symlink_fallback_workflow_dir}/${WORKFLOW_SOURCES[1]##*/}"
install_symlink_fallback_victim="${TEST_ROOT}/symlink fallback victim"
install_symlink_fallback_ln_marker="${TEST_ROOT}/install-symlink-fallback-ln"
install_symlink_fallback_pin_marker="${TEST_ROOT}/install-symlink-fallback-pin"
install_symlink_fallback_cp_marker="${TEST_ROOT}/install-symlink-fallback-cp"
install_symlink_fallback_mv_marker="${TEST_ROOT}/install-symlink-fallback-mv"
install_symlink_fallback_mv_state="${TEST_ROOT}/install-symlink-fallback-mv-state"
mkdir -p "${install_symlink_fallback_tools}" \
    "${install_symlink_fallback_workflow_dir}"
printf 'symlink fallback victim sentinel\n' \
    >"${install_symlink_fallback_victim}"
ln -s -- "${install_symlink_fallback_victim}" \
    "${install_symlink_fallback_first}"
install_symlink_fallback_value="$(readlink -- \
    "${install_symlink_fallback_first}")"
install_symlink_fallback_metadata="$(stat -c '%u:%g:%a' -- \
    "${install_symlink_fallback_first}")"
# shellcheck disable=SC2016
printf '%s\n' \
    '#!/bin/sh' \
    'set -eu' \
    'last=' \
    'for argument do last=${argument}; done' \
    'case "${last}" in' \
    '    /proc/self/fd/*/*.rollback-pin.*)' \
    '        /usr/bin/ln "$@"' \
    '        : >"${FROST_TEST_INSTALL_SYMLINK_FALLBACK_PIN_MARKER:?}"' \
    '        exit 92' \
    '        ;;' \
    '    /proc/self/fd/*/*.rollback.*)' \
    '        : >"${FROST_TEST_INSTALL_SYMLINK_FALLBACK_LN_MARKER:?}"' \
    '        exit 90' \
    '        ;;' \
    'esac' \
    'exec /usr/bin/ln "$@"' \
    >"${install_symlink_fallback_tools}/ln"
# shellcheck disable=SC2016
printf '%s\n' \
    '#!/bin/sh' \
    'set -eu' \
    '/usr/bin/cp "$@"' \
    ': >"${FROST_TEST_INSTALL_SYMLINK_FALLBACK_CP_MARKER:?}"' \
    'exit 91' \
    >"${install_symlink_fallback_tools}/cp"
# shellcheck disable=SC2016
printf '%s\n' \
    '#!/bin/sh' \
    'set -eu' \
    'previous=' \
    'last=' \
    'for argument do previous=${last}; last=${argument}; done' \
    'case "${previous}" in' \
    '    /proc/self/fd/*/*.install.*)' \
    '        state=${FROST_TEST_INSTALL_SYMLINK_FALLBACK_MV_STATE:?}' \
    '        count=0' \
    '        [ ! -f "${state}" ] || read -r count <"${state}"' \
    '        count=$((count + 1))' \
    '        printf "%s\n" "${count}" >"${state}"' \
    '        if [ "${count}" -eq 2 ]; then' \
    '            : >"${FROST_TEST_INSTALL_SYMLINK_FALLBACK_MV_MARKER:?}"' \
    '            exit 93' \
    '        fi' \
    '        ;;' \
    'esac' \
    'exec /usr/bin/mv "$@"' \
    >"${install_symlink_fallback_tools}/mv"
chmod 0755 "${install_symlink_fallback_tools}/ln" \
    "${install_symlink_fallback_tools}/cp" \
    "${install_symlink_fallback_tools}/mv"
if env HOME="${TEST_HOME}" \
    PATH="${install_symlink_fallback_tools}:${TEST_PATH}" \
    FROST_TEST_INSTALL_SYMLINK_FALLBACK_LN_MARKER="${install_symlink_fallback_ln_marker}" \
    FROST_TEST_INSTALL_SYMLINK_FALLBACK_PIN_MARKER="${install_symlink_fallback_pin_marker}" \
    FROST_TEST_INSTALL_SYMLINK_FALLBACK_CP_MARKER="${install_symlink_fallback_cp_marker}" \
    FROST_TEST_INSTALL_SYMLINK_FALLBACK_MV_MARKER="${install_symlink_fallback_mv_marker}" \
    FROST_TEST_INSTALL_SYMLINK_FALLBACK_MV_STATE="${install_symlink_fallback_mv_state}" \
    DESTDIR="${install_symlink_fallback_stage}" "${INSTALLER}" \
    --binary "${cross_device_prebuilt}" \
    --prefix "${install_symlink_fallback_prefix}" --no-desktop \
    >"${TEST_ROOT}/install-symlink-fallback.log" 2>&1; then
    fail "installer ignored a publish failure after symlink copy fallback"
fi
assert_regular_file "forced symlink hardlink failure marker" \
    "${install_symlink_fallback_ln_marker}"
assert_regular_file "completed symlink fallback copy marker" \
    "${install_symlink_fallback_cp_marker}"
assert_regular_file "completed symlink fallback pin marker" \
    "${install_symlink_fallback_pin_marker}"
assert_regular_file "symlink fallback publish failure marker" \
    "${install_symlink_fallback_mv_marker}"
[[ -L "${install_symlink_fallback_first}" ]] \
    || fail "symlink copy fallback rollback did not restore a symlink"
[[ "$(readlink -- "${install_symlink_fallback_first}")" == \
    "${install_symlink_fallback_value}" ]] \
    || fail "symlink copy fallback rollback changed the link text"
[[ "$(stat -c '%u:%g:%a' -- "${install_symlink_fallback_first}")" == \
    "${install_symlink_fallback_metadata}" ]] \
    || fail "symlink copy fallback rollback changed owner/group/mode"
[[ "$(<"${install_symlink_fallback_victim}")" == \
    'symlink fallback victim sentinel' ]] \
    || fail "symlink copy fallback followed the link referent"
assert_absent "second target after symlink fallback rollback" \
    "${install_symlink_fallback_second}"
assert_contains "symlink fallback publish failure diagnostic" \
    "$(<"${TEST_ROOT}/install-symlink-fallback.log")" \
    "cannot atomically replace ${install_symlink_fallback_second}"
[[ -z "$(find "${install_symlink_fallback_stage}" \
    \( -name '*.install.*' -o -name '*.rollback.*' \
        -o -name '*.rollback-pin.*' -o -name '*.rollback-anchor.*' \) \
        -print -quit)" ]] \
    || fail "symlink copy fallback rollback left an owned artifact"

# The pin placeholder is owned only while its exact reservation inode remains
# at the bound name. Replace it from inside rm: backup preparation must stop
# before publish. Without a private anchor, cleanup must retain both the exact
# main hardlink and the unowned pin substitute without following either one.
install_symlink_pin_aba_tools="${TEST_ROOT}/install-symlink-pin-aba-tools"
install_symlink_pin_aba_stage="${TEST_ROOT}/install-symlink-pin-aba-stage"
install_symlink_pin_aba_prefix="/opt/frost-install-symlink-pin-aba"
install_symlink_pin_aba_first="${install_symlink_pin_aba_stage}${install_symlink_pin_aba_prefix}/share/frost/workflows/${WORKFLOW_SOURCES[0]##*/}"
install_symlink_pin_aba_original_victim="${TEST_ROOT}/install-symlink-pin-aba-original-victim"
install_symlink_pin_aba_substitute_victim="${TEST_ROOT}/install-symlink-pin-aba-substitute-victim"
install_symlink_pin_aba_path_log="${TEST_ROOT}/install-symlink-pin-aba-path"
mkdir -p "${install_symlink_pin_aba_tools}" \
    "${install_symlink_pin_aba_first%/*}"
printf 'pin ABA original victim sentinel\n' \
    >"${install_symlink_pin_aba_original_victim}"
printf 'pin ABA substitute victim sentinel\n' \
    >"${install_symlink_pin_aba_substitute_victim}"
ln -s -- "${install_symlink_pin_aba_original_victim}" \
    "${install_symlink_pin_aba_first}"
install_symlink_pin_aba_identity="$(stat -c '%d:%i:%u:%g:%a' -- \
    "${install_symlink_pin_aba_first}")"
install_symlink_pin_aba_value="$(readlink -- \
    "${install_symlink_pin_aba_first}")"
# shellcheck disable=SC2016
printf '%s\n' \
    '#!/bin/sh' \
    'set -eu' \
    'last=' \
    'for argument do last=${argument}; done' \
    'case "${last}" in' \
    '    /proc/self/fd/*/*.rollback-pin.*)' \
    '        /usr/bin/rm "$@"' \
    '        parent=$(/usr/bin/readlink -- "${last%/*}")' \
    '        physical=${parent}/${last##*/}' \
    '        printf "%s\n" "${physical}" >"${FROST_TEST_INSTALL_SYMLINK_PIN_ABA_PATH_LOG:?}"' \
    '        /usr/bin/ln -s -- "${FROST_TEST_INSTALL_SYMLINK_PIN_ABA_VICTIM:?}" "${last}"' \
    '        exit 0' \
    '        ;;' \
    'esac' \
    'exec /usr/bin/rm "$@"' \
    >"${install_symlink_pin_aba_tools}/rm"
chmod 0755 "${install_symlink_pin_aba_tools}/rm"
if env HOME="${TEST_HOME}" \
    PATH="${install_symlink_pin_aba_tools}:${TEST_PATH}" \
    FROST_TEST_INSTALL_SYMLINK_PIN_ABA_PATH_LOG="${install_symlink_pin_aba_path_log}" \
    FROST_TEST_INSTALL_SYMLINK_PIN_ABA_VICTIM="${install_symlink_pin_aba_substitute_victim}" \
    DESTDIR="${install_symlink_pin_aba_stage}" "${INSTALLER}" \
    --binary "${prebuilt_binary}" \
    --prefix "${install_symlink_pin_aba_prefix}" --no-desktop \
    >"${TEST_ROOT}/install-symlink-pin-aba.log" 2>&1; then
    fail "installer accepted a replaced symlink pin reservation"
fi
install_symlink_pin_aba_path="$(<"${install_symlink_pin_aba_path_log}")"
[[ "$(stat -c '%d:%i:%u:%g:%a' -- \
    "${install_symlink_pin_aba_first}")" == \
    "${install_symlink_pin_aba_identity}" ]] \
    || fail "symlink pin reservation ABA changed the original inode metadata"
[[ "$(readlink -- "${install_symlink_pin_aba_first}")" == \
    "${install_symlink_pin_aba_value}" ]] \
    || fail "symlink pin reservation ABA changed the original link text"
[[ -L "${install_symlink_pin_aba_path}" ]] \
    || fail "cleanup deleted the symlink pin reservation substitute"
[[ "$(readlink -- "${install_symlink_pin_aba_path}")" == \
    "${install_symlink_pin_aba_substitute_victim}" ]] \
    || fail "cleanup changed the symlink pin reservation substitute"
[[ "$(<"${install_symlink_pin_aba_substitute_victim}")" == \
    'pin ABA substitute victim sentinel' ]] \
    || fail "cleanup followed the symlink pin reservation substitute"
assert_contains "symlink pin reservation ABA diagnostic" \
    "$(<"${TEST_ROOT}/install-symlink-pin-aba.log")" \
    "symlink rollback pin reservation name changed while removing it beside ${install_symlink_pin_aba_first}"
assert_contains "symlink pin reservation cleanup warning" \
    "$(<"${TEST_ROOT}/install-symlink-pin-aba.log")" \
    "changed rollback pin retained at ${install_symlink_pin_aba_path}"
install_symlink_pin_aba_backup="$(find \
    "${install_symlink_pin_aba_first%/*}" -maxdepth 1 \
    -name ".${install_symlink_pin_aba_first##*/}.rollback.??????" \
    -print -quit)"
[[ -n "${install_symlink_pin_aba_backup}" \
    && -L "${install_symlink_pin_aba_backup}" ]] \
    || fail "symlink pin reservation ABA lost the exact main recovery link"
[[ "$(stat -c '%d:%i:%u:%g:%a' -- \
    "${install_symlink_pin_aba_backup}")" == \
    "${install_symlink_pin_aba_identity}" ]] \
    || fail "symlink pin reservation ABA changed the main recovery identity"
[[ "$(readlink -- "${install_symlink_pin_aba_backup}")" == \
    "${install_symlink_pin_aba_value}" ]] \
    || fail "symlink pin reservation ABA changed the main recovery link text"
assert_contains "symlink pin reservation main-backup warning" \
    "$(<"${TEST_ROOT}/install-symlink-pin-aba.log")" \
    "exact rollback backup retained at ${install_symlink_pin_aba_backup}"

# Keep the copied/original symlink inode alive through publish with the second
# name. After the first staged inode is installed, replace the main backup and
# return non-zero; a later publish failure must leave that substitute alone and
# retain the exact pin as an explicitly diagnosed recovery copy.
install_symlink_publish_aba_tools="${TEST_ROOT}/install-symlink-publish-aba-tools"
install_symlink_publish_aba_stage="${TEST_ROOT}/install-symlink-publish-aba-stage"
install_symlink_publish_aba_prefix="/opt/frost-install-symlink-publish-aba"
install_symlink_publish_aba_workflow_dir="${install_symlink_publish_aba_stage}${install_symlink_publish_aba_prefix}/share/frost/workflows"
install_symlink_publish_aba_first="${install_symlink_publish_aba_workflow_dir}/${WORKFLOW_SOURCES[0]##*/}"
install_symlink_publish_aba_second="${install_symlink_publish_aba_workflow_dir}/${WORKFLOW_SOURCES[1]##*/}"
install_symlink_publish_aba_original_victim="${TEST_ROOT}/install-symlink-publish-aba-original-victim"
install_symlink_publish_aba_substitute_victim="${TEST_ROOT}/install-symlink-publish-aba-substitute-victim"
install_symlink_publish_aba_backup_log="${TEST_ROOT}/install-symlink-publish-aba-backup"
install_symlink_publish_aba_pin_log="${TEST_ROOT}/install-symlink-publish-aba-pin"
install_symlink_publish_aba_state="${TEST_ROOT}/install-symlink-publish-aba-state"
install_symlink_publish_aba_second_marker="${TEST_ROOT}/install-symlink-publish-aba-second"
mkdir -p "${install_symlink_publish_aba_tools}" \
    "${install_symlink_publish_aba_workflow_dir}"
printf 'publish ABA original symlink victim\n' \
    >"${install_symlink_publish_aba_original_victim}"
printf 'publish ABA substitute symlink victim\n' \
    >"${install_symlink_publish_aba_substitute_victim}"
ln -s -- "${install_symlink_publish_aba_original_victim}" \
    "${install_symlink_publish_aba_first}"
install_symlink_publish_aba_identity="$(stat -c '%d:%i' -- \
    "${install_symlink_publish_aba_first}")"
install_symlink_publish_aba_metadata="$(stat -c '%u:%g:%a' -- \
    "${install_symlink_publish_aba_first}")"
install_symlink_publish_aba_value="$(readlink -- \
    "${install_symlink_publish_aba_first}")"
# shellcheck disable=SC2016
printf '%s\n' \
    '#!/bin/sh' \
    'set -eu' \
    'previous=' \
    'last=' \
    'for argument do previous=${last}; last=${argument}; done' \
    'case "${previous}" in' \
    '    /proc/self/fd/*/*.install.*)' \
    '        state=${FROST_TEST_INSTALL_SYMLINK_PUBLISH_ABA_STATE:?}' \
    '        count=0' \
    '        [ ! -f "${state}" ] || read -r count <"${state}"' \
    '        count=$((count + 1))' \
    '        printf "%s\n" "${count}" >"${state}"' \
    '        if [ "${count}" -eq 1 ]; then' \
    '            /usr/bin/mv "$@"' \
    '            parent=$(/usr/bin/readlink -- "${last%/*}")' \
    '            backup=$(/usr/bin/find "${parent}" -maxdepth 1 -name ".${FROST_TEST_INSTALL_SYMLINK_PUBLISH_ABA_BASENAME:?}.rollback.??????" -print -quit)' \
    '            pin=$(/usr/bin/find "${parent}" -maxdepth 1 -name ".${FROST_TEST_INSTALL_SYMLINK_PUBLISH_ABA_BASENAME}.rollback-pin.*" -print -quit)' \
    '            : "${backup:?missing symlink publish backup}"' \
    '            : "${pin:?missing symlink publish pin}"' \
    '            printf "%s\n" "${backup}" >"${FROST_TEST_INSTALL_SYMLINK_PUBLISH_ABA_BACKUP_LOG:?}"' \
    '            printf "%s\n" "${pin}" >"${FROST_TEST_INSTALL_SYMLINK_PUBLISH_ABA_PIN_LOG:?}"' \
    '            /usr/bin/rm -f -- "${backup}"' \
    '            /usr/bin/ln -s -- "${FROST_TEST_INSTALL_SYMLINK_PUBLISH_ABA_VICTIM:?}" "${backup}"' \
    '            exit 94' \
    '        fi' \
    '        if [ "${count}" -eq 2 ]; then' \
    '            : >"${FROST_TEST_INSTALL_SYMLINK_PUBLISH_ABA_SECOND_MARKER:?}"' \
    '            exit 95' \
    '        fi' \
    '        ;;' \
    'esac' \
    'exec /usr/bin/mv "$@"' \
    >"${install_symlink_publish_aba_tools}/mv"
chmod 0755 "${install_symlink_publish_aba_tools}/mv"
if env HOME="${TEST_HOME}" \
    PATH="${install_symlink_publish_aba_tools}:${TEST_PATH}" \
    FROST_TEST_INSTALL_SYMLINK_PUBLISH_ABA_STATE="${install_symlink_publish_aba_state}" \
    FROST_TEST_INSTALL_SYMLINK_PUBLISH_ABA_BASENAME="${install_symlink_publish_aba_first##*/}" \
    FROST_TEST_INSTALL_SYMLINK_PUBLISH_ABA_BACKUP_LOG="${install_symlink_publish_aba_backup_log}" \
    FROST_TEST_INSTALL_SYMLINK_PUBLISH_ABA_PIN_LOG="${install_symlink_publish_aba_pin_log}" \
    FROST_TEST_INSTALL_SYMLINK_PUBLISH_ABA_VICTIM="${install_symlink_publish_aba_substitute_victim}" \
    FROST_TEST_INSTALL_SYMLINK_PUBLISH_ABA_SECOND_MARKER="${install_symlink_publish_aba_second_marker}" \
    DESTDIR="${install_symlink_publish_aba_stage}" "${INSTALLER}" \
    --binary "${prebuilt_binary}" \
    --prefix "${install_symlink_publish_aba_prefix}" --no-desktop \
    >"${TEST_ROOT}/install-symlink-publish-aba.log" 2>&1; then
    fail "installer ignored a later failure after symlink backup ABA"
fi
install_symlink_publish_aba_backup="$(<"${install_symlink_publish_aba_backup_log}")"
install_symlink_publish_aba_pin="$(<"${install_symlink_publish_aba_pin_log}")"
assert_regular_file "later symlink publish failure marker" \
    "${install_symlink_publish_aba_second_marker}"
cmp -- "${WORKFLOW_SOURCES[0]}" "${install_symlink_publish_aba_first}" \
    || fail "symlink publish ABA lost the exact staged destination"
assert_absent "second target after symlink publish ABA" \
    "${install_symlink_publish_aba_second}"
[[ -L "${install_symlink_publish_aba_backup}" ]] \
    || fail "rollback deleted the symlink backup substitute"
[[ "$(readlink -- "${install_symlink_publish_aba_backup}")" == \
    "${install_symlink_publish_aba_substitute_victim}" ]] \
    || fail "rollback changed the symlink backup substitute"
[[ -L "${install_symlink_publish_aba_pin}" ]] \
    || fail "rollback deleted the exact symlink recovery pin"
[[ "$(stat -c '%d:%i' -- "${install_symlink_publish_aba_pin}")" == \
    "${install_symlink_publish_aba_identity}" ]] \
    || fail "symlink recovery pin lost the original inode identity"
[[ "$(stat -c '%u:%g:%a' -- "${install_symlink_publish_aba_pin}")" == \
    "${install_symlink_publish_aba_metadata}" ]] \
    || fail "symlink recovery pin changed the original inode metadata"
[[ "$(readlink -- "${install_symlink_publish_aba_pin}")" == \
    "${install_symlink_publish_aba_value}" ]] \
    || fail "symlink recovery pin changed the original link text"
[[ "$(<"${install_symlink_publish_aba_substitute_victim}")" == \
    'publish ABA substitute symlink victim' ]] \
    || fail "rollback followed the symlink backup substitute"
assert_contains "later symlink publish failure diagnostic" \
    "$(<"${TEST_ROOT}/install-symlink-publish-aba.log")" \
    "cannot atomically replace ${install_symlink_publish_aba_second}"
assert_contains "changed symlink backup rollback diagnostic" \
    "$(<"${TEST_ROOT}/install-symlink-publish-aba.log")" \
    "rollback refused changed backup for ${install_symlink_publish_aba_first}; unexpected entry retained at ${install_symlink_publish_aba_backup}"
assert_contains "symlink recovery pin diagnostic" \
    "$(<"${TEST_ROOT}/install-symlink-publish-aba.log")" \
    "exact symlink recovery pin for ${install_symlink_publish_aba_first} retained at ${install_symlink_publish_aba_pin}"
[[ -z "$(find "${install_symlink_publish_aba_stage}" \
    -name '*.install.*' -print -quit)" ]] \
    || fail "symlink publish ABA left a transaction-owned temporary"

# A successful publish may be followed by a main-backup substitution before
# cleanup. Rename its already-bound parent to a path containing newline and ESC
# bytes: pair preflight must retain the foreign name and exact pin, never
# disclose link text, and emit one single-line %q command that restores only
# inside that bound physical directory rather than the replacement symlink.
install_symlink_cleanup_main_tools="${TEST_ROOT}/install-symlink-cleanup-main-tools"
install_symlink_cleanup_main_stage="${TEST_ROOT}/install-symlink-cleanup-main-stage"
install_symlink_cleanup_main_prefix="/opt/frost-cleanup-main"
install_symlink_cleanup_main_workflow_dir="${install_symlink_cleanup_main_stage}${install_symlink_cleanup_main_prefix}/share/frost/workflows"
install_symlink_cleanup_main_first="${install_symlink_cleanup_main_workflow_dir}/${WORKFLOW_SOURCES[0]##*/}"
install_symlink_cleanup_main_displaced="${TEST_ROOT}"$'/cleanup-main\nnext-\033-dir'
install_symlink_cleanup_main_outside="${TEST_ROOT}/cleanup-main-outside"
install_symlink_cleanup_main_secret="${TEST_ROOT}/cleanup-main-private-referent"
install_symlink_cleanup_main_backup_log="${TEST_ROOT}/install-symlink-cleanup-main-backup"
install_symlink_cleanup_main_pin_log="${TEST_ROOT}/install-symlink-cleanup-main-pin"
mkdir -p "${install_symlink_cleanup_main_tools}" \
    "${install_symlink_cleanup_main_workflow_dir}" \
    "${install_symlink_cleanup_main_outside}"
printf 'cleanup main private sentinel\n' \
    >"${install_symlink_cleanup_main_secret}"
printf 'cleanup main outside target sentinel\n' \
    >"${install_symlink_cleanup_main_outside}/${WORKFLOW_SOURCES[0]##*/}"
ln -s -- "${install_symlink_cleanup_main_secret}" \
    "${install_symlink_cleanup_main_first}"
install_symlink_cleanup_main_identity="$(stat -c '%d:%i:%u:%g:%a' -- \
    "${install_symlink_cleanup_main_first}")"
install_symlink_cleanup_main_value="$(readlink -- \
    "${install_symlink_cleanup_main_first}")"
# shellcheck disable=SC2016
printf '%s\n' \
    '#!/bin/sh' \
    'set -eu' \
    'previous=' \
    'last=' \
    'for argument do previous=${last}; last=${argument}; done' \
    'case "${previous}" in' \
    '    /proc/self/fd/*/.frost.install.*)' \
    '        /usr/bin/mv "$@"' \
    '        parent=${FROST_TEST_INSTALL_SYMLINK_CLEANUP_MAIN_PARENT:?}' \
    '        basename=${FROST_TEST_INSTALL_SYMLINK_CLEANUP_MAIN_BASENAME:?}' \
    '        backup=$(/usr/bin/find "${parent}" -maxdepth 1 -name ".${basename}.rollback.??????" -print -quit)' \
    '        pin=$(/usr/bin/find "${parent}" -maxdepth 1 -name ".${basename}.rollback-pin.*" -print -quit)' \
    '        : "${backup:?missing cleanup main backup}"' \
    '        : "${pin:?missing cleanup main pin}"' \
    '        printf "%s\n" "${backup}" >"${FROST_TEST_INSTALL_SYMLINK_CLEANUP_MAIN_BACKUP_LOG:?}"' \
    '        printf "%s\n" "${pin}" >"${FROST_TEST_INSTALL_SYMLINK_CLEANUP_MAIN_PIN_LOG:?}"' \
    '        /usr/bin/rm -f -- "${backup}"' \
    '        /usr/bin/ln -s -- "${FROST_TEST_INSTALL_SYMLINK_CLEANUP_MAIN_SECRET:?}" "${backup}"' \
    '        /usr/bin/mv -- "${parent}" "${FROST_TEST_INSTALL_SYMLINK_CLEANUP_MAIN_DISPLACED:?}"' \
    '        /usr/bin/ln -s -- "${FROST_TEST_INSTALL_SYMLINK_CLEANUP_MAIN_OUTSIDE:?}" "${parent}"' \
    '        exit 0' \
    '        ;;' \
    'esac' \
    'exec /usr/bin/mv "$@"' \
    >"${install_symlink_cleanup_main_tools}/mv"
chmod 0755 "${install_symlink_cleanup_main_tools}/mv"
install_symlink_cleanup_main_log="${TEST_ROOT}/install-symlink-cleanup-main.log"
install_symlink_cleanup_main_output="$({
    env HOME="${TEST_HOME}" \
        PATH="${install_symlink_cleanup_main_tools}:${TEST_PATH}" \
        FROST_TEST_INSTALL_SYMLINK_CLEANUP_MAIN_PARENT="${install_symlink_cleanup_main_workflow_dir}" \
        FROST_TEST_INSTALL_SYMLINK_CLEANUP_MAIN_BASENAME="${install_symlink_cleanup_main_first##*/}" \
        FROST_TEST_INSTALL_SYMLINK_CLEANUP_MAIN_BACKUP_LOG="${install_symlink_cleanup_main_backup_log}" \
        FROST_TEST_INSTALL_SYMLINK_CLEANUP_MAIN_PIN_LOG="${install_symlink_cleanup_main_pin_log}" \
        FROST_TEST_INSTALL_SYMLINK_CLEANUP_MAIN_SECRET="${install_symlink_cleanup_main_secret}" \
        FROST_TEST_INSTALL_SYMLINK_CLEANUP_MAIN_DISPLACED="${install_symlink_cleanup_main_displaced}" \
        FROST_TEST_INSTALL_SYMLINK_CLEANUP_MAIN_OUTSIDE="${install_symlink_cleanup_main_outside}" \
        DESTDIR="${install_symlink_cleanup_main_stage}" "${INSTALLER}" \
        --binary "${prebuilt_binary}" \
        --prefix "${install_symlink_cleanup_main_prefix}" --no-desktop
} 2>&1 | tee "${install_symlink_cleanup_main_log}")"
install_symlink_cleanup_main_backup="${install_symlink_cleanup_main_displaced}/$(basename -- "$(<"${install_symlink_cleanup_main_backup_log}")")"
install_symlink_cleanup_main_pin="${install_symlink_cleanup_main_displaced}/$(basename -- "$(<"${install_symlink_cleanup_main_pin_log}")")"
install_symlink_cleanup_main_restored="${install_symlink_cleanup_main_displaced}/${WORKFLOW_SOURCES[0]##*/}"
[[ -L "${install_symlink_cleanup_main_backup}" ]] \
    || fail "cleanup deleted the main-backup substitute"
[[ "$(readlink -- "${install_symlink_cleanup_main_backup}")" == \
    "${install_symlink_cleanup_main_secret}" ]] \
    || fail "cleanup changed the main-backup substitute"
[[ -L "${install_symlink_cleanup_main_pin}" ]] \
    || fail "cleanup deleted the exact recovery pin after main replacement"
[[ "$(stat -c '%d:%i:%u:%g:%a' -- \
    "${install_symlink_cleanup_main_pin}")" == \
    "${install_symlink_cleanup_main_identity}" ]] \
    || fail "cleanup changed the exact pin identity after main replacement"
[[ "$(readlink -- "${install_symlink_cleanup_main_pin}")" == \
    "${install_symlink_cleanup_main_value}" ]] \
    || fail "cleanup changed the exact pin text after main replacement"
[[ "${install_symlink_cleanup_main_output}" != \
    *"${install_symlink_cleanup_main_secret}"* ]] \
    || fail "cleanup diagnostics disclosed symlink contents"
install_symlink_cleanup_main_command="$(extract_symlink_recovery_command \
    "main-backup replacement" "${install_symlink_cleanup_main_log}")"
[[ "${install_symlink_cleanup_main_command}" != *$'\n'* \
    && "${install_symlink_cleanup_main_command}" != *$'\033'* ]] \
    || fail "recovery command contains a literal control character"
[[ "${install_symlink_cleanup_main_command}" == *'\n'* \
    && "${install_symlink_cleanup_main_command}" == *'\E'* ]] \
    || fail "recovery command did not %q-encode newline and ESC path bytes"
bash -n -c "${install_symlink_cleanup_main_command}" \
    || fail "main-backup recovery command is not valid shell"
(cd -- "${TEST_ROOT}" \
    && PATH="${TEST_PATH}" bash -c \
        "${install_symlink_cleanup_main_command}")
[[ -L "${install_symlink_cleanup_main_restored}" ]] \
    || fail "main-backup recovery command did not restore the symlink"
[[ "$(readlink -- "${install_symlink_cleanup_main_restored}")" == \
    "${install_symlink_cleanup_main_value}" ]] \
    || fail "main-backup recovery command changed the link text"
[[ "$(<"${install_symlink_cleanup_main_outside}/${WORKFLOW_SOURCES[0]##*/}")" == \
    'cleanup main outside target sentinel' ]] \
    || fail "main-backup recovery command followed the replacement parent"
assert_absent "consumed main-replacement recovery pin" \
    "${install_symlink_cleanup_main_pin}"
[[ -L "${install_symlink_cleanup_main_backup}" ]] \
    || fail "recovery command touched the main-backup substitute"

# The symmetric pin substitution must retain the exact main backup and the
# foreign pin. The command should select only that exact main inode and leave
# the substitute untouched when copied and executed.
install_symlink_cleanup_pin_tools="${TEST_ROOT}/install-symlink-cleanup-pin-tools"
install_symlink_cleanup_pin_stage="${TEST_ROOT}/install-symlink-cleanup-pin-stage"
install_symlink_cleanup_pin_prefix="/opt/frost-install-symlink-cleanup-pin"
install_symlink_cleanup_pin_workflow_dir="${install_symlink_cleanup_pin_stage}${install_symlink_cleanup_pin_prefix}/share/frost/workflows"
install_symlink_cleanup_pin_first="${install_symlink_cleanup_pin_workflow_dir}/${WORKFLOW_SOURCES[0]##*/}"
install_symlink_cleanup_pin_secret="${TEST_ROOT}/cleanup-pin-private-referent"
install_symlink_cleanup_pin_backup_log="${TEST_ROOT}/install-symlink-cleanup-pin-backup"
install_symlink_cleanup_pin_path_log="${TEST_ROOT}/install-symlink-cleanup-pin-path"
mkdir -p "${install_symlink_cleanup_pin_tools}" \
    "${install_symlink_cleanup_pin_workflow_dir}"
printf 'cleanup pin private sentinel\n' \
    >"${install_symlink_cleanup_pin_secret}"
ln -s -- "${install_symlink_cleanup_pin_secret}" \
    "${install_symlink_cleanup_pin_first}"
install_symlink_cleanup_pin_value="$(readlink -- \
    "${install_symlink_cleanup_pin_first}")"
# shellcheck disable=SC2016
printf '%s\n' \
    '#!/bin/sh' \
    'set -eu' \
    'previous=' \
    'last=' \
    'for argument do previous=${last}; last=${argument}; done' \
    'case "${previous}" in' \
    '    /proc/self/fd/*/.frost.install.*)' \
    '        /usr/bin/mv "$@"' \
    '        parent=${FROST_TEST_INSTALL_SYMLINK_CLEANUP_PIN_PARENT:?}' \
    '        basename=${FROST_TEST_INSTALL_SYMLINK_CLEANUP_PIN_BASENAME:?}' \
    '        backup=$(/usr/bin/find "${parent}" -maxdepth 1 -name ".${basename}.rollback.??????" -print -quit)' \
    '        pin=$(/usr/bin/find "${parent}" -maxdepth 1 -name ".${basename}.rollback-pin.*" -print -quit)' \
    '        : "${backup:?missing cleanup pin backup}"' \
    '        : "${pin:?missing cleanup pin}"' \
    '        printf "%s\n" "${backup}" >"${FROST_TEST_INSTALL_SYMLINK_CLEANUP_PIN_BACKUP_LOG:?}"' \
    '        printf "%s\n" "${pin}" >"${FROST_TEST_INSTALL_SYMLINK_CLEANUP_PIN_PATH_LOG:?}"' \
    '        /usr/bin/rm -f -- "${pin}"' \
    '        /usr/bin/ln -s -- "${FROST_TEST_INSTALL_SYMLINK_CLEANUP_PIN_SECRET:?}" "${pin}"' \
    '        exit 0' \
    '        ;;' \
    'esac' \
    'exec /usr/bin/mv "$@"' \
    >"${install_symlink_cleanup_pin_tools}/mv"
chmod 0755 "${install_symlink_cleanup_pin_tools}/mv"
install_symlink_cleanup_pin_log="${TEST_ROOT}/install-symlink-cleanup-pin.log"
env HOME="${TEST_HOME}" \
    PATH="${install_symlink_cleanup_pin_tools}:${TEST_PATH}" \
    FROST_TEST_INSTALL_SYMLINK_CLEANUP_PIN_PARENT="${install_symlink_cleanup_pin_workflow_dir}" \
    FROST_TEST_INSTALL_SYMLINK_CLEANUP_PIN_BASENAME="${install_symlink_cleanup_pin_first##*/}" \
    FROST_TEST_INSTALL_SYMLINK_CLEANUP_PIN_BACKUP_LOG="${install_symlink_cleanup_pin_backup_log}" \
    FROST_TEST_INSTALL_SYMLINK_CLEANUP_PIN_PATH_LOG="${install_symlink_cleanup_pin_path_log}" \
    FROST_TEST_INSTALL_SYMLINK_CLEANUP_PIN_SECRET="${install_symlink_cleanup_pin_secret}" \
    DESTDIR="${install_symlink_cleanup_pin_stage}" "${INSTALLER}" \
    --binary "${prebuilt_binary}" \
    --prefix "${install_symlink_cleanup_pin_prefix}" --no-desktop \
    >"${install_symlink_cleanup_pin_log}" 2>&1
install_symlink_cleanup_pin_backup="$(<"${install_symlink_cleanup_pin_backup_log}")"
install_symlink_cleanup_pin_path="$(<"${install_symlink_cleanup_pin_path_log}")"
[[ -L "${install_symlink_cleanup_pin_backup}" ]] \
    || fail "cleanup deleted the exact main backup after pin replacement"
[[ -L "${install_symlink_cleanup_pin_path}" \
    && "$(readlink -- "${install_symlink_cleanup_pin_path}")" == \
        "${install_symlink_cleanup_pin_secret}" ]] \
    || fail "cleanup changed or deleted the pin substitute"
if grep -Fq -- "${install_symlink_cleanup_pin_secret}" \
    "${install_symlink_cleanup_pin_log}"; then
    fail "pin-replacement diagnostics disclosed symlink contents"
fi
install_symlink_cleanup_pin_command="$(extract_symlink_recovery_command \
    "pin replacement" "${install_symlink_cleanup_pin_log}")"
bash -n -c "${install_symlink_cleanup_pin_command}" \
    || fail "pin-replacement recovery command is not valid shell"
PATH="${TEST_PATH}" bash -c "${install_symlink_cleanup_pin_command}"
[[ -L "${install_symlink_cleanup_pin_first}" \
    && "$(readlink -- "${install_symlink_cleanup_pin_first}")" == \
        "${install_symlink_cleanup_pin_value}" ]] \
    || fail "pin-replacement recovery command did not restore the symlink"
assert_absent "consumed exact main recovery backup" \
    "${install_symlink_cleanup_pin_backup}"
[[ -L "${install_symlink_cleanup_pin_path}" ]] \
    || fail "pin-replacement recovery touched the substitute"

# Main and pin are two names for the same owned inode. Exchanging the names and
# then recreating the pin as another hardlink to that exact inode produces the
# same recorded (directory,name,device,inode) state, so cleanup may remove both.
install_symlink_cleanup_exact_tools="${TEST_ROOT}/install-symlink-cleanup-exact-tools"
install_symlink_cleanup_exact_stage="${TEST_ROOT}/install-symlink-cleanup-exact-stage"
install_symlink_cleanup_exact_prefix="/opt/frost-install-symlink-cleanup-exact"
install_symlink_cleanup_exact_workflow_dir="${install_symlink_cleanup_exact_stage}${install_symlink_cleanup_exact_prefix}/share/frost/workflows"
install_symlink_cleanup_exact_first="${install_symlink_cleanup_exact_workflow_dir}/${WORKFLOW_SOURCES[0]##*/}"
install_symlink_cleanup_exact_secret="${TEST_ROOT}/cleanup-exact-private-referent"$'\n\n'
install_symlink_cleanup_exact_marker="${TEST_ROOT}/install-symlink-cleanup-exact-marker"
mkdir -p "${install_symlink_cleanup_exact_tools}" \
    "${install_symlink_cleanup_exact_workflow_dir}"
printf 'cleanup exact private sentinel\n' \
    >"${install_symlink_cleanup_exact_secret}"
ln -s -- "${install_symlink_cleanup_exact_secret}" \
    "${install_symlink_cleanup_exact_first}"
# shellcheck disable=SC2016
printf '%s\n' \
    '#!/bin/sh' \
    'set -eu' \
    'previous=' \
    'last=' \
    'for argument do previous=${last}; last=${argument}; done' \
    'case "${previous}" in' \
    '    /proc/self/fd/*/.frost.install.*)' \
    '        /usr/bin/mv "$@"' \
    '        parent=${FROST_TEST_INSTALL_SYMLINK_CLEANUP_EXACT_PARENT:?}' \
    '        basename=${FROST_TEST_INSTALL_SYMLINK_CLEANUP_EXACT_BASENAME:?}' \
    '        backup=$(/usr/bin/find "${parent}" -maxdepth 1 -name ".${basename}.rollback.??????" -print -quit)' \
    '        pin=$(/usr/bin/find "${parent}" -maxdepth 1 -name ".${basename}.rollback-pin.*" -print -quit)' \
    '        swap=${parent}/.${basename}.rollback-swap' \
    '        : "${backup:?missing exact cleanup backup}"' \
    '        : "${pin:?missing exact cleanup pin}"' \
    '        /usr/bin/mv -- "${backup}" "${swap}"' \
    '        /usr/bin/mv -- "${pin}" "${backup}"' \
    '        /usr/bin/mv -- "${swap}" "${pin}"' \
    '        /usr/bin/rm -f -- "${pin}"' \
    '        /usr/bin/ln -P -- "${backup}" "${pin}"' \
    '        : >"${FROST_TEST_INSTALL_SYMLINK_CLEANUP_EXACT_MARKER:?}"' \
    '        exit 0' \
    '        ;;' \
    'esac' \
    'exec /usr/bin/mv "$@"' \
    >"${install_symlink_cleanup_exact_tools}/mv"
chmod 0755 "${install_symlink_cleanup_exact_tools}/mv"
install_symlink_cleanup_exact_log="${TEST_ROOT}/install-symlink-cleanup-exact.log"
env HOME="${TEST_HOME}" \
    PATH="${install_symlink_cleanup_exact_tools}:${TEST_PATH}" \
    FROST_TEST_INSTALL_SYMLINK_CLEANUP_EXACT_PARENT="${install_symlink_cleanup_exact_workflow_dir}" \
    FROST_TEST_INSTALL_SYMLINK_CLEANUP_EXACT_BASENAME="${install_symlink_cleanup_exact_first##*/}" \
    FROST_TEST_INSTALL_SYMLINK_CLEANUP_EXACT_MARKER="${install_symlink_cleanup_exact_marker}" \
    DESTDIR="${install_symlink_cleanup_exact_stage}" "${INSTALLER}" \
    --binary "${prebuilt_binary}" \
    --prefix "${install_symlink_cleanup_exact_prefix}" --no-desktop \
    >"${install_symlink_cleanup_exact_log}" 2>&1
assert_regular_file "exact symlink cleanup exchange marker" \
    "${install_symlink_cleanup_exact_marker}"
[[ -z "$(find "${install_symlink_cleanup_exact_stage}" \
    \( -name '*.rollback.*' -o -name '*.rollback-pin.*' \
        -o -name '*.rollback-anchor.*' -o -name '*.rollback-swap' \) \
        -print -quit)" ]] \
    || fail "exact name exchange/hardlink merge left a rollback artifact"
if grep -Fq 'recovery command (symlink contents not displayed)' \
    "${install_symlink_cleanup_exact_log}"; then
    fail "exact name exchange incorrectly retained a recovery artifact"
fi
[[ "$(<"${install_symlink_cleanup_exact_secret}")" == \
    'cleanup exact private sentinel' ]] \
    || fail "exact name exchange followed the old symlink referent"

# In contrast, merging both public names as hardlinks to one foreign symlink
# does not make either name transaction-owned. The private anchor keeps the old
# inode live, so both substitutes survive and recovery selects only that anchor.
install_symlink_cleanup_foreign_tools="${TEST_ROOT}/install-symlink-cleanup-foreign-tools"
install_symlink_cleanup_foreign_stage="${TEST_ROOT}/install-symlink-cleanup-foreign-stage"
install_symlink_cleanup_foreign_prefix="/opt/frost-install-symlink-cleanup-foreign"
install_symlink_cleanup_foreign_workflow_dir="${install_symlink_cleanup_foreign_stage}${install_symlink_cleanup_foreign_prefix}/share/frost/workflows"
install_symlink_cleanup_foreign_first="${install_symlink_cleanup_foreign_workflow_dir}/${WORKFLOW_SOURCES[0]##*/}"
install_symlink_cleanup_foreign_secret="${TEST_ROOT}/cleanup-foreign-private-referent"
install_symlink_cleanup_foreign_backup_log="${TEST_ROOT}/install-symlink-cleanup-foreign-backup"
install_symlink_cleanup_foreign_pin_log="${TEST_ROOT}/install-symlink-cleanup-foreign-pin"
mkdir -p "${install_symlink_cleanup_foreign_tools}" \
    "${install_symlink_cleanup_foreign_workflow_dir}"
printf 'cleanup foreign private sentinel\n' \
    >"${install_symlink_cleanup_foreign_secret}"
ln -s -- "${TEST_ROOT}/cleanup-foreign-original-referent" \
    "${install_symlink_cleanup_foreign_first}"
# shellcheck disable=SC2016
printf '%s\n' \
    '#!/bin/sh' \
    'set -eu' \
    'previous=' \
    'last=' \
    'for argument do previous=${last}; last=${argument}; done' \
    'case "${previous}" in' \
    '    /proc/self/fd/*/.frost.install.*)' \
    '        /usr/bin/mv "$@"' \
    '        parent=${FROST_TEST_INSTALL_SYMLINK_CLEANUP_FOREIGN_PARENT:?}' \
    '        basename=${FROST_TEST_INSTALL_SYMLINK_CLEANUP_FOREIGN_BASENAME:?}' \
    '        backup=$(/usr/bin/find "${parent}" -maxdepth 1 -name ".${basename}.rollback.??????" -print -quit)' \
    '        pin=$(/usr/bin/find "${parent}" -maxdepth 1 -name ".${basename}.rollback-pin.*" -print -quit)' \
    '        foreign=${parent}/.${basename}.foreign-link' \
    '        : "${backup:?missing foreign cleanup backup}"' \
    '        : "${pin:?missing foreign cleanup pin}"' \
    '        printf "%s\n" "${backup}" >"${FROST_TEST_INSTALL_SYMLINK_CLEANUP_FOREIGN_BACKUP_LOG:?}"' \
    '        printf "%s\n" "${pin}" >"${FROST_TEST_INSTALL_SYMLINK_CLEANUP_FOREIGN_PIN_LOG:?}"' \
    '        /usr/bin/rm -f -- "${backup}" "${pin}"' \
    '        /usr/bin/ln -s -- "${FROST_TEST_INSTALL_SYMLINK_CLEANUP_FOREIGN_SECRET:?}" "${foreign}"' \
    '        /usr/bin/ln -P -- "${foreign}" "${backup}"' \
    '        /usr/bin/ln -P -- "${foreign}" "${pin}"' \
    '        /usr/bin/rm -f -- "${foreign}"' \
    '        exit 0' \
    '        ;;' \
    'esac' \
    'exec /usr/bin/mv "$@"' \
    >"${install_symlink_cleanup_foreign_tools}/mv"
chmod 0755 "${install_symlink_cleanup_foreign_tools}/mv"
install_symlink_cleanup_foreign_log="${TEST_ROOT}/install-symlink-cleanup-foreign.log"
env HOME="${TEST_HOME}" \
    PATH="${install_symlink_cleanup_foreign_tools}:${TEST_PATH}" \
    FROST_TEST_INSTALL_SYMLINK_CLEANUP_FOREIGN_PARENT="${install_symlink_cleanup_foreign_workflow_dir}" \
    FROST_TEST_INSTALL_SYMLINK_CLEANUP_FOREIGN_BASENAME="${install_symlink_cleanup_foreign_first##*/}" \
    FROST_TEST_INSTALL_SYMLINK_CLEANUP_FOREIGN_SECRET="${install_symlink_cleanup_foreign_secret}" \
    FROST_TEST_INSTALL_SYMLINK_CLEANUP_FOREIGN_BACKUP_LOG="${install_symlink_cleanup_foreign_backup_log}" \
    FROST_TEST_INSTALL_SYMLINK_CLEANUP_FOREIGN_PIN_LOG="${install_symlink_cleanup_foreign_pin_log}" \
    DESTDIR="${install_symlink_cleanup_foreign_stage}" "${INSTALLER}" \
    --binary "${prebuilt_binary}" \
    --prefix "${install_symlink_cleanup_foreign_prefix}" --no-desktop \
    >"${install_symlink_cleanup_foreign_log}" 2>&1
install_symlink_cleanup_foreign_backup="$(<"${install_symlink_cleanup_foreign_backup_log}")"
install_symlink_cleanup_foreign_pin="$(<"${install_symlink_cleanup_foreign_pin_log}")"
install_symlink_cleanup_foreign_anchor="$(find \
    "${install_symlink_cleanup_foreign_workflow_dir}" -mindepth 2 -maxdepth 2 \
    -path "*/.${install_symlink_cleanup_foreign_first##*/}.rollback-anchor.??????/snapshot" \
    -print -quit)"
[[ -L "${install_symlink_cleanup_foreign_backup}" \
    && -L "${install_symlink_cleanup_foreign_pin}" ]] \
    || fail "cleanup removed a foreign hardlink-merged substitute"
[[ "$(stat -c '%d:%i' -- "${install_symlink_cleanup_foreign_backup}")" == \
    "$(stat -c '%d:%i' -- "${install_symlink_cleanup_foreign_pin}")" ]] \
    || fail "foreign hardlink-merged names no longer share one inode"
[[ "$(readlink -- "${install_symlink_cleanup_foreign_backup}")" == \
    "${install_symlink_cleanup_foreign_secret}" \
    && "$(readlink -- "${install_symlink_cleanup_foreign_pin}")" == \
        "${install_symlink_cleanup_foreign_secret}" ]] \
    || fail "cleanup changed a foreign hardlink-merged substitute"
if grep -Fq -- "${install_symlink_cleanup_foreign_secret}" \
    "${install_symlink_cleanup_foreign_log}"; then
    fail "foreign-pair diagnostics disclosed symlink contents"
fi
[[ -L "${install_symlink_cleanup_foreign_anchor}" \
    && "$(readlink -- "${install_symlink_cleanup_foreign_anchor}")" == \
        "${TEST_ROOT}/cleanup-foreign-original-referent" ]] \
    || fail "foreign hardlink merge lost the private exact recovery anchor"
install_symlink_cleanup_foreign_command="$(extract_symlink_recovery_command \
    "foreign public-name merge" "${install_symlink_cleanup_foreign_log}")"
bash -n -c "${install_symlink_cleanup_foreign_command}" \
    || fail "foreign-merge recovery command is not valid shell"
PATH="${TEST_PATH}" bash -c "${install_symlink_cleanup_foreign_command}"
[[ -L "${install_symlink_cleanup_foreign_first}" \
    && "$(readlink -- "${install_symlink_cleanup_foreign_first}")" == \
        "${TEST_ROOT}/cleanup-foreign-original-referent" ]] \
    || fail "private anchor recovery did not restore the original symlink"
[[ -L "${install_symlink_cleanup_foreign_backup}" \
    && -L "${install_symlink_cleanup_foreign_pin}" ]] \
    || fail "private anchor recovery touched a foreign public substitute"

# Even the private name can be removed by another process running as the same
# uid. Force all three links away, churn the private name until the filesystem
# reuses the recorded inode number, and recreate a foreign three-link set. The
# stored no-follow text/metadata fingerprint must reject that numeric ABA and
# retain every substitute without advertising an exact recovery command.
install_symlink_cleanup_reuse_tools="${TEST_ROOT}/install-symlink-cleanup-reuse-tools"
install_symlink_cleanup_reuse_stage="${TEST_ROOT}/install-symlink-cleanup-reuse-stage"
install_symlink_cleanup_reuse_prefix="/opt/frost-install-symlink-cleanup-reuse"
install_symlink_cleanup_reuse_workflow_dir="${install_symlink_cleanup_reuse_stage}${install_symlink_cleanup_reuse_prefix}/share/frost/workflows"
install_symlink_cleanup_reuse_first="${install_symlink_cleanup_reuse_workflow_dir}/${WORKFLOW_SOURCES[0]##*/}"
install_symlink_cleanup_reuse_secret="${TEST_ROOT}/cleanup-reuse-private-referent"
install_symlink_cleanup_reuse_marker="${TEST_ROOT}/install-symlink-cleanup-reuse-marker"
install_symlink_cleanup_reuse_backup_log="${TEST_ROOT}/install-symlink-cleanup-reuse-backup"
install_symlink_cleanup_reuse_pin_log="${TEST_ROOT}/install-symlink-cleanup-reuse-pin"
install_symlink_cleanup_reuse_anchor_log="${TEST_ROOT}/install-symlink-cleanup-reuse-anchor"
mkdir -p "${install_symlink_cleanup_reuse_tools}" \
    "${install_symlink_cleanup_reuse_workflow_dir}"
printf 'numeric ABA private sentinel\n' \
    >"${install_symlink_cleanup_reuse_secret}"
ln -s -- "${TEST_ROOT}/cleanup-reuse-original-referent" \
    "${install_symlink_cleanup_reuse_first}"
# shellcheck disable=SC2016
printf '%s\n' \
    '#!/bin/sh' \
    'set -eu' \
    'previous=' \
    'last=' \
    'for argument do previous=${last}; last=${argument}; done' \
    'case "${previous}" in' \
    '    /proc/self/fd/*/.frost.install.*)' \
    '        /usr/bin/mv "$@"' \
    '        parent=${FROST_TEST_INSTALL_SYMLINK_CLEANUP_REUSE_PARENT:?}' \
    '        basename=${FROST_TEST_INSTALL_SYMLINK_CLEANUP_REUSE_BASENAME:?}' \
    '        backup=$(/usr/bin/find "${parent}" -maxdepth 1 -name ".${basename}.rollback.??????" -print -quit)' \
    '        pin=$(/usr/bin/find "${parent}" -maxdepth 1 -name ".${basename}.rollback-pin.*" -print -quit)' \
    '        anchor=$(/usr/bin/find "${parent}" -mindepth 2 -maxdepth 2 -path "*/.${basename}.rollback-anchor.??????/snapshot" -print -quit)' \
    '        : "${backup:?missing reuse backup}"' \
    '        : "${pin:?missing reuse pin}"' \
    '        : "${anchor:?missing reuse anchor}"' \
    '        expected=$(/usr/bin/stat -c "%d:%i" -- "${anchor}")' \
    '        printf "%s\n" "${backup}" >"${FROST_TEST_INSTALL_SYMLINK_CLEANUP_REUSE_BACKUP_LOG:?}"' \
    '        printf "%s\n" "${pin}" >"${FROST_TEST_INSTALL_SYMLINK_CLEANUP_REUSE_PIN_LOG:?}"' \
    '        printf "%s\n" "${anchor}" >"${FROST_TEST_INSTALL_SYMLINK_CLEANUP_REUSE_ANCHOR_LOG:?}"' \
    '        /usr/bin/rm -f -- "${backup}" "${pin}" "${anchor}"' \
    '        actual=' \
    '        attempt=0' \
    '        while [ "${attempt}" -lt 20000 ]; do' \
    '            /usr/bin/ln -s -- "${FROST_TEST_INSTALL_SYMLINK_CLEANUP_REUSE_SECRET:?}" "${anchor}"' \
    '            actual=$(/usr/bin/stat -c "%d:%i" -- "${anchor}")' \
    '            [ "${actual}" != "${expected}" ] || break' \
    '            /usr/bin/rm -f -- "${anchor}"' \
    '            attempt=$((attempt + 1))' \
    '        done' \
    '        [ "${actual}" = "${expected}" ] || exit 97' \
    '        /usr/bin/ln -P -- "${anchor}" "${backup}"' \
    '        /usr/bin/ln -P -- "${anchor}" "${pin}"' \
    '        printf "%s %s\n" "${expected}" "${actual}" >"${FROST_TEST_INSTALL_SYMLINK_CLEANUP_REUSE_MARKER:?}"' \
    '        exit 0' \
    '        ;;' \
    'esac' \
    'exec /usr/bin/mv "$@"' \
    >"${install_symlink_cleanup_reuse_tools}/mv"
chmod 0755 "${install_symlink_cleanup_reuse_tools}/mv"
install_symlink_cleanup_reuse_log="${TEST_ROOT}/install-symlink-cleanup-reuse.log"
env HOME="${TEST_HOME}" \
    PATH="${install_symlink_cleanup_reuse_tools}:${TEST_PATH}" \
    FROST_TEST_INSTALL_SYMLINK_CLEANUP_REUSE_PARENT="${install_symlink_cleanup_reuse_workflow_dir}" \
    FROST_TEST_INSTALL_SYMLINK_CLEANUP_REUSE_BASENAME="${install_symlink_cleanup_reuse_first##*/}" \
    FROST_TEST_INSTALL_SYMLINK_CLEANUP_REUSE_SECRET="${install_symlink_cleanup_reuse_secret}" \
    FROST_TEST_INSTALL_SYMLINK_CLEANUP_REUSE_MARKER="${install_symlink_cleanup_reuse_marker}" \
    FROST_TEST_INSTALL_SYMLINK_CLEANUP_REUSE_BACKUP_LOG="${install_symlink_cleanup_reuse_backup_log}" \
    FROST_TEST_INSTALL_SYMLINK_CLEANUP_REUSE_PIN_LOG="${install_symlink_cleanup_reuse_pin_log}" \
    FROST_TEST_INSTALL_SYMLINK_CLEANUP_REUSE_ANCHOR_LOG="${install_symlink_cleanup_reuse_anchor_log}" \
    DESTDIR="${install_symlink_cleanup_reuse_stage}" "${INSTALLER}" \
    --binary "${prebuilt_binary}" \
    --prefix "${install_symlink_cleanup_reuse_prefix}" --no-desktop \
    >"${install_symlink_cleanup_reuse_log}" 2>&1
read -r install_symlink_cleanup_reuse_expected \
    install_symlink_cleanup_reuse_actual \
    <"${install_symlink_cleanup_reuse_marker}"
[[ "${install_symlink_cleanup_reuse_expected}" == \
    "${install_symlink_cleanup_reuse_actual}" ]] \
    || fail "numeric ABA fixture did not reuse the old symlink inode"
for install_symlink_cleanup_reuse_path_log in \
    "${install_symlink_cleanup_reuse_backup_log}" \
    "${install_symlink_cleanup_reuse_pin_log}" \
    "${install_symlink_cleanup_reuse_anchor_log}"; do
    install_symlink_cleanup_reuse_path="$(<"${install_symlink_cleanup_reuse_path_log}")"
    [[ -L "${install_symlink_cleanup_reuse_path}" \
        && "$(readlink -- "${install_symlink_cleanup_reuse_path}")" == \
            "${install_symlink_cleanup_reuse_secret}" ]] \
        || fail "numeric ABA cleanup removed or changed a foreign substitute"
done
if grep -Fq -- "${install_symlink_cleanup_reuse_secret}" \
    "${install_symlink_cleanup_reuse_log}"; then
    fail "numeric ABA diagnostics disclosed symlink contents"
fi
if grep -Fq 'recovery command (symlink contents not displayed)' \
    "${install_symlink_cleanup_reuse_log}"; then
    fail "numeric ABA cleanup advertised a foreign recovery name"
fi
assert_contains "numeric ABA fail-closed diagnostic" \
    "$(<"${install_symlink_cleanup_reuse_log}")" \
    "retaining symlink rollback snapshot (preflight identity changed)"

# Treat cleanup rm as a use point in its own right. After the main link is
# removed, rename the bound workflow parent and replace the logical path. The
# installer must stop before pin/anchor removal and recover only in the old
# physical directory.
install_symlink_cleanup_main_rm_tools="${TEST_ROOT}/install-symlink-cleanup-main-rm-tools"
install_symlink_cleanup_main_rm_stage="${TEST_ROOT}/install-symlink-cleanup-main-rm-stage"
install_symlink_cleanup_main_rm_prefix="/opt/frost-install-symlink-cleanup-main-rm"
install_symlink_cleanup_main_rm_parent="${install_symlink_cleanup_main_rm_stage}${install_symlink_cleanup_main_rm_prefix}/share/frost/workflows"
install_symlink_cleanup_main_rm_first="${install_symlink_cleanup_main_rm_parent}/${WORKFLOW_SOURCES[0]##*/}"
install_symlink_cleanup_main_rm_displaced="${TEST_ROOT}/install-symlink-cleanup-main-rm-displaced"
install_symlink_cleanup_main_rm_outside="${TEST_ROOT}/install-symlink-cleanup-main-rm-outside"
install_symlink_cleanup_main_rm_secret="${TEST_ROOT}/install-symlink-cleanup-main-rm-secret"
install_symlink_cleanup_main_rm_state="${TEST_ROOT}/install-symlink-cleanup-main-rm-state"
mkdir -p "${install_symlink_cleanup_main_rm_tools}" \
    "${install_symlink_cleanup_main_rm_parent}" \
    "${install_symlink_cleanup_main_rm_outside}"
printf 'main rm private sentinel\n' >"${install_symlink_cleanup_main_rm_secret}"
printf 'main rm outside sentinel\n' \
    >"${install_symlink_cleanup_main_rm_outside}/${WORKFLOW_SOURCES[0]##*/}"
ln -s -- "${install_symlink_cleanup_main_rm_secret}" \
    "${install_symlink_cleanup_main_rm_first}"
install_symlink_cleanup_main_rm_value="$(readlink -- \
    "${install_symlink_cleanup_main_rm_first}")"
# shellcheck disable=SC2016
printf '%s\n' \
    '#!/bin/sh' \
    'set -eu' \
    'last=' \
    'for argument do last=${argument}; done' \
    'case "${last}" in' \
    '    /proc/self/fd/*/.docker-tail-logs.yaml.rollback.??????)' \
    '        state=${FROST_TEST_INSTALL_SYMLINK_CLEANUP_MAIN_RM_STATE:?}' \
    '        count=0' \
    '        [ ! -f "${state}" ] || read -r count <"${state}"' \
    '        count=$((count + 1))' \
    '        printf "%s\n" "${count}" >"${state}"' \
    '        /usr/bin/rm "$@"' \
    '        if [ "${count}" -eq 2 ]; then' \
    '            /usr/bin/mv -- "${FROST_TEST_INSTALL_SYMLINK_CLEANUP_MAIN_RM_PARENT:?}" "${FROST_TEST_INSTALL_SYMLINK_CLEANUP_MAIN_RM_DISPLACED:?}"' \
    '            /usr/bin/ln -s -- "${FROST_TEST_INSTALL_SYMLINK_CLEANUP_MAIN_RM_OUTSIDE:?}" "${FROST_TEST_INSTALL_SYMLINK_CLEANUP_MAIN_RM_PARENT}"' \
    '        fi' \
    '        exit 0' \
    '        ;;' \
    'esac' \
    'exec /usr/bin/rm "$@"' \
    >"${install_symlink_cleanup_main_rm_tools}/rm"
chmod 0755 "${install_symlink_cleanup_main_rm_tools}/rm"
install_symlink_cleanup_main_rm_log="${TEST_ROOT}/install-symlink-cleanup-main-rm.log"
env HOME="${TEST_HOME}" \
    PATH="${install_symlink_cleanup_main_rm_tools}:${TEST_PATH}" \
    FROST_TEST_INSTALL_SYMLINK_CLEANUP_MAIN_RM_STATE="${install_symlink_cleanup_main_rm_state}" \
    FROST_TEST_INSTALL_SYMLINK_CLEANUP_MAIN_RM_PARENT="${install_symlink_cleanup_main_rm_parent}" \
    FROST_TEST_INSTALL_SYMLINK_CLEANUP_MAIN_RM_DISPLACED="${install_symlink_cleanup_main_rm_displaced}" \
    FROST_TEST_INSTALL_SYMLINK_CLEANUP_MAIN_RM_OUTSIDE="${install_symlink_cleanup_main_rm_outside}" \
    DESTDIR="${install_symlink_cleanup_main_rm_stage}" "${INSTALLER}" \
    --binary "${prebuilt_binary}" \
    --prefix "${install_symlink_cleanup_main_rm_prefix}" --no-desktop \
    >"${install_symlink_cleanup_main_rm_log}" 2>&1
[[ "$(<"${install_symlink_cleanup_main_rm_state}")" == 2 ]] \
    || fail "main rollback name was not removed at both reservation and cleanup"
[[ -L "${install_symlink_cleanup_main_rm_parent}" ]] \
    || fail "main cleanup use point did not replace the logical parent"
install_symlink_cleanup_main_rm_pin="$(find \
    "${install_symlink_cleanup_main_rm_displaced}" -maxdepth 1 \
    -name '.docker-tail-logs.yaml.rollback-pin.*' -print -quit)"
install_symlink_cleanup_main_rm_anchor="$(find \
    "${install_symlink_cleanup_main_rm_displaced}" -mindepth 2 -maxdepth 2 \
    -path '*/.docker-tail-logs.yaml.rollback-anchor.??????/snapshot' \
    -print -quit)"
[[ -L "${install_symlink_cleanup_main_rm_pin}" \
    && -L "${install_symlink_cleanup_main_rm_anchor}" ]] \
    || fail "parent rename after main cleanup lost pin/anchor recovery links"
[[ "$(stat -c '%d:%i' -- "${install_symlink_cleanup_main_rm_pin}")" == \
    "$(stat -c '%d:%i' -- "${install_symlink_cleanup_main_rm_anchor}")" ]] \
    || fail "retained pin/anchor no longer share the exact inode"
install_symlink_cleanup_main_rm_command="$(extract_symlink_recovery_command \
    "main cleanup use point" "${install_symlink_cleanup_main_rm_log}")"
bash -n -c "${install_symlink_cleanup_main_rm_command}" \
    || fail "main cleanup use-point recovery command is not valid shell"
PATH="${TEST_PATH}" bash -c "${install_symlink_cleanup_main_rm_command}"
[[ -L "${install_symlink_cleanup_main_rm_displaced}/${WORKFLOW_SOURCES[0]##*/}" \
    && "$(readlink -- \
        "${install_symlink_cleanup_main_rm_displaced}/${WORKFLOW_SOURCES[0]##*/}")" == \
        "${install_symlink_cleanup_main_rm_value}" ]] \
    || fail "main cleanup use-point recovery restored the wrong symlink"
[[ "$(<"${install_symlink_cleanup_main_rm_outside}/${WORKFLOW_SOURCES[0]##*/}")" == \
    'main rm outside sentinel' ]] \
    || fail "main cleanup use point crossed into the replacement parent"

# The pin unlink is a separate use point after main is already gone. Recreate a
# foreign pin only after rm completed: cleanup must not touch it or proceed to
# the exact private anchor, which remains the sole recovery source.
install_symlink_cleanup_pin_rm_tools="${TEST_ROOT}/install-symlink-cleanup-pin-rm-tools"
install_symlink_cleanup_pin_rm_stage="${TEST_ROOT}/install-symlink-cleanup-pin-rm-stage"
install_symlink_cleanup_pin_rm_prefix="/opt/frost-install-symlink-cleanup-pin-rm"
install_symlink_cleanup_pin_rm_parent="${install_symlink_cleanup_pin_rm_stage}${install_symlink_cleanup_pin_rm_prefix}/share/frost/workflows"
install_symlink_cleanup_pin_rm_first="${install_symlink_cleanup_pin_rm_parent}/${WORKFLOW_SOURCES[0]##*/}"
install_symlink_cleanup_pin_rm_original="${TEST_ROOT}/install-symlink-cleanup-pin-rm-original"
install_symlink_cleanup_pin_rm_foreign="${TEST_ROOT}/install-symlink-cleanup-pin-rm-foreign"
install_symlink_cleanup_pin_rm_state="${TEST_ROOT}/install-symlink-cleanup-pin-rm-state"
install_symlink_cleanup_pin_rm_path_log="${TEST_ROOT}/install-symlink-cleanup-pin-rm-path"
mkdir -p "${install_symlink_cleanup_pin_rm_tools}" \
    "${install_symlink_cleanup_pin_rm_parent}"
printf 'pin rm original sentinel\n' >"${install_symlink_cleanup_pin_rm_original}"
printf 'pin rm foreign sentinel\n' >"${install_symlink_cleanup_pin_rm_foreign}"
ln -s -- "${install_symlink_cleanup_pin_rm_original}" \
    "${install_symlink_cleanup_pin_rm_first}"
# shellcheck disable=SC2016
printf '%s\n' \
    '#!/bin/sh' \
    'set -eu' \
    'last=' \
    'for argument do last=${argument}; done' \
    'case "${last}" in' \
    '    /proc/self/fd/*/*.rollback-pin.*)' \
    '        state=${FROST_TEST_INSTALL_SYMLINK_CLEANUP_PIN_RM_STATE:?}' \
    '        count=0' \
    '        [ ! -f "${state}" ] || read -r count <"${state}"' \
    '        count=$((count + 1))' \
    '        printf "%s\n" "${count}" >"${state}"' \
    '        /usr/bin/rm "$@"' \
    '        if [ "${count}" -eq 2 ]; then' \
    '            parent=$(/usr/bin/readlink -- "${last%/*}")' \
    '            printf "%s/%s\n" "${parent}" "${last##*/}" >"${FROST_TEST_INSTALL_SYMLINK_CLEANUP_PIN_RM_PATH_LOG:?}"' \
    '            /usr/bin/ln -s -- "${FROST_TEST_INSTALL_SYMLINK_CLEANUP_PIN_RM_FOREIGN:?}" "${last}"' \
    '        fi' \
    '        exit 0' \
    '        ;;' \
    'esac' \
    'exec /usr/bin/rm "$@"' \
    >"${install_symlink_cleanup_pin_rm_tools}/rm"
chmod 0755 "${install_symlink_cleanup_pin_rm_tools}/rm"
install_symlink_cleanup_pin_rm_log="${TEST_ROOT}/install-symlink-cleanup-pin-rm.log"
env HOME="${TEST_HOME}" \
    PATH="${install_symlink_cleanup_pin_rm_tools}:${TEST_PATH}" \
    FROST_TEST_INSTALL_SYMLINK_CLEANUP_PIN_RM_STATE="${install_symlink_cleanup_pin_rm_state}" \
    FROST_TEST_INSTALL_SYMLINK_CLEANUP_PIN_RM_PATH_LOG="${install_symlink_cleanup_pin_rm_path_log}" \
    FROST_TEST_INSTALL_SYMLINK_CLEANUP_PIN_RM_FOREIGN="${install_symlink_cleanup_pin_rm_foreign}" \
    DESTDIR="${install_symlink_cleanup_pin_rm_stage}" "${INSTALLER}" \
    --binary "${prebuilt_binary}" \
    --prefix "${install_symlink_cleanup_pin_rm_prefix}" --no-desktop \
    >"${install_symlink_cleanup_pin_rm_log}" 2>&1
install_symlink_cleanup_pin_rm_path="$(<"${install_symlink_cleanup_pin_rm_path_log}")"
install_symlink_cleanup_pin_rm_anchor="$(find \
    "${install_symlink_cleanup_pin_rm_parent}" -mindepth 2 -maxdepth 2 \
    -path '*/.docker-tail-logs.yaml.rollback-anchor.??????/snapshot' \
    -print -quit)"
[[ -L "${install_symlink_cleanup_pin_rm_path}" \
    && "$(readlink -- "${install_symlink_cleanup_pin_rm_path}")" == \
        "${install_symlink_cleanup_pin_rm_foreign}" ]] \
    || fail "pin cleanup use point removed or changed its substitute"
[[ -L "${install_symlink_cleanup_pin_rm_anchor}" \
    && "$(readlink -- "${install_symlink_cleanup_pin_rm_anchor}")" == \
        "${install_symlink_cleanup_pin_rm_original}" ]] \
    || fail "pin cleanup use point did not retain the private anchor"
install_symlink_cleanup_pin_rm_command="$(extract_symlink_recovery_command \
    "pin cleanup use point" "${install_symlink_cleanup_pin_rm_log}")"
bash -n -c "${install_symlink_cleanup_pin_rm_command}" \
    || fail "pin cleanup use-point recovery command is not valid shell"
PATH="${TEST_PATH}" bash -c "${install_symlink_cleanup_pin_rm_command}"
[[ -L "${install_symlink_cleanup_pin_rm_first}" \
    && "$(readlink -- "${install_symlink_cleanup_pin_rm_first}")" == \
        "${install_symlink_cleanup_pin_rm_original}" ]] \
    || fail "pin cleanup use-point recovery restored the wrong symlink"
[[ -L "${install_symlink_cleanup_pin_rm_path}" ]] \
    || fail "pin cleanup recovery touched the foreign pin"

# Finally, rename the random 0700 directory immediately after the exact anchor
# unlink. No public recovery link remains, but the installer must not follow or
# remove a new logical directory name; the displaced empty directory is kept
# as an explicit fail-closed residue.
install_symlink_cleanup_anchor_rm_tools="${TEST_ROOT}/install-symlink-cleanup-anchor-rm-tools"
install_symlink_cleanup_anchor_rm_stage="${TEST_ROOT}/install-symlink-cleanup-anchor-rm-stage"
install_symlink_cleanup_anchor_rm_prefix="/opt/frost-install-symlink-cleanup-anchor-rm"
install_symlink_cleanup_anchor_rm_parent="${install_symlink_cleanup_anchor_rm_stage}${install_symlink_cleanup_anchor_rm_prefix}/share/frost/workflows"
install_symlink_cleanup_anchor_rm_first="${install_symlink_cleanup_anchor_rm_parent}/${WORKFLOW_SOURCES[0]##*/}"
install_symlink_cleanup_anchor_rm_original="${TEST_ROOT}/install-symlink-cleanup-anchor-rm-original"
install_symlink_cleanup_anchor_rm_displaced="${TEST_ROOT}/install-symlink-cleanup-anchor-rm-displaced"
install_symlink_cleanup_anchor_rm_marker="${TEST_ROOT}/install-symlink-cleanup-anchor-rm-marker"
mkdir -p "${install_symlink_cleanup_anchor_rm_tools}" \
    "${install_symlink_cleanup_anchor_rm_parent}"
printf 'anchor rm original sentinel\n' \
    >"${install_symlink_cleanup_anchor_rm_original}"
ln -s -- "${install_symlink_cleanup_anchor_rm_original}" \
    "${install_symlink_cleanup_anchor_rm_first}"
# shellcheck disable=SC2016
printf '%s\n' \
    '#!/bin/sh' \
    'set -eu' \
    'last=' \
    'for argument do last=${argument}; done' \
    'case "${last}" in' \
    '    /proc/self/fd/*/snapshot)' \
    '        /usr/bin/rm "$@"' \
    '        physical=$(/usr/bin/readlink -- "${last%/*}")' \
    '        /usr/bin/mv -- "${physical}" "${FROST_TEST_INSTALL_SYMLINK_CLEANUP_ANCHOR_RM_DISPLACED:?}"' \
    '        : >"${FROST_TEST_INSTALL_SYMLINK_CLEANUP_ANCHOR_RM_MARKER:?}"' \
    '        exit 0' \
    '        ;;' \
    'esac' \
    'exec /usr/bin/rm "$@"' \
    >"${install_symlink_cleanup_anchor_rm_tools}/rm"
chmod 0755 "${install_symlink_cleanup_anchor_rm_tools}/rm"
install_symlink_cleanup_anchor_rm_log="${TEST_ROOT}/install-symlink-cleanup-anchor-rm.log"
env HOME="${TEST_HOME}" \
    PATH="${install_symlink_cleanup_anchor_rm_tools}:${TEST_PATH}" \
    FROST_TEST_INSTALL_SYMLINK_CLEANUP_ANCHOR_RM_DISPLACED="${install_symlink_cleanup_anchor_rm_displaced}" \
    FROST_TEST_INSTALL_SYMLINK_CLEANUP_ANCHOR_RM_MARKER="${install_symlink_cleanup_anchor_rm_marker}" \
    DESTDIR="${install_symlink_cleanup_anchor_rm_stage}" "${INSTALLER}" \
    --binary "${prebuilt_binary}" \
    --prefix "${install_symlink_cleanup_anchor_rm_prefix}" --no-desktop \
    >"${install_symlink_cleanup_anchor_rm_log}" 2>&1
assert_regular_file "private anchor cleanup rename marker" \
    "${install_symlink_cleanup_anchor_rm_marker}"
[[ -d "${install_symlink_cleanup_anchor_rm_displaced}" \
    && -z "$(find "${install_symlink_cleanup_anchor_rm_displaced}" \
        -mindepth 1 -print -quit)" ]] \
    || fail "anchor cleanup did not retain the displaced empty private directory"
assert_mode "displaced private anchor directory" \
    "${install_symlink_cleanup_anchor_rm_displaced}" 700
assert_contains "private anchor directory rename warning" \
    "$(<"${install_symlink_cleanup_anchor_rm_log}")" \
    'empty private rollback directory retained after its identity changed'
if grep -Fq 'recovery command (symlink contents not displayed)' \
    "${install_symlink_cleanup_anchor_rm_log}"; then
    fail "completed anchor unlink advertised a nonexistent recovery name"
fi

# External wrappers may report failure after completing an operation. Exact
# post-action state wins: reservation/cleanup rm must observe absence, ln must
# observe the original inode at the backup name, and publish mv must observe the
# staged inode at the destination with its temporary name gone.
install_post_action_tools="${TEST_ROOT}/install-post-action-tools"
install_post_action_stage="${TEST_ROOT}/install-post-action-stage"
install_post_action_prefix="/opt/frost-install-post-action"
install_post_action_binary="${install_post_action_stage}${install_post_action_prefix}/bin/frost"
install_post_action_rm_state="${TEST_ROOT}/install-post-action-rm-count"
install_post_action_ln_marker="${TEST_ROOT}/install-post-action-ln-called"
install_post_action_mv_marker="${TEST_ROOT}/install-post-action-mv-called"
mkdir -p "${install_post_action_tools}" "${install_post_action_binary%/*}"
printf 'old binary before post-action reconciliation\n' \
    >"${install_post_action_binary}"
# shellcheck disable=SC2016
printf '%s\n' \
    '#!/bin/sh' \
    'set -eu' \
    'last=' \
    'for argument do last=${argument}; done' \
    'case "${last}" in' \
    '    /proc/self/fd/*/.frost.rollback.*)' \
    '        state=${FROST_TEST_INSTALL_POST_ACTION_RM_STATE:?}' \
    '        count=0' \
    '        [ ! -f "${state}" ] || read -r count <"${state}"' \
    '        count=$((count + 1))' \
    '        printf "%s\n" "${count}" >"${state}"' \
    '        /usr/bin/rm "$@"' \
    '        exit 91' \
    '        ;;' \
    'esac' \
    'exec /usr/bin/rm "$@"' \
    >"${install_post_action_tools}/rm"
# shellcheck disable=SC2016
printf '%s\n' \
    '#!/bin/sh' \
    'set -eu' \
    '/usr/bin/ln "$@"' \
    ': >"${FROST_TEST_INSTALL_POST_ACTION_LN_MARKER:?}"' \
    'exit 92' \
    >"${install_post_action_tools}/ln"
# shellcheck disable=SC2016
printf '%s\n' \
    '#!/bin/sh' \
    'set -eu' \
    'last=' \
    'for argument do last=${argument}; done' \
    'case "${last}" in' \
    '    /proc/self/fd/*/frost)' \
    '        /usr/bin/mv "$@"' \
    '        : >"${FROST_TEST_INSTALL_POST_ACTION_MV_MARKER:?}"' \
    '        exit 93' \
    '        ;;' \
    'esac' \
    'exec /usr/bin/mv "$@"' \
    >"${install_post_action_tools}/mv"
chmod 0755 "${install_post_action_tools}/rm" \
    "${install_post_action_tools}/ln" "${install_post_action_tools}/mv"
install_post_action_output="$(
    env HOME="${TEST_HOME}" PATH="${install_post_action_tools}:${TEST_PATH}" \
        FROST_TEST_INSTALL_POST_ACTION_RM_STATE="${install_post_action_rm_state}" \
        FROST_TEST_INSTALL_POST_ACTION_LN_MARKER="${install_post_action_ln_marker}" \
        FROST_TEST_INSTALL_POST_ACTION_MV_MARKER="${install_post_action_mv_marker}" \
        DESTDIR="${install_post_action_stage}" "${INSTALLER}" \
        --binary "${prebuilt_binary}" --prefix "${install_post_action_prefix}" \
        --no-desktop 2>&1
)"
cmp -- "${prebuilt_binary}" "${install_post_action_binary}" \
    || fail "post-action reconciliation did not commit the new binary"
[[ "$(<"${install_post_action_rm_state}")" == 2 ]] \
    || fail "post-action reconciliation did not exercise both backup unlinks"
assert_regular_file "post-action hardlink marker" \
    "${install_post_action_ln_marker}"
assert_regular_file "post-action publish marker" \
    "${install_post_action_mv_marker}"
[[ -z "$(find "${install_post_action_stage}" \
    \( -name '*.install.*' -o -name '*.rollback.*' \) -print -quit)" ]] \
    || fail "post-action reconciliation left a temporary or backup"
assert_contains "post-action install success summary" \
    "${install_post_action_output}" \
    "Installed frost to ${install_post_action_prefix}/bin/frost"
[[ "${install_post_action_output}" != *'cannot atomically replace'* ]] \
    || fail "completed publish was reported as an atomic replacement failure"

# Once the staged inode reaches its destination, a different inode inserted at
# the old source name is not evidence that mv failed. Reconcile the completed
# non-zero publish, revoke both source-name and replaced-backup ownership, then
# prove a later rollback leaves both substitutes untouched.
install_publish_aba_tools="${TEST_ROOT}/install-publish-aba-tools"
install_publish_aba_stage="${TEST_ROOT}/install-publish-aba-stage"
install_publish_aba_prefix="/opt/frost-install-publish-aba"
install_publish_aba_workflow_dir="${install_publish_aba_stage}${install_publish_aba_prefix}/share/frost/workflows"
install_publish_aba_first="${install_publish_aba_workflow_dir}/${WORKFLOW_SOURCES[0]##*/}"
install_publish_aba_second="${install_publish_aba_workflow_dir}/${WORKFLOW_SOURCES[1]##*/}"
install_publish_aba_source_victim="${TEST_ROOT}/install-publish-aba-source-victim"
install_publish_aba_backup_victim="${TEST_ROOT}/install-publish-aba-backup-victim"
install_publish_aba_source_log="${TEST_ROOT}/install-publish-aba-source-path"
install_publish_aba_backup_log="${TEST_ROOT}/install-publish-aba-backup-path"
install_publish_aba_second_marker="${TEST_ROOT}/install-publish-aba-second-mv"
install_publish_aba_state="${TEST_ROOT}/install-publish-aba-state"
mkdir -p "${install_publish_aba_tools}" \
    "${install_publish_aba_workflow_dir}"
printf 'old workflow before publish source ABA\n' \
    >"${install_publish_aba_first}"
printf 'publish source ABA victim sentinel\n' \
    >"${install_publish_aba_source_victim}"
printf 'publish backup ABA victim sentinel\n' \
    >"${install_publish_aba_backup_victim}"
# shellcheck disable=SC2016
printf '%s\n' \
    '#!/bin/sh' \
    'set -eu' \
    'previous=' \
    'last=' \
    'for argument do previous=${last}; last=${argument}; done' \
    'case "${previous}" in' \
    '    /proc/self/fd/*/*.install.*)' \
    '        state=${FROST_TEST_INSTALL_PUBLISH_ABA_STATE:?}' \
    '        count=0' \
    '        [ ! -f "${state}" ] || read -r count <"${state}"' \
    '        count=$((count + 1))' \
    '        printf "%s\n" "${count}" >"${state}"' \
    '        if [ "${count}" -eq 1 ]; then' \
    '            /usr/bin/mv "$@"' \
    '            parent=$(/usr/bin/readlink -- "${last%/*}")' \
    '            source_path=${parent}/${previous##*/}' \
    '            backup_path=$(/usr/bin/find "${parent}" -maxdepth 1 -name ".${FROST_TEST_INSTALL_PUBLISH_ABA_FIRST_BASENAME:?}.rollback.*" -print -quit)' \
    '            : "${backup_path:?missing publish rollback backup}"' \
    '            printf "%s\n" "${source_path}" >"${FROST_TEST_INSTALL_PUBLISH_ABA_SOURCE_LOG:?}"' \
    '            printf "%s\n" "${backup_path}" >"${FROST_TEST_INSTALL_PUBLISH_ABA_BACKUP_LOG:?}"' \
    '            /usr/bin/ln -s -- "${FROST_TEST_INSTALL_PUBLISH_ABA_SOURCE_VICTIM:?}" "${previous}"' \
    '            /usr/bin/rm -f -- "${backup_path}"' \
    '            /usr/bin/ln -s -- "${FROST_TEST_INSTALL_PUBLISH_ABA_BACKUP_VICTIM:?}" "${backup_path}"' \
    '            exit 93' \
    '        fi' \
    '        if [ "${count}" -eq 2 ]; then' \
    '            : >"${FROST_TEST_INSTALL_PUBLISH_ABA_SECOND_MARKER:?}"' \
    '            exit 94' \
    '        fi' \
    '        ;;' \
    'esac' \
    'exec /usr/bin/mv "$@"' \
    >"${install_publish_aba_tools}/mv"
chmod 0755 "${install_publish_aba_tools}/mv"
if env HOME="${TEST_HOME}" PATH="${install_publish_aba_tools}:${TEST_PATH}" \
    FROST_TEST_INSTALL_PUBLISH_ABA_STATE="${install_publish_aba_state}" \
    FROST_TEST_INSTALL_PUBLISH_ABA_FIRST_BASENAME="${install_publish_aba_first##*/}" \
    FROST_TEST_INSTALL_PUBLISH_ABA_SOURCE_LOG="${install_publish_aba_source_log}" \
    FROST_TEST_INSTALL_PUBLISH_ABA_BACKUP_LOG="${install_publish_aba_backup_log}" \
    FROST_TEST_INSTALL_PUBLISH_ABA_SOURCE_VICTIM="${install_publish_aba_source_victim}" \
    FROST_TEST_INSTALL_PUBLISH_ABA_BACKUP_VICTIM="${install_publish_aba_backup_victim}" \
    FROST_TEST_INSTALL_PUBLISH_ABA_SECOND_MARKER="${install_publish_aba_second_marker}" \
    DESTDIR="${install_publish_aba_stage}" "${INSTALLER}" \
    --binary "${prebuilt_binary}" --prefix "${install_publish_aba_prefix}" \
    --no-desktop >"${TEST_ROOT}/install-publish-aba.log" 2>&1; then
    fail "installer ignored a later failure after completed publish ABA"
fi
install_publish_aba_source_path="$(<"${install_publish_aba_source_log}")"
install_publish_aba_backup_path="$(<"${install_publish_aba_backup_log}")"
assert_regular_file "later publish failure marker" \
    "${install_publish_aba_second_marker}"
[[ "$(<"${install_publish_aba_state}")" == 2 ]] \
    || fail "completed publish ABA did not reach the later publish"
cmp -- "${WORKFLOW_SOURCES[0]}" "${install_publish_aba_first}" \
    || fail "completed publish ABA lost the exact staged destination"
assert_absent "second target after publish ABA failure" \
    "${install_publish_aba_second}"
[[ -L "${install_publish_aba_source_path}" ]] \
    || fail "rollback deleted the published-source substitute"
[[ "$(readlink -- "${install_publish_aba_source_path}")" == \
    "${install_publish_aba_source_victim}" ]] \
    || fail "rollback changed the published-source substitute"
[[ -L "${install_publish_aba_backup_path}" ]] \
    || fail "rollback deleted the published-backup substitute"
[[ "$(readlink -- "${install_publish_aba_backup_path}")" == \
    "${install_publish_aba_backup_victim}" ]] \
    || fail "rollback changed the published-backup substitute"
[[ "$(<"${install_publish_aba_source_victim}")" == \
    'publish source ABA victim sentinel' ]] \
    || fail "rollback followed the published-source substitute"
[[ "$(<"${install_publish_aba_backup_victim}")" == \
    'publish backup ABA victim sentinel' ]] \
    || fail "rollback followed the published-backup substitute"
assert_contains "completed publish source ABA warning" \
    "$(<"${TEST_ROOT}/install-publish-aba.log")" \
    "install temporary name changed after publish; replacement retained at ${install_publish_aba_source_path}"
assert_contains "later publish failure diagnostic" \
    "$(<"${TEST_ROOT}/install-publish-aba.log")" \
    "cannot atomically replace ${install_publish_aba_second}"
assert_contains "changed backup rollback diagnostic" \
    "$(<"${TEST_ROOT}/install-publish-aba.log")" \
    "rollback refused changed backup for ${install_publish_aba_first}; unexpected entry retained at ${install_publish_aba_backup_path}"

# Cleanup performs one unlink attempt only. If the owned backup disappears but
# that same name is repopulated before rm returns, the new inode is retained and
# reported; cleanup never retries against the now-unowned name.
install_cleanup_aba_tools="${TEST_ROOT}/install-cleanup-aba-tools"
install_cleanup_aba_stage="${TEST_ROOT}/install-cleanup-aba-stage"
install_cleanup_aba_prefix="/opt/frost-install-cleanup-aba"
install_cleanup_aba_binary="${install_cleanup_aba_stage}${install_cleanup_aba_prefix}/bin/frost"
install_cleanup_aba_victim="${TEST_ROOT}/install-cleanup-aba-victim"
install_cleanup_aba_path_log="${TEST_ROOT}/install-cleanup-aba-path"
install_cleanup_aba_state="${TEST_ROOT}/install-cleanup-aba-rm-count"
mkdir -p "${install_cleanup_aba_tools}" "${install_cleanup_aba_binary%/*}"
printf 'old binary before cleanup ABA\n' >"${install_cleanup_aba_binary}"
printf 'cleanup ABA victim sentinel\n' >"${install_cleanup_aba_victim}"
# shellcheck disable=SC2016
printf '%s\n' \
    '#!/bin/sh' \
    'set -eu' \
    'last=' \
    'for argument do last=${argument}; done' \
    'case "${last}" in' \
    '    /proc/self/fd/*/.frost.rollback.*)' \
    '        state=${FROST_TEST_INSTALL_CLEANUP_ABA_STATE:?}' \
    '        count=0' \
    '        [ ! -f "${state}" ] || read -r count <"${state}"' \
    '        count=$((count + 1))' \
    '        printf "%s\n" "${count}" >"${state}"' \
    '        /usr/bin/rm "$@"' \
    '        if [ "${count}" -eq 2 ]; then' \
    '            parent=$(/usr/bin/readlink -- "${last%/*}")' \
    '            printf "%s/%s\n" "${parent}" "${last##*/}" >"${FROST_TEST_INSTALL_CLEANUP_ABA_PATH_LOG:?}"' \
    '            /usr/bin/ln -s -- "${FROST_TEST_INSTALL_CLEANUP_ABA_VICTIM:?}" "${last}"' \
    '        fi' \
    '        exit 0' \
    '        ;;' \
    'esac' \
    'exec /usr/bin/rm "$@"' \
    >"${install_cleanup_aba_tools}/rm"
chmod 0755 "${install_cleanup_aba_tools}/rm"
install_cleanup_aba_output="$(
    env HOME="${TEST_HOME}" PATH="${install_cleanup_aba_tools}:${TEST_PATH}" \
        FROST_TEST_INSTALL_CLEANUP_ABA_STATE="${install_cleanup_aba_state}" \
        FROST_TEST_INSTALL_CLEANUP_ABA_PATH_LOG="${install_cleanup_aba_path_log}" \
        FROST_TEST_INSTALL_CLEANUP_ABA_VICTIM="${install_cleanup_aba_victim}" \
        DESTDIR="${install_cleanup_aba_stage}" "${INSTALLER}" \
        --binary "${prebuilt_binary}" --prefix "${install_cleanup_aba_prefix}" \
        --no-desktop 2>&1
)"
install_cleanup_aba_path="$(<"${install_cleanup_aba_path_log}")"
cmp -- "${prebuilt_binary}" "${install_cleanup_aba_binary}" \
    || fail "cleanup ABA did not retain the committed binary"
[[ -L "${install_cleanup_aba_path}" ]] \
    || fail "cleanup retried and deleted the replacement backup name"
[[ "$(readlink -- "${install_cleanup_aba_path}")" == \
    "${install_cleanup_aba_victim}" ]] \
    || fail "cleanup changed the replacement backup name"
[[ "$(<"${install_cleanup_aba_victim}")" == \
    'cleanup ABA victim sentinel' ]] \
    || fail "cleanup followed the replacement backup name"
assert_contains "cleanup ABA replacement warning" \
    "${install_cleanup_aba_output}" \
    "rollback backup name changed during removal; replacement retained at ${install_cleanup_aba_path}"
assert_contains "cleanup ABA success summary" \
    "${install_cleanup_aba_output}" \
    "Installed frost to ${install_cleanup_aba_prefix}/bin/frost"

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

# Bind the applications directory before the publish commit. If the legacy-rm
# helper replaces that directory afterwards, unlink stays in the displaced
# inode and cannot remove an identically named launcher behind the new symlink.
install_legacy_bound_tools="${TEST_ROOT}/install-legacy-bound-tools"
install_legacy_bound_stage="${TEST_ROOT}/install-legacy-bound-stage"
install_legacy_bound_prefix="/opt/frost-install-legacy-bound"
install_legacy_bound_app_dir="${install_legacy_bound_stage}${install_legacy_bound_prefix}/share/applications"
install_legacy_bound_entry="${install_legacy_bound_app_dir}/io.github.beamiter.jterm3.desktop"
install_legacy_bound_displaced="${TEST_ROOT}/install-legacy-bound-original-applications"
install_legacy_bound_victim="${TEST_ROOT}/install-legacy-bound-victim"
install_legacy_bound_arg_log="${TEST_ROOT}/install-legacy-bound-rm-argument"
mkdir -p "${install_legacy_bound_tools}" "${install_legacy_bound_app_dir}" \
    "${install_legacy_bound_victim}"
printf 'legacy launcher removed through bound parent\n' \
    >"${install_legacy_bound_entry}"
printf 'outside legacy launcher sentinel\n' \
    >"${install_legacy_bound_victim}/io.github.beamiter.jterm3.desktop"
# shellcheck disable=SC2016
printf '%s\n' \
    '#!/bin/sh' \
    'set -eu' \
    'last=' \
    'for argument do last=${argument}; done' \
    'printf "%s\n" "${last}" >"${FROST_TEST_INSTALL_LEGACY_BOUND_ARG_LOG:?}"' \
    '/usr/bin/mv "${FROST_TEST_INSTALL_LEGACY_BOUND_APP_DIR:?}" "${FROST_TEST_INSTALL_LEGACY_BOUND_DISPLACED:?}"' \
    '/usr/bin/ln -s -- "${FROST_TEST_INSTALL_LEGACY_BOUND_VICTIM:?}" "${FROST_TEST_INSTALL_LEGACY_BOUND_APP_DIR}"' \
    'exec /usr/bin/rm "$@"' \
    >"${install_legacy_bound_tools}/rm"
chmod 0755 "${install_legacy_bound_tools}/rm"
install_legacy_bound_output="$(
    env HOME="${TEST_HOME}" \
        PATH="${install_legacy_bound_tools}:${TEST_PATH}" \
        FROST_TEST_INSTALL_LEGACY_BOUND_ARG_LOG="${install_legacy_bound_arg_log}" \
        FROST_TEST_INSTALL_LEGACY_BOUND_APP_DIR="${install_legacy_bound_app_dir}" \
        FROST_TEST_INSTALL_LEGACY_BOUND_DISPLACED="${install_legacy_bound_displaced}" \
        FROST_TEST_INSTALL_LEGACY_BOUND_VICTIM="${install_legacy_bound_victim}" \
        DESTDIR="${install_legacy_bound_stage}" "${INSTALLER}" \
        --binary "${prebuilt_binary}" --prefix "${install_legacy_bound_prefix}" 2>&1
)"
[[ -L "${install_legacy_bound_app_dir}" ]] \
    || fail "install legacy cleanup did not replace its parent"
assert_absent "legacy launcher in displaced applications directory" \
    "${install_legacy_bound_displaced}/io.github.beamiter.jterm3.desktop"
assert_regular_file "new launcher in displaced applications directory" \
    "${install_legacy_bound_displaced}/${app_id}.desktop"
[[ "$(<"${install_legacy_bound_victim}/io.github.beamiter.jterm3.desktop")" == \
    'outside legacy launcher sentinel' ]] \
    || fail "bound install legacy cleanup touched the replacement referent"
[[ "$(<"${install_legacy_bound_arg_log}")" == \
    /proc/self/fd/*/io.github.beamiter.jterm3.desktop ]] \
    || fail "install legacy cleanup did not receive a directory-fd path"
assert_contains "bound install legacy parent warning" \
    "${install_legacy_bound_output}" \
    "applications directory changed during bound legacy cleanup (non-fatal): ${install_legacy_bound_app_dir}"
assert_before "bound install legacy diagnostic priority" \
    "${install_legacy_bound_output}" \
    "applications directory changed during bound legacy cleanup" \
    "Installed frost to ${install_legacy_bound_prefix}/bin/frost"

# Cache helpers use the same pre-opened applications fd as desktop validation,
# plus one bounded icon fd. Replacing either logical directory inside a helper
# cannot redirect its writes; failures remain optional and precede success.
install_cache_bound_tools="${TEST_ROOT}/install-cache-bound-tools"
install_cache_bound_prefix="${TEST_ROOT}/install-cache-bound-prefix"
install_cache_bound_app_dir="${install_cache_bound_prefix}/share/applications"
install_cache_bound_icon_dir="${install_cache_bound_prefix}/share/icons/hicolor"
install_cache_bound_app_displaced="${TEST_ROOT}/install-cache-bound-original-applications"
install_cache_bound_icon_displaced="${TEST_ROOT}/install-cache-bound-original-hicolor"
install_cache_bound_app_victim="${TEST_ROOT}/install-cache-bound-app-victim"
install_cache_bound_icon_victim="${TEST_ROOT}/install-cache-bound-icon-victim"
install_cache_bound_validate_log="${TEST_ROOT}/install-cache-bound-validate-argument"
install_cache_bound_update_log="${TEST_ROOT}/install-cache-bound-update-argument"
install_cache_bound_icon_log="${TEST_ROOT}/install-cache-bound-icon-argument"
install_cache_bound_prebuilt_leak="${TEST_ROOT}/install-cache-bound-prebuilt-fd-leaked"
install_cache_bound_backup_leak="${TEST_ROOT}/install-cache-bound-backup-fd-leaked"
mkdir -p "${install_cache_bound_tools}" "${install_cache_bound_app_victim}" \
    "${install_cache_bound_icon_victim}" \
    "${install_cache_bound_prefix}/bin"
printf 'old binary whose backup fd must close before helpers\n' \
    >"${install_cache_bound_prefix}/bin/frost"
printf 'outside install applications sentinel\n' \
    >"${install_cache_bound_app_victim}/sentinel"
printf 'outside install icon sentinel\n' \
    >"${install_cache_bound_icon_victim}/sentinel"
# shellcheck disable=SC2016
printf '%s\n' \
    '#!/bin/sh' \
    'set -eu' \
    'printf "%s\n" "${1}" >"${FROST_TEST_INSTALL_CACHE_BOUND_VALIDATE_LOG:?}"' \
    'for candidate in /proc/self/fd/*; do' \
    '    target=$(/usr/bin/readlink -- "${candidate}" 2>/dev/null || :)' \
    '    [ "${target}" != "${FROST_TEST_INSTALL_CACHE_BOUND_PREBUILT:?}" ] || : >"${FROST_TEST_INSTALL_CACHE_BOUND_PREBUILT_LEAK:?}"' \
    '    case "${target}" in *.rollback.*) : >"${FROST_TEST_INSTALL_CACHE_BOUND_BACKUP_LEAK:?}" ;; esac' \
    'done' \
    '[ -f "${1}" ]' \
    >"${install_cache_bound_tools}/desktop-file-validate"
# shellcheck disable=SC2016
printf '%s\n' \
    '#!/bin/sh' \
    'set -eu' \
    'printf "%s\n" "${1}" >"${FROST_TEST_INSTALL_CACHE_BOUND_UPDATE_LOG:?}"' \
    '/usr/bin/mv "${FROST_TEST_INSTALL_CACHE_BOUND_APP_DIR:?}" "${FROST_TEST_INSTALL_CACHE_BOUND_APP_DISPLACED:?}"' \
    '/usr/bin/ln -s -- "${FROST_TEST_INSTALL_CACHE_BOUND_APP_VICTIM:?}" "${FROST_TEST_INSTALL_CACHE_BOUND_APP_DIR}"' \
    ': >"${1}/desktop-cache-bound-marker"' \
    'exit 94' \
    >"${install_cache_bound_tools}/update-desktop-database"
# shellcheck disable=SC2016
printf '%s\n' \
    '#!/bin/sh' \
    'set -eu' \
    'bound=' \
    'for argument do bound=${argument}; done' \
    'printf "%s\n" "${bound}" >"${FROST_TEST_INSTALL_CACHE_BOUND_ICON_LOG:?}"' \
    '/usr/bin/mv "${FROST_TEST_INSTALL_CACHE_BOUND_ICON_DIR:?}" "${FROST_TEST_INSTALL_CACHE_BOUND_ICON_DISPLACED:?}"' \
    '/usr/bin/ln -s -- "${FROST_TEST_INSTALL_CACHE_BOUND_ICON_VICTIM:?}" "${FROST_TEST_INSTALL_CACHE_BOUND_ICON_DIR}"' \
    ': >"${bound}/icon-cache-bound-marker"' \
    'exit 95' \
    >"${install_cache_bound_tools}/gtk-update-icon-cache"
chmod 0755 "${install_cache_bound_tools}/desktop-file-validate" \
    "${install_cache_bound_tools}/update-desktop-database" \
    "${install_cache_bound_tools}/gtk-update-icon-cache"
install_cache_bound_output="$(
    env HOME="${TEST_HOME}" PATH="${install_cache_bound_tools}:${TEST_PATH}" \
        FROST_TEST_INSTALL_CACHE_BOUND_VALIDATE_LOG="${install_cache_bound_validate_log}" \
        FROST_TEST_INSTALL_CACHE_BOUND_UPDATE_LOG="${install_cache_bound_update_log}" \
        FROST_TEST_INSTALL_CACHE_BOUND_ICON_LOG="${install_cache_bound_icon_log}" \
        FROST_TEST_INSTALL_CACHE_BOUND_PREBUILT="${prebuilt_binary}" \
        FROST_TEST_INSTALL_CACHE_BOUND_PREBUILT_LEAK="${install_cache_bound_prebuilt_leak}" \
        FROST_TEST_INSTALL_CACHE_BOUND_BACKUP_LEAK="${install_cache_bound_backup_leak}" \
        FROST_TEST_INSTALL_CACHE_BOUND_APP_DIR="${install_cache_bound_app_dir}" \
        FROST_TEST_INSTALL_CACHE_BOUND_APP_DISPLACED="${install_cache_bound_app_displaced}" \
        FROST_TEST_INSTALL_CACHE_BOUND_APP_VICTIM="${install_cache_bound_app_victim}" \
        FROST_TEST_INSTALL_CACHE_BOUND_ICON_DIR="${install_cache_bound_icon_dir}" \
        FROST_TEST_INSTALL_CACHE_BOUND_ICON_DISPLACED="${install_cache_bound_icon_displaced}" \
        FROST_TEST_INSTALL_CACHE_BOUND_ICON_VICTIM="${install_cache_bound_icon_victim}" \
        DESTDIR= "${INSTALLER}" --binary "${prebuilt_binary}" \
        --prefix "${install_cache_bound_prefix}" 2>&1
)"
install_cache_bound_validate_arg="$(<"${install_cache_bound_validate_log}")"
install_cache_bound_update_arg="$(<"${install_cache_bound_update_log}")"
install_cache_bound_icon_arg="$(<"${install_cache_bound_icon_log}")"
assert_absent "prebuilt fd before post-install helpers" \
    "${install_cache_bound_prebuilt_leak}"
assert_absent "rollback backup fd before post-install helpers" \
    "${install_cache_bound_backup_leak}"
[[ "${install_cache_bound_validate_arg%/*}" == \
    "${install_cache_bound_update_arg}" ]] \
    || fail "install desktop helpers did not reuse one applications fd"
[[ "${install_cache_bound_update_arg}" == /proc/self/fd/* ]] \
    || fail "install desktop cache helper did not receive a directory-fd path"
[[ "${install_cache_bound_icon_arg}" == /proc/self/fd/* ]] \
    || fail "install icon cache helper did not receive a directory-fd path"
[[ -f "${install_cache_bound_app_displaced}/desktop-cache-bound-marker" ]] \
    || fail "install desktop cache helper missed its bound directory"
[[ -f "${install_cache_bound_icon_displaced}/icon-cache-bound-marker" ]] \
    || fail "install icon cache helper missed its bound directory"
assert_absent "outside install desktop cache marker" \
    "${install_cache_bound_app_victim}/desktop-cache-bound-marker"
assert_absent "outside install icon cache marker" \
    "${install_cache_bound_icon_victim}/icon-cache-bound-marker"
[[ "$(<"${install_cache_bound_app_victim}/sentinel")" == \
    'outside install applications sentinel' ]] \
    || fail "install desktop cache helper touched the replacement referent"
[[ "$(<"${install_cache_bound_icon_victim}/sentinel")" == \
    'outside install icon sentinel' ]] \
    || fail "install icon cache helper touched the replacement referent"
assert_contains "install desktop cache failure warning" \
    "${install_cache_bound_output}" \
    "update-desktop-database failed (non-fatal)"
assert_contains "install desktop cache parent warning" \
    "${install_cache_bound_output}" \
    "applications directory changed during bound update-desktop-database (non-fatal): ${install_cache_bound_app_dir}"
assert_contains "install icon cache failure warning" \
    "${install_cache_bound_output}" \
    "gtk-update-icon-cache failed (non-fatal)"
assert_contains "install icon cache parent warning" \
    "${install_cache_bound_output}" \
    "icon directory changed during bound gtk-update-icon-cache (non-fatal): ${install_cache_bound_icon_dir}"
assert_before "install cache diagnostic priority" \
    "${install_cache_bound_output}" \
    "gtk-update-icon-cache failed (non-fatal)" \
    "Installed frost to ${install_cache_bound_prefix}/bin/frost"

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
    '/usr/bin/cat "$@"' \
    'kill -TERM "${PPID}"' \
    >"${interrupt_tools}/cat"
chmod 0755 "${interrupt_tools}/cat"
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

# Keep reservation creation anchored to the directory inode opened before
# mktemp. The wrapper swaps the logical bin directory for an outside symlink;
# the empty reservation must be created and cleaned only in the displaced
# original directory, while both real files remain untouched.
uninstall_reserve_parent_tools="${TEST_ROOT}/uninstall-reserve-parent-tools"
uninstall_reserve_parent_stage="${TEST_ROOT}/uninstall-reserve-parent-stage"
uninstall_reserve_parent_prefix="/opt/frost-uninstall-reserve-parent"
uninstall_reserve_parent_dir="${uninstall_reserve_parent_stage}${uninstall_reserve_parent_prefix}/bin"
uninstall_reserve_parent_binary="${uninstall_reserve_parent_dir}/frost"
uninstall_reserve_parent_displaced="${TEST_ROOT}/uninstall-reserve-parent-original-bin"
uninstall_reserve_parent_victim="${TEST_ROOT}/uninstall-reserve-parent-victim"
uninstall_reserve_parent_marker="${TEST_ROOT}/uninstall-reserve-parent-replaced"
mkdir -p "${uninstall_reserve_parent_tools}" "${uninstall_reserve_parent_dir}" \
    "${uninstall_reserve_parent_victim}"
printf 'original binary before reservation parent replacement\n' \
    >"${uninstall_reserve_parent_binary}"
printf 'outside binary must remain untouched\n' \
    >"${uninstall_reserve_parent_victim}/frost"
uninstall_reserve_parent_identity="$(stat -c '%d:%i:%u:%g:%a' -- \
    "${uninstall_reserve_parent_binary}")"
# shellcheck disable=SC2016
printf '%s\n' \
    '#!/bin/sh' \
    'set -eu' \
    'if [ ! -e "${FROST_TEST_UNINSTALL_RESERVE_PARENT_MARKER:?}" ]; then' \
    '    /usr/bin/mv "${FROST_TEST_UNINSTALL_RESERVE_PARENT_DIR:?}" "${FROST_TEST_UNINSTALL_RESERVE_PARENT_DISPLACED:?}"' \
    '    /usr/bin/ln -s -- "${FROST_TEST_UNINSTALL_RESERVE_PARENT_VICTIM:?}" "${FROST_TEST_UNINSTALL_RESERVE_PARENT_DIR}"' \
    '    : >"${FROST_TEST_UNINSTALL_RESERVE_PARENT_MARKER}"' \
    'fi' \
    'exec /usr/bin/mktemp "$@"' \
    >"${uninstall_reserve_parent_tools}/mktemp"
chmod 0755 "${uninstall_reserve_parent_tools}/mktemp"
if env HOME="${TEST_HOME}" \
    PATH="${uninstall_reserve_parent_tools}:${TEST_PATH}" \
    FROST_TEST_UNINSTALL_RESERVE_PARENT_MARKER="${uninstall_reserve_parent_marker}" \
    FROST_TEST_UNINSTALL_RESERVE_PARENT_DIR="${uninstall_reserve_parent_dir}" \
    FROST_TEST_UNINSTALL_RESERVE_PARENT_DISPLACED="${uninstall_reserve_parent_displaced}" \
    FROST_TEST_UNINSTALL_RESERVE_PARENT_VICTIM="${uninstall_reserve_parent_victim}" \
    DESTDIR="${uninstall_reserve_parent_stage}" "${UNINSTALLER}" \
    --prefix "${uninstall_reserve_parent_prefix}" \
    >"${TEST_ROOT}/uninstall-reserve-parent.log" 2>&1; then
    fail "uninstaller accepted a parent replacement during reservation"
fi
assert_contains "reservation parent diagnostic" \
    "$(<"${TEST_ROOT}/uninstall-reserve-parent.log")" \
    "uninstall target directory changed while reserving quarantine: ${uninstall_reserve_parent_dir}"
[[ -L "${uninstall_reserve_parent_dir}" ]] \
    || fail "reservation parent replacement did not occur"
[[ "$(<"${uninstall_reserve_parent_victim}/frost")" == \
    'outside binary must remain untouched' ]] \
    || fail "reservation followed the replacement parent symlink"
[[ "$(<"${uninstall_reserve_parent_displaced}/frost")" == \
    'original binary before reservation parent replacement' ]] \
    || fail "reservation changed the original binary"
[[ "$(stat -c '%d:%i:%u:%g:%a' -- \
    "${uninstall_reserve_parent_displaced}/frost")" == \
    "${uninstall_reserve_parent_identity}" ]] \
    || fail "reservation parent replacement changed original inode metadata"
[[ -z "$(find "${uninstall_reserve_parent_displaced}" \
    -name '*.uninstall.*' -print -quit)" ]] \
    || fail "reservation parent replacement left a bound placeholder"
[[ -z "$(find "${uninstall_reserve_parent_victim}" \
    -name '*.uninstall.*' -print -quit)" ]] \
    || fail "reservation parent replacement created an outside placeholder"

# Bind the actual quarantine rename to that same directory fd. A wrapper swaps
# the logical parent immediately before delegating mv; post-use validation
# fails and rollback restores the exact original inside the displaced bound
# directory without touching the new symlink referent.
uninstall_stage_parent_tools="${TEST_ROOT}/uninstall-stage-parent-tools"
uninstall_stage_parent_stage="${TEST_ROOT}/uninstall-stage-parent-stage"
uninstall_stage_parent_prefix="/opt/frost-uninstall-stage-parent"
uninstall_stage_parent_dir="${uninstall_stage_parent_stage}${uninstall_stage_parent_prefix}/bin"
uninstall_stage_parent_binary="${uninstall_stage_parent_dir}/frost"
uninstall_stage_parent_displaced="${TEST_ROOT}/uninstall-stage-parent-original-bin"
uninstall_stage_parent_victim="${TEST_ROOT}/uninstall-stage-parent-victim"
uninstall_stage_parent_marker="${TEST_ROOT}/uninstall-stage-parent-replaced"
mkdir -p "${uninstall_stage_parent_tools}" "${uninstall_stage_parent_dir}" \
    "${uninstall_stage_parent_victim}"
printf 'original binary restored in bound parent\n' \
    >"${uninstall_stage_parent_binary}"
printf 'outside stage binary remains untouched\n' \
    >"${uninstall_stage_parent_victim}/frost"
uninstall_stage_parent_identity="$(stat -c '%d:%i:%u:%g:%a' -- \
    "${uninstall_stage_parent_binary}")"
# shellcheck disable=SC2016
printf '%s\n' \
    '#!/bin/sh' \
    'set -eu' \
    'if [ ! -e "${FROST_TEST_UNINSTALL_STAGE_PARENT_MARKER:?}" ]; then' \
    '    /usr/bin/mv "${FROST_TEST_UNINSTALL_STAGE_PARENT_DIR:?}" "${FROST_TEST_UNINSTALL_STAGE_PARENT_DISPLACED:?}"' \
    '    /usr/bin/ln -s -- "${FROST_TEST_UNINSTALL_STAGE_PARENT_VICTIM:?}" "${FROST_TEST_UNINSTALL_STAGE_PARENT_DIR}"' \
    '    : >"${FROST_TEST_UNINSTALL_STAGE_PARENT_MARKER}"' \
    'fi' \
    'exec /usr/bin/mv "$@"' \
    >"${uninstall_stage_parent_tools}/mv"
chmod 0755 "${uninstall_stage_parent_tools}/mv"
if env HOME="${TEST_HOME}" PATH="${uninstall_stage_parent_tools}:${TEST_PATH}" \
    FROST_TEST_UNINSTALL_STAGE_PARENT_MARKER="${uninstall_stage_parent_marker}" \
    FROST_TEST_UNINSTALL_STAGE_PARENT_DIR="${uninstall_stage_parent_dir}" \
    FROST_TEST_UNINSTALL_STAGE_PARENT_DISPLACED="${uninstall_stage_parent_displaced}" \
    FROST_TEST_UNINSTALL_STAGE_PARENT_VICTIM="${uninstall_stage_parent_victim}" \
    DESTDIR="${uninstall_stage_parent_stage}" "${UNINSTALLER}" \
    --prefix "${uninstall_stage_parent_prefix}" \
    >"${TEST_ROOT}/uninstall-stage-parent.log" 2>&1; then
    fail "uninstaller accepted a parent replacement during quarantine rename"
fi
assert_contains "stage parent diagnostic" \
    "$(<"${TEST_ROOT}/uninstall-stage-parent.log")" \
    "staged uninstall path contains a symbolic-link ancestor: ${uninstall_stage_parent_dir}"
[[ -L "${uninstall_stage_parent_dir}" ]] \
    || fail "stage parent replacement did not occur"
[[ "$(<"${uninstall_stage_parent_victim}/frost")" == \
    'outside stage binary remains untouched' ]] \
    || fail "quarantine rename followed the replacement parent symlink"
[[ "$(<"${uninstall_stage_parent_displaced}/frost")" == \
    'original binary restored in bound parent' ]] \
    || fail "bound rollback did not restore the original binary"
[[ "$(stat -c '%d:%i:%u:%g:%a' -- \
    "${uninstall_stage_parent_displaced}/frost")" == \
    "${uninstall_stage_parent_identity}" ]] \
    || fail "bound rollback changed original binary inode metadata"
[[ -z "$(find "${uninstall_stage_parent_displaced}" \
    -name '*.uninstall.*' -print -quit)" ]] \
    || fail "bound parent rollback left a quarantine"
[[ -z "$(find "${uninstall_stage_parent_victim}" \
    -name '*.uninstall.*' -print -quit)" ]] \
    || fail "stage parent replacement wrote an outside quarantine"

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
uninstall_changed_purge_replace_marker="${TEST_ROOT}/uninstall-changed-purge-replaced"
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
    'if [ ! -e "${FROST_TEST_UNINSTALL_CHANGED_PURGE_REPLACE_MARKER:?}" ]; then' \
    '    : >"${FROST_TEST_UNINSTALL_CHANGED_PURGE_REPLACE_MARKER}"' \
    '    found=' \
    '    for candidate in "${FROST_TEST_UNINSTALL_CHANGED_PURGE_DIR:?}"/.frost.uninstall.*; do' \
    '        [ -e "${candidate}" ] || continue' \
    '        found=${candidate}' \
    '        break' \
    '    done' \
    '    [ -n "${found}" ]' \
    '    /usr/bin/mv "${found}" "${FROST_TEST_UNINSTALL_CHANGED_PURGE_DISPLACED:?}"' \
    '    /usr/bin/ln -s -- "${FROST_TEST_UNINSTALL_CHANGED_PURGE_VICTIM:?}" "${found}"' \
    'fi' \
    'exec /usr/bin/readlink "$@"' \
    >"${uninstall_changed_purge_tools}/readlink"
# shellcheck disable=SC2016
printf '%s\n' \
    '#!/bin/sh' \
    'set -eu' \
    ': >"${FROST_TEST_UNINSTALL_CHANGED_PURGE_RM_MARKER:?}"' \
    'exec /usr/bin/rm "$@"' \
    >"${uninstall_changed_purge_tools}/rm"
chmod 0755 "${uninstall_changed_purge_tools}/readlink" \
    "${uninstall_changed_purge_tools}/rm"
uninstall_changed_purge_output="$(
    env HOME="${TEST_HOME}" \
        PATH="${uninstall_changed_purge_tools}:${TEST_PATH}" \
        FROST_TEST_UNINSTALL_CHANGED_PURGE_DIR="${uninstall_changed_purge_binary%/*}" \
        FROST_TEST_UNINSTALL_CHANGED_PURGE_DISPLACED="${uninstall_changed_purge_displaced}" \
        FROST_TEST_UNINSTALL_CHANGED_PURGE_VICTIM="${uninstall_changed_purge_victim}" \
        FROST_TEST_UNINSTALL_CHANGED_PURGE_RM_MARKER="${uninstall_changed_purge_rm_marker}" \
        FROST_TEST_UNINSTALL_CHANGED_PURGE_REPLACE_MARKER="${uninstall_changed_purge_replace_marker}" \
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

# Finally, replace the parent from inside rm itself. The pre-opened fd keeps
# unlink bound to the original directory even after its logical path becomes a
# symlink. Identically named files in the new referent must survive unchanged.
uninstall_purge_parent_tools="${TEST_ROOT}/uninstall-purge-parent-tools"
uninstall_purge_parent_stage="${TEST_ROOT}/uninstall-purge-parent-stage"
uninstall_purge_parent_prefix="/opt/frost-uninstall-purge-parent"
uninstall_purge_parent_dir="${uninstall_purge_parent_stage}${uninstall_purge_parent_prefix}/bin"
uninstall_purge_parent_binary="${uninstall_purge_parent_dir}/frost"
uninstall_purge_parent_displaced="${TEST_ROOT}/uninstall-purge-parent-original-bin"
uninstall_purge_parent_victim="${TEST_ROOT}/uninstall-purge-parent-victim"
uninstall_purge_parent_marker="${TEST_ROOT}/uninstall-purge-parent-replaced"
mkdir -p "${uninstall_purge_parent_tools}" "${uninstall_purge_parent_dir}" \
    "${uninstall_purge_parent_victim}"
printf 'original binary committed before bound purge\n' \
    >"${uninstall_purge_parent_binary}"
printf 'outside purge binary sentinel\n' \
    >"${uninstall_purge_parent_victim}/frost"
printf 'outside quarantine-like sentinel\n' \
    >"${uninstall_purge_parent_victim}/.frost.uninstall.outside"
# shellcheck disable=SC2016
printf '%s\n' \
    '#!/bin/sh' \
    'set -eu' \
    'if [ ! -e "${FROST_TEST_UNINSTALL_PURGE_PARENT_MARKER:?}" ]; then' \
    '    /usr/bin/mv "${FROST_TEST_UNINSTALL_PURGE_PARENT_DIR:?}" "${FROST_TEST_UNINSTALL_PURGE_PARENT_DISPLACED:?}"' \
    '    /usr/bin/ln -s -- "${FROST_TEST_UNINSTALL_PURGE_PARENT_VICTIM:?}" "${FROST_TEST_UNINSTALL_PURGE_PARENT_DIR}"' \
    '    : >"${FROST_TEST_UNINSTALL_PURGE_PARENT_MARKER}"' \
    'fi' \
    'exec /usr/bin/rm "$@"' \
    >"${uninstall_purge_parent_tools}/rm"
chmod 0755 "${uninstall_purge_parent_tools}/rm"
uninstall_purge_parent_output="$(
    env HOME="${TEST_HOME}" PATH="${uninstall_purge_parent_tools}:${TEST_PATH}" \
        FROST_TEST_UNINSTALL_PURGE_PARENT_MARKER="${uninstall_purge_parent_marker}" \
        FROST_TEST_UNINSTALL_PURGE_PARENT_DIR="${uninstall_purge_parent_dir}" \
        FROST_TEST_UNINSTALL_PURGE_PARENT_DISPLACED="${uninstall_purge_parent_displaced}" \
        FROST_TEST_UNINSTALL_PURGE_PARENT_VICTIM="${uninstall_purge_parent_victim}" \
        DESTDIR="${uninstall_purge_parent_stage}" "${UNINSTALLER}" \
        --prefix "${uninstall_purge_parent_prefix}" 2>&1
)"
[[ -L "${uninstall_purge_parent_dir}" ]] \
    || fail "purge parent replacement did not occur"
[[ "$(<"${uninstall_purge_parent_victim}/frost")" == \
    'outside purge binary sentinel' ]] \
    || fail "bound purge removed the outside binary sentinel"
[[ "$(<"${uninstall_purge_parent_victim}/.frost.uninstall.outside")" == \
    'outside quarantine-like sentinel' ]] \
    || fail "bound purge removed an outside quarantine-like sentinel"
[[ -z "$(find "${uninstall_purge_parent_displaced}" \
    -mindepth 1 -print -quit)" ]] \
    || fail "bound purge left the committed target or quarantine behind"
assert_contains "purge parent change warning" "${uninstall_purge_parent_output}" \
    "target directory changed after bound purge of ${uninstall_purge_parent_binary}"
assert_contains "purge parent success summary" "${uninstall_purge_parent_output}" \
    "Removed frost from ${uninstall_purge_parent_prefix}/bin"

# If that same parent replacement is followed by a purge failure, the exact
# original inode must remain recoverable in the displaced bound directory.
# The copy-safe recovery command must name that directory on both sides rather
# than offer the now-symlinked logical destination.
uninstall_purge_fail_parent_tools="${TEST_ROOT}/uninstall-purge-fail-parent-tools"
uninstall_purge_fail_parent_stage="${TEST_ROOT}/uninstall-purge-fail-parent-stage"
uninstall_purge_fail_parent_prefix="/opt/frost-uninstall-purge-fail-parent"
uninstall_purge_fail_parent_dir="${uninstall_purge_fail_parent_stage}${uninstall_purge_fail_parent_prefix}/bin"
uninstall_purge_fail_parent_binary="${uninstall_purge_fail_parent_dir}/frost"
uninstall_purge_fail_parent_displaced="${TEST_ROOT}/uninstall-purge-fail-parent-original-bin"
uninstall_purge_fail_parent_victim="${TEST_ROOT}/uninstall-purge-fail-parent-victim"
uninstall_purge_fail_parent_marker="${TEST_ROOT}/uninstall-purge-fail-parent-replaced"
mkdir -p "${uninstall_purge_fail_parent_tools}" \
    "${uninstall_purge_fail_parent_dir}" "${uninstall_purge_fail_parent_victim}"
printf 'original binary retained after failed bound purge\n' \
    >"${uninstall_purge_fail_parent_binary}"
printf 'outside failed-purge sentinel\n' \
    >"${uninstall_purge_fail_parent_victim}/frost"
uninstall_purge_fail_parent_identity="$(stat -c '%d:%i:%u:%g:%a' -- \
    "${uninstall_purge_fail_parent_binary}")"
# shellcheck disable=SC2016
printf '%s\n' \
    '#!/bin/sh' \
    'set -eu' \
    'if [ ! -e "${FROST_TEST_UNINSTALL_PURGE_FAIL_PARENT_MARKER:?}" ]; then' \
    '    /usr/bin/mv "${FROST_TEST_UNINSTALL_PURGE_FAIL_PARENT_DIR:?}" "${FROST_TEST_UNINSTALL_PURGE_FAIL_PARENT_DISPLACED:?}"' \
    '    /usr/bin/ln -s -- "${FROST_TEST_UNINSTALL_PURGE_FAIL_PARENT_VICTIM:?}" "${FROST_TEST_UNINSTALL_PURGE_FAIL_PARENT_DIR}"' \
    '    : >"${FROST_TEST_UNINSTALL_PURGE_FAIL_PARENT_MARKER}"' \
    'fi' \
    'exit 80' \
    >"${uninstall_purge_fail_parent_tools}/rm"
chmod 0755 "${uninstall_purge_fail_parent_tools}/rm"
uninstall_purge_fail_parent_output="$(
    env HOME="${TEST_HOME}" \
        PATH="${uninstall_purge_fail_parent_tools}:${TEST_PATH}" \
        FROST_TEST_UNINSTALL_PURGE_FAIL_PARENT_MARKER="${uninstall_purge_fail_parent_marker}" \
        FROST_TEST_UNINSTALL_PURGE_FAIL_PARENT_DIR="${uninstall_purge_fail_parent_dir}" \
        FROST_TEST_UNINSTALL_PURGE_FAIL_PARENT_DISPLACED="${uninstall_purge_fail_parent_displaced}" \
        FROST_TEST_UNINSTALL_PURGE_FAIL_PARENT_VICTIM="${uninstall_purge_fail_parent_victim}" \
        DESTDIR="${uninstall_purge_fail_parent_stage}" "${UNINSTALLER}" \
        --prefix "${uninstall_purge_fail_parent_prefix}" 2>&1
)"
[[ -L "${uninstall_purge_fail_parent_dir}" ]] \
    || fail "failed-purge parent replacement did not occur"
[[ "$(<"${uninstall_purge_fail_parent_victim}/frost")" == \
    'outside failed-purge sentinel' ]] \
    || fail "failed bound purge touched the replacement parent referent"
mapfile -t uninstall_purge_fail_parent_quarantines < <(
    find "${uninstall_purge_fail_parent_displaced}" -maxdepth 1 \
        -name '.frost.uninstall.*' -print
)
(( ${#uninstall_purge_fail_parent_quarantines[@]} == 1 )) \
    || fail "failed bound purge did not retain exactly one quarantine"
uninstall_purge_fail_parent_quarantine="${uninstall_purge_fail_parent_quarantines[0]}"
[[ "$(<"${uninstall_purge_fail_parent_quarantine}")" == \
    'original binary retained after failed bound purge' ]] \
    || fail "failed bound purge changed retained quarantine content"
[[ "$(stat -c '%d:%i:%u:%g:%a' -- \
    "${uninstall_purge_fail_parent_quarantine}")" == \
    "${uninstall_purge_fail_parent_identity}" ]] \
    || fail "failed bound purge changed retained quarantine inode metadata"
uninstall_purge_fail_parent_recovery_target="${uninstall_purge_fail_parent_displaced}/frost"
assert_contains "failed bound purge recovery command" \
    "${uninstall_purge_fail_parent_output}" \
    "recovery after inspecting destination: mv -fT -- ${uninstall_purge_fail_parent_quarantine} ${uninstall_purge_fail_parent_recovery_target}"
[[ "${uninstall_purge_fail_parent_output}" != \
    *"recovery after inspecting destination: mv -fT -- ${uninstall_purge_fail_parent_quarantine} ${uninstall_purge_fail_parent_binary}"* ]] \
    || fail "failed bound purge advertised the replacement symlink destination"
assert_contains "failed bound purge success summary" \
    "${uninstall_purge_fail_parent_output}" \
    "Removed frost from ${uninstall_purge_fail_parent_prefix}/bin"

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

# Cleanup is planned before the uninstall commit. Replace its parent from the
# purge wrapper: the committed workflow stays removed, but the post-commit
# rmdir must be skipped rather than traverse the new parent symlink.
uninstall_cleanup_parent_tools="${TEST_ROOT}/uninstall-cleanup-parent-tools"
uninstall_cleanup_parent_stage="${TEST_ROOT}/uninstall-cleanup-parent-stage"
uninstall_cleanup_parent_prefix="/opt/frost-uninstall-cleanup-parent"
uninstall_cleanup_parent_dir="${uninstall_cleanup_parent_stage}${uninstall_cleanup_parent_prefix}/share/frost"
uninstall_cleanup_parent_workflows="${uninstall_cleanup_parent_dir}/workflows"
uninstall_cleanup_parent_file="${uninstall_cleanup_parent_workflows}/git-feature.yaml"
uninstall_cleanup_parent_displaced="${TEST_ROOT}/uninstall-cleanup-parent-original-frost"
uninstall_cleanup_parent_victim="${TEST_ROOT}/uninstall-cleanup-parent-victim"
uninstall_cleanup_parent_rm_marker="${TEST_ROOT}/uninstall-cleanup-parent-replaced"
uninstall_cleanup_parent_rmdir_marker="${TEST_ROOT}/uninstall-cleanup-parent-rmdir-called"
mkdir -p "${uninstall_cleanup_parent_tools}" \
    "${uninstall_cleanup_parent_workflows}" \
    "${uninstall_cleanup_parent_victim}/workflows"
printf 'workflow removed before skipped cleanup\n' \
    >"${uninstall_cleanup_parent_file}"
printf 'outside cleanup sentinel\n' \
    >"${uninstall_cleanup_parent_victim}/workflows/sentinel"
# shellcheck disable=SC2016
printf '%s\n' \
    '#!/bin/sh' \
    'set -eu' \
    'if [ ! -e "${FROST_TEST_UNINSTALL_CLEANUP_PARENT_RM_MARKER:?}" ]; then' \
    '    /usr/bin/mv "${FROST_TEST_UNINSTALL_CLEANUP_PARENT_DIR:?}" "${FROST_TEST_UNINSTALL_CLEANUP_PARENT_DISPLACED:?}"' \
    '    /usr/bin/ln -s -- "${FROST_TEST_UNINSTALL_CLEANUP_PARENT_VICTIM:?}" "${FROST_TEST_UNINSTALL_CLEANUP_PARENT_DIR}"' \
    '    : >"${FROST_TEST_UNINSTALL_CLEANUP_PARENT_RM_MARKER}"' \
    'fi' \
    'exec /usr/bin/rm "$@"' \
    >"${uninstall_cleanup_parent_tools}/rm"
# shellcheck disable=SC2016
printf '%s\n' \
    '#!/bin/sh' \
    'set -eu' \
    ': >"${FROST_TEST_UNINSTALL_CLEANUP_PARENT_RMDIR_MARKER:?}"' \
    'exec /usr/bin/rmdir "$@"' \
    >"${uninstall_cleanup_parent_tools}/rmdir"
chmod 0755 "${uninstall_cleanup_parent_tools}/rm" \
    "${uninstall_cleanup_parent_tools}/rmdir"
uninstall_cleanup_parent_output="$(
    env HOME="${TEST_HOME}" \
        PATH="${uninstall_cleanup_parent_tools}:${TEST_PATH}" \
        FROST_TEST_UNINSTALL_CLEANUP_PARENT_RM_MARKER="${uninstall_cleanup_parent_rm_marker}" \
        FROST_TEST_UNINSTALL_CLEANUP_PARENT_RMDIR_MARKER="${uninstall_cleanup_parent_rmdir_marker}" \
        FROST_TEST_UNINSTALL_CLEANUP_PARENT_DIR="${uninstall_cleanup_parent_dir}" \
        FROST_TEST_UNINSTALL_CLEANUP_PARENT_DISPLACED="${uninstall_cleanup_parent_displaced}" \
        FROST_TEST_UNINSTALL_CLEANUP_PARENT_VICTIM="${uninstall_cleanup_parent_victim}" \
        DESTDIR="${uninstall_cleanup_parent_stage}" "${UNINSTALLER}" \
        --prefix "${uninstall_cleanup_parent_prefix}" 2>&1
)"
[[ -L "${uninstall_cleanup_parent_dir}" ]] \
    || fail "cleanup parent replacement did not occur"
assert_absent "workflow in displaced cleanup parent" \
    "${uninstall_cleanup_parent_displaced}/workflows/git-feature.yaml"
[[ -d "${uninstall_cleanup_parent_displaced}/workflows" ]] \
    || fail "identity-changed cleanup removed the bound workflow directory"
[[ "$(<"${uninstall_cleanup_parent_victim}/workflows/sentinel")" == \
    'outside cleanup sentinel' ]] \
    || fail "post-commit cleanup touched the replacement parent referent"
assert_absent "rmdir call after cleanup parent replacement" \
    "${uninstall_cleanup_parent_rmdir_marker}"
assert_contains "changed cleanup parent warning" \
    "${uninstall_cleanup_parent_output}" \
    "skipped post-commit directory cleanup because identity changed after preflight: ${uninstall_cleanup_parent_workflows} (non-fatal)"
assert_contains "changed cleanup parent success summary" \
    "${uninstall_cleanup_parent_output}" \
    "Removed frost from ${uninstall_cleanup_parent_prefix}/bin"

# A replacement from inside rmdir lands after the identity decision. The
# actual cleanup name is still relative to the pre-opened parent fd, so the
# original empty directory may be removed but the new referent cannot be.
uninstall_cleanup_bound_tools="${TEST_ROOT}/uninstall-cleanup-bound-tools"
uninstall_cleanup_bound_stage="${TEST_ROOT}/uninstall-cleanup-bound-stage"
uninstall_cleanup_bound_prefix="/opt/frost-uninstall-cleanup-bound"
uninstall_cleanup_bound_binary="${uninstall_cleanup_bound_stage}${uninstall_cleanup_bound_prefix}/bin/frost"
uninstall_cleanup_bound_parent="${uninstall_cleanup_bound_stage}${uninstall_cleanup_bound_prefix}/share/frost"
uninstall_cleanup_bound_workflows="${uninstall_cleanup_bound_parent}/workflows"
uninstall_cleanup_bound_displaced="${TEST_ROOT}/uninstall-cleanup-bound-original-frost"
uninstall_cleanup_bound_victim="${TEST_ROOT}/uninstall-cleanup-bound-victim"
mkdir -p "${uninstall_cleanup_bound_tools}" \
    "${uninstall_cleanup_bound_binary%/*}" \
    "${uninstall_cleanup_bound_workflows}" \
    "${uninstall_cleanup_bound_victim}/workflows"
printf 'binary removed before bound directory cleanup\n' \
    >"${uninstall_cleanup_bound_binary}"
printf 'outside bound cleanup sentinel\n' \
    >"${uninstall_cleanup_bound_victim}/workflows/sentinel"
# shellcheck disable=SC2016
printf '%s\n' \
    '#!/bin/sh' \
    'set -eu' \
    '/usr/bin/mv "${FROST_TEST_UNINSTALL_CLEANUP_BOUND_PARENT:?}" "${FROST_TEST_UNINSTALL_CLEANUP_BOUND_DISPLACED:?}"' \
    '/usr/bin/ln -s -- "${FROST_TEST_UNINSTALL_CLEANUP_BOUND_VICTIM:?}" "${FROST_TEST_UNINSTALL_CLEANUP_BOUND_PARENT}"' \
    'exec /usr/bin/rmdir "$@"' \
    >"${uninstall_cleanup_bound_tools}/rmdir"
chmod 0755 "${uninstall_cleanup_bound_tools}/rmdir"
uninstall_cleanup_bound_output="$(
    env HOME="${TEST_HOME}" \
        PATH="${uninstall_cleanup_bound_tools}:${TEST_PATH}" \
        FROST_TEST_UNINSTALL_CLEANUP_BOUND_PARENT="${uninstall_cleanup_bound_parent}" \
        FROST_TEST_UNINSTALL_CLEANUP_BOUND_DISPLACED="${uninstall_cleanup_bound_displaced}" \
        FROST_TEST_UNINSTALL_CLEANUP_BOUND_VICTIM="${uninstall_cleanup_bound_victim}" \
        DESTDIR="${uninstall_cleanup_bound_stage}" "${UNINSTALLER}" \
        --prefix "${uninstall_cleanup_bound_prefix}" 2>&1
)"
[[ -L "${uninstall_cleanup_bound_parent}" ]] \
    || fail "bound cleanup parent replacement did not occur"
assert_absent "empty workflow directory after bound cleanup" \
    "${uninstall_cleanup_bound_displaced}/workflows"
[[ "$(<"${uninstall_cleanup_bound_victim}/workflows/sentinel")" == \
    'outside bound cleanup sentinel' ]] \
    || fail "bound rmdir touched the replacement parent referent"
assert_contains "bound cleanup parent warning" \
    "${uninstall_cleanup_bound_output}" \
    "cleanup parent changed during bound rmdir: ${uninstall_cleanup_bound_parent} (non-fatal)"
assert_before "bound cleanup diagnostic priority" \
    "${uninstall_cleanup_bound_output}" \
    "cleanup parent changed during bound rmdir" \
    "Removed frost from ${uninstall_cleanup_bound_prefix}/bin"

# Cache refresh is likewise planned and bound before removal. A purge-time
# replacement makes the logical applications directory stale, so the optional
# refresh must be skipped without invoking the helper or changing success.
uninstall_cache_skip_tools="${TEST_ROOT}/uninstall-cache-skip-tools"
uninstall_cache_skip_prefix="${TEST_ROOT}/uninstall-cache-skip-prefix"
uninstall_cache_skip_app_dir="${uninstall_cache_skip_prefix}/share/applications"
uninstall_cache_skip_desktop="${uninstall_cache_skip_app_dir}/${app_id}.desktop"
uninstall_cache_skip_displaced="${TEST_ROOT}/uninstall-cache-skip-original-applications"
uninstall_cache_skip_victim="${TEST_ROOT}/uninstall-cache-skip-victim"
uninstall_cache_skip_rm_marker="${TEST_ROOT}/uninstall-cache-skip-replaced"
uninstall_cache_skip_refresh_marker="${TEST_ROOT}/uninstall-cache-skip-refresh-called"
mkdir -p "${uninstall_cache_skip_tools}" "${uninstall_cache_skip_app_dir}" \
    "${uninstall_cache_skip_victim}"
printf 'desktop removed before skipped refresh\n' \
    >"${uninstall_cache_skip_desktop}"
printf 'outside desktop cache sentinel\n' \
    >"${uninstall_cache_skip_victim}/${app_id}.desktop"
# shellcheck disable=SC2016
printf '%s\n' \
    '#!/bin/sh' \
    'set -eu' \
    'if [ ! -e "${FROST_TEST_UNINSTALL_CACHE_SKIP_RM_MARKER:?}" ]; then' \
    '    /usr/bin/mv "${FROST_TEST_UNINSTALL_CACHE_SKIP_APP_DIR:?}" "${FROST_TEST_UNINSTALL_CACHE_SKIP_DISPLACED:?}"' \
    '    /usr/bin/ln -s -- "${FROST_TEST_UNINSTALL_CACHE_SKIP_VICTIM:?}" "${FROST_TEST_UNINSTALL_CACHE_SKIP_APP_DIR}"' \
    '    : >"${FROST_TEST_UNINSTALL_CACHE_SKIP_RM_MARKER}"' \
    'fi' \
    'exec /usr/bin/rm "$@"' \
    >"${uninstall_cache_skip_tools}/rm"
# shellcheck disable=SC2016
printf '%s\n' \
    '#!/bin/sh' \
    'set -eu' \
    ': >"${FROST_TEST_UNINSTALL_CACHE_SKIP_REFRESH_MARKER:?}"' \
    'exit 91' \
    >"${uninstall_cache_skip_tools}/update-desktop-database"
chmod 0755 "${uninstall_cache_skip_tools}/rm" \
    "${uninstall_cache_skip_tools}/update-desktop-database"
uninstall_cache_skip_output="$(
    env HOME="${TEST_HOME}" PATH="${uninstall_cache_skip_tools}:${TEST_PATH}" \
        FROST_TEST_UNINSTALL_CACHE_SKIP_RM_MARKER="${uninstall_cache_skip_rm_marker}" \
        FROST_TEST_UNINSTALL_CACHE_SKIP_REFRESH_MARKER="${uninstall_cache_skip_refresh_marker}" \
        FROST_TEST_UNINSTALL_CACHE_SKIP_APP_DIR="${uninstall_cache_skip_app_dir}" \
        FROST_TEST_UNINSTALL_CACHE_SKIP_DISPLACED="${uninstall_cache_skip_displaced}" \
        FROST_TEST_UNINSTALL_CACHE_SKIP_VICTIM="${uninstall_cache_skip_victim}" \
        DESTDIR= "${UNINSTALLER}" --prefix "${uninstall_cache_skip_prefix}" 2>&1
)"
[[ -L "${uninstall_cache_skip_app_dir}" ]] \
    || fail "cache parent replacement did not occur"
assert_absent "refresh call after cache directory replacement" \
    "${uninstall_cache_skip_refresh_marker}"
[[ "$(<"${uninstall_cache_skip_victim}/${app_id}.desktop")" == \
    'outside desktop cache sentinel' ]] \
    || fail "skipped cache refresh touched the replacement directory"
[[ -z "$(find "${uninstall_cache_skip_displaced}" -mindepth 1 -print -quit)" ]] \
    || fail "cache replacement purge left an unexpected entry"
assert_contains "changed cache directory warning" \
    "${uninstall_cache_skip_output}" \
    "skipped optional desktop database refresh because directory identity changed: ${uninstall_cache_skip_app_dir} (non-fatal)"
assert_contains "changed cache directory success summary" \
    "${uninstall_cache_skip_output}" \
    "Removed frost from ${uninstall_cache_skip_prefix}/bin"

# If a cache helper replaces its directory after the use-point check, its fd
# argument remains anchored to the preflight inode. Exercise both refreshers,
# force helper failures, and prove diagnostics remain non-fatal and precede a
# truthful uninstall success summary.
uninstall_cache_bound_tools="${TEST_ROOT}/uninstall-cache-bound-tools"
uninstall_cache_bound_prefix="${TEST_ROOT}/uninstall-cache-bound-prefix"
uninstall_cache_bound_app_dir="${uninstall_cache_bound_prefix}/share/applications"
uninstall_cache_bound_desktop="${uninstall_cache_bound_app_dir}/${app_id}.desktop"
uninstall_cache_bound_legacy="${uninstall_cache_bound_app_dir}/io.github.beamiter.jterm3.desktop"
uninstall_cache_bound_icon_dir="${uninstall_cache_bound_prefix}/share/icons/hicolor"
uninstall_cache_bound_icon="${uninstall_cache_bound_icon_dir}/scalable/apps/${app_id}.svg"
uninstall_cache_bound_app_displaced="${TEST_ROOT}/uninstall-cache-bound-original-applications"
uninstall_cache_bound_icon_displaced="${TEST_ROOT}/uninstall-cache-bound-original-hicolor"
uninstall_cache_bound_app_victim="${TEST_ROOT}/uninstall-cache-bound-app-victim"
uninstall_cache_bound_icon_victim="${TEST_ROOT}/uninstall-cache-bound-icon-victim"
uninstall_cache_bound_rm_log="${TEST_ROOT}/uninstall-cache-bound-rm-directories"
uninstall_cache_bound_update_log="${TEST_ROOT}/uninstall-cache-bound-update-directory"
mkdir -p "${uninstall_cache_bound_tools}" "${uninstall_cache_bound_app_dir}" \
    "${uninstall_cache_bound_icon%/*}" "${uninstall_cache_bound_app_victim}" \
    "${uninstall_cache_bound_icon_victim}"
printf 'desktop removed before bound cache refresh\n' \
    >"${uninstall_cache_bound_desktop}"
printf 'legacy desktop removed through the shared parent fd\n' \
    >"${uninstall_cache_bound_legacy}"
printf 'icon removed before bound cache refresh\n' \
    >"${uninstall_cache_bound_icon}"
printf 'outside applications sentinel\n' \
    >"${uninstall_cache_bound_app_victim}/sentinel"
printf 'outside icon cache sentinel\n' \
    >"${uninstall_cache_bound_icon_victim}/sentinel"
# shellcheck disable=SC2016
printf '%s\n' \
    '#!/bin/sh' \
    'set -eu' \
    'last=' \
    'for argument do last=${argument}; done' \
    'case "${last}" in' \
    '    /proc/self/fd/*/.io.github.beamiter.frost.desktop.uninstall.*|/proc/self/fd/*/.io.github.beamiter.jterm3.desktop.uninstall.*)' \
    '        printf "%s\n" "${last%/*}" >>"${FROST_TEST_UNINSTALL_CACHE_BOUND_RM_LOG:?}"' \
    '        ;;' \
    'esac' \
    'exec /usr/bin/rm "$@"' \
    >"${uninstall_cache_bound_tools}/rm"
# shellcheck disable=SC2016
printf '%s\n' \
    '#!/bin/sh' \
    'set -eu' \
    'printf "%s\n" "${1}" >"${FROST_TEST_UNINSTALL_CACHE_BOUND_UPDATE_LOG:?}"' \
    '/usr/bin/mv "${FROST_TEST_UNINSTALL_CACHE_BOUND_APP_DIR:?}" "${FROST_TEST_UNINSTALL_CACHE_BOUND_APP_DISPLACED:?}"' \
    '/usr/bin/ln -s -- "${FROST_TEST_UNINSTALL_CACHE_BOUND_APP_VICTIM:?}" "${FROST_TEST_UNINSTALL_CACHE_BOUND_APP_DIR}"' \
    ': >"${1}/desktop-cache-bound-marker"' \
    'exit 92' \
    >"${uninstall_cache_bound_tools}/update-desktop-database"
# shellcheck disable=SC2016
printf '%s\n' \
    '#!/bin/sh' \
    'set -eu' \
    'bound=' \
    'for argument do bound=${argument}; done' \
    '/usr/bin/mv "${FROST_TEST_UNINSTALL_CACHE_BOUND_ICON_DIR:?}" "${FROST_TEST_UNINSTALL_CACHE_BOUND_ICON_DISPLACED:?}"' \
    '/usr/bin/ln -s -- "${FROST_TEST_UNINSTALL_CACHE_BOUND_ICON_VICTIM:?}" "${FROST_TEST_UNINSTALL_CACHE_BOUND_ICON_DIR}"' \
    ': >"${bound}/icon-cache-bound-marker"' \
    'exit 93' \
    >"${uninstall_cache_bound_tools}/gtk-update-icon-cache"
chmod 0755 "${uninstall_cache_bound_tools}/rm" \
    "${uninstall_cache_bound_tools}/update-desktop-database" \
    "${uninstall_cache_bound_tools}/gtk-update-icon-cache"
uninstall_cache_bound_output="$(
    env HOME="${TEST_HOME}" PATH="${uninstall_cache_bound_tools}:${TEST_PATH}" \
        FROST_TEST_UNINSTALL_CACHE_BOUND_APP_DIR="${uninstall_cache_bound_app_dir}" \
        FROST_TEST_UNINSTALL_CACHE_BOUND_APP_DISPLACED="${uninstall_cache_bound_app_displaced}" \
        FROST_TEST_UNINSTALL_CACHE_BOUND_APP_VICTIM="${uninstall_cache_bound_app_victim}" \
        FROST_TEST_UNINSTALL_CACHE_BOUND_RM_LOG="${uninstall_cache_bound_rm_log}" \
        FROST_TEST_UNINSTALL_CACHE_BOUND_UPDATE_LOG="${uninstall_cache_bound_update_log}" \
        FROST_TEST_UNINSTALL_CACHE_BOUND_ICON_DIR="${uninstall_cache_bound_icon_dir}" \
        FROST_TEST_UNINSTALL_CACHE_BOUND_ICON_DISPLACED="${uninstall_cache_bound_icon_displaced}" \
        FROST_TEST_UNINSTALL_CACHE_BOUND_ICON_VICTIM="${uninstall_cache_bound_icon_victim}" \
        DESTDIR= "${UNINSTALLER}" --prefix "${uninstall_cache_bound_prefix}" 2>&1
)"
uninstall_cache_bound_update_arg="$(<"${uninstall_cache_bound_update_log}")"
mapfile -t uninstall_cache_bound_rm_dirs \
    <"${uninstall_cache_bound_rm_log}"
(( ${#uninstall_cache_bound_rm_dirs[@]} == 2 )) \
    || fail "shared applications parent did not purge both launcher names"
for uninstall_cache_bound_rm_dir in "${uninstall_cache_bound_rm_dirs[@]}"; do
    [[ "${uninstall_cache_bound_rm_dir}" == \
        "${uninstall_cache_bound_update_arg}" ]] \
        || fail "uninstall did not reuse one applications fd across removal and cache refresh"
done
[[ -f "${uninstall_cache_bound_app_displaced}/desktop-cache-bound-marker" ]] \
    || fail "desktop cache helper did not receive its bound directory"
[[ -f "${uninstall_cache_bound_icon_displaced}/icon-cache-bound-marker" ]] \
    || fail "icon cache helper did not receive its bound directory"
assert_absent "outside desktop cache marker" \
    "${uninstall_cache_bound_app_victim}/desktop-cache-bound-marker"
assert_absent "outside icon cache marker" \
    "${uninstall_cache_bound_icon_victim}/icon-cache-bound-marker"
[[ "$(<"${uninstall_cache_bound_app_victim}/sentinel")" == \
    'outside applications sentinel' ]] \
    || fail "desktop cache helper changed the replacement directory"
[[ "$(<"${uninstall_cache_bound_icon_victim}/sentinel")" == \
    'outside icon cache sentinel' ]] \
    || fail "icon cache helper changed the replacement directory"
assert_contains "desktop cache failure warning" \
    "${uninstall_cache_bound_output}" \
    "optional desktop database refresh failed for ${uninstall_cache_bound_app_dir} (non-fatal)"
assert_contains "desktop cache parent change warning" \
    "${uninstall_cache_bound_output}" \
    "directory changed during bound desktop database refresh: ${uninstall_cache_bound_app_dir} (non-fatal)"
assert_contains "icon cache failure warning" \
    "${uninstall_cache_bound_output}" \
    "optional icon cache refresh failed for ${uninstall_cache_bound_icon_dir} (non-fatal)"
assert_contains "icon cache parent change warning" \
    "${uninstall_cache_bound_output}" \
    "directory changed during bound icon cache refresh: ${uninstall_cache_bound_icon_dir} (non-fatal)"
assert_contains "bound cache failure success summary" \
    "${uninstall_cache_bound_output}" \
    "Removed frost from ${uninstall_cache_bound_prefix}/bin"
assert_before "cache diagnostic priority" "${uninstall_cache_bound_output}" \
    "optional icon cache refresh failed" \
    "Removed frost from ${uninstall_cache_bound_prefix}/bin"

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
