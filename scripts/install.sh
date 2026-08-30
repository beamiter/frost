#!/usr/bin/env bash
# Install frost and its Linux desktop integration from a source checkout.

set -Eeuo pipefail
umask 077

APP_ID="io.github.beamiter.frost"
SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd -- "${SCRIPT_DIR}/.." && pwd)"
HOME_DIR="${HOME:-}"
DESTDIR="${DESTDIR:-}"
DESTDIR_ACTIVE=0
if [[ -n "${DESTDIR}" ]]; then
    DESTDIR_ACTIVE=1
fi
PREFIX="${HOME_DIR}/.local"
PREFIX_EXPLICIT=0
BIN_DIR=""
PREBUILT_BINARY=""
PREBUILT_FD=""
INSTALL_DESKTOP=1
DRY_RUN=0
INSTALL_TEMPS=()
INSTALL_DESTS=()
INSTALL_BACKUPS=()
INSTALL_ORIGINAL_PRESENT=()
PUBLISH_IN_PROGRESS=0
PUBLISH_LAST_ATTEMPT=-1
KEEP_INSTALL_BACKUPS=0

usage() {
    cat <<'USAGE'
Usage: ./scripts/install.sh [options]

Options:
  --prefix PATH          Runtime prefix (default: ~/.local)
  --bin-dir PATH         Runtime binary directory (default: PREFIX/bin)
  --binary PATH          Install a prebuilt frost binary instead of building
  --no-desktop           Do not install desktop, AppStream, or icon files
  --dry-run              Print commands without changing files
  -h, --help             Show this help

Environment:
  DESTDIR                Optional staging root for packaging
  XDG_DATA_HOME          Workflow data base when --prefix is not specified
  CARGO_TARGET_DIR       Cargo target directory when building (default: <repo>/target)
USAGE
}

die() {
    printf 'frost install: %s\n' "$*" >&2
    exit 1
}

cleanup_install_artifacts() {
    local temp path
    for temp in "${INSTALL_TEMPS[@]}"; do
        if ((DRY_RUN == 0)) && [[ -n "${temp}" ]]; then
            rm -f -- "${temp}" \
                || printf 'frost install: warning: cannot remove temporary %s\n' \
                    "${temp}" >&2
        fi
    done
    if ((KEEP_INSTALL_BACKUPS == 0)); then
        for path in "${INSTALL_BACKUPS[@]}"; do
            if ((DRY_RUN == 0)) && [[ -n "${path}" ]]; then
                rm -f -- "${path}" \
                    || printf 'frost install: warning: cannot remove rollback backup %s\n' \
                        "${path}" >&2
            fi
        done
    fi
    INSTALL_TEMPS=()
    INSTALL_DESTS=()
    INSTALL_BACKUPS=()
    INSTALL_ORIGINAL_PRESENT=()
}

paths_share_inode() {
    local first="$1" second="$2" first_identity second_identity
    [[ ( -e "${first}" || -L "${first}" ) \
        && ( -e "${second}" || -L "${second}" ) ]] || return 1
    first_identity="$(stat -c '%d:%i' -- "${first}")" || return 1
    second_identity="$(stat -c '%d:%i' -- "${second}")" || return 1
    [[ "${first_identity}" == "${second_identity}" ]]
}

