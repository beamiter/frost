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
INSTALL_BACKUP_BASENAMES=()
INSTALL_BACKUP_IDENTITIES=()
INSTALL_BACKUP_FDS=()
INSTALL_BACKUP_PINS=()
INSTALL_BACKUP_PIN_BASENAMES=()
INSTALL_BACKUP_PIN_IDENTITIES=()
INSTALL_BACKUP_ANCHORS=()
INSTALL_BACKUP_ANCHOR_BASENAMES=()
INSTALL_BACKUP_ANCHOR_IDENTITIES=()
INSTALL_BACKUP_ANCHOR_DIRS=()
INSTALL_BACKUP_ANCHOR_DIR_BASENAMES=()
INSTALL_BACKUP_ANCHOR_DIR_FDS=()
INSTALL_BACKUP_ANCHOR_DIR_IDENTITIES=()
INSTALL_BACKUP_ANCHOR_DIR_METADATA=()
INSTALL_BACKUP_SYMLINK_VALUES=()
INSTALL_BACKUP_SYMLINK_METADATA=()
INSTALL_ORIGINAL_PRESENT=()
INSTALL_ORIGINAL_IDENTITIES=()
INSTALL_PARENT_FDS=()
INSTALL_PARENT_IDENTITIES=()
INSTALL_DEST_BASENAMES=()
INSTALL_TEMP_BASENAMES=()
INSTALL_STAGED_IDENTITIES=()
PUBLISH_IN_PROGRESS=0
PUBLISH_LAST_ATTEMPT=-1
KEEP_INSTALL_BACKUPS=0
INSTALL_BOUND_DIRECTORY_FDS=()
INSTALL_BOUND_DIRECTORY_IDENTITIES=()
MAX_INSTALL_BOUND_DIRECTORY_FDS=16
MAX_INSTALL_BACKUP_FDS=16
MAX_INSTALL_BACKUP_ANCHOR_DIR_FDS=16
POST_INSTALL_APP_DIR=""
POST_INSTALL_APP_FD=""
POST_INSTALL_APP_IDENTITY=""
POST_INSTALL_APP_BIND_ERROR=""
POST_INSTALL_ICON_DIR=""
POST_INSTALL_ICON_FD=""
POST_INSTALL_ICON_IDENTITY=""
POST_INSTALL_ICON_BIND_ERROR=""

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
    local index temp path expected display fd
    local -a parent_warning_emitted=()
    for index in "${!INSTALL_TEMPS[@]}"; do
        temp="${INSTALL_TEMPS[index]:-}"
        if ((DRY_RUN == 0)) && [[ -n "${temp}" ]] \
            && [[ -e "${temp}" || -L "${temp}" ]]; then
            expected="${INSTALL_STAGED_IDENTITIES[index]:-}"
            display="${temp}"
            if [[ -n "${INSTALL_PARENT_FDS[index]:-}" \
                && -n "${INSTALL_TEMP_BASENAMES[index]:-}" ]]; then
                display="$(bound_install_entry_display "${index}" \
                    "${INSTALL_TEMP_BASENAMES[index]}")"
            fi
            if [[ -z "${expected}" ]] \
                || ! path_matches_identity "${temp}" "${expected}"; then
                printf 'frost install: warning: refusing to remove changed temporary %s\n' \
                    "${display}" >&2
            else
                # The exact post-action name state is authoritative: a
                # non-zero wrapper may have completed the unlink already.
                rm -f -- "${temp}" || :
                if [[ -e "${temp}" || -L "${temp}" ]]; then
                    if path_matches_identity "${temp}" "${expected}"; then
                        printf 'frost install: warning: cannot remove temporary %s\n' \
                            "${display}" >&2
                    else
                        printf 'frost install: warning: temporary name changed during removal; replacement retained at %s\n' \
                            "${display}" >&2
                    fi
                fi
            fi
        fi
        if [[ -n "${INSTALL_PARENT_FDS[index]:-}" ]] \
            && (( ${parent_warning_emitted[index]:-0} == 0 )) \
            && ! logical_install_parent_matches "${index}"; then
            printf 'frost install: warning: destination directory changed during bound artifact cleanup (non-fatal): %s\n' \
                "${INSTALL_DESTS[index]%/*}" >&2
            parent_warning_emitted[index]=1
        fi
    done
    if ((KEEP_INSTALL_BACKUPS == 0)); then
        # Symlink snapshots retain a third link inside a private, descriptor-
        # bound directory. It remains live while the public backup and pin are
        # removed, so neither public-name replacement can recycle the owned
        # inode number. Every unlink is nevertheless a fresh use point.
        for index in "${!INSTALL_BACKUP_ANCHORS[@]}"; do
            [[ -n "${INSTALL_BACKUP_ANCHOR_DIRS[index]:-}" ]] || continue
            cleanup_symlink_install_snapshot "${index}"
        done
        for index in "${!INSTALL_BACKUP_PINS[@]}"; do
            [[ -n "${INSTALL_BACKUP_PINS[index]:-}" \
                && -z "${INSTALL_BACKUP_ANCHOR_DIRS[index]:-}" ]] \
                || continue
            warn_retained_symlink_install_snapshot "${index}" \
                'private anchor was not established'
        done
        for index in "${!INSTALL_BACKUPS[@]}"; do
            [[ -z "${INSTALL_BACKUP_ANCHOR_DIRS[index]:-}" \
                && -z "${INSTALL_BACKUP_PINS[index]:-}" ]] || continue
            path="${INSTALL_BACKUPS[index]:-}"
            if ((DRY_RUN == 0)) && [[ -n "${path}" ]]; then
                display="$(bound_install_backup_display "${index}")"
                if [[ -e "${path}" || -L "${path}" ]]; then
                    expected="${INSTALL_BACKUP_IDENTITIES[index]:-}"
                    if ! logical_install_parent_matches "${index}"; then
                        printf 'frost install: warning: skipped rollback backup cleanup because destination directory identity changed (non-fatal): %q\n' \
                            "${INSTALL_DESTS[index]%/*}" >&2
                        parent_warning_emitted[index]=1
                        continue
                    elif [[ -z "${expected}" ]] \
                        || ! install_backup_path_matches_identity "${index}" \
                            "${path}" "${expected}"; then
                        printf 'frost install: warning: refusing to remove changed rollback backup %q\n' \
                            "${display}" >&2
                    else
                        rm -f -- "${path}" || :
                        if [[ -e "${path}" || -L "${path}" ]]; then
                            if install_backup_path_matches_identity "${index}" \
                                "${path}" "${expected}"; then
                                printf 'frost install: warning: cannot remove rollback backup %q\n' \
                                    "${display}" >&2
                            else
                                printf 'frost install: warning: rollback backup name changed during removal; replacement retained at %q\n' \
                                    "${display}" >&2
                            fi
                        else
                            INSTALL_BACKUPS[index]=""
                        fi
                    fi
                fi
            fi
            if [[ -n "${INSTALL_PARENT_FDS[index]:-}" ]] \
                && (( ${parent_warning_emitted[index]:-0} == 0 )) \
                && ! logical_install_parent_matches "${index}"; then
                printf 'frost install: warning: destination directory changed during bound artifact cleanup (non-fatal): %s\n' \
                    "${INSTALL_DESTS[index]%/*}" >&2
                parent_warning_emitted[index]=1
            fi
        done
    fi
    for index in "${!INSTALL_BACKUP_FDS[@]}"; do
        fd="${INSTALL_BACKUP_FDS[index]:-}"
        [[ -n "${fd}" ]] || continue
        exec {fd}<&-
    done
    for index in "${!INSTALL_BACKUP_ANCHOR_DIR_FDS[@]}"; do
        fd="${INSTALL_BACKUP_ANCHOR_DIR_FDS[index]:-}"
        [[ -n "${fd}" ]] || continue
        exec {fd}<&-
    done
    INSTALL_TEMPS=()
    INSTALL_DESTS=()
    INSTALL_BACKUPS=()
    INSTALL_BACKUP_BASENAMES=()
    INSTALL_BACKUP_IDENTITIES=()
    INSTALL_BACKUP_FDS=()
    INSTALL_BACKUP_PINS=()
    INSTALL_BACKUP_PIN_BASENAMES=()
    INSTALL_BACKUP_PIN_IDENTITIES=()
    INSTALL_BACKUP_ANCHORS=()
    INSTALL_BACKUP_ANCHOR_BASENAMES=()
    INSTALL_BACKUP_ANCHOR_IDENTITIES=()
    INSTALL_BACKUP_ANCHOR_DIRS=()
    INSTALL_BACKUP_ANCHOR_DIR_BASENAMES=()
    INSTALL_BACKUP_ANCHOR_DIR_FDS=()
    INSTALL_BACKUP_ANCHOR_DIR_IDENTITIES=()
    INSTALL_BACKUP_ANCHOR_DIR_METADATA=()
    INSTALL_BACKUP_SYMLINK_VALUES=()
    INSTALL_BACKUP_SYMLINK_METADATA=()
    INSTALL_ORIGINAL_PRESENT=()
    INSTALL_ORIGINAL_IDENTITIES=()
    INSTALL_PARENT_FDS=()
    INSTALL_PARENT_IDENTITIES=()
    INSTALL_DEST_BASENAMES=()
    INSTALL_TEMP_BASENAMES=()
    INSTALL_STAGED_IDENTITIES=()
}

path_matches_identity() {
    local path="$1" expected="$2" actual
    [[ -e "${path}" || -L "${path}" ]] || return 1
    actual="$(stat -c '%d:%i' -- "${path}")" || return 1
    [[ "${actual}" == "${expected}" ]]
}

staged_install_artifact_matches() {
    local path="$1" expected="$2" link_count
    [[ -f "${path}" && ! -L "${path}" ]] \
        && path_matches_identity "${path}" "${expected}" \
        || return 1
    link_count="$(stat -c '%h' -- "${path}")" || return 1
    [[ "${link_count}" == 1 ]]
}

opened_staged_install_artifact_matches() {
    local fd_path="$1" expected="$2" actual link_count
    [[ -f "${fd_path}" ]] || return 1
    actual="$(stat -Lc '%d:%i' -- "${fd_path}")" || return 1
    [[ "${actual}" == "${expected}" ]] || return 1
    link_count="$(stat -Lc '%h' -- "${fd_path}")" || return 1
    [[ "${link_count}" == 1 ]]
}

real_directory_matches_identity() {
    local path="$1" expected="$2"
    [[ -d "${path}" && ! -L "${path}" ]] \
        && path_matches_identity "${path}" "${expected}"
}

directory_referent_matches_identity() {
    local path="$1" expected="$2" actual
    [[ -d "${path}" ]] || return 1
    actual="$(stat -Lc '%d:%i' -- "${path}")" || return 1
    [[ "${actual}" == "${expected}" ]]
}

