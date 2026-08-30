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
REMOVAL_PARENT_FDS=()
REMOVAL_PARENT_IDENTITIES=()
REMOVAL_TARGET_BASENAMES=()
REMOVAL_QUARANTINE_BASENAMES=()
REMOVAL_ORIGINAL_PRESENT=()
REMOVAL_ORIGINAL_IDENTITIES=()
REMOVAL_STAGED=()
CLEANUP_PARENT_FDS=()
CLEANUP_PARENT_IDENTITIES=()
CLEANUP_TARGET_BASENAMES=()
CLEANUP_TARGET_IDENTITIES=()
CLEANUP_ORIGINAL_PRESENT=()
CLEANUP_BIND_ERRORS=()
CACHE_REFRESH_DIRS=()
CACHE_REFRESH_LABELS=()
CACHE_REFRESH_COMMANDS=()
CACHE_REFRESH_FDS=()
CACHE_REFRESH_IDENTITIES=()
CACHE_REFRESH_NEEDED=()
CACHE_REFRESH_ENABLED=()
CACHE_REFRESH_BIND_ERRORS=()
BOUND_DIRECTORY_FDS=()
BOUND_DIRECTORY_IDENTITIES=()
MAX_BOUND_DIRECTORY_FDS=16
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

real_directory_matches_identity() {
    local path="$1" expected="$2"
    [[ -d "${path}" && ! -L "${path}" ]] \
        && path_matches_identity "${path}" "${expected}"
}