rollback_install_plan() {
    local index dest backup rollback_failed=0
    for ((index = PUBLISH_LAST_ATTEMPT; index >= 0; index--)); do
        dest="${INSTALL_DESTS[index]}"
        if (( ${INSTALL_ORIGINAL_PRESENT[index]:-0} == 1 )); then
            backup="${INSTALL_BACKUPS[index]:-}"
            if [[ -n "${backup}" && ( -e "${backup}" || -L "${backup}" ) ]]; then
                if paths_share_inode "${backup}" "${dest}"; then
                    if rm -f -- "${backup}"; then
                        INSTALL_BACKUPS[index]=""
                    else
                        printf 'frost install: rollback restored %s but could not remove backup link %s\n' \
                            "${dest}" "${backup}" >&2
                        rollback_failed=1
                    fi
                elif mv -fT -- "${backup}" "${dest}"; then
                    INSTALL_BACKUPS[index]=""
                else
                    printf 'frost install: rollback failed for %s; backup retained at %s\n' \
                        "${dest}" "${backup}" >&2
                    rollback_failed=1
                fi
            else
                printf 'frost install: rollback backup missing for %s\n' \
                    "${dest}" >&2
                rollback_failed=1
            fi
        elif ! rm -f -- "${dest}"; then
            printf 'frost install: rollback could not remove new target %s\n' \
                "${dest}" >&2
            rollback_failed=1
        fi
    done
    PUBLISH_IN_PROGRESS=0
    PUBLISH_LAST_ATTEMPT=-1
    if ((rollback_failed == 1)); then
        KEEP_INSTALL_BACKUPS=1
        return 1
    fi
}

finish_install() {
    local status=$?
    trap - EXIT
    if ((PUBLISH_IN_PROGRESS == 1)); then
        rollback_install_plan || status=1
    fi
    cleanup_install_artifacts
    exit "${status}"
}

trap finish_install EXIT

print_command() {
    printf '  '
    printf '%q ' "$@"
    printf '\n'
}

run() {
    print_command "$@"
    if ((DRY_RUN == 0)); then
        "$@"
    fi
}

run_optional() {
    print_command "$@"
    if ((DRY_RUN == 0)); then
        "$@" || printf 'frost install: warning: %s failed (non-fatal)\n' "$1" >&2
    fi
}

# Like run_optional, but relaxes this script's restrictive umask: the desktop
# and icon caches are generated files that every user of a shared prefix has to
# be able to read.
run_optional_public() {
    print_command "$@"
    if ((DRY_RUN == 0)); then
        (umask 022 && "$@") \
            || printf 'frost install: warning: %s failed (non-fatal)\n' "$1" >&2
    fi
}

run_in_repo() {
    printf '  (cd %q && ' "${REPO_ROOT}"
    printf '%q ' "$@"
    printf ')\n'
    if ((DRY_RUN == 0)); then
        (cd -- "${REPO_ROOT}" && "$@")
    fi
}

require_command() {
    if command -v "$1" >/dev/null 2>&1; then
        return
    fi
    ((DRY_RUN == 1)) || die "required command not found: $1"
}