acquire_install_bound_directory() {
    local path="$1" result_fd_name="$2" result_identity_name="$3"
    local candidate_fd directory_identity index existing_fd
    unset candidate_fd
    if ! { exec {candidate_fd}<"${path}"; } 2>/dev/null; then
        return 1
    fi
    if ! directory_identity="$(stat -Lc '%d:%i' -- \
        "/proc/self/fd/${candidate_fd}")"; then
        exec {candidate_fd}<&-
        return 1
    fi
    for index in "${!INSTALL_BOUND_DIRECTORY_IDENTITIES[@]}"; do
        if [[ "${INSTALL_BOUND_DIRECTORY_IDENTITIES[index]}" == \
            "${directory_identity}" ]]; then
            existing_fd="${INSTALL_BOUND_DIRECTORY_FDS[index]}"
            exec {candidate_fd}<&-
            printf -v "${result_fd_name}" '%s' "${existing_fd}"
            printf -v "${result_identity_name}" '%s' "${directory_identity}"
            return 0
        fi
    done
    if ((${#INSTALL_BOUND_DIRECTORY_FDS[@]} \
        >= MAX_INSTALL_BOUND_DIRECTORY_FDS)); then
        exec {candidate_fd}<&-
        return 2
    fi
    INSTALL_BOUND_DIRECTORY_FDS+=("${candidate_fd}")
    INSTALL_BOUND_DIRECTORY_IDENTITIES+=("${directory_identity}")
    printf -v "${result_fd_name}" '%s' "${candidate_fd}"
    printf -v "${result_identity_name}" '%s' "${directory_identity}"
}

close_install_bound_directory_fds() {
    local index fd
    for index in "${!INSTALL_BOUND_DIRECTORY_FDS[@]}"; do
        fd="${INSTALL_BOUND_DIRECTORY_FDS[index]:-}"
        [[ -n "${fd}" ]] || continue
        exec {fd}<&-
    done
    INSTALL_BOUND_DIRECTORY_FDS=()
    INSTALL_BOUND_DIRECTORY_IDENTITIES=()
}

bound_install_entry_path() {
    local index="$1" basename="$2"
    printf '/proc/self/fd/%s/%s' "${INSTALL_PARENT_FDS[index]}" "${basename}"
}

bound_install_dest_path() {
    local index="$1"
    bound_install_entry_path "${index}" "${INSTALL_DEST_BASENAMES[index]}"
}

bound_install_temp_path() {
    local index="$1"
    bound_install_entry_path "${index}" "${INSTALL_TEMP_BASENAMES[index]}"
}

bound_install_backup_path() {
    local index="$1"
    bound_install_entry_path "${index}" "${INSTALL_BACKUP_BASENAMES[index]}"
}

bound_install_entry_display() {
    local index="$1" basename="$2" parent
    parent="$(readlink -- "/proc/$$/fd/${INSTALL_PARENT_FDS[index]}" \
        2>/dev/null)" || parent="${INSTALL_DESTS[index]%/*}"
    printf '%s/%s' "${parent}" "${basename}"
}

bound_install_backup_display() {
    local index="$1"
    bound_install_entry_display "${index}" "${INSTALL_BACKUP_BASENAMES[index]}"
}

bound_install_backup_pin_display() {
    local index="$1"
    bound_install_entry_display "${index}" \
        "${INSTALL_BACKUP_PIN_BASENAMES[index]}"
}

bound_install_backup_anchor_dir_path() {
    local index="$1"
    bound_install_entry_path "${index}" \
        "${INSTALL_BACKUP_ANCHOR_DIR_BASENAMES[index]}"
}

bound_install_backup_anchor_display() {
    local index="$1" directory
    directory="$(readlink -- \
        "/proc/$$/fd/${INSTALL_BACKUP_ANCHOR_DIR_FDS[index]}" \
        2>/dev/null)" || return 1
    printf '%s/%s' "${directory}" \
        "${INSTALL_BACKUP_ANCHOR_BASENAMES[index]}"
}

read_symlink_text_exact() {
    local path="$1" value
    value="$({ readlink -n -- "${path}" || exit; printf .; })" || return 1
    printf '%s' "${value%.}"
}

symlink_install_snapshot_path_matches() {
    local index="$1" path="$2" expected="$3" value metadata
    [[ -L "${path}" ]] \
        && path_matches_identity "${path}" "${expected}" || return 1
    value="$(read_symlink_text_exact "${path}")" || return 1
    metadata="$(stat -c '%u:%g:%a' -- "${path}")" || return 1
    [[ -v 'INSTALL_BACKUP_SYMLINK_VALUES[index]' \
        && "${value}" == "${INSTALL_BACKUP_SYMLINK_VALUES[index]}" \
        && "${metadata}" == "${INSTALL_BACKUP_SYMLINK_METADATA[index]:-}" ]]
}

private_install_anchor_directory_matches() {
    local index="$1" directory fd expected metadata
    local fd_identity fd_metadata
    directory="${INSTALL_BACKUP_ANCHOR_DIRS[index]:-}"
    fd="${INSTALL_BACKUP_ANCHOR_DIR_FDS[index]:-}"
    expected="${INSTALL_BACKUP_ANCHOR_DIR_IDENTITIES[index]:-}"
    metadata="${INSTALL_BACKUP_ANCHOR_DIR_METADATA[index]:-}"
    [[ -n "${directory}" && -n "${fd}" && -n "${expected}" \
        && -n "${metadata}" && -d "${directory}" \
        && ! -L "${directory}" ]] || return 1
    path_matches_identity "${directory}" "${expected}" || return 1
    fd_identity="$(stat -Lc '%d:%i' -- "/proc/self/fd/${fd}")" \
        || return 1
    fd_metadata="$(stat -Lc '%u:%g:%a' -- "/proc/self/fd/${fd}")" \
        || return 1
    [[ "${fd_identity}" == "${expected}" \
        && "${fd_metadata}" == "${metadata}" \
        && "$(stat -c '%u:%g:%a' -- "${directory}")" == "${metadata}" ]]
}

symlink_install_snapshot_names_match() {
    local index="$1" expected backup pin anchor
    expected="${INSTALL_BACKUP_ANCHOR_IDENTITIES[index]:-}"
    backup="${INSTALL_BACKUPS[index]:-}"
    pin="${INSTALL_BACKUP_PINS[index]:-}"
    anchor="${INSTALL_BACKUP_ANCHORS[index]:-}"
    [[ -n "${expected}" && -n "${anchor}" ]] \
        && symlink_install_snapshot_path_matches "${index}" "${anchor}" \
            "${expected}" \
        || return 1
    if [[ -n "${backup}" ]]; then
        symlink_install_snapshot_path_matches "${index}" "${backup}" \
            "${expected}" \
            || return 1
    fi
    if [[ -n "${pin}" ]]; then
        symlink_install_snapshot_path_matches "${index}" "${pin}" \
            "${expected}" \
            || return 1
    fi
    private_install_anchor_directory_matches "${index}"
}

print_exact_symlink_install_recovery_command() {
    local index="$1" expected source_basename source_display dest_display
    local backup pin anchor
    expected="${INSTALL_BACKUP_ANCHOR_IDENTITIES[index]:-}"
    [[ -n "${expected}" ]] \
        || expected="${INSTALL_BACKUP_PIN_IDENTITIES[index]:-}"
    [[ -n "${expected}" ]] \
        || expected="${INSTALL_BACKUP_IDENTITIES[index]:-}"
    [[ -n "${expected}" ]] || return 1
    backup="${INSTALL_BACKUPS[index]:-}"
    pin="${INSTALL_BACKUP_PINS[index]:-}"
    anchor="${INSTALL_BACKUP_ANCHORS[index]:-}"
    if [[ -n "${backup}" ]] \
        && symlink_install_snapshot_path_matches "${index}" "${backup}" \
            "${expected}"; then
        source_basename="${INSTALL_BACKUP_BASENAMES[index]}"
        source_display="$(bound_install_entry_display "${index}" \
            "${source_basename}")"
    elif [[ -n "${pin}" ]] \
        && symlink_install_snapshot_path_matches "${index}" "${pin}" \
            "${expected}"; then
        source_basename="${INSTALL_BACKUP_PIN_BASENAMES[index]}"
        source_display="$(bound_install_entry_display "${index}" \
            "${source_basename}")"
    elif [[ -n "${anchor}" ]] \
        && symlink_install_snapshot_path_matches "${index}" "${anchor}" \
            "${expected}"; then
        source_display="$(bound_install_backup_anchor_display "${index}")" \
            || return 1
    else
        return 1
    fi
    dest_display="$(bound_install_entry_display "${index}" \
        "${INSTALL_DEST_BASENAMES[index]}")"
    {
        printf 'frost install: recovery command (symlink contents not displayed): '
        printf '%q ' mv -fT -- "${source_display}" "${dest_display}"
        printf '\n'
    } >&2
}

warn_retained_symlink_install_snapshot() {
    local index="$1" reason="$2" expected path display label
    local exact_recovery=0
    expected="${INSTALL_BACKUP_ANCHOR_IDENTITIES[index]:-}"
    [[ -n "${expected}" ]] \
        || expected="${INSTALL_BACKUP_PIN_IDENTITIES[index]:-}"
    [[ -n "${expected}" ]] \
        || expected="${INSTALL_BACKUP_IDENTITIES[index]:-}"
    printf 'frost install: warning: retaining symlink rollback snapshot (%s): %q\n' \
        "${reason}" "${INSTALL_DESTS[index]}" >&2
    for label in backup pin anchor; do
        case "${label}" in
            backup)
                path="${INSTALL_BACKUPS[index]:-}"
                [[ -n "${path}" ]] || continue
                display="$(bound_install_backup_display "${index}")"
                ;;
            pin)
                path="${INSTALL_BACKUP_PINS[index]:-}"
                [[ -n "${path}" ]] || continue
                display="$(bound_install_backup_pin_display "${index}")"
                ;;
            anchor)
                path="${INSTALL_BACKUP_ANCHORS[index]:-}"
                [[ -n "${path}" ]] || continue
                display="$(bound_install_backup_anchor_display "${index}" \
                    2>/dev/null)" || display='<bound-private-anchor>'
                ;;
        esac
        if [[ -n "${expected}" ]] \
            && symlink_install_snapshot_path_matches "${index}" "${path}" \
                "${expected}"; then
            printf 'frost install: warning: exact rollback %s retained at %q\n' \
                "${label}" "${display}" >&2
            exact_recovery=1
        elif [[ -e "${path}" || -L "${path}" ]]; then
            printf 'frost install: warning: changed rollback %s retained at %q\n' \
                "${label}" "${display}" >&2
        else
            printf 'frost install: warning: rollback %s missing at %q\n' \
                "${label}" "${display}" >&2
        fi
    done
    if ((exact_recovery == 1)); then
        print_exact_symlink_install_recovery_command "${index}" || :
    else
        display="$(bound_install_entry_display "${index}" \
            "${INSTALL_DEST_BASENAMES[index]}")"
        printf 'frost install: warning: no exact symlink recovery name remains for %q\n' \
            "${display}" >&2
    fi
}

