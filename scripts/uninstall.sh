#!/usr/bin/env bash
# Remove frost and its Linux desktop integration.

set -Eeuo pipefail
umask 077

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
REMOVAL_QUARANTINES=()
REMOVAL_RESERVATION_IDENTITIES=()
REMOVAL_ORIGINAL_PRESENT=()
REMOVAL_ORIGINAL_IDENTITIES=()
REMOVAL_STAGED=()
UNINSTALL_IN_PROGRESS=0
UNINSTALL_COMMITTED=0

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
  XDG_CONFIG_HOME        Absolute configuration base (relative values ignored)
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

require_command() {
    command -v "$1" >/dev/null 2>&1 \
        || die "required command not found: $1"
}

path_matches_identity() {
    local path="$1" expected="$2" actual
    [[ -e "${path}" || -L "${path}" ]] || return 1
    actual="$(stat -c '%d:%i' -- "${path}")" || return 1
    [[ "${actual}" == "${expected}" ]]
}

print_uninstall_recovery() {
    local quarantine="$1" target="$2"
    printf 'frost uninstall: recovery after inspecting destination: mv -fT -- %q %q\n' \
        "${quarantine}" "${target}" >&2
}

cleanup_uninstall_reservations() {
    local index quarantine reservation_identity
    for index in "${!REMOVAL_QUARANTINES[@]}"; do
        quarantine="${REMOVAL_QUARANTINES[index]:-}"
        [[ -n "${quarantine}" ]] || continue
        # A staged entry still contains the removed original. Never discard it
        # after a failed rollback; only empty, unused reservations are cleanup.
        if (( ${REMOVAL_STAGED[index]:-0} == 0 )) \
            && [[ -e "${quarantine}" || -L "${quarantine}" ]]; then
            reservation_identity="${REMOVAL_RESERVATION_IDENTITIES[index]:-}"
            if [[ -z "${reservation_identity}" ]] \
                || ! path_matches_identity "${quarantine}" \
                    "${reservation_identity}"; then
                printf 'frost uninstall: warning: refusing to remove changed unused quarantine %s\n' \
                    "${quarantine}" >&2
            elif rm -f -- "${quarantine}"; then
                REMOVAL_QUARANTINES[index]=""
            else
                printf 'frost uninstall: warning: cannot remove unused quarantine %s\n' \
                    "${quarantine}" >&2
            fi
        fi
    done
}

reconcile_uninstall_attempt() {
    local index="$1" target quarantine identity
    target="${REMOVAL_FILES[index]}"
    quarantine="${REMOVAL_QUARANTINES[index]:-}"
    identity="${REMOVAL_ORIGINAL_IDENTITIES[index]:-}"
    # State 2 means a signal may have interrupted the tiny interval between mv
    # and its bookkeeping. Identity determines which name still owns the exact
    # preflight inode without ever treating the empty reservation as a backup.
    if path_matches_identity "${target}" "${identity}"; then
        REMOVAL_STAGED[index]=0
        return 0
    fi
    if [[ -n "${quarantine}" ]] \
        && path_matches_identity "${quarantine}" "${identity}"; then
        REMOVAL_STAGED[index]=1
        return 0
    fi
    REMOVAL_STAGED[index]=0
    printf 'frost uninstall: cannot reconcile interrupted quarantine rename for %s\n' \
        "${target}" >&2
    return 1
}