# DESTDIR is prepended by string concatenation. An absolute path containing a
# lexical `..` component could otherwise escape that staging root. Preserve
# valid spaces, repeated separators and Unicode names.
validate_absolute_path() {
    local label="$1" path="$2"
    [[ -n "${path}" ]] || die "${label} must not be empty"
    [[ "${path}" == /* ]] || die "${label} must be an absolute path"
    if [[ "${path}" =~ [[:cntrl:]] ]]; then
        die "${label} must not contain control characters"
    fi
    case "/${path#/}/" in
        */../*) die "${label} must not contain '..' path components" ;;
    esac
}

normalize_absolute_path() {
    local path="$1" normalized="" component
    local -a components=()
    IFS='/' read -r -a components <<<"${path}"
    for component in "${components[@]}"; do
        [[ -n "${component}" && "${component}" != . ]] || continue
        normalized="${normalized}/${component}"
    done
    printf '%s' "${normalized:-/}"
}

# Reject every existing symlink component in the normalized DESTDIR chain.
# This point-in-time check deliberately makes no concurrent-mutation promise.
validate_destdir_root() {
    local suffix current="" component
    local -a components=()
    ((DESTDIR_ACTIVE == 1)) || return 0
    [[ -n "${DESTDIR}" && "${DESTDIR}" != / ]] || return 0
    suffix="${DESTDIR#/}"
    IFS='/' read -r -a components <<<"${suffix}"
    for component in "${components[@]}"; do
        [[ -n "${component}" ]] || continue
        current="${current}/${component}"
        [[ ! -L "${current}" ]] \
            || die "DESTDIR path contains a symbolic-link component: ${current}"
        [[ -e "${current}" ]] || break
    done
}

require_source_file() {
    local source="$1"
    [[ ! -L "${source}" && -f "${source}" && -r "${source}" ]] \
        || die "required install source is not a readable regular file: ${source}"
}

# Only the caller-controlled DESTDIR namespace gets this no-symlink policy;
# ordinary system prefixes may legitimately contain compatibility symlinks.
validate_staging_target() {
    local target="$1" suffix current component
    local -a components=()
    ((DESTDIR_ACTIVE == 1)) || return 0
    [[ -n "${DESTDIR}" ]] || return 0
    validate_destdir_root
    suffix="${target#"${DESTDIR}"}"
    suffix="${suffix#/}"
    current="${DESTDIR}"
    IFS='/' read -r -a components <<<"${suffix}"
    for component in "${components[@]}"; do
        [[ -n "${component}" && "${component}" != . ]] || continue
        current="${current}/${component}"
        [[ ! -L "${current}" ]] \
            || die "staged install path contains a symbolic-link ancestor: ${current}"
        [[ -e "${current}" ]] || break
    done
}

stage_install_file() {
    local mode="$1" source="$2" dest="$3" directory basename temp
    printf '  install -m %q %q %q\n' \
        "${mode}" "${source}" "${dest}.<temporary>"
    if ((DRY_RUN == 1)); then
        INSTALL_TEMPS+=("${dest}.<temporary>")
        INSTALL_DESTS+=("${dest}")
        return 0
    fi
    directory="${dest%/*}"
    basename="${dest##*/}"
    install -d -m 0755 "${directory}" \
        || die "cannot create destination directory for ${dest}"
    temp="$(mktemp "${directory}/.${basename}.install.XXXXXX")" \
        || die "cannot create temporary file beside ${dest}"
    INSTALL_TEMPS+=("${temp}")
    INSTALL_DESTS+=("${dest}")
    if ! install -m "${mode}" "${source}" "${temp}"; then
        die "cannot stage ${dest}"
    fi
}

# Keep the binary temporary on its destination filesystem. It is queued after
# every resource so publish_install_plan makes the executable the last commit.
stage_install_binary() {
    local source="$1" dest="$2" directory temp
    printf '  install -m 0755 %q %q\n' "${source}" "${dest}.<temporary>"
    if ((DRY_RUN == 1)); then
        INSTALL_TEMPS+=("${dest}.<temporary>")
        INSTALL_DESTS+=("${dest}")
        return 0
    fi
    directory="${dest%/*}"
    install -d -m 0755 "${directory}" \
        || die "cannot create binary directory for ${dest}"
    temp="$(mktemp "${dest}.install.XXXXXX")" \
        || die "cannot create temporary binary beside ${dest}"
    INSTALL_TEMPS+=("${temp}")
    INSTALL_DESTS+=("${dest}")
    if ! install -m 0755 "${source}" "${temp}"; then
        die "cannot stage binary for ${dest}"
    fi
}

prepare_install_backups() {
    local index dest directory basename backup
    INSTALL_BACKUPS=()
    INSTALL_ORIGINAL_PRESENT=()
    for index in "${!INSTALL_DESTS[@]}"; do
        dest="${INSTALL_DESTS[index]}"
        if [[ -e "${dest}" || -L "${dest}" ]]; then
            [[ -f "${dest}" || -L "${dest}" ]] \
                || die "install destination is not a regular file or symlink: ${dest}"
            directory="${dest%/*}"
            basename="${dest##*/}"
            backup="$(mktemp "${directory}/.${basename}.rollback.XXXXXX")" \
                || die "cannot reserve rollback backup beside ${dest}"
            INSTALL_BACKUPS[index]="${backup}"
            INSTALL_ORIGINAL_PRESENT[index]=1
            rm -f -- "${backup}" \
                || die "cannot prepare rollback backup beside ${dest}"
            # A same-directory hard link retains the exact inode: owner/group,
            # mode, xattrs, and even a dangling symlink's link object. Some
            # filesystems or protected-hardlink policies reject it; the copy
            # fallback still preserves content/mode and never follows links.
            if ! ln -P -- "${dest}" "${backup}" 2>/dev/null \
                && ! cp -a --no-dereference --no-preserve=ownership -- \
                    "${dest}" "${backup}"; then
                die "cannot back up existing install target ${dest}"
            fi
        else
            INSTALL_BACKUPS[index]=""
            INSTALL_ORIGINAL_PRESENT[index]=0
        fi
    done
}

publish_install_plan() {
    local index temp dest
    if ((DRY_RUN == 0)); then
        prepare_install_backups
        PUBLISH_IN_PROGRESS=1
    fi
    for index in "${!INSTALL_TEMPS[@]}"; do
        temp="${INSTALL_TEMPS[index]}"
        dest="${INSTALL_DESTS[index]}"
        print_command mv -fT -- "${temp}" "${dest}"
        PUBLISH_LAST_ATTEMPT="${index}"
        if ((DRY_RUN == 0)) && ! mv -fT -- "${temp}" "${dest}"; then
            die "cannot atomically replace ${dest}"
        fi
        INSTALL_TEMPS[index]=""
    done
    PUBLISH_IN_PROGRESS=0
    PUBLISH_LAST_ATTEMPT=-1
    cleanup_install_artifacts
}

# Bash cannot make the initial no-symlink check and open atomic. Once the open
# succeeds and pathname/descriptor identity verification succeeds, copy via the
# inherited /proc descriptor so a later pathname replacement cannot change the
# inode being copied.
pin_prebuilt_binary() {
    local requested="$1" fd_path fd_identity path_identity
    [[ -d /proc/self/fd && -r /proc/self/fd ]] \
        || die "cannot pin prebuilt binary: /proc/self/fd is unavailable"
    [[ ! -L "${requested}" ]] \
        || die "prebuilt binary must not be a symbolic link: ${requested}"
    [[ -f "${requested}" ]] \
        || die "prebuilt binary is not a regular file: ${requested}"
    [[ -r "${requested}" ]] \
        || die "prebuilt binary is not readable: ${requested}"

    exec {PREBUILT_FD}<"${requested}" \
        || die "cannot open prebuilt binary: ${requested}"
    fd_path="/proc/self/fd/${PREBUILT_FD}"
    [[ -e "${fd_path}" ]] \
        || die "cannot pin prebuilt binary: /proc/self/fd is unavailable"
    [[ -f "${fd_path}" ]] \
        || die "opened prebuilt binary is not a regular file: ${requested}"
    [[ -s "${fd_path}" ]] \
        || die "prebuilt binary must not be empty: ${requested}"
    fd_identity="$(stat -Lc '%d:%i' -- "${fd_path}")" \
        || die "cannot identify opened prebuilt binary (GNU stat required): ${requested}"
    [[ ! -L "${requested}" && -f "${requested}" ]] \
        || die "prebuilt binary changed while being opened: ${requested}"
    path_identity="$(stat -Lc '%d:%i' -- "${requested}")" \
        || die "cannot identify prebuilt binary (GNU stat required): ${requested}"
    [[ ! -L "${requested}" && "${path_identity}" == "${fd_identity}" ]] \
        || die "prebuilt binary changed while being opened: ${requested}"

    BINARY="${fd_path}"
}

bin_dir_on_path() {
    case ":${PATH:-}:" in
        *":${BIN_DIR}:"*) return 0 ;;
        *) return 1 ;;
    esac
}