cleanup_symlink_install_snapshot() {
    local index="$1" expected path display directory
    expected="${INSTALL_BACKUP_ANCHOR_IDENTITIES[index]:-}"
    if ! logical_install_parent_matches "${index}" \
        || ! symlink_install_snapshot_names_match "${index}"; then
        warn_retained_symlink_install_snapshot "${index}" \
            'preflight identity changed'
        return
    fi

    # The public backup is the first independent use point. The pin and the
    # private anchor must both still identify the owned inode immediately
    # before it is removed.
    path="${INSTALL_BACKUPS[index]:-}"
    if [[ -n "${path}" ]]; then
        display="$(bound_install_backup_display "${index}")"
        if ! logical_install_parent_matches "${index}" \
            || ! symlink_install_snapshot_names_match "${index}"; then
            warn_retained_symlink_install_snapshot "${index}" \
                'backup use-point identity changed'
            return
        fi
        rm -f -- "${path}" || :
        if [[ -e "${path}" || -L "${path}" ]]; then
            if symlink_install_snapshot_path_matches "${index}" "${path}" \
                "${expected}"; then
                printf 'frost install: warning: cannot remove rollback backup %q\n' \
                    "${display}" >&2
            else
                printf 'frost install: warning: rollback backup name changed during removal; replacement retained at %q\n' \
                    "${display}" >&2
            fi
            warn_retained_symlink_install_snapshot "${index}" \
                'backup removal did not reach the exact post-state'
            return
        fi
        INSTALL_BACKUPS[index]=""
        if ! logical_install_parent_matches "${index}" \
            || ! private_install_anchor_directory_matches "${index}"; then
            warn_retained_symlink_install_snapshot "${index}" \
                'directory identity changed after backup removal'
            return
        fi
    fi

    # Removing the first link does not authorize removing the second. Recheck
    # the public parent, the pin name, and the private anchor at this use point.
    path="${INSTALL_BACKUP_PINS[index]:-}"
    if [[ -n "${path}" ]]; then
        display="$(bound_install_backup_pin_display "${index}")"
        if ! logical_install_parent_matches "${index}" \
            || ! private_install_anchor_directory_matches "${index}" \
            || ! symlink_install_snapshot_path_matches "${index}" "${path}" \
                "${expected}" \
            || ! symlink_install_snapshot_path_matches "${index}" \
                "${INSTALL_BACKUP_ANCHORS[index]}" "${expected}"; then
            warn_retained_symlink_install_snapshot "${index}" \
                'pin use-point identity changed'
            return
        fi
        rm -f -- "${path}" || :
        if [[ -e "${path}" || -L "${path}" ]]; then
            if symlink_install_snapshot_path_matches "${index}" "${path}" \
                "${expected}"; then
                printf 'frost install: warning: cannot remove rollback pin %q\n' \
                    "${display}" >&2
            else
                printf 'frost install: warning: rollback pin name changed during removal; replacement retained at %q\n' \
                    "${display}" >&2
            fi
            warn_retained_symlink_install_snapshot "${index}" \
                'pin removal did not reach the exact post-state'
            return
        fi
        INSTALL_BACKUP_PINS[index]=""
        if ! logical_install_parent_matches "${index}" \
            || ! private_install_anchor_directory_matches "${index}"; then
            warn_retained_symlink_install_snapshot "${index}" \
                'directory identity changed after pin removal'
            return
        fi
    fi

    # The last link is reachable only inside the random 0700 directory held by
    # its own descriptor. If that directory or name changed, leave it intact.
    path="${INSTALL_BACKUP_ANCHORS[index]:-}"
    display="$(bound_install_backup_anchor_display "${index}" 2>/dev/null)" \
        || display='<bound-private-anchor>'
    if ! logical_install_parent_matches "${index}" \
        || ! private_install_anchor_directory_matches "${index}" \
        || ! symlink_install_snapshot_path_matches "${index}" "${path}" \
            "${expected}"; then
        warn_retained_symlink_install_snapshot "${index}" \
            'private anchor use-point identity changed'
        return
    fi
    rm -f -- "${path}" || :
    if [[ -e "${path}" || -L "${path}" ]]; then
        if symlink_install_snapshot_path_matches "${index}" "${path}" \
            "${expected}"; then
            printf 'frost install: warning: cannot remove private rollback anchor %q\n' \
                "${display}" >&2
        else
            printf 'frost install: warning: private rollback anchor name changed during removal; replacement retained at %q\n' \
                "${display}" >&2
        fi
        return
    fi
    INSTALL_BACKUP_ANCHORS[index]=""
    INSTALL_BACKUP_ANCHOR_IDENTITIES[index]=""

    directory="${INSTALL_BACKUP_ANCHOR_DIRS[index]}"
    if ! logical_install_parent_matches "${index}" \
        || ! private_install_anchor_directory_matches "${index}"; then
        printf 'frost install: warning: empty private rollback directory retained after its identity changed: %q\n' \
            "${directory}" >&2
        return
    fi
    rmdir -- "${directory}" 2>/dev/null || :
    if [[ -e "${directory}" || -L "${directory}" ]]; then
        if private_install_anchor_directory_matches "${index}"; then
            printf 'frost install: warning: cannot remove empty private rollback directory %q\n' \
                "${directory}" >&2
        else
            printf 'frost install: warning: private rollback directory name changed during removal; replacement retained at %q\n' \
                "${directory}" >&2
        fi
    else
        INSTALL_BACKUP_ANCHOR_DIRS[index]=""
    fi
}

logical_install_parent_matches() {
    local index="$1"
    directory_referent_matches_identity "${INSTALL_DESTS[index]%/*}" \
        "${INSTALL_PARENT_IDENTITIES[index]}"
}

install_backup_copy_matches() {
    local source="$1" expected_source_identity="$2" backup="$3"
    local source_value backup_value source_metadata backup_metadata
    local source_mode backup_mode
    path_matches_identity "${source}" "${expected_source_identity}" \
        || return 1
    if [[ -L "${source}" ]]; then
        [[ -L "${backup}" ]] || return 1
        source_value="$(readlink -- "${source}")" || return 1
        backup_value="$(readlink -- "${backup}")" || return 1
        [[ "${backup_value}" == "${source_value}" ]] || return 1
        source_metadata="$(stat -c '%u:%g:%a' -- "${source}")" \
            || return 1
        backup_metadata="$(stat -c '%u:%g:%a' -- "${backup}")" \
            || return 1
        [[ "${backup_metadata}" == "${source_metadata}" ]]
        return
    fi
    [[ -f "${source}" && ! -L "${source}" \
        && -f "${backup}" && ! -L "${backup}" ]] || return 1
    cmp -s -- "${source}" "${backup}" || return 1
    source_mode="$(stat -c '%a' -- "${source}")" || return 1
    backup_mode="$(stat -c '%a' -- "${backup}")" || return 1
    [[ "${backup_mode}" == "${source_mode}" ]]
}

opened_regular_backup_copy_matches() {
    local source="$1" expected_source_identity="$2"
    local backup_fd_path="$3" expected_backup_identity="$4"
    local source_mode backup_mode
    path_matches_identity "${source}" "${expected_source_identity}" \
        || return 1
    [[ -f "${source}" && ! -L "${source}" ]] || return 1
    opened_staged_install_artifact_matches "${backup_fd_path}" \
        "${expected_backup_identity}" || return 1
    cmp -s -- "${source}" "${backup_fd_path}" || return 1
    source_mode="$(stat -c '%a' -- "${source}")" || return 1
    backup_mode="$(stat -Lc '%a' -- "${backup_fd_path}")" || return 1
    [[ "${backup_mode}" == "${source_mode}" ]]
}

opened_regular_install_backup_matches() {
    local fd="$1" expected="$2" actual
    [[ -n "${fd}" && -f "/proc/self/fd/${fd}" ]] || return 1
    actual="$(stat -Lc '%d:%i' -- "/proc/self/fd/${fd}")" || return 1
    [[ "${actual}" == "${expected}" ]]
}

install_backup_path_matches_identity() {
    local index="$1" path="$2" expected="$3" fd pin anchor
    if [[ -v 'INSTALL_BACKUP_SYMLINK_VALUES[index]' ]]; then
        symlink_install_snapshot_path_matches "${index}" "${path}" \
            "${expected}" || return 1
    else
        path_matches_identity "${path}" "${expected}" || return 1
    fi
    fd="${INSTALL_BACKUP_FDS[index]:-}"
    if [[ -n "${fd}" ]]; then
        opened_regular_install_backup_matches "${fd}" "${expected}" \
            || return 1
        return 0
    fi
    pin="${INSTALL_BACKUP_PINS[index]:-}"
    anchor="${INSTALL_BACKUP_ANCHORS[index]:-}"
    if [[ -n "${pin}" ]]; then
        symlink_install_snapshot_path_matches "${index}" "${pin}" \
            "${expected}" || return 1
    fi
    if [[ -n "${anchor}" ]]; then
        private_install_anchor_directory_matches "${index}" \
            && symlink_install_snapshot_path_matches "${index}" "${anchor}" \
                "${expected}" || return 1
    fi
}

retain_regular_install_backup_fd() {
    local index="$1" backup="$2" expected="$3" dest="$4"
    local candidate_fd="${5:-}" fd count=0
    for fd in "${INSTALL_BACKUP_FDS[@]}"; do
        [[ -n "${fd}" ]] && count=$((count + 1))
    done
    ((count < MAX_INSTALL_BACKUP_FDS)) \
        || die "too many regular rollback backups (limit ${MAX_INSTALL_BACKUP_FDS})"
    if [[ -z "${candidate_fd}" ]]; then
        unset candidate_fd
        exec {candidate_fd}<"${backup}" \
            || die "cannot pin regular rollback backup for ${dest}"
    fi
    if [[ -L "${backup}" || ! -f "${backup}" ]] \
        || ! path_matches_identity "${backup}" "${expected}" \
        || ! opened_regular_install_backup_matches "${candidate_fd}" \
            "${expected}"; then
        exec {candidate_fd}<&-
        INSTALL_BACKUP_IDENTITIES[index]=""
        die "regular rollback backup changed while pinning ${dest}"
    fi
    INSTALL_BACKUP_FDS[index]="${candidate_fd}"
}