# Callers keep their own logical-path identity checks, while this pool owns one
# descriptor per physical directory inode. Return 2 when its explicit resource
# ceiling is exhausted so critical and optional callers can choose policy.
acquire_bound_directory() {
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
    for index in "${!BOUND_DIRECTORY_IDENTITIES[@]}"; do
        if [[ "${BOUND_DIRECTORY_IDENTITIES[index]}" == \
            "${directory_identity}" ]]; then
            existing_fd="${BOUND_DIRECTORY_FDS[index]}"
            exec {candidate_fd}<&-
            printf -v "${result_fd_name}" '%s' "${existing_fd}"
            printf -v "${result_identity_name}" '%s' "${directory_identity}"
            return 0
        fi
    done
    if ((${#BOUND_DIRECTORY_FDS[@]} >= MAX_BOUND_DIRECTORY_FDS)); then
        exec {candidate_fd}<&-
        return 2
    fi
    BOUND_DIRECTORY_FDS+=("${candidate_fd}")
    BOUND_DIRECTORY_IDENTITIES+=("${directory_identity}")
    printf -v "${result_fd_name}" '%s' "${candidate_fd}"
    printf -v "${result_identity_name}" '%s' "${directory_identity}"
}

close_bound_directory_fds() {
    local index fd
    for index in "${!BOUND_DIRECTORY_FDS[@]}"; do
        fd="${BOUND_DIRECTORY_FDS[index]:-}"
        [[ -n "${fd}" ]] || continue
        exec {fd}<&-
    done
    BOUND_DIRECTORY_FDS=()
    BOUND_DIRECTORY_IDENTITIES=()
}

bound_entry_path() {
    local index="$1" basename="$2"
    printf '/proc/self/fd/%s/%s' "${REMOVAL_PARENT_FDS[index]}" "${basename}"
}

bound_target_path() {
    local index="$1"
    bound_entry_path "${index}" "${REMOVAL_TARGET_BASENAMES[index]}"
}

bound_quarantine_path() {
    local index="$1"
    bound_entry_path "${index}" "${REMOVAL_QUARANTINE_BASENAMES[index]}"
}

bound_entry_display() {
    local index="$1" basename="$2" parent
    parent="$(readlink -- "/proc/$$/fd/${REMOVAL_PARENT_FDS[index]}" 2>/dev/null)" \
        || parent="${REMOVAL_FILES[index]%/*}"
    printf '%s/%s' "${parent}" "${basename}"
}

bound_target_display() {
    local index="$1"
    bound_entry_display "${index}" "${REMOVAL_TARGET_BASENAMES[index]}"
}

bound_quarantine_display() {
    local index="$1"
    bound_entry_display "${index}" "${REMOVAL_QUARANTINE_BASENAMES[index]}"
}

logical_parent_matches_identity() {
    local index="$1" parent
    parent="${REMOVAL_FILES[index]%/*}"
    real_directory_matches_identity "${parent}" \
        "${REMOVAL_PARENT_IDENTITIES[index]}"
}

bound_cleanup_path() {
    local index="$1"
    printf '/proc/self/fd/%s/%s' "${CLEANUP_PARENT_FDS[index]}" \
        "${CLEANUP_TARGET_BASENAMES[index]}"
}

logical_cleanup_parent_matches_identity() {
    local index="$1" parent
    parent="${REMOVAL_DIRS[index]%/*}"
    real_directory_matches_identity "${parent}" \
        "${CLEANUP_PARENT_IDENTITIES[index]}"
}

print_uninstall_recovery() {
    local quarantine="$1" target="$2"
    printf 'frost uninstall: recovery after inspecting destination: mv -fT -- %q %q\n' \
        "${quarantine}" "${target}" >&2
}

cleanup_uninstall_reservations() {
    local index quarantine quarantine_path quarantine_display reservation_identity
    for index in "${!REMOVAL_QUARANTINES[@]}"; do
        quarantine="${REMOVAL_QUARANTINES[index]:-}"
        [[ -n "${quarantine}" ]] || continue
        quarantine_path="$(bound_quarantine_path "${index}")"
        quarantine_display="$(bound_quarantine_display "${index}")"
        # A staged entry still contains the removed original. Never discard it
        # after a failed rollback; only empty, unused reservations are cleanup.
        if (( ${REMOVAL_STAGED[index]:-0} == 0 )) \
            && [[ -e "${quarantine_path}" || -L "${quarantine_path}" ]]; then
            reservation_identity="${REMOVAL_RESERVATION_IDENTITIES[index]:-}"
            if [[ -z "${reservation_identity}" ]] \
                || ! path_matches_identity "${quarantine_path}" \
                    "${reservation_identity}"; then
                printf 'frost uninstall: warning: refusing to remove changed unused quarantine %s\n' \
                    "${quarantine_display}" >&2
            elif rm -f -- "${quarantine_path}"; then
                REMOVAL_QUARANTINES[index]=""
            else
                printf 'frost uninstall: warning: cannot remove unused quarantine %s\n' \
                    "${quarantine_display}" >&2
            fi
        fi
    done
}

reconcile_uninstall_attempt() {
    local index="$1" target quarantine target_path quarantine_path identity
    target="${REMOVAL_FILES[index]}"
    quarantine="${REMOVAL_QUARANTINES[index]:-}"
    target_path="$(bound_target_path "${index}")"
    quarantine_path="$(bound_quarantine_path "${index}")"
    identity="${REMOVAL_ORIGINAL_IDENTITIES[index]:-}"
    # State 2 means a signal may have interrupted the tiny interval between mv
    # and its bookkeeping. Identity determines which name still owns the exact
    # preflight inode without ever treating the empty reservation as a backup.
    if path_matches_identity "${target_path}" "${identity}"; then
        REMOVAL_STAGED[index]=0
        return 0
    fi
    if [[ -n "${quarantine}" ]] \
        && path_matches_identity "${quarantine_path}" "${identity}"; then
        REMOVAL_STAGED[index]=1
        return 0
    fi
    REMOVAL_STAGED[index]=0
    printf 'frost uninstall: cannot reconcile interrupted quarantine rename for %s\n' \
        "${target}" >&2
    return 1
}

rollback_uninstall_plan() {
    local index target quarantine target_path quarantine_path target_display
    local quarantine_display identity state rollback_failed=0
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
        target_path="$(bound_target_path "${index}")"
        quarantine_path="$(bound_quarantine_path "${index}")"
        target_display="$(bound_target_display "${index}")"
        quarantine_display="$(bound_quarantine_display "${index}")"
        identity="${REMOVAL_ORIGINAL_IDENTITIES[index]:-}"
        if [[ -z "${quarantine}" \
            || ( ! -e "${quarantine_path}" && ! -L "${quarantine_path}" ) ]]; then
            printf 'frost uninstall: rollback quarantine missing for %s\n' \
                "${target}" >&2
            rollback_failed=1
            continue
        fi
        if ! path_matches_identity "${quarantine_path}" "${identity}"; then
            printf 'frost uninstall: rollback refused changed quarantine for %s; unexpected entry retained at %s\n' \
                "${target}" "${quarantine_display}" >&2
            rollback_failed=1
            continue
        fi
        if [[ -e "${target_path}" || -L "${target_path}" ]]; then
            printf 'frost uninstall: rollback refused to overwrite reappeared target %s; quarantine retained at %s\n' \
                "${target_display}" "${quarantine_display}" >&2
            print_uninstall_recovery "${quarantine_display}" "${target_display}"
            rollback_failed=1
            continue
        fi
        if mv -fT -- "${quarantine_path}" "${target_path}"; then
            REMOVAL_QUARANTINES[index]=""
            REMOVAL_STAGED[index]=0
        elif path_matches_identity "${target_path}" "${identity}"; then
            # Treat a wrapper's post-rename failure by observed state, not its
            # exit status. Never claim a quarantine is retained when restore
            # already put the exact original inode back at its target.
            REMOVAL_STAGED[index]=0
            if [[ ! -e "${quarantine_path}" && ! -L "${quarantine_path}" ]]; then
                REMOVAL_QUARANTINES[index]=""
            else
                quarantine_display="$(bound_quarantine_display "${index}")"
                printf 'frost uninstall: warning: rollback restored %s but a changed quarantine entry remains at %s\n' \
                    "${target_display}" "${quarantine_display}" >&2
            fi
        else
            target_display="$(bound_target_display "${index}")"
            quarantine_display="$(bound_quarantine_display "${index}")"
            printf 'frost uninstall: rollback failed for %s; quarantine retained at %s\n' \
                "${target_display}" "${quarantine_display}" >&2
            print_uninstall_recovery "${quarantine_display}" "${target_display}"
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
    close_bound_directory_fds
    exit "${status}"
}

prepare_uninstall_plan() {
    local index target directory basename quarantine quarantine_path identity
    local reservation_identity parent_fd parent_identity bind_status
    REMOVAL_QUARANTINES=()
    REMOVAL_RESERVATION_IDENTITIES=()
    REMOVAL_PARENT_FDS=()
    REMOVAL_PARENT_IDENTITIES=()
    REMOVAL_TARGET_BASENAMES=()
    REMOVAL_QUARANTINE_BASENAMES=()
    REMOVAL_ORIGINAL_PRESENT=()
    REMOVAL_ORIGINAL_IDENTITIES=()
    REMOVAL_STAGED=()
    for index in "${!REMOVAL_FILES[@]}"; do
        target="${REMOVAL_FILES[index]}"
        REMOVAL_STAGED[index]=0
        REMOVAL_TARGET_BASENAMES[index]="${target##*/}"
        if [[ -e "${target}" || -L "${target}" ]]; then
            validate_removal_file_target "${target}"
            identity="$(stat -c '%d:%i' -- "${target}")" \
                || die "cannot identify uninstall target ${target}"
            directory="${target%/*}"
            basename="${target##*/}"
            if acquire_bound_directory "${directory}" parent_fd \
                parent_identity; then
                :
            else
                bind_status=$?
                if ((bind_status == 2)); then
                    die "too many distinct uninstall directories (limit ${MAX_BOUND_DIRECTORY_FDS})"
                fi
                die "cannot bind uninstall target directory ${directory}"
            fi
            REMOVAL_PARENT_FDS[index]="${parent_fd}"
            REMOVAL_PARENT_IDENTITIES[index]="${parent_identity}"
            validate_removal_file_target "${target}"
            logical_parent_matches_identity "${index}" \
                || die "uninstall target directory changed after preflight: ${directory}"
            path_matches_identity "$(bound_target_path "${index}")" \
                "${identity}" \
                || die "uninstall target changed while binding directory: ${target}"
            quarantine_path="$(mktemp \
                "/proc/self/fd/${parent_fd}/.${basename}.uninstall.XXXXXX")" \
                || die "cannot reserve uninstall quarantine beside ${target}"
            REMOVAL_QUARANTINE_BASENAMES[index]="${quarantine_path##*/}"
            quarantine="${directory}/${REMOVAL_QUARANTINE_BASENAMES[index]}"
            REMOVAL_QUARANTINES[index]="${quarantine}"
            reservation_identity="$(stat -c '%d:%i' -- \
                "$(bound_quarantine_path "${index}")")" \
                || die "cannot identify uninstall quarantine ${quarantine}"
            REMOVAL_RESERVATION_IDENTITIES[index]="${reservation_identity}"
            logical_parent_matches_identity "${index}" \
                || die "uninstall target directory changed while reserving quarantine: ${directory}"
            validate_removal_file_target "${target}"
            path_matches_identity "$(bound_target_path "${index}")" \
                "${identity}" \
                || die "uninstall target changed while reserving quarantine: ${target}"
            REMOVAL_ORIGINAL_PRESENT[index]=1
            REMOVAL_ORIGINAL_IDENTITIES[index]="${identity}"
        else
            REMOVAL_QUARANTINES[index]=""
            REMOVAL_RESERVATION_IDENTITIES[index]=""
            REMOVAL_PARENT_FDS[index]=""
            REMOVAL_PARENT_IDENTITIES[index]=""
            REMOVAL_QUARANTINE_BASENAMES[index]=""
            REMOVAL_ORIGINAL_PRESENT[index]=0
            REMOVAL_ORIGINAL_IDENTITIES[index]=""
        fi
    done
}

prepare_cleanup_plan() {
    local index target parent target_identity parent_fd parent_identity
    local bind_status
    CLEANUP_PARENT_FDS=()
    CLEANUP_PARENT_IDENTITIES=()
    CLEANUP_TARGET_BASENAMES=()
    CLEANUP_TARGET_IDENTITIES=()
    CLEANUP_ORIGINAL_PRESENT=()
    CLEANUP_BIND_ERRORS=()
    for index in "${!REMOVAL_DIRS[@]}"; do
        target="${REMOVAL_DIRS[index]}"
        parent="${target%/*}"
        CLEANUP_TARGET_BASENAMES[index]="${target##*/}"
        CLEANUP_PARENT_FDS[index]=""
        CLEANUP_PARENT_IDENTITIES[index]=""
        CLEANUP_TARGET_IDENTITIES[index]=""
        CLEANUP_BIND_ERRORS[index]=""
        if [[ -d "${target}" && ! -L "${target}" ]]; then
            CLEANUP_ORIGINAL_PRESENT[index]=1
            if ! target_identity="$(stat -c '%d:%i' -- "${target}")"; then
                CLEANUP_BIND_ERRORS[index]="cannot identify cleanup directory ${target}"
                continue
            fi
            if acquire_bound_directory "${parent}" parent_fd \
                parent_identity; then
                :
            else
                bind_status=$?
                if ((bind_status == 2)); then
                    CLEANUP_BIND_ERRORS[index]="bound directory fd limit reached while binding cleanup parent ${parent}"
                else
                    CLEANUP_BIND_ERRORS[index]="cannot bind cleanup parent directory ${parent}"
                fi
                continue
            fi
            CLEANUP_PARENT_FDS[index]="${parent_fd}"
            CLEANUP_PARENT_IDENTITIES[index]="${parent_identity}"
            CLEANUP_TARGET_IDENTITIES[index]="${target_identity}"
            if ! logical_cleanup_parent_matches_identity "${index}" \
                || ! path_matches_identity "${target}" "${target_identity}" \
                || ! path_matches_identity "$(bound_cleanup_path "${index}")" \
                    "${target_identity}"; then
                CLEANUP_BIND_ERRORS[index]="cleanup directory changed while binding ${target}"
                CLEANUP_PARENT_FDS[index]=""
            fi
        elif [[ -e "${target}" || -L "${target}" ]]; then
            # The complete preflight accepted a directory, so a different
            # object here is a race. Cleanup is optional; retain it untouched.
            CLEANUP_ORIGINAL_PRESENT[index]=1
            CLEANUP_BIND_ERRORS[index]="cleanup directory changed before binding ${target}"
        else
            CLEANUP_ORIGINAL_PRESENT[index]=0
        fi
    done
}

prepare_cache_refresh_plan() {
    local index removal_index path command fd identity bind_status
    CACHE_REFRESH_DIRS=(
        "${SHARE_DIR}/applications"
        "${SHARE_DIR}/icons/hicolor"
    )
    CACHE_REFRESH_LABELS=("desktop database" "icon cache")
    CACHE_REFRESH_COMMANDS=(update-desktop-database gtk-update-icon-cache)
    CACHE_REFRESH_FDS=("" "")
    CACHE_REFRESH_IDENTITIES=("" "")
    CACHE_REFRESH_NEEDED=(0 0)
    CACHE_REFRESH_ENABLED=(0 0)
    CACHE_REFRESH_BIND_ERRORS=("" "")

    ((DESTDIR_ACTIVE == 0)) || return 0
    for removal_index in "${!REMOVAL_FILES[@]}"; do
        (( ${REMOVAL_ORIGINAL_PRESENT[removal_index]:-0} == 1 )) \
            || continue
        case "${REMOVAL_FILES[removal_index]}" in
            "${CACHE_REFRESH_DIRS[0]}/"*) CACHE_REFRESH_NEEDED[0]=1 ;;
            "${CACHE_REFRESH_DIRS[1]}/"*) CACHE_REFRESH_NEEDED[1]=1 ;;
        esac
    done

    for index in "${!CACHE_REFRESH_DIRS[@]}"; do
        ((CACHE_REFRESH_NEEDED[index] == 1)) || continue
        command="${CACHE_REFRESH_COMMANDS[index]}"
        command -v "${command}" >/dev/null 2>&1 || continue
        CACHE_REFRESH_ENABLED[index]=1
        path="${CACHE_REFRESH_DIRS[index]}"
        if [[ ! -d "${path}" || -L "${path}" ]]; then
            CACHE_REFRESH_BIND_ERRORS[index]="cache directory is not a real directory: ${path}"
            continue
        fi
        if acquire_bound_directory "${path}" fd identity; then
            :
        else
            bind_status=$?
            if ((bind_status == 2)); then
                CACHE_REFRESH_BIND_ERRORS[index]="bound directory fd limit reached while binding cache directory ${path}"
            else
                CACHE_REFRESH_BIND_ERRORS[index]="cannot bind cache directory ${path}"
            fi
            continue
        fi
        CACHE_REFRESH_FDS[index]="${fd}"
        CACHE_REFRESH_IDENTITIES[index]="${identity}"
        if ! real_directory_matches_identity "${path}" "${identity}"; then
            CACHE_REFRESH_BIND_ERRORS[index]="cache directory changed while binding ${path}"
            CACHE_REFRESH_FDS[index]=""
        fi
    done
}

remove_bound_dir_if_empty() {
    local index="$1" target parent bound identity bind_error
    target="${REMOVAL_DIRS[index]}"
    parent="${target%/*}"
    if (( ${CLEANUP_ORIGINAL_PRESENT[index]:-0} == 0 )); then
        if [[ -e "${target}" || -L "${target}" ]]; then
            printf 'frost uninstall: warning: skipped post-commit directory cleanup because target appeared after preflight: %s (non-fatal)\n' \
                "${target}" >&2
        fi
        return 0
    fi
    bind_error="${CLEANUP_BIND_ERRORS[index]:-}"
    if [[ -n "${bind_error}" ]]; then
        printf 'frost uninstall: warning: skipped post-commit directory cleanup: %s (non-fatal)\n' \
            "${bind_error}" >&2
        return 0
    fi
    bound="$(bound_cleanup_path "${index}")"
    identity="${CLEANUP_TARGET_IDENTITIES[index]}"
    if ! logical_cleanup_parent_matches_identity "${index}" \
        || ! path_matches_identity "${target}" "${identity}" \
        || ! path_matches_identity "${bound}" "${identity}"; then
        printf 'frost uninstall: warning: skipped post-commit directory cleanup because identity changed after preflight: %s (non-fatal)\n' \
            "${target}" >&2
        return 0
    fi
    print_command rmdir --ignore-fail-on-non-empty -- "${target}"
    if ! rmdir --ignore-fail-on-non-empty -- "${bound}"; then
        printf 'frost uninstall: warning: could not remove empty cleanup directory %s (non-fatal)\n' \
            "${target}" >&2
    elif [[ -e "${bound}" || -L "${bound}" ]] \
        && ! path_matches_identity "${bound}" "${identity}"; then
        printf 'frost uninstall: warning: cleanup entry changed during bound rmdir: %s (non-fatal)\n' \
            "${target}" >&2
    fi
    if ! real_directory_matches_identity "${parent}" \
        "${CLEANUP_PARENT_IDENTITIES[index]}"; then
        printf 'frost uninstall: warning: cleanup parent changed during bound rmdir: %s (non-fatal)\n' \
            "${parent}" >&2
    fi
}

refresh_bound_caches() {
    local index path label command fd bound bind_error refresh_failed
    for index in "${!CACHE_REFRESH_DIRS[@]}"; do
        (( ${CACHE_REFRESH_NEEDED[index]:-0} == 1 )) || continue
        (( ${CACHE_REFRESH_ENABLED[index]:-0} == 1 )) || continue
        path="${CACHE_REFRESH_DIRS[index]}"
        label="${CACHE_REFRESH_LABELS[index]}"
        command="${CACHE_REFRESH_COMMANDS[index]}"
        bind_error="${CACHE_REFRESH_BIND_ERRORS[index]:-}"
        if [[ -n "${bind_error}" ]]; then
            printf 'frost uninstall: warning: skipped optional %s refresh: %s (non-fatal)\n' \
                "${label}" "${bind_error}" >&2
            continue
        fi
        fd="${CACHE_REFRESH_FDS[index]}"
        if ! real_directory_matches_identity "${path}" \
            "${CACHE_REFRESH_IDENTITIES[index]}"; then
            printf 'frost uninstall: warning: skipped optional %s refresh because directory identity changed: %s (non-fatal)\n' \
                "${label}" "${path}" >&2
            continue
        fi
        bound="/proc/self/fd/${fd}"
        refresh_failed=0
        if ((index == 0)); then
            (umask 022 && "${command}" "${bound}") >/dev/null 2>&1 \
                || refresh_failed=1
        else
            (umask 022 && "${command}" --force --ignore-theme-index \
                --quiet "${bound}") >/dev/null 2>&1 \
                || refresh_failed=1
        fi
        if ((refresh_failed == 1)); then
            printf 'frost uninstall: warning: optional %s refresh failed for %s (non-fatal)\n' \
                "${label}" "${path}" >&2
        fi
        if ! real_directory_matches_identity "${path}" \
            "${CACHE_REFRESH_IDENTITIES[index]}"; then
            printf 'frost uninstall: warning: directory changed during bound %s refresh: %s (non-fatal)\n' \
                "${label}" "${path}" >&2
        fi
    done
}

stage_uninstall_plan() {
    local index target quarantine target_path quarantine_path identity
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
        logical_parent_matches_identity "${index}" \
            || die "uninstall target directory changed after preflight: ${target%/*}"
        target_path="$(bound_target_path "${index}")"
        quarantine_path="$(bound_quarantine_path "${index}")"
        path_matches_identity "${target_path}" "${identity}" \
            || die "uninstall target changed after preflight: ${target}"
        quarantine="${REMOVAL_QUARANTINES[index]}"
        path_matches_identity "${quarantine_path}" \
            "${REMOVAL_RESERVATION_IDENTITIES[index]}" \
            || die "uninstall quarantine reservation changed after preflight: ${quarantine}"
        print_command mv -fT -- "${target}" "${quarantine}"
        # Mark the in-flight syscall before entering mv. EXIT reconciles state
        # 2 by inode if a catchable signal lands after rename but before the
        # success assignment below.
        REMOVAL_STAGED[index]=2
        if ! mv -fT -- "${target_path}" "${quarantine_path}"; then
            die "cannot quarantine uninstall target ${target}"
        fi
        if path_matches_identity "${quarantine_path}" "${identity}" \
            && ! path_matches_identity "${target_path}" "${identity}"; then
            REMOVAL_STAGED[index]=1
        elif path_matches_identity "${target_path}" "${identity}"; then
            REMOVAL_STAGED[index]=0
            die "quarantine rename did not remove uninstall target ${target}"
        else
            die "cannot reconcile quarantine rename for ${target}"
        fi
        validate_removal_file_target "${target}"
        logical_parent_matches_identity "${index}" \
            || die "uninstall target directory changed during quarantine: ${target%/*}"
    done
    UNINSTALL_COMMITTED=1
    UNINSTALL_IN_PROGRESS=0
}

purge_uninstall_quarantines() {
    local index target target_display quarantine quarantine_path quarantine_display
    local identity
    local validation_error
    for index in "${!REMOVAL_QUARANTINES[@]}"; do
        (( ${REMOVAL_STAGED[index]:-0} == 1 )) || continue
        target="${REMOVAL_FILES[index]}"
        quarantine="${REMOVAL_QUARANTINES[index]:-}"
        quarantine_path="$(bound_quarantine_path "${index}")"
        quarantine_display="$(bound_quarantine_display "${index}")"
        identity="${REMOVAL_ORIGINAL_IDENTITIES[index]:-}"
        if [[ -z "${quarantine}" \
            || ( ! -e "${quarantine_path}" && ! -L "${quarantine_path}" ) ]]; then
            REMOVAL_QUARANTINES[index]=""
            REMOVAL_STAGED[index]=0
            continue
        fi
        if ! validation_error="$(
            validate_staging_removal_target "${target}" 2>&1
        )" || ! logical_parent_matches_identity "${index}"; then
            target_display="$(bound_target_display "${index}")"
            printf 'frost uninstall: warning: refusing to purge through changed target directory for %s; quarantine retained at %s%s\n' \
                "${target}" "${quarantine_display}" \
                "${validation_error:+ (${validation_error})}" >&2
            print_uninstall_recovery "${quarantine_display}" "${target_display}"
            continue
        fi
        if ! path_matches_identity "${quarantine_path}" "${identity}"; then
            printf 'frost uninstall: warning: refusing to purge changed quarantine for %s; unexpected entry retained at %s\n' \
                "${target}" "${quarantine_display}" >&2
            continue
        fi
        print_command rm -f -- "${quarantine}"
        if rm -f -- "${quarantine_path}"; then
            REMOVAL_QUARANTINES[index]=""
            REMOVAL_STAGED[index]=0
            if ! logical_parent_matches_identity "${index}"; then
                printf 'frost uninstall: warning: target directory changed after bound purge of %s\n' \
                    "${target}" >&2
            fi
        else
            # The parent itself may have moved while rm ran. Re-resolve the
            # bound fd before naming either side of a recovery command.
            quarantine_display="$(bound_quarantine_display "${index}")"
            target_display="$(bound_target_display "${index}")"
            if [[ ! -e "${quarantine_path}" && ! -L "${quarantine_path}" ]]; then
                # The requested unlink completed even though a wrapper reported
                # failure. No recovery inode exists; do not print a false path.
                REMOVAL_QUARANTINES[index]=""
                REMOVAL_STAGED[index]=0
                printf 'frost uninstall: warning: purge reported failure after removing %s\n' \
                    "${target}" >&2
            elif ! path_matches_identity "${quarantine_path}" "${identity}"; then
                printf 'frost uninstall: warning: purge failed and quarantine identity changed for %s; unexpected entry retained at %s\n' \
                    "${target}" "${quarantine_display}" >&2
            else
                printf 'frost uninstall: warning: cannot purge removed target %s; quarantine retained at %s\n' \
                    "${target}" "${quarantine_display}" >&2
                print_uninstall_recovery "${quarantine_display}" "${target_display}"
            fi
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
    require_command readlink
    [[ -d /proc/self/fd ]] \
        || die "transactional uninstall requires /proc/self/fd"
    prepare_uninstall_plan
    prepare_cleanup_plan
    prepare_cache_refresh_plan
    stage_uninstall_plan
    # Every owned name is now absent: this is the uninstall commit point.
    # Purge cannot truthfully roll that result back, so failures retain a named
    # quarantine and an exact manual recovery command while the exit stays 0.
    purge_uninstall_quarantines
fi
if ((DRY_RUN == 1)); then
    for target in "${REMOVAL_DIRS[@]}"; do
        remove_dir_if_empty "${target}"
    done
else
    for index in "${!REMOVAL_DIRS[@]}"; do
        remove_bound_dir_if_empty "${index}"
    done
    # These refreshes are optional and run only for directories bound before
    # the uninstall commit. A failed or stale cache never reverses success.
    refresh_bound_caches
    close_bound_directory_fds
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