# A desktop session fixes its PATH at login, so an entry that only says
# `Exec=frost` fails TryExec and is hidden from the launcher whenever the
# binary lives in a per-user bin dir that PATH does not list. Point the entry at
# the real path unless the target is a system bin dir that is always on PATH.
desktop_exec_path() {
    case "${BIN_DIR}" in
        /usr/bin | /usr/local/bin | /bin) printf 'frost' ;;
        *) printf '%s/frost' "${BIN_DIR}" ;;
    esac
}

# Exec is a command line, not a plain path. Always quote an absolute program
# path and apply both layers of escaping required by the Desktop Entry spec:
# generic string decoding happens before Exec quoting/field-code expansion.
desktop_exec_value() {
    local remaining="$1" escaped="" character
    if [[ "${remaining}" == frost ]]; then
        printf 'frost'
        return
    fi
    while [[ -n "${remaining}" ]]; do
        character="${remaining:0:1}"
        remaining="${remaining:1}"
        case "${character}" in
            \\) escaped="${escaped}\\\\\\\\" ;;
            '"') escaped+='\"' ;;
            '`') escaped+='\`' ;;
            '$') escaped+='\\$' ;;
            *) escaped+="${character}" ;;
        esac
    done
    printf '"%s"' "${escaped}"
}