retain_symlink_install_backup_anchor() {
    local index="$1" backup="$2" expected="$3" dest="$4"
    local source="$5" source_identity="$6"
    local basename directory directory_identity directory_metadata
    local anchor anchor_fd command_status=0 fd count=0
    for fd in "${INSTALL_BACKUP_ANCHOR_DIR_FDS[@]}"; do
        [[ -n "${fd}" ]] && count=$((count + 1))
    done
    ((count < MAX_INSTALL_BACKUP_ANCHOR_DIR_FDS)) \
        || die "too many symlink rollback anchors (limit ${MAX_INSTALL_BACKUP_ANCHOR_DIR_FDS})"

    basename="${INSTALL_DEST_BASENAMES[index]}"
    directory="$(mktemp -d "$(bound_install_entry_path "${index}" \
        ".${basename}.rollback-anchor.XXXXXX")")" \
        || die "cannot reserve private symlink rollback directory beside ${dest}"
    INSTALL_BACKUP_ANCHOR_DIR_BASENAMES[index]="${directory##*/}"
    INSTALL_BACKUP_ANCHOR_DIRS[index]="$(bound_install_backup_anchor_dir_path \
        "${index}")"
    directory="${INSTALL_BACKUP_ANCHOR_DIRS[index]}"
    directory_identity="$(stat -c '%d:%i' -- "${directory}")" \
        || die "cannot identify private symlink rollback directory beside ${dest}"
    directory_metadata="$(stat -c '%u:%g:%a' -- "${directory}")" \
        || die "cannot inspect private symlink rollback directory beside ${dest}"
    [[ "${directory_metadata##*:}" == 700 ]] \
        || die "private symlink rollback directory has unsafe permissions beside ${dest}"
    INSTALL_BACKUP_ANCHOR_DIR_IDENTITIES[index]="${directory_identity}"
    INSTALL_BACKUP_ANCHOR_DIR_METADATA[index]="${directory_metadata}"
    unset anchor_fd
    exec {anchor_fd}<"${directory}" \
        || die "cannot bind private symlink rollback directory beside ${dest}"
    INSTALL_BACKUP_ANCHOR_DIR_FDS[index]="${anchor_fd}"
    private_install_anchor_directory_matches "${index}" \
        || die "private symlink rollback directory changed while binding ${dest}"
    logical_install_parent_matches "${index}" \
        || die "install destination directory changed while reserving private symlink rollback anchor: ${dest%/*}"
    install_backup_copy_matches "${source}" "${source_identity}" \
        "${backup}" \
        || die "symlink rollback backup changed while reserving private anchor for ${dest}"
    path_matches_identity "${backup}" "${expected}" \
        || die "symlink rollback backup identity changed while reserving private anchor for ${dest}"
    path_matches_identity "${INSTALL_BACKUP_PINS[index]}" "${expected}" \
        || die "symlink rollback pin identity changed while reserving private anchor for ${dest}"

    INSTALL_BACKUP_ANCHOR_BASENAMES[index]="snapshot"
    INSTALL_BACKUP_ANCHORS[index]="/proc/self/fd/${anchor_fd}/snapshot"
    anchor="${INSTALL_BACKUP_ANCHORS[index]}"
    INSTALL_BACKUP_ANCHOR_IDENTITIES[index]=""
    ln -P -- "${backup}" "${anchor}" 2>/dev/null || command_status=$?
    if private_install_anchor_directory_matches "${index}" \
        && path_matches_identity "${backup}" "${expected}" \
        && path_matches_identity "${INSTALL_BACKUP_PINS[index]}" \
            "${expected}" \
        && [[ -L "${anchor}" ]] \
        && path_matches_identity "${anchor}" "${expected}" \
        && install_backup_copy_matches "${source}" "${source_identity}" \
            "${backup}"; then
        # The exact three-link state wins over a wrapper reporting failure
        # after it created the private anchor.
        INSTALL_BACKUP_ANCHOR_IDENTITIES[index]="${expected}"
    elif [[ -e "${anchor}" || -L "${anchor}" ]]; then
        die "private symlink rollback anchor was replaced while linking ${dest}"
    elif ((command_status == 0)); then
        die "private symlink rollback anchor disappeared while linking ${dest}"
    else
        die "cannot create private symlink rollback anchor for ${dest}"
    fi
    logical_install_parent_matches "${index}" \
        || die "install destination directory changed while pinning private symlink rollback anchor: ${dest%/*}"
}

retain_symlink_install_backup_pin() {
    local index="$1" backup="$2" expected="$3" dest="$4"
    local source="$5" source_identity="$6"
    local basename pin reservation_identity reservation_fd
    local reservation_fd_path value metadata command_status=0
    value="$(read_symlink_text_exact "${backup}")" \
        || die "cannot record symlink rollback backup text for ${dest}"
    metadata="$(stat -c '%u:%g:%a' -- "${backup}")" \
        || die "cannot inspect symlink rollback backup for ${dest}"
    INSTALL_BACKUP_SYMLINK_VALUES[index]="${value}"
    INSTALL_BACKUP_SYMLINK_METADATA[index]="${metadata}"
    basename="${INSTALL_DEST_BASENAMES[index]}"
    pin="$(mktemp "$(bound_install_entry_path "${index}" \
        ".${basename}.rollback-pin.XXXXXX")")" \
        || die "cannot reserve symlink rollback pin beside ${dest}"
    INSTALL_BACKUP_PIN_BASENAMES[index]="${pin##*/}"
    INSTALL_BACKUP_PINS[index]="$(bound_install_entry_path "${index}" \
        "${INSTALL_BACKUP_PIN_BASENAMES[index]}")"
    pin="${INSTALL_BACKUP_PINS[index]}"
    reservation_identity="$(stat -c '%d:%i' -- "${pin}")" \
        || die "cannot identify symlink rollback pin reservation beside ${dest}"
    INSTALL_BACKUP_PIN_IDENTITIES[index]="${reservation_identity}"
    unset reservation_fd
    exec {reservation_fd}<>"${pin}" \
        || die "cannot open symlink rollback pin reservation beside ${dest}"
    reservation_fd_path="/proc/self/fd/${reservation_fd}"
    if ! opened_staged_install_artifact_matches "${reservation_fd_path}" \
        "${reservation_identity}" \
        || ! path_matches_identity "${pin}" "${reservation_identity}"; then
        INSTALL_BACKUP_PIN_IDENTITIES[index]=""
        exec {reservation_fd}>&-
        die "symlink rollback pin reservation changed while opening ${dest}"
    fi
    if ! logical_install_parent_matches "${index}"; then
        exec {reservation_fd}>&-
        die "install destination directory changed while reserving symlink rollback pin: ${dest%/*}"
    fi
    if ! install_backup_copy_matches "${source}" "${source_identity}" \
        "${backup}"; then
        exec {reservation_fd}>&-
        die "symlink rollback backup changed while reserving pin for ${dest}"
    fi
    if ! path_matches_identity "${backup}" "${expected}"; then
        exec {reservation_fd}>&-
        die "symlink rollback backup identity changed while reserving pin for ${dest}"
    fi
    rm -f -- "${pin}" || :
    if [[ -e "${pin}" || -L "${pin}" ]]; then
        if path_matches_identity "${pin}" "${reservation_identity}"; then
            exec {reservation_fd}>&-
            die "cannot prepare symlink rollback pin beside ${dest}"
        fi
        INSTALL_BACKUP_PIN_IDENTITIES[index]=""
        exec {reservation_fd}>&-
        die "symlink rollback pin reservation name changed while removing it beside ${dest}"
    fi
    exec {reservation_fd}>&-
    INSTALL_BACKUP_PIN_IDENTITIES[index]=""
    logical_install_parent_matches "${index}" \
        || die "install destination directory changed while preparing symlink rollback pin: ${dest%/*}"
    install_backup_copy_matches "${source}" "${source_identity}" \
        "${backup}" \
        || die "symlink rollback backup changed before pinning ${dest}"
    path_matches_identity "${backup}" "${expected}" \
        || die "symlink rollback backup identity changed before pinning ${dest}"
    ln -P -- "${backup}" "${pin}" 2>/dev/null || command_status=$?
    if path_matches_identity "${backup}" "${expected}" \
        && path_matches_identity "${pin}" "${expected}" \
        && install_backup_copy_matches "${source}" "${source_identity}" \
            "${backup}"; then
        # Exact post-state is authoritative when an instrumented ln reports
        # failure after creating the second link to the copied symlink inode.
        INSTALL_BACKUP_PIN_IDENTITIES[index]="${expected}"
    elif [[ -e "${pin}" || -L "${pin}" ]]; then
        INSTALL_BACKUP_PIN_IDENTITIES[index]=""
        die "symlink rollback pin name was replaced while linking ${dest}"
    elif ((command_status == 0)); then
        die "symlink rollback pin disappeared while linking ${dest}"
    else
        die "cannot pin symlink rollback backup for ${dest}"
    fi
    logical_install_parent_matches "${index}" \
        || die "install destination directory changed while pinning symlink rollback backup: ${dest%/*}"
    retain_symlink_install_backup_anchor "${index}" "${backup}" \
        "${expected}" "${dest}" "${source}" "${source_identity}"
}

bind_install_destination() {
    local index="$1" dest="$2" directory fd identity bind_status
    directory="${dest%/*}"
    if acquire_install_bound_directory "${directory}" fd identity; then
        :
    else
        bind_status=$?
        if ((bind_status == 2)); then
            die "too many distinct install directories (limit ${MAX_INSTALL_BOUND_DIRECTORY_FDS})"
        fi
        die "cannot bind install destination directory ${directory}"
    fi
    INSTALL_PARENT_FDS[index]="${fd}"
    INSTALL_PARENT_IDENTITIES[index]="${identity}"
    INSTALL_DEST_BASENAMES[index]="${dest##*/}"
    directory_referent_matches_identity "${directory}" "${identity}" \
        || die "install destination directory changed while binding: ${directory}"
}