rollback_uninstall_plan() {
    local index target quarantine identity validation_error state rollback_failed=0
    for ((index = ${#REMOVAL_FILES[@]} - 1; index >= 0; index--)); do
        state="${REMOVAL_STAGED[index]:-0}"
        if ((state == 2)); then
            if ! reconcile_uninstall_attempt "${index}"; then
                rollback_failed=1
                continue
            fi
        fi
        (( ${REMOVAL_STAGED[index]:-0} == 1 )) || continue
        target="${REMOVAL_FILES[index]}"
        quarantine="${REMOVAL_QUARANTINES[index]:-}"
        identity="${REMOVAL_ORIGINAL_IDENTITIES[index]:-}"
        if [[ -z "${quarantine}" \
            || ( ! -e "${quarantine}" && ! -L "${quarantine}" ) ]]; then
            printf 'frost uninstall: rollback quarantine missing for %s\n' \
                "${target}" >&2
            rollback_failed=1
            continue
        fi
        if ! path_matches_identity "${quarantine}" "${identity}"; then
            printf 'frost uninstall: rollback refused changed quarantine for %s; unexpected entry retained at %s\n' \
                "${target}" "${quarantine}" >&2
            rollback_failed=1
            continue
        fi
        if [[ -e "${target}" || -L "${target}" ]]; then
            printf 'frost uninstall: rollback refused to overwrite reappeared target %s; quarantine retained at %s\n' \
                "${target}" "${quarantine}" >&2
            print_uninstall_recovery "${quarantine}" "${target}"
            rollback_failed=1
            continue
        fi
        if ! validation_error="$(
            validate_staging_removal_target "${target}" 2>&1
        )"; then
            printf 'frost uninstall: rollback cannot safely restore %s: %s; quarantine retained at %s\n' \
                "${target}" "${validation_error}" "${quarantine}" >&2
            print_uninstall_recovery "${quarantine}" "${target}"
            rollback_failed=1
            continue
        fi
        if mv -fT -- "${quarantine}" "${target}"; then
            REMOVAL_QUARANTINES[index]=""
            REMOVAL_STAGED[index]=0
        elif path_matches_identity "${target}" "${identity}"; then
            # Treat a wrapper's post-rename failure by observed state, not its
            # exit status. Never claim a quarantine is retained when restore
            # already put the exact original inode back at its target.
            REMOVAL_STAGED[index]=0
            if [[ ! -e "${quarantine}" && ! -L "${quarantine}" ]]; then
                REMOVAL_QUARANTINES[index]=""
            else
                printf 'frost uninstall: warning: rollback restored %s but a changed quarantine entry remains at %s\n' \
                    "${target}" "${quarantine}" >&2
            fi
        else
            printf 'frost uninstall: rollback failed for %s; quarantine retained at %s\n' \
                "${target}" "${quarantine}" >&2
            print_uninstall_recovery "${quarantine}" "${target}"
            rollback_failed=1
        fi
    done
    UNINSTALL_IN_PROGRESS=0
    ((rollback_failed == 0))
}

finish_uninstall() {
    local status=$?
    trap - EXIT
    if ((UNINSTALL_IN_PROGRESS == 1 && UNINSTALL_COMMITTED == 0)); then
        rollback_uninstall_plan || status=1
    fi
    cleanup_uninstall_reservations
    exit "${status}"
}

prepare_uninstall_plan() {
    local index target directory basename quarantine identity reservation_identity
    REMOVAL_QUARANTINES=()
    REMOVAL_RESERVATION_IDENTITIES=()
    REMOVAL_ORIGINAL_PRESENT=()
    REMOVAL_ORIGINAL_IDENTITIES=()
    REMOVAL_STAGED=()
    for index in "${!REMOVAL_FILES[@]}"; do
        target="${REMOVAL_FILES[index]}"
        REMOVAL_STAGED[index]=0
        if [[ -e "${target}" || -L "${target}" ]]; then
            validate_removal_file_target "${target}"
            identity="$(stat -c '%d:%i' -- "${target}")" \
                || die "cannot identify uninstall target ${target}"
            directory="${target%/*}"
            basename="${target##*/}"
            quarantine="$(mktemp "${directory}/.${basename}.uninstall.XXXXXX")" \
                || die "cannot reserve uninstall quarantine beside ${target}"
            REMOVAL_QUARANTINES[index]="${quarantine}"
            reservation_identity="$(stat -c '%d:%i' -- "${quarantine}")" \
                || die "cannot identify uninstall quarantine ${quarantine}"
            REMOVAL_RESERVATION_IDENTITIES[index]="${reservation_identity}"
            REMOVAL_ORIGINAL_PRESENT[index]=1
            REMOVAL_ORIGINAL_IDENTITIES[index]="${identity}"
        else
            REMOVAL_QUARANTINES[index]=""
            REMOVAL_RESERVATION_IDENTITIES[index]=""
            REMOVAL_ORIGINAL_PRESENT[index]=0
            REMOVAL_ORIGINAL_IDENTITIES[index]=""
        fi
    done
}

stage_uninstall_plan() {
    local index target quarantine identity
    for target in "${REMOVAL_DIRS[@]}"; do
        validate_removal_dir_target "${target}"
    done
    UNINSTALL_IN_PROGRESS=1
    for index in "${!REMOVAL_FILES[@]}"; do
        target="${REMOVAL_FILES[index]}"
        validate_removal_file_target "${target}"
        if (( ${REMOVAL_ORIGINAL_PRESENT[index]:-0} == 0 )); then
            [[ ! -e "${target}" && ! -L "${target}" ]] \
                || die "uninstall target appeared after preflight: ${target}"
            continue
        fi
        identity="${REMOVAL_ORIGINAL_IDENTITIES[index]}"
        path_matches_identity "${target}" "${identity}" \
            || die "uninstall target changed after preflight: ${target}"
        quarantine="${REMOVAL_QUARANTINES[index]}"
        path_matches_identity "${quarantine}" \
            "${REMOVAL_RESERVATION_IDENTITIES[index]}" \
            || die "uninstall quarantine reservation changed after preflight: ${quarantine}"
        print_command mv -fT -- "${target}" "${quarantine}"
        # Mark the in-flight syscall before entering mv. EXIT reconciles state
        # 2 by inode if a catchable signal lands after rename but before the
        # success assignment below.
        REMOVAL_STAGED[index]=2
        if ! mv -fT -- "${target}" "${quarantine}"; then
            die "cannot quarantine uninstall target ${target}"
        fi
        REMOVAL_STAGED[index]=1
    done
    UNINSTALL_COMMITTED=1
    UNINSTALL_IN_PROGRESS=0
}

purge_uninstall_quarantines() {
    local index target quarantine identity
    for index in "${!REMOVAL_QUARANTINES[@]}"; do
        (( ${REMOVAL_STAGED[index]:-0} == 1 )) || continue
        target="${REMOVAL_FILES[index]}"
        quarantine="${REMOVAL_QUARANTINES[index]:-}"
        identity="${REMOVAL_ORIGINAL_IDENTITIES[index]:-}"
        if [[ -z "${quarantine}" \
            || ( ! -e "${quarantine}" && ! -L "${quarantine}" ) ]]; then
            REMOVAL_QUARANTINES[index]=""
            REMOVAL_STAGED[index]=0
            continue
        fi
        if ! path_matches_identity "${quarantine}" "${identity}"; then
            printf 'frost uninstall: warning: refusing to purge changed quarantine for %s; unexpected entry retained at %s\n' \
                "${target}" "${quarantine}" >&2
            continue
        fi
        print_command rm -f -- "${quarantine}"
        if rm -f -- "${quarantine}"; then
            REMOVAL_QUARANTINES[index]=""
            REMOVAL_STAGED[index]=0
        elif [[ ! -e "${quarantine}" && ! -L "${quarantine}" ]]; then
            # The requested unlink completed even though a wrapper reported
            # failure. The committed target remains absent and no recovery
            # inode exists, so do not print a false retained-path instruction.
            REMOVAL_QUARANTINES[index]=""
            REMOVAL_STAGED[index]=0
            printf 'frost uninstall: warning: purge reported failure after removing %s\n' \
                "${target}" >&2
        elif ! path_matches_identity "${quarantine}" "${identity}"; then
            printf 'frost uninstall: warning: purge failed and quarantine identity changed for %s; unexpected entry retained at %s\n' \
                "${target}" "${quarantine}" >&2
        else
            printf 'frost uninstall: warning: cannot purge removed target %s; quarantine retained at %s\n' \
                "${target}" "${quarantine}" >&2
            print_uninstall_recovery "${quarantine}" "${target}"
        fi
    done
}

trap finish_uninstall EXIT
trap 'exit 129' HUP
trap 'exit 130' INT
trap 'exit 143' TERM

remove_file() {
    local path="$1" quarantine
    validate_removal_file_target "${path}"
    if [[ -e "${path}" || -L "${path}" ]]; then
        if ((DRY_RUN == 1)); then
            quarantine="${path%/*}/.${path##*/}.uninstall.<temporary>"
            print_command mv -fT -- "${path}" "${quarantine}"
            print_command rm -f -- "${quarantine}"
        else
            run rm -f -- "${path}"
        fi
    fi
}

remove_dir_if_empty() {
    local path="$1" validation_error
    if ! validation_error="$(
        validate_removal_dir_target "${path}" 2>&1
    )"; then
        if ((UNINSTALL_COMMITTED == 1)); then
            printf 'frost uninstall: warning: skipped post-commit directory cleanup: %s\n' \
                "${validation_error}" >&2
            return 0
        fi
        printf '%s\n' "${validation_error}" >&2
        return 1
    fi
    if [[ -d "${path}" ]]; then
        print_command rmdir --ignore-fail-on-non-empty -- "${path}"
        if ((DRY_RUN == 0)) \
            && ! rmdir --ignore-fail-on-non-empty -- "${path}"; then
            printf 'frost uninstall: warning: could not remove empty cleanup directory %s (non-fatal)\n' \
                "${path}" >&2
        fi
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

validate_removal_file_target() {
    local path="$1"
    validate_staging_removal_target "${path}"
    if [[ -e "${path}" || -L "${path}" ]]; then
        [[ -f "${path}" || -L "${path}" ]] \
            || die "uninstall target is not a regular file or symlink: ${path}"
    fi
}

validate_removal_dir_target() {
    local path="$1"
    validate_staging_removal_target "${path}"
    if [[ -e "${path}" || -L "${path}" ]]; then
        [[ -d "${path}" && ! -L "${path}" ]] \
            || die "uninstall cleanup target is not a directory: ${path}"
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

# Validate the complete caller-owned plan before the first removal. Cleanup
# paths must remain real directories and are checked first; exact final file
# symlinks are deliberately allowed and unlinked without following them, while
# directories and special files at file destinations fail closed. Per-target
# checks remain as a best-effort guard against replacement after preflight.
for target in "${REMOVAL_DIRS[@]}"; do
    validate_removal_dir_target "${target}"
done
for target in "${REMOVAL_FILES[@]}"; do
    validate_removal_file_target "${target}"
done
if ((DRY_RUN == 1)); then
    for target in "${REMOVAL_FILES[@]}"; do
        remove_file "${target}"
    done
else
    require_command mktemp
    require_command mv
    require_command rm
    require_command rmdir
    require_command stat
    prepare_uninstall_plan
    stage_uninstall_plan
    # Every owned name is now absent: this is the uninstall commit point.
    # Purge cannot truthfully roll that result back, so failures retain a named
    # quarantine and an exact manual recovery command while the exit stays 0.
    purge_uninstall_quarantines
fi
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

printf 'Removed frost from %s\n' "${BIN_DIR}"

# `dirs::config_dir`, used by frost itself, accepts XDG_CONFIG_HOME only when
# it is absolute and otherwise falls back to HOME/.config. Keep the handoff
# message on that exact contract. The value is environment-controlled and may
# contain terminal control bytes, so render it with Bash's reversible quoting.
CONFIG_BASE="${HOME_DIR}/.config"
if [[ -n "${XDG_CONFIG_HOME:-}" && "${XDG_CONFIG_HOME}" == /* ]]; then
    CONFIG_BASE="${XDG_CONFIG_HOME}"
fi
CONFIG_DIR="${CONFIG_BASE}/frost"
printf 'Preserved configuration and history under '
printf '%q\n' "${CONFIG_DIR}"