# TryExec is a plain desktop-entry string rather than a command line. Only the
# generic string layer applies, so a literal backslash needs one doubling.
desktop_try_exec_value() {
    local remaining="$1" escaped="" character
    while [[ -n "${remaining}" ]]; do
        character="${remaining:0:1}"
        remaining="${remaining:1}"
        case "${character}" in
            \\) escaped="${escaped}\\\\" ;;
            *) escaped+="${character}" ;;
        esac
    done
    printf '%s' "${escaped}"
}

validate_desktop_exec_path() {
    local path="$1"
    [[ "${path}" != *'='* ]] \
        || die "desktop executable path must not contain '=': ${path}"
    # A literal percent is written as `%%`, but the specification leaves field
    # codes inside a quoted argument undefined. Absolute paths are quoted so
    # spaces and other reserved characters work; accepting `%` here therefore
    # creates entries that validate yet fail to launch in common GLib desktops.
    [[ "${path}" != *'%'* ]] \
        || die "desktop executable path must not contain '%': ${path}"
    if [[ "${path}" =~ [[:cntrl:]] ]]; then
        die "desktop executable path must not contain control characters"
    fi
}

stage_desktop_entry() {
    local source="$1" dest="$2" exec_path exec_value try_exec_value desktop_dir temp
    exec_path="$(desktop_exec_path)"
    validate_desktop_exec_path "${exec_path}"
    exec_value="$(desktop_exec_value "${exec_path}")"
    try_exec_value="$(desktop_try_exec_value "${exec_path}")"
    printf '  install -Dm0644 (Exec=%s) %q %q\n' \
        "${exec_path}" "${source}" "${dest}.<temporary>"
    if ((DRY_RUN == 1)); then
        INSTALL_TEMPS+=("${dest}.<temporary>")
        INSTALL_DESTS+=("${dest}")
        return 0
    fi
    desktop_dir="${dest%/*}"
    install -d -m 0755 "${desktop_dir}" \
        || die "cannot create desktop-entry directory for ${dest}"
    temp="$(mktemp "${desktop_dir}/.${APP_ID}.desktop.install.XXXXXX")" \
        || die "cannot create temporary desktop entry beside ${dest}"
    INSTALL_TEMPS+=("${temp}")
    INSTALL_DESTS+=("${dest}")
    if ! FROST_DESKTOP_EXEC_VALUE="${exec_value}" \
        FROST_DESKTOP_TRY_EXEC_VALUE="${try_exec_value}" \
        awk '
        BEGIN { exec_count = 0; try_exec_count = 0 }
        /^Exec=frost([[:space:]]|$)/ {
            exec_count++
            eq = index($0, "=")
            print substr($0, 1, eq) ENVIRON["FROST_DESKTOP_EXEC_VALUE"] \
                substr($0, eq + 6)
            next
        }
        /^Exec=/ { exit 42 }
        /^TryExec=frost([[:space:]]|$)/ {
            try_exec_count++
            eq = index($0, "=")
            print substr($0, 1, eq) ENVIRON["FROST_DESKTOP_TRY_EXEC_VALUE"] \
                substr($0, eq + 6)
            next
        }
        /^TryExec=/ { exit 43 }
        { print }
        END {
            if (exec_count < 1 || try_exec_count != 1) exit 44
        }
    ' "${source}" >"${temp}" \
        || ! chmod 0644 "${temp}"; then
        die "cannot stage desktop entry for ${dest}"
    fi
}

