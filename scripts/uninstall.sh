#!/usr/bin/env bash
# Remove frost and its Linux desktop integration.

set -Eeuo pipefail

APP_ID="io.github.beamiter.frost"
HOME_DIR="${HOME:-}"
DESTDIR="${DESTDIR:-}"
DESTDIR_ACTIVE=0
if [[ -n "${DESTDIR}" ]]; then
    DESTDIR_ACTIVE=1
fi
PREFIX="${HOME_DIR}/.local"
PREFIX_EXPLICIT=0
BIN_DIR=""
DRY_RUN=0

usage() {
    cat <<'USAGE'
Usage: ./scripts/uninstall.sh [options]

Options:
  --prefix PATH          Runtime prefix (default: ~/.local)
  --bin-dir PATH         Runtime binary directory (default: PREFIX/bin)
  --dry-run              Print commands without changing files
  -h, --help             Show this help

Environment:
  DESTDIR                Optional staging root for packaging
  XDG_DATA_HOME          Workflow data base when --prefix is not specified
USAGE
}

die() {
    printf 'frost uninstall: %s\n' "$*" >&2
    exit 1
}

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

remove_file() {
    local path="$1"
    validate_staging_removal_target "${path}"
    if [[ -e "${path}" || -L "${path}" ]]; then
        run rm -f -- "${path}"
    fi
}

remove_dir_if_empty() {
    local path="$1"
    validate_staging_removal_target "${path}"
    if [[ -d "${path}" ]]; then
        run rmdir --ignore-fail-on-non-empty -- "${path}"
    fi
}

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

# Full-chain point-in-time validation for the normalized packaging root.
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

# Non-root DESTDIR is a package boundary.  Permit removal of a final symlink,
# but reject any symlink in the directory chain that would redirect rm outside
# that boundary.
validate_staging_removal_target() {
    local target="$1" parent suffix current component
    local -a components=()
    ((DESTDIR_ACTIVE == 1)) || return 0
    [[ -n "${DESTDIR}" ]] || return 0
    validate_destdir_root
    case "${target}" in
        "${DESTDIR}"/*) ;;
        *) die "staged uninstall target is outside DESTDIR: ${target}" ;;
    esac
    parent="${target%/*}"
    suffix="${parent#"${DESTDIR}"}"
    suffix="${suffix#/}"
    current="${DESTDIR}"
    IFS='/' read -r -a components <<<"${suffix}"
    for component in "${components[@]}"; do
        [[ -n "${component}" && "${component}" != . ]] || continue
        current="${current}/${component}"
        [[ ! -L "${current}" ]] \
            || die "staged uninstall path contains a symbolic-link ancestor: ${current}"
        [[ -e "${current}" ]] || break
    done
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

SHARE_DIR="${DESTDIR}${PREFIX}/share"
OWNED_WORKFLOWS=(
    docker-tail-logs.yaml
    find-large-files.yaml
    git-feature.yaml
    git-rebase-interactive.yaml
    kill-port.yaml
    ssh-tunnel.yaml
)
REMOVAL_FILES=(
    "${DESTDIR}${BIN_DIR}/frost"
    "${SHARE_DIR}/applications/${APP_ID}.desktop"
    "${SHARE_DIR}/metainfo/${APP_ID}.metainfo.xml"
    "${SHARE_DIR}/icons/hicolor/scalable/apps/${APP_ID}.svg"
    "${SHARE_DIR}/icons/hicolor/128x128/apps/${APP_ID}.png"
    "${SHARE_DIR}/icons/hicolor/256x256/apps/${APP_ID}.png"
)
for workflow in "${OWNED_WORKFLOWS[@]}"; do
    REMOVAL_FILES+=(
        "${DESTDIR}${WORKFLOW_SHARE_DIR}/frost/workflows/${workflow}"
    )
done
# Desktop integration from before the jterm3 -> frost rename.
REMOVAL_FILES+=(
    "${SHARE_DIR}/applications/io.github.beamiter.jterm3.desktop"
    "${SHARE_DIR}/metainfo/io.github.beamiter.jterm3.metainfo.xml"
    "${SHARE_DIR}/icons/hicolor/scalable/apps/io.github.beamiter.jterm3.svg"
    "${SHARE_DIR}/icons/hicolor/128x128/apps/io.github.beamiter.jterm3.png"
    "${SHARE_DIR}/icons/hicolor/256x256/apps/io.github.beamiter.jterm3.png"
)
REMOVAL_DIRS=("${DESTDIR}${WORKFLOW_SHARE_DIR}/frost/workflows")

# Validate the complete caller-owned staging plan before the first removal.
# Per-target checks remain in remove_file/remove_dir_if_empty as a best-effort
# guard against a path being replaced between this preflight and its use.
for target in "${REMOVAL_FILES[@]}" "${REMOVAL_DIRS[@]}"; do
    validate_staging_removal_target "${target}"
done
for target in "${REMOVAL_FILES[@]}"; do
    remove_file "${target}"
done
for target in "${REMOVAL_DIRS[@]}"; do
    remove_dir_if_empty "${target}"
done

# Without this the launcher keeps offering a dead entry and a cached icon.
if ((DESTDIR_ACTIVE == 0 && DRY_RUN == 0)); then
    if command -v update-desktop-database >/dev/null 2>&1 \
        && [[ -d "${SHARE_DIR}/applications" ]]; then
        (umask 022 && update-desktop-database "${SHARE_DIR}/applications") \
            >/dev/null 2>&1 || true
    fi
    if command -v gtk-update-icon-cache >/dev/null 2>&1 \
        && [[ -d "${SHARE_DIR}/icons/hicolor" ]]; then
        (umask 022 && gtk-update-icon-cache --force --ignore-theme-index --quiet \
            "${SHARE_DIR}/icons/hicolor") >/dev/null 2>&1 || true
    fi
fi

CONFIG_DIR="${XDG_CONFIG_HOME:-${HOME_DIR}/.config}/frost"
printf 'Removed frost from %s\n' "${BIN_DIR}"
printf 'Preserved configuration and history under %s\n' "${CONFIG_DIR}"