rollback_install_plan() {
    local index dest dest_path dest_display backup backup_display
    local backup_identity original_identity staged_identity pin pin_identity
    local pin_display rollback_failed=0
    for ((index = PUBLISH_LAST_ATTEMPT; index >= 0; index--)); do
        dest="${INSTALL_DESTS[index]}"
        dest_path="$(bound_install_dest_path "${index}")"
        dest_display="$(bound_install_entry_display "${index}" \
            "${INSTALL_DEST_BASENAMES[index]}")"
        staged_identity="${INSTALL_STAGED_IDENTITIES[index]:-}"
        if (( ${INSTALL_ORIGINAL_PRESENT[index]:-0} == 1 )); then
            backup="${INSTALL_BACKUPS[index]:-}"
            backup_identity="${INSTALL_BACKUP_IDENTITIES[index]:-}"
            original_identity="${INSTALL_ORIGINAL_IDENTITIES[index]:-}"
            backup_display="$(bound_install_backup_display "${index}")"
            if [[ -n "${backup}" && ( -e "${backup}" || -L "${backup}" ) ]]; then
                if [[ -z "${backup_identity}" ]] \
                    || ! install_backup_path_matches_identity "${index}" \
                        "${backup}" "${backup_identity}"; then
                    printf 'frost install: rollback refused changed backup for %s; unexpected entry retained at %s\n' \
                        "${dest_display}" "${backup_display}" >&2
                    pin="${INSTALL_BACKUP_PINS[index]:-}"
                    pin_identity="${INSTALL_BACKUP_PIN_IDENTITIES[index]:-}"
                    if [[ -n "${pin}" && -n "${pin_identity}" \
                        && ( -e "${pin}" || -L "${pin}" ) ]] \
                        && path_matches_identity "${pin}" \
                            "${pin_identity}"; then
                        pin_display="$(bound_install_backup_pin_display \
                            "${index}")"
                        printf 'frost install: exact symlink recovery pin for %q retained at %q\n' \
                            "${dest_display}" "${pin_display}" >&2
                        print_exact_symlink_install_recovery_command \
                            "${index}" || :
                    fi
                    rollback_failed=1
                elif path_matches_identity "${dest_path}" \
                    "${original_identity}"; then
                    if rm -f -- "${backup}"; then
                        INSTALL_BACKUPS[index]=""
                    else
                        printf 'frost install: rollback restored %s but could not remove backup link %s\n' \
                            "${dest_display}" "${backup_display}" >&2
                        rollback_failed=1
                    fi
                elif [[ ! -e "${dest_path}" && ! -L "${dest_path}" ]] \
                    || path_matches_identity "${dest_path}" \
                        "${staged_identity}"; then
                    if mv -fT -- "${backup}" "${dest_path}"; then
                        INSTALL_BACKUPS[index]=""
                    elif install_backup_path_matches_identity "${index}" \
                        "${dest_path}" "${backup_identity}" \
                        && [[ ! -e "${backup}" && ! -L "${backup}" ]]; then
                        # Reconcile a wrapper that reports failure after the
                        # bound restore rename already completed.
                        INSTALL_BACKUPS[index]=""
                    else
                        printf 'frost install: rollback failed for %s; backup retained at %s\n' \
                            "${dest_display}" "${backup_display}" >&2
                        rollback_failed=1
                    fi
                else
                    printf 'frost install: rollback refused to overwrite changed target %s; backup retained at %s\n' \
                        "${dest_display}" "${backup_display}" >&2
                    rollback_failed=1
                fi
            else
                printf 'frost install: rollback backup missing for %s\n' \
                    "${dest_display}" >&2
                rollback_failed=1
            fi
        elif [[ ! -e "${dest_path}" && ! -L "${dest_path}" ]]; then
            :
        elif [[ -n "${staged_identity}" ]] \
            && path_matches_identity "${dest_path}" "${staged_identity}"; then
            if rm -f -- "${dest_path}"; then
                :
            elif [[ ! -e "${dest_path}" && ! -L "${dest_path}" ]]; then
                # As above, observed post-action state wins over a wrapper's
                # non-zero status.
                :
            else
                printf 'frost install: rollback could not remove new target %s\n' \
                    "${dest_display}" >&2
                rollback_failed=1
            fi
        else
            printf 'frost install: rollback refused to remove changed new target %s\n' \
                "${dest_display}" >&2
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
    if [[ -n "${PREBUILT_FD}" ]]; then
        exec {PREBUILT_FD}<&-
        PREBUILT_FD=""
    fi
    close_install_bound_directory_fds
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

populate_staged_install_artifact() {
    local index="$1" source="$2" mode="$3" dest="$4"
    local temp="${INSTALL_TEMPS[index]}" staged_identity staged_fd fd_path
    local actual_mode command_status
    staged_identity="${INSTALL_STAGED_IDENTITIES[index]}"

    # GNU install deliberately unlinks an existing destination before copying,
    # so it cannot preserve the inode mktemp gave this transaction. Keep that
    # inode open and address the descriptor itself while copying and chmodding;
    # an ABA replacement of the logical temporary name can then neither receive
    # bytes nor be mistaken for this transaction's artifact.
    unset staged_fd
    exec {staged_fd}<>"${temp}" \
        || die "cannot open install temporary for ${dest}"
    fd_path="/proc/self/fd/${staged_fd}"
    if ! opened_staged_install_artifact_matches "${fd_path}" \
        "${staged_identity}" \
        || ! staged_install_artifact_matches "${temp}" \
            "${staged_identity}"; then
        INSTALL_STAGED_IDENTITIES[index]=""
        exec {staged_fd}>&-
        die "install temporary changed while opening ${dest}"
    fi

    printf 'frost install reservation %s\n' \
        "${INSTALL_TEMP_BASENAMES[index]}" >"${fd_path}" \
        || die "cannot initialize install temporary for ${dest}"
    if cmp -s -- "${source}" "${fd_path}"; then
        printf '\n' >>"${fd_path}" \
            || die "cannot distinguish install temporary for ${dest}"
    fi
    if ! opened_staged_install_artifact_matches "${fd_path}" \
        "${staged_identity}" \
        || ! staged_install_artifact_matches "${temp}" \
            "${staged_identity}"; then
        INSTALL_STAGED_IDENTITIES[index]=""
        exec {staged_fd}>&-
        die "install temporary changed while initializing ${dest}"
    fi

    command_status=0
    cat -- "${source}" >"${fd_path}" || command_status=$?
    if ! opened_staged_install_artifact_matches "${fd_path}" \
        "${staged_identity}" \
        || ! staged_install_artifact_matches "${temp}" \
            "${staged_identity}"; then
        INSTALL_STAGED_IDENTITIES[index]=""
        exec {staged_fd}>&-
        die "install temporary changed while copying ${dest}"
    fi
    if ! cmp -s -- "${source}" "${fd_path}"; then
        exec {staged_fd}>&-
        if ((command_status != 0)); then
            die "cannot copy staged content for ${dest}"
        fi
        die "cannot reconcile staged content for ${dest}"
    fi
    # Exact descriptor and pathname state is authoritative when an
    # instrumented byte-copy command reports failure after completing output.

    command_status=0
    chmod "${mode}" "${fd_path}" || command_status=$?
    if ! opened_staged_install_artifact_matches "${fd_path}" \
        "${staged_identity}" \
        || ! staged_install_artifact_matches "${temp}" \
            "${staged_identity}"; then
        INSTALL_STAGED_IDENTITIES[index]=""
        exec {staged_fd}>&-
        die "install temporary changed while setting mode for ${dest}"
    fi
    actual_mode="$(stat -Lc '%a' -- "${fd_path}")" \
        || die "cannot identify staged mode for ${dest}"
    if [[ "${actual_mode}" != "${mode#0}" ]]; then
        exec {staged_fd}>&-
        if ((command_status != 0)); then
            die "cannot set staged mode for ${dest}"
        fi
        die "cannot reconcile staged mode for ${dest}"
    fi
    if ! cmp -s -- "${source}" "${fd_path}"; then
        exec {staged_fd}>&-
        die "staged content changed while setting mode for ${dest}"
    fi
    exec {staged_fd}>&-
}

stage_install_file() {
    local mode="$1" source="$2" dest="$3" directory basename temp
    local index staged_identity
    printf '  install -m %q %q %q\n' \
        "${mode}" "${source}" "${dest}.<temporary>"
    index="${#INSTALL_DESTS[@]}"
    INSTALL_DESTS[index]="${dest}"
    INSTALL_PARENT_FDS[index]=""
    INSTALL_PARENT_IDENTITIES[index]=""
    INSTALL_DEST_BASENAMES[index]="${dest##*/}"
    INSTALL_TEMP_BASENAMES[index]=""
    INSTALL_STAGED_IDENTITIES[index]=""
    if ((DRY_RUN == 1)); then
        INSTALL_TEMPS[index]="${dest}.<temporary>"
        return 0
    fi
    directory="${dest%/*}"
    basename="${dest##*/}"
    install -d -m 0755 "${directory}" \
        || die "cannot create destination directory for ${dest}"
    bind_install_destination "${index}" "${dest}"
    temp="$(mktemp "$(bound_install_entry_path "${index}" \
        ".${basename}.install.XXXXXX")")" \
        || die "cannot create temporary file beside ${dest}"
    INSTALL_TEMP_BASENAMES[index]="${temp##*/}"
    INSTALL_TEMPS[index]="$(bound_install_temp_path "${index}")"
    staged_identity="$(stat -c '%d:%i' -- "${INSTALL_TEMPS[index]}")" \
        || die "cannot identify install temporary for ${dest}"
    INSTALL_STAGED_IDENTITIES[index]="${staged_identity}"
    if ! staged_install_artifact_matches "${INSTALL_TEMPS[index]}" \
        "${staged_identity}"; then
        INSTALL_STAGED_IDENTITIES[index]=""
        die "install temporary is not a private regular file for ${dest}"
    fi
    logical_install_parent_matches "${index}" \
        || die "install destination directory changed while staging: ${directory}"
    populate_staged_install_artifact "${index}" "${source}" "${mode}" \
        "${dest}"
    logical_install_parent_matches "${index}" \
        || die "install destination directory changed while staging: ${directory}"
}

# Keep the binary temporary on its destination filesystem. It is queued after
# every resource so publish_install_plan makes the executable the last commit.
stage_install_binary() {
    local source="$1" dest="$2" directory temp index staged_identity
    printf '  install -m 0755 %q %q\n' "${source}" "${dest}.<temporary>"
    index="${#INSTALL_DESTS[@]}"
    INSTALL_DESTS[index]="${dest}"
    INSTALL_PARENT_FDS[index]=""
    INSTALL_PARENT_IDENTITIES[index]=""
    INSTALL_DEST_BASENAMES[index]="${dest##*/}"
    INSTALL_TEMP_BASENAMES[index]=""
    INSTALL_STAGED_IDENTITIES[index]=""
    if ((DRY_RUN == 1)); then
        INSTALL_TEMPS[index]="${dest}.<temporary>"
        return 0
    fi
    directory="${dest%/*}"
    install -d -m 0755 "${directory}" \
        || die "cannot create binary directory for ${dest}"
    bind_install_destination "${index}" "${dest}"
    temp="$(mktemp "$(bound_install_entry_path "${index}" \
        ".${dest##*/}.install.XXXXXX")")" \
        || die "cannot create temporary binary beside ${dest}"
    INSTALL_TEMP_BASENAMES[index]="${temp##*/}"
    INSTALL_TEMPS[index]="$(bound_install_temp_path "${index}")"
    staged_identity="$(stat -c '%d:%i' -- "${INSTALL_TEMPS[index]}")" \
        || die "cannot identify temporary binary for ${dest}"
    INSTALL_STAGED_IDENTITIES[index]="${staged_identity}"
    if ! staged_install_artifact_matches "${INSTALL_TEMPS[index]}" \
        "${staged_identity}"; then
        INSTALL_STAGED_IDENTITIES[index]=""
        die "temporary binary is not a private regular file for ${dest}"
    fi
    logical_install_parent_matches "${index}" \
        || die "binary destination directory changed while staging: ${directory}"
    populate_staged_install_artifact "${index}" "${source}" 0755 "${dest}"
    logical_install_parent_matches "${index}" \
        || die "binary destination directory changed while staging: ${directory}"
}

prepare_install_backups() {
    local index dest dest_path directory basename backup reservation_identity
    local original_identity backup_identity staged_identity command_status
    local fallback fallback_identity fallback_is_symlink
    local fallback_fd fallback_fd_path
    INSTALL_BACKUPS=()
    INSTALL_BACKUP_BASENAMES=()
    INSTALL_BACKUP_IDENTITIES=()
    INSTALL_BACKUP_FDS=()
    INSTALL_BACKUP_PINS=()
    INSTALL_BACKUP_PIN_BASENAMES=()
    INSTALL_BACKUP_PIN_IDENTITIES=()
    INSTALL_BACKUP_ANCHORS=()
    INSTALL_BACKUP_ANCHOR_BASENAMES=()
    INSTALL_BACKUP_ANCHOR_IDENTITIES=()
    INSTALL_BACKUP_ANCHOR_DIRS=()
    INSTALL_BACKUP_ANCHOR_DIR_BASENAMES=()
    INSTALL_BACKUP_ANCHOR_DIR_FDS=()
    INSTALL_BACKUP_ANCHOR_DIR_IDENTITIES=()
    INSTALL_BACKUP_ANCHOR_DIR_METADATA=()
    INSTALL_BACKUP_SYMLINK_VALUES=()
    INSTALL_BACKUP_SYMLINK_METADATA=()
    INSTALL_ORIGINAL_PRESENT=()
    INSTALL_ORIGINAL_IDENTITIES=()
    # Validate the complete final-target set before creating even the first
    # rollback link. A late FIFO/socket/device/directory must not leave earlier
    # targets with transient backup names, and no special file is ever opened.
    for index in "${!INSTALL_DESTS[@]}"; do
        dest="${INSTALL_DESTS[index]}"
        dest_path="$(bound_install_dest_path "${index}")"
        staged_identity="${INSTALL_STAGED_IDENTITIES[index]}"
        logical_install_parent_matches "${index}" \
            || die "install destination directory changed before backup: ${dest%/*}"
        staged_install_artifact_matches \
            "$(bound_install_temp_path "${index}")" \
            "${staged_identity}" \
            || die "install temporary changed before backup: ${dest}"
        if [[ -e "${dest_path}" || -L "${dest_path}" ]]; then
            [[ -f "${dest_path}" || -L "${dest_path}" ]] \
                || die "install destination is not a regular file or symlink: ${dest}"
        fi
    done
    for index in "${!INSTALL_DESTS[@]}"; do
        dest="${INSTALL_DESTS[index]}"
        dest_path="$(bound_install_dest_path "${index}")"
        INSTALL_BACKUPS[index]=""
        INSTALL_BACKUP_BASENAMES[index]=""
        INSTALL_BACKUP_IDENTITIES[index]=""
        INSTALL_BACKUP_FDS[index]=""
        INSTALL_BACKUP_PINS[index]=""
        INSTALL_BACKUP_PIN_BASENAMES[index]=""
        INSTALL_BACKUP_PIN_IDENTITIES[index]=""
        INSTALL_BACKUP_ANCHORS[index]=""
        INSTALL_BACKUP_ANCHOR_BASENAMES[index]=""
        INSTALL_BACKUP_ANCHOR_IDENTITIES[index]=""
        INSTALL_BACKUP_ANCHOR_DIRS[index]=""
        INSTALL_BACKUP_ANCHOR_DIR_BASENAMES[index]=""
        INSTALL_BACKUP_ANCHOR_DIR_FDS[index]=""
        INSTALL_BACKUP_ANCHOR_DIR_IDENTITIES[index]=""
        INSTALL_BACKUP_ANCHOR_DIR_METADATA[index]=""
        unset 'INSTALL_BACKUP_SYMLINK_VALUES[index]'
        INSTALL_BACKUP_SYMLINK_METADATA[index]=""
        fallback_fd=""
        if [[ -e "${dest_path}" || -L "${dest_path}" ]]; then
            [[ -f "${dest_path}" || -L "${dest_path}" ]] \
                || die "install destination is not a regular file or symlink: ${dest}"
            directory="${dest%/*}"
            basename="${dest##*/}"
            original_identity="$(stat -c '%d:%i' -- "${dest_path}")" \
                || die "cannot identify existing install target ${dest}"
            INSTALL_ORIGINAL_PRESENT[index]=1
            INSTALL_ORIGINAL_IDENTITIES[index]="${original_identity}"
            backup="$(mktemp "$(bound_install_entry_path "${index}" \
                ".${basename}.rollback.XXXXXX")")" \
                || die "cannot reserve rollback backup beside ${dest}"
            INSTALL_BACKUP_BASENAMES[index]="${backup##*/}"
            INSTALL_BACKUPS[index]="$(bound_install_backup_path "${index}")"
            reservation_identity="$(stat -c '%d:%i' -- \
                "${INSTALL_BACKUPS[index]}")" \
                || die "cannot identify rollback reservation beside ${dest}"
            INSTALL_BACKUP_IDENTITIES[index]="${reservation_identity}"
            logical_install_parent_matches "${index}" \
                || die "install destination directory changed while reserving backup: ${directory}"
            path_matches_identity "${dest_path}" "${original_identity}" \
                || die "install target changed while reserving backup: ${dest}"
            rm -f -- "${INSTALL_BACKUPS[index]}" || :
            if [[ -e "${INSTALL_BACKUPS[index]}" \
                || -L "${INSTALL_BACKUPS[index]}" ]]; then
                if path_matches_identity "${INSTALL_BACKUPS[index]}" \
                    "${reservation_identity}"; then
                    die "cannot prepare rollback backup beside ${dest}"
                fi
                # Once the name contains another inode, ownership is
                # ambiguous. Do not let exit cleanup unlink it a second time.
                INSTALL_BACKUP_IDENTITIES[index]=""
                die "rollback reservation name changed while removing it beside ${dest}"
            fi
            INSTALL_BACKUP_IDENTITIES[index]=""
            # A same-directory hard link retains the exact inode: owner/group,
            # mode, xattrs, and even a dangling symlink's link object. Some
            # filesystems or protected-hardlink policies reject it; the copy
            # fallback still preserves content/mode and never follows links.
            command_status=0
            ln -P -- "${dest_path}" "${INSTALL_BACKUPS[index]}" \
                2>/dev/null || command_status=$?
            if path_matches_identity "${INSTALL_BACKUPS[index]}" \
                "${original_identity}"; then
                # Reconcile a hard link that was created successfully even if
                # an instrumented command returned a non-zero status.
                :
            elif [[ -e "${INSTALL_BACKUPS[index]}" \
                || -L "${INSTALL_BACKUPS[index]}" ]]; then
                INSTALL_BACKUP_IDENTITIES[index]=""
                die "rollback backup name was replaced while linking ${dest}"
            elif ((command_status == 0)); then
                die "rollback backup disappeared while linking ${dest}"
            else
                # Hardlink creation can be forbidden by filesystem or policy.
                # Reserve a fresh regular inode instead of asking cp to create
                # an unknown name, so ordinary-file copies have a known exact
                # destination identity before the external command runs.
                fallback="$(mktemp "$(bound_install_entry_path "${index}" \
                    ".${basename}.rollback.XXXXXX")")" \
                    || die "cannot reserve fallback rollback backup beside ${dest}"
                INSTALL_BACKUP_BASENAMES[index]="${fallback##*/}"
                INSTALL_BACKUPS[index]="$(bound_install_backup_path "${index}")"
                fallback_identity="$(stat -c '%d:%i' -- \
                    "${INSTALL_BACKUPS[index]}")" \
                    || die "cannot identify fallback rollback reservation beside ${dest}"
                INSTALL_BACKUP_IDENTITIES[index]="${fallback_identity}"
                [[ -f "${INSTALL_BACKUPS[index]}" \
                    && ! -L "${INSTALL_BACKUPS[index]}" ]] \
                    || die "fallback rollback reservation is not a regular file beside ${dest}"
                fallback_is_symlink=0
                [[ -L "${dest_path}" ]] && fallback_is_symlink=1
                if ((fallback_is_symlink == 0)); then
                    unset fallback_fd
                    exec {fallback_fd}<>"${INSTALL_BACKUPS[index]}" \
                        || die "cannot open fallback rollback reservation beside ${dest}"
                    fallback_fd_path="/proc/self/fd/${fallback_fd}"
                    if ! opened_staged_install_artifact_matches \
                        "${fallback_fd_path}" "${fallback_identity}" \
                        || ! path_matches_identity \
                            "${INSTALL_BACKUPS[index]}" \
                            "${fallback_identity}"; then
                        INSTALL_BACKUP_IDENTITIES[index]=""
                        exec {fallback_fd}>&-
                        die "fallback rollback reservation changed while opening ${dest}"
                    fi
                    printf 'frost rollback reservation %s\n' \
                        "${fallback##*/}" >"${fallback_fd_path}" \
                        || die "cannot initialize fallback rollback reservation beside ${dest}"
                    if cmp -s -- "${dest_path}" \
                        "${fallback_fd_path}"; then
                        printf '\n' >>"${fallback_fd_path}" \
                            || die "cannot distinguish fallback rollback reservation beside ${dest}"
                    fi
                    if ! opened_staged_install_artifact_matches \
                        "${fallback_fd_path}" "${fallback_identity}" \
                        || ! path_matches_identity \
                            "${INSTALL_BACKUPS[index]}" \
                            "${fallback_identity}"; then
                        INSTALL_BACKUP_IDENTITIES[index]=""
                        exec {fallback_fd}>&-
                        die "fallback rollback reservation changed while initializing ${dest}"
                    fi
                fi
                logical_install_parent_matches "${index}" \
                    || die "install destination directory changed while reserving fallback backup: ${directory}"
                path_matches_identity "${dest_path}" "${original_identity}" \
                    || die "install target changed while reserving fallback backup: ${dest}"
                command_status=0
                if ((fallback_is_symlink == 0)); then
                    cp -a --no-dereference --no-preserve=ownership -- \
                        "${dest_path}" "${fallback_fd_path}" \
                        || command_status=$?
                    if ! opened_staged_install_artifact_matches \
                        "${fallback_fd_path}" "${fallback_identity}" \
                        || ! path_matches_identity \
                            "${INSTALL_BACKUPS[index]}" \
                            "${fallback_identity}"; then
                        INSTALL_BACKUP_IDENTITIES[index]=""
                        exec {fallback_fd}>&-
                        die "fallback rollback backup name was replaced while copying ${dest}"
                    fi
                    if ! opened_regular_backup_copy_matches "${dest_path}" \
                        "${original_identity}" "${fallback_fd_path}" \
                        "${fallback_identity}"; then
                        exec {fallback_fd}>&-
                        if ((command_status != 0)); then
                            die "cannot copy fallback rollback backup for ${dest}"
                        fi
                        die "cannot reconcile fallback rollback backup for ${dest}"
                    fi
                else
                    cp -a --no-dereference -- \
                        "${dest_path}" "${INSTALL_BACKUPS[index]}" \
                        || command_status=$?
                    if ! install_backup_copy_matches "${dest_path}" \
                        "${original_identity}" \
                        "${INSTALL_BACKUPS[index]}"; then
                        if [[ -e "${INSTALL_BACKUPS[index]}" \
                            || -L "${INSTALL_BACKUPS[index]}" ]] \
                            && ! path_matches_identity \
                                "${INSTALL_BACKUPS[index]}" \
                                "${fallback_identity}"; then
                            INSTALL_BACKUP_IDENTITIES[index]=""
                        fi
                        if ((command_status != 0)); then
                            die "cannot copy fallback rollback backup for ${dest}"
                        fi
                        die "cannot reconcile fallback rollback backup for ${dest}"
                    fi
                fi
                # Exact copy semantics are authoritative even when a wrapper
                # reports a failure after completing the operation.
            fi
            backup_identity="$(stat -c '%d:%i' -- \
                "${INSTALL_BACKUPS[index]}")" \
                || die "cannot identify rollback backup for ${dest}"
            INSTALL_BACKUP_IDENTITIES[index]="${backup_identity}"
            if [[ -f "${INSTALL_BACKUPS[index]}" \
                && ! -L "${INSTALL_BACKUPS[index]}" ]]; then
                retain_regular_install_backup_fd "${index}" \
                    "${INSTALL_BACKUPS[index]}" "${backup_identity}" \
                    "${dest}" "${fallback_fd:-}"
                fallback_fd=""
            elif [[ -L "${INSTALL_BACKUPS[index]}" ]]; then
                retain_symlink_install_backup_pin "${index}" \
                    "${INSTALL_BACKUPS[index]}" "${backup_identity}" \
                    "${dest}" "${dest_path}" "${original_identity}"
            else
                INSTALL_BACKUP_IDENTITIES[index]=""
                die "rollback backup is not a regular file or symlink for ${dest}"
            fi
            logical_install_parent_matches "${index}" \
                || die "install destination directory changed while backing up: ${directory}"
            path_matches_identity "${dest_path}" "${original_identity}" \
                || die "install target changed while backing up: ${dest}"
        else
            INSTALL_ORIGINAL_PRESENT[index]=0
            INSTALL_ORIGINAL_IDENTITIES[index]=""
        fi
    done
}

publish_install_plan() {
    local index temp temp_display dest dest_path staged_identity command_status
    local backup backup_identity source_released
    if ((DRY_RUN == 0)); then
        prepare_install_backups
        PUBLISH_IN_PROGRESS=1
    fi
    for index in "${!INSTALL_TEMPS[@]}"; do
        temp="${INSTALL_TEMPS[index]}"
        dest="${INSTALL_DESTS[index]}"
        if ((DRY_RUN == 1)); then
            print_command mv -fT -- "${temp}" "${dest}"
            continue
        fi
        temp="$(bound_install_temp_path "${index}")"
        temp_display="$(bound_install_entry_display "${index}" \
            "${INSTALL_TEMP_BASENAMES[index]}")"
        dest_path="$(bound_install_dest_path "${index}")"
        staged_identity="${INSTALL_STAGED_IDENTITIES[index]}"
        logical_install_parent_matches "${index}" \
            || die "install destination directory changed before publish: ${dest%/*}"
        staged_install_artifact_matches "${temp}" "${staged_identity}" \
            || die "install temporary changed before publish: ${temp_display}"
        if (( ${INSTALL_ORIGINAL_PRESENT[index]:-0} == 1 )); then
            path_matches_identity "${dest_path}" \
                "${INSTALL_ORIGINAL_IDENTITIES[index]}" \
                || die "install target changed after backup: ${dest}"
        else
            [[ ! -e "${dest_path}" && ! -L "${dest_path}" ]] \
                || die "install target appeared after backup: ${dest}"
        fi
        print_command mv -fT -- "${temp_display}" "${dest}"
        PUBLISH_LAST_ATTEMPT="${index}"
        command_status=0
        mv -fT -- "${temp}" "${dest_path}" || command_status=$?
        source_released=0
        if [[ ! -e "${temp}" && ! -L "${temp}" ]]; then
            source_released=1
        elif ! path_matches_identity "${temp}" "${staged_identity}"; then
            # The expected source inode left this name, so a completed rename
            # remains complete even if an unrelated entry was inserted before
            # the external command returned. Revoke ownership of that name;
            # neither EXIT cleanup nor rollback may touch the replacement.
            printf 'frost install: warning: install temporary name changed after publish; replacement retained at %s\n' \
                "${temp_display}" >&2
            INSTALL_TEMPS[index]=""
            source_released=1
        fi
        if ! path_matches_identity "${dest_path}" "${staged_identity}" \
            || ((source_released == 0)); then
            if ((command_status != 0)); then
                die "cannot atomically replace ${dest}"
            fi
            die "cannot reconcile published install target ${dest}"
        fi
        # A non-zero wrapper status after the exact staged inode reached the
        # destination and left its recorded source name is a completed
        # publication. A different inode at that source name is not ours.
        logical_install_parent_matches "${index}" \
            || die "install destination directory changed during publish: ${dest%/*}"
        if (( ${INSTALL_ORIGINAL_PRESENT[index]:-0} == 1 )); then
            backup="${INSTALL_BACKUPS[index]:-}"
            backup_identity="${INSTALL_BACKUP_IDENTITIES[index]:-}"
            if [[ -z "${backup}" \
                || ( ! -e "${backup}" && ! -L "${backup}" ) ]] \
                || [[ -z "${backup_identity}" ]] \
                || ! install_backup_path_matches_identity "${index}" \
                    "${backup}" "${backup_identity}"; then
                # Publication no longer needs this snapshot to be considered
                # complete. Mark a missing/replaced backup unowned so a later
                # rollback can report degradation without moving or unlinking
                # an entry that this transaction did not create.
                INSTALL_BACKUP_IDENTITIES[index]=""
            fi
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
    local source="$1" dest="$2" exec_path exec_value try_exec_value
    local desktop_dir temp index staged_identity staged_fd fd_path
    local actual_mode command_status
    exec_path="$(desktop_exec_path)"
    validate_desktop_exec_path "${exec_path}"
    exec_value="$(desktop_exec_value "${exec_path}")"
    try_exec_value="$(desktop_try_exec_value "${exec_path}")"
    printf '  install -Dm0644 (Exec=%s) %q %q\n' \
        "${exec_path}" "${source}" "${dest}.<temporary>"
    index="${#INSTALL_DESTS[@]}"
    INSTALL_DESTS[index]="${dest}"
    INSTALL_PARENT_FDS[index]=""
    INSTALL_PARENT_IDENTITIES[index]=""
    INSTALL_DEST_BASENAMES[index]="${dest##*/}"
    INSTALL_TEMP_BASENAMES[index]=""
    INSTALL_STAGED_IDENTITIES[index]=""
    if ((DRY_RUN == 1)); then
        INSTALL_TEMPS[index]="${dest}.<temporary>"
        return 0
    fi
    desktop_dir="${dest%/*}"
    install -d -m 0755 "${desktop_dir}" \
        || die "cannot create desktop-entry directory for ${dest}"
    bind_install_destination "${index}" "${dest}"
    temp="$(mktemp "$(bound_install_entry_path "${index}" \
        ".${APP_ID}.desktop.install.XXXXXX")")" \
        || die "cannot create temporary desktop entry beside ${dest}"
    INSTALL_TEMP_BASENAMES[index]="${temp##*/}"
    INSTALL_TEMPS[index]="$(bound_install_temp_path "${index}")"
    staged_identity="$(stat -c '%d:%i' -- "${INSTALL_TEMPS[index]}")" \
        || die "cannot identify temporary desktop entry for ${dest}"
    INSTALL_STAGED_IDENTITIES[index]="${staged_identity}"
    if ! staged_install_artifact_matches "${INSTALL_TEMPS[index]}" \
        "${staged_identity}"; then
        INSTALL_STAGED_IDENTITIES[index]=""
        die "temporary desktop entry is not a private regular file for ${dest}"
    fi
    logical_install_parent_matches "${index}" \
        || die "desktop-entry directory changed while staging: ${desktop_dir}"
    unset staged_fd
    exec {staged_fd}<>"${INSTALL_TEMPS[index]}" \
        || die "cannot open temporary desktop entry for ${dest}"
    fd_path="/proc/self/fd/${staged_fd}"
    if ! opened_staged_install_artifact_matches "${fd_path}" \
        "${staged_identity}" \
        || ! staged_install_artifact_matches "${INSTALL_TEMPS[index]}" \
            "${staged_identity}"; then
        INSTALL_STAGED_IDENTITIES[index]=""
        exec {staged_fd}>&-
        die "temporary desktop entry changed while opening ${dest}"
    fi
    command_status=0
    FROST_DESKTOP_EXEC_VALUE="${exec_value}" \
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
    ' "${source}" >"${fd_path}" || command_status=$?
    if ! opened_staged_install_artifact_matches "${fd_path}" \
        "${staged_identity}" \
        || ! staged_install_artifact_matches "${INSTALL_TEMPS[index]}" \
            "${staged_identity}"; then
        INSTALL_STAGED_IDENTITIES[index]=""
        exec {staged_fd}>&-
        die "temporary desktop entry changed while staging ${dest}"
    fi
    if ((command_status != 0)); then
        exec {staged_fd}>&-
        die "cannot stage desktop entry for ${dest}"
    fi
    command_status=0
    chmod 0644 "${fd_path}" || command_status=$?
    if ! opened_staged_install_artifact_matches "${fd_path}" \
        "${staged_identity}" \
        || ! staged_install_artifact_matches "${INSTALL_TEMPS[index]}" \
            "${staged_identity}"; then
        INSTALL_STAGED_IDENTITIES[index]=""
        exec {staged_fd}>&-
        die "temporary desktop entry changed while setting mode for ${dest}"
    fi
    actual_mode="$(stat -Lc '%a' -- "${fd_path}")" \
        || die "cannot identify temporary desktop-entry mode for ${dest}"
    if [[ "${actual_mode}" != 644 ]]; then
        exec {staged_fd}>&-
        if ((command_status != 0)); then
            die "cannot set desktop-entry mode for ${dest}"
        fi
        die "cannot reconcile desktop-entry mode for ${dest}"
    fi
    exec {staged_fd}>&-
    logical_install_parent_matches "${index}" \
        || die "desktop-entry directory changed while staging: ${desktop_dir}"
}

prepare_post_install_plan() {
    local bind_status
    POST_INSTALL_APP_DIR="${SHARE_DIR}/applications"
    POST_INSTALL_APP_FD=""
    POST_INSTALL_APP_IDENTITY=""
    POST_INSTALL_APP_BIND_ERROR=""
    POST_INSTALL_ICON_DIR="${SHARE_DIR}/icons/hicolor"
    POST_INSTALL_ICON_FD=""
    POST_INSTALL_ICON_IDENTITY=""
    POST_INSTALL_ICON_BIND_ERROR=""
    ((DRY_RUN == 0 && INSTALL_DESKTOP == 1)) || return 0

    if [[ ! -d /proc/self/fd ]]; then
        POST_INSTALL_APP_BIND_ERROR="/proc/self/fd is unavailable"
        if ((DESTDIR_ACTIVE == 0)); then
            POST_INSTALL_ICON_BIND_ERROR="/proc/self/fd is unavailable"
        fi
        return 0
    fi
    if [[ ! -d "${POST_INSTALL_APP_DIR}" \
        || -L "${POST_INSTALL_APP_DIR}" ]]; then
        POST_INSTALL_APP_BIND_ERROR="post-install applications path is not a real directory: ${POST_INSTALL_APP_DIR}"
    elif acquire_install_bound_directory "${POST_INSTALL_APP_DIR}" \
        POST_INSTALL_APP_FD POST_INSTALL_APP_IDENTITY; then
        if ! real_directory_matches_identity "${POST_INSTALL_APP_DIR}" \
            "${POST_INSTALL_APP_IDENTITY}"; then
            POST_INSTALL_APP_BIND_ERROR="post-install applications directory changed while binding: ${POST_INSTALL_APP_DIR}"
            POST_INSTALL_APP_FD=""
        fi
    else
        bind_status=$?
        if ((bind_status == 2)); then
            POST_INSTALL_APP_BIND_ERROR="post-install directory fd limit reached (limit ${MAX_INSTALL_BOUND_DIRECTORY_FDS})"
        else
            POST_INSTALL_APP_BIND_ERROR="cannot bind post-install applications directory: ${POST_INSTALL_APP_DIR}"
        fi
    fi

    ((DESTDIR_ACTIVE == 0)) || return 0
    if [[ ! -d "${POST_INSTALL_ICON_DIR}" \
        || -L "${POST_INSTALL_ICON_DIR}" ]]; then
        POST_INSTALL_ICON_BIND_ERROR="post-install icon path is not a real directory: ${POST_INSTALL_ICON_DIR}"
    elif acquire_install_bound_directory "${POST_INSTALL_ICON_DIR}" \
        POST_INSTALL_ICON_FD POST_INSTALL_ICON_IDENTITY; then
        if ! real_directory_matches_identity "${POST_INSTALL_ICON_DIR}" \
            "${POST_INSTALL_ICON_IDENTITY}"; then
            POST_INSTALL_ICON_BIND_ERROR="post-install icon directory changed while binding: ${POST_INSTALL_ICON_DIR}"
            POST_INSTALL_ICON_FD=""
        fi
    else
        bind_status=$?
        if ((bind_status == 2)); then
            POST_INSTALL_ICON_BIND_ERROR="post-install directory fd limit reached (limit ${MAX_INSTALL_BOUND_DIRECTORY_FDS})"
        else
            POST_INSTALL_ICON_BIND_ERROR="cannot bind post-install icon directory: ${POST_INSTALL_ICON_DIR}"
        fi
    fi
}

post_install_app_directory_matches() {
    [[ -n "${POST_INSTALL_APP_FD}" ]] \
        && real_directory_matches_identity "${POST_INSTALL_APP_DIR}" \
            "${POST_INSTALL_APP_IDENTITY}"
}

post_install_icon_directory_matches() {
    [[ -n "${POST_INSTALL_ICON_FD}" ]] \
        && real_directory_matches_identity "${POST_INSTALL_ICON_DIR}" \
            "${POST_INSTALL_ICON_IDENTITY}"
}

remove_legacy_desktop_entry() {
    local path="${SHARE_DIR}/applications/io.github.beamiter.jterm3.desktop"
    local bound validation_error
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
    if [[ -n "${POST_INSTALL_APP_BIND_ERROR}" ]]; then
        printf 'frost install: warning: skipped legacy launcher cleanup (non-fatal): %s\n' \
            "${POST_INSTALL_APP_BIND_ERROR}" >&2
        return 0
    fi
    if ! post_install_app_directory_matches; then
        printf 'frost install: warning: skipped legacy launcher cleanup because applications directory identity changed (non-fatal): %s\n' \
            "${POST_INSTALL_APP_DIR}" >&2
        return 0
    fi
    bound="/proc/self/fd/${POST_INSTALL_APP_FD}/${path##*/}"
    if [[ -e "${bound}" || -L "${bound}" ]] \
        && [[ ! -f "${bound}" && ! -L "${bound}" ]]; then
        printf 'frost install: warning: skipped legacy launcher cleanup because target is not a regular file or symlink (non-fatal): %s\n' \
            "${path}" >&2
        return 0
    fi
    if ! rm -f -- "${bound}"; then
        printf 'frost install: warning: could not remove legacy launcher (non-fatal): %s\n' \
            "${path}" >&2
    fi
    if ! post_install_app_directory_matches; then
        printf 'frost install: warning: applications directory changed during bound legacy cleanup (non-fatal): %s\n' \
            "${POST_INSTALL_APP_DIR}" >&2
    fi
}

# Freshly installed entries and icons stay invisible until the shell's caches
# are rebuilt; a stale icon cache can even shadow icons that are already there.
refresh_desktop_caches() {
    local logical bound
    if ((DESTDIR_ACTIVE == 1)); then
        printf 'Staged install (DESTDIR set); skipping desktop cache refresh.\n'
        return 0
    fi
    if ((DRY_RUN == 1)); then
        if command -v desktop-file-validate >/dev/null 2>&1; then
            run_optional desktop-file-validate \
                "${SHARE_DIR}/applications/${APP_ID}.desktop"
        fi
        if command -v update-desktop-database >/dev/null 2>&1; then
            run_optional_public update-desktop-database \
                "${SHARE_DIR}/applications"
        fi
        if command -v gtk-update-icon-cache >/dev/null 2>&1; then
            run_optional_public gtk-update-icon-cache --force \
                --ignore-theme-index --quiet "${SHARE_DIR}/icons/hicolor"
        fi
        return 0
    fi
    if command -v desktop-file-validate >/dev/null 2>&1; then
        logical="${SHARE_DIR}/applications/${APP_ID}.desktop"
        print_command desktop-file-validate "${logical}"
        if [[ -n "${POST_INSTALL_APP_BIND_ERROR}" ]]; then
            printf 'frost install: warning: skipped optional desktop-file-validate: %s (non-fatal)\n' \
                "${POST_INSTALL_APP_BIND_ERROR}" >&2
        elif ! post_install_app_directory_matches; then
            printf 'frost install: warning: skipped optional desktop-file-validate because applications directory identity changed (non-fatal): %s\n' \
                "${POST_INSTALL_APP_DIR}" >&2
        else
            bound="/proc/self/fd/${POST_INSTALL_APP_FD}/${APP_ID}.desktop"
            if [[ ! -f "${bound}" || -L "${bound}" ]]; then
                printf 'frost install: warning: skipped optional desktop-file-validate because launcher identity/type changed (non-fatal): %s\n' \
                    "${logical}" >&2
            elif ! desktop-file-validate "${bound}"; then
                printf 'frost install: warning: desktop-file-validate failed (non-fatal)\n' >&2
            fi
            if ! post_install_app_directory_matches; then
                printf 'frost install: warning: applications directory changed during bound desktop-file-validate (non-fatal): %s\n' \
                    "${POST_INSTALL_APP_DIR}" >&2
            fi
        fi
    fi
    if command -v update-desktop-database >/dev/null 2>&1; then
        print_command update-desktop-database "${POST_INSTALL_APP_DIR}"
        if [[ -n "${POST_INSTALL_APP_BIND_ERROR}" ]]; then
            printf 'frost install: warning: skipped optional update-desktop-database: %s (non-fatal)\n' \
                "${POST_INSTALL_APP_BIND_ERROR}" >&2
        elif ! post_install_app_directory_matches; then
            printf 'frost install: warning: skipped optional update-desktop-database because applications directory identity changed (non-fatal): %s\n' \
                "${POST_INSTALL_APP_DIR}" >&2
        else
            bound="/proc/self/fd/${POST_INSTALL_APP_FD}"
            (umask 022 && update-desktop-database "${bound}") \
                || printf 'frost install: warning: update-desktop-database failed (non-fatal)\n' >&2
            if ! post_install_app_directory_matches; then
                printf 'frost install: warning: applications directory changed during bound update-desktop-database (non-fatal): %s\n' \
                    "${POST_INSTALL_APP_DIR}" >&2
            fi
        fi
    fi
    if command -v gtk-update-icon-cache >/dev/null 2>&1; then
        print_command gtk-update-icon-cache --force --ignore-theme-index --quiet \
            "${POST_INSTALL_ICON_DIR}"
        if [[ -n "${POST_INSTALL_ICON_BIND_ERROR}" ]]; then
            printf 'frost install: warning: skipped optional gtk-update-icon-cache: %s (non-fatal)\n' \
                "${POST_INSTALL_ICON_BIND_ERROR}" >&2
        elif ! post_install_icon_directory_matches; then
            printf 'frost install: warning: skipped optional gtk-update-icon-cache because icon directory identity changed (non-fatal): %s\n' \
                "${POST_INSTALL_ICON_DIR}" >&2
        else
            bound="/proc/self/fd/${POST_INSTALL_ICON_FD}"
            (umask 022 && gtk-update-icon-cache --force \
                --ignore-theme-index --quiet "${bound}") \
                || printf 'frost install: warning: gtk-update-icon-cache failed (non-fatal)\n' >&2
            if ! post_install_icon_directory_matches; then
                printf 'frost install: warning: icon directory changed during bound gtk-update-icon-cache (non-fatal): %s\n' \
                    "${POST_INSTALL_ICON_DIR}" >&2
            fi
        fi
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
require_command readlink
require_command cmp
require_command cat
require_command chmod
if ((INSTALL_DESKTOP == 1)); then
    require_command awk
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
    PREBUILT_FD=""
fi
prepare_post_install_plan
publish_install_plan

if ((INSTALL_DESKTOP == 1)); then
    # Launcher left by installs from before the jterm3 -> frost rename; left in
    # place it shows up as a second "jterm3" entry beside the new one.
    remove_legacy_desktop_entry
    refresh_desktop_caches
fi
close_install_bound_directory_fds

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