remove_legacy_desktop_entry() {
    local path="${SHARE_DIR}/applications/io.github.beamiter.jterm3.desktop"
    local validation_error
    print_command rm -f -- "${path}"
    ((DRY_RUN == 0)) || return 0

    # Re-check the staged ancestor at the post-commit use point. The core
    # generation is already installed, so a changed/unsafe path is a warning,
    # never a reason to turn the successful upgrade into an error exit.
    if ((DESTDIR_ACTIVE == 1)) \
        && ! validation_error="$(
            validate_staging_target "${path%/*}" 2>&1
        )"; then
        printf 'frost install: warning: skipped legacy launcher cleanup (non-fatal): %s\n' \
            "${validation_error}" >&2
        return 0
    fi
    if ! rm -f -- "${path}"; then
        printf 'frost install: warning: could not remove legacy launcher (non-fatal): %s\n' \
            "${path}" >&2
    fi
}

# Freshly installed entries and icons stay invisible until the shell's caches
# are rebuilt; a stale icon cache can even shadow icons that are already there.
refresh_desktop_caches() {
    if ((DESTDIR_ACTIVE == 1)); then
        printf 'Staged install (DESTDIR set); skipping desktop cache refresh.\n'
        return 0
    fi
    if command -v desktop-file-validate >/dev/null 2>&1; then
        run_optional desktop-file-validate "${SHARE_DIR}/applications/${APP_ID}.desktop"
    fi
    if command -v update-desktop-database >/dev/null 2>&1; then
        run_optional_public update-desktop-database "${SHARE_DIR}/applications"
    fi
    if command -v gtk-update-icon-cache >/dev/null 2>&1; then
        run_optional_public gtk-update-icon-cache --force --ignore-theme-index --quiet \
            "${SHARE_DIR}/icons/hicolor"
    fi
}

while (($# > 0)); do
    case "$1" in
        --prefix)
            (($# >= 2)) || die "--prefix requires a path"
            PREFIX="$2"
            [[ -n "${PREFIX}" ]] || die "--prefix must not be empty"
            PREFIX_EXPLICIT=1
            shift 2
            ;;
        --prefix=*)
            PREFIX="${1#*=}"
            [[ -n "${PREFIX}" ]] || die "--prefix must not be empty"
            PREFIX_EXPLICIT=1
            shift
            ;;
        --bin-dir)
            (($# >= 2)) || die "--bin-dir requires a path"
            BIN_DIR="$2"
            [[ -n "${BIN_DIR}" ]] || die "--bin-dir must not be empty"
            shift 2
            ;;
        --bin-dir=*)
            BIN_DIR="${1#*=}"
            [[ -n "${BIN_DIR}" ]] || die "--bin-dir must not be empty"
            shift
            ;;
        --binary)
            (($# >= 2)) || die "--binary requires a path"
            PREBUILT_BINARY="$2"
            [[ -n "${PREBUILT_BINARY}" ]] || die "--binary must not be empty"
            shift 2
            ;;
        --binary=*)
            PREBUILT_BINARY="${1#*=}"
            [[ -n "${PREBUILT_BINARY}" ]] || die "--binary must not be empty"
            shift
            ;;
        --no-desktop)
            INSTALL_DESKTOP=0
            shift
            ;;
        --dry-run)
            DRY_RUN=1
            shift
            ;;
        -h|--help)
            usage
            exit 0
            ;;
        --)
            shift
            (($# == 0)) || die "unexpected positional arguments: $*"
            ;;
        *)
            die "unknown option: $1"
            ;;
    esac
done

[[ -n "${HOME_DIR}" ]] || die "HOME is not set"
validate_absolute_path "--prefix" "${PREFIX}"
WORKFLOW_SHARE_DIR="${PREFIX}/share"
if ((PREFIX_EXPLICIT == 0)) && [[ -n "${XDG_DATA_HOME:-}" ]]; then
    validate_absolute_path "XDG_DATA_HOME" "${XDG_DATA_HOME}"
    WORKFLOW_SHARE_DIR="$(normalize_absolute_path "${XDG_DATA_HOME}")"
fi
if [[ -z "${BIN_DIR}" ]]; then
    BIN_DIR="${PREFIX}/bin"
fi
validate_absolute_path "--bin-dir" "${BIN_DIR}"
if ((DESTDIR_ACTIVE == 1)); then
    validate_absolute_path "DESTDIR" "${DESTDIR}"
    DESTDIR="$(normalize_absolute_path "${DESTDIR}")"
    validate_destdir_root
    if [[ "${DESTDIR}" == / ]]; then
        DESTDIR=""
    fi
fi

# Install every format the shared loader accepts. Nullglob makes an empty
# source directory an explicit preflight failure instead of copying a literal
# wildcard; the per-file check also rejects symlinks and special files.
shopt -s nullglob
WORKFLOW_SOURCES=(
    "${REPO_ROOT}/scripts/workflows/"*.toml
    "${REPO_ROOT}/scripts/workflows/"*.yaml
    "${REPO_ROOT}/scripts/workflows/"*.yml
)
shopt -u nullglob
((${#WORKFLOW_SOURCES[@]} > 0)) \
    || die "no bundled workflow sources found under ${REPO_ROOT}/scripts/workflows"
for source in "${WORKFLOW_SOURCES[@]}"; do
    require_source_file "${source}"
done

STAGED_BIN_DIR="${DESTDIR}${BIN_DIR}"
SHARE_DIR="${DESTDIR}${PREFIX}/share"
WORKFLOW_DIR="${DESTDIR}${WORKFLOW_SHARE_DIR}/frost/workflows"
INSTALL_DIRECTORIES=(
    "${STAGED_BIN_DIR}"
    "${WORKFLOW_DIR}"
)
if ((INSTALL_DESKTOP == 1)); then
    require_source_file "${REPO_ROOT}/data/${APP_ID}.desktop"
    require_source_file "${REPO_ROOT}/data/${APP_ID}.metainfo.xml"
    require_source_file "${REPO_ROOT}/data/${APP_ID}.svg"
    require_source_file "${REPO_ROOT}/data/${APP_ID}-128.png"
    require_source_file "${REPO_ROOT}/data/${APP_ID}-256.png"
    INSTALL_DIRECTORIES+=(
        "${SHARE_DIR}/applications"
        "${SHARE_DIR}/metainfo"
        "${SHARE_DIR}/icons/hicolor/scalable/apps"
        "${SHARE_DIR}/icons/hicolor/128x128/apps"
        "${SHARE_DIR}/icons/hicolor/256x256/apps"
    )
fi

# Validate every destination branch before replacing the binary. Checking only
# PREFIX/share misses a later applications/metainfo/icon ancestor symlink and
# can otherwise leave a partially upgraded package or write outside DESTDIR.
for directory in "${INSTALL_DIRECTORIES[@]}"; do
    validate_staging_target "${directory}"
done

require_command install
require_command mktemp
require_command mv
require_command rm
require_command cp
require_command ln
require_command stat
if ((INSTALL_DESKTOP == 1)); then
    require_command awk
    require_command chmod
    validate_desktop_exec_path "$(desktop_exec_path)"
fi

if [[ -n "${PREBUILT_BINARY}" ]]; then
    BINARY="${PREBUILT_BINARY}"
    printf 'Using prebuilt frost binary: '
    printf '%q\n' "${BINARY}"
    if ((DRY_RUN == 0)); then
        pin_prebuilt_binary "${PREBUILT_BINARY}"
    fi
else
    TARGET_DIR="${CARGO_TARGET_DIR:-${REPO_ROOT}/target}"
    if [[ "${TARGET_DIR}" != /* ]]; then
        TARGET_DIR="${REPO_ROOT}/${TARGET_DIR}"
    fi
    export CARGO_TARGET_DIR="${TARGET_DIR}"

    printf 'Building frost...\n'
    require_command cargo
    run_in_repo cargo build --release --locked

    BINARY="${TARGET_DIR}/release/frost"
    if ((DRY_RUN == 0)) && [[ ! -x "${BINARY}" ]]; then
        die "release binary was not produced at ${BINARY}"
    fi
fi

# Stage the complete plan without changing any existing destination. Resource
# temps are published first and the executable is the final rename, so a copy
# or desktop transformation failure leaves the installed generation intact.
for source in "${WORKFLOW_SOURCES[@]}"; do
    stage_install_file 0644 "${source}" "${WORKFLOW_DIR}/${source##*/}"
done
if ((INSTALL_DESKTOP == 1)); then
    stage_desktop_entry "${REPO_ROOT}/data/${APP_ID}.desktop" \
        "${SHARE_DIR}/applications/${APP_ID}.desktop"
    stage_install_file 0644 "${REPO_ROOT}/data/${APP_ID}.metainfo.xml" \
        "${SHARE_DIR}/metainfo/${APP_ID}.metainfo.xml"
    stage_install_file 0644 "${REPO_ROOT}/data/${APP_ID}.svg" \
        "${SHARE_DIR}/icons/hicolor/scalable/apps/${APP_ID}.svg"
    for size in 128 256; do
        stage_install_file 0644 "${REPO_ROOT}/data/${APP_ID}-${size}.png" \
            "${SHARE_DIR}/icons/hicolor/${size}x${size}/apps/${APP_ID}.png"
    done
fi
stage_install_binary "${BINARY}" "${STAGED_BIN_DIR}/frost"
if [[ -n "${PREBUILT_FD}" ]]; then
    exec {PREBUILT_FD}<&-
fi
publish_install_plan

if ((INSTALL_DESKTOP == 1)); then
    # Launcher left by installs from before the jterm3 -> frost rename; left in
    # place it shows up as a second "jterm3" entry beside the new one.
    remove_legacy_desktop_entry
    refresh_desktop_caches
fi

printf 'Installed frost to %s\n' "${BIN_DIR}/frost"
printf 'Installed workflow examples under %s/frost/workflows\n' "${WORKFLOW_SHARE_DIR}"
if ((INSTALL_DESKTOP == 1)); then
    printf 'Installed desktop integration under %s/share\n' "${PREFIX}"
    printf 'Launcher entry: %s (Exec=%s)\n' \
        "${SHARE_DIR}/applications/${APP_ID}.desktop" "$(desktop_exec_path)"
fi
if ((DESTDIR_ACTIVE == 1)); then
    printf 'Staged file: %s\n' "${STAGED_BIN_DIR}/frost"
fi
if ((DESTDIR_ACTIVE == 0)); then
    if ! bin_dir_on_path; then
        printf '\nNote: '
        printf '%q' "${BIN_DIR}"
        printf ' is not in PATH; the launcher entry uses the absolute path,\n'
        printf 'but shells will not find frost. Add this line to ~/.profile:\n'
        printf '  export PATH='
        printf '%q' "${BIN_DIR}"
        # Keep PATH expansion in the generated profile line, not this process.
        # shellcheck disable=SC2016
        printf ':"$PATH"\n'
    fi
    SHADOWING_BIN="$(command -v frost 2>/dev/null || true)"
    if [[ -n "${SHADOWING_BIN}" && "${SHADOWING_BIN}" != "${BIN_DIR}/frost" ]]; then
        # The backticks are literal command-name markup in user-facing prose.
        # shellcheck disable=SC2016
        printf '\nNote: typing `frost` still runs '
        printf '%q' "${SHADOWING_BIN}"
        printf ', an older copy earlier in PATH.\nRemove it, or put '
        printf '%q' "${BIN_DIR}"
        printf ' ahead of it in PATH.\nThe launcher entry is unaffected: it runs '
        printf '%q' "${BIN_DIR}/frost"
        printf ' directly.\n'
    fi
fi
