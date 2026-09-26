#!/bin/sh
# SPDX-License-Identifier: Apache-2.0

set -eu
umask 077

SCRIPT_DIRECTORY=$(CDPATH= cd "$(dirname "$0")" && pwd)
INSTALLER=$SCRIPT_DIRECTORY/install.sh
INSTALL_SHELL=${HARDKNOCK_INSTALL_TEST_SHELL:-/bin/sh}
TEST_ROOT=$(mktemp -d "${TMPDIR:-/tmp}/hardknock-install-tests.XXXXXX")
ORIGINAL_PATH=$PATH
REAL_MV=$(command -v mv)
REAL_LINK=$(command -v link)
DEFAULT_TAG_OBJECT=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa
DEFAULT_TAG_COMMIT=0123456789abcdef0123456789abcdef01234567
DEFAULT_TAG_TREE=89abcdef0123456789abcdef0123456789abcdef
DEFAULT_WORKFLOW_COMMIT=fedcba9876543210fedcba9876543210fedcba98
DEFAULT_BRANCH_HEAD=1111111111111111111111111111111111111111
TEST_NUMBER=0
CURRENT_TEST=

cleanup() {
    cleanup_status=$?
    trap - 0 1 2 3 15
    if [ -n "${TEST_ROOT:-}" ] &&
        [ -d "$TEST_ROOT" ] &&
        [ "$TEST_ROOT" != / ]; then
        rm -rf "$TEST_ROOT"
    fi
    exit "$cleanup_status"
}

trap cleanup 0
trap 'exit 1' 1 2 3 15

fail_test() {
    printf 'not ok %s - %s: %s\n' \
        "$TEST_NUMBER" "$CURRENT_TEST" "$1" >&2
    exit 1
}

begin_test() {
    TEST_NUMBER=$((TEST_NUMBER + 1))
    CURRENT_TEST=$1
}

pass_test() {
    printf 'ok %s - %s\n' "$TEST_NUMBER" "$CURRENT_TEST"
}

assert_file() {
    [ -f "$1" ] || fail_test "expected regular file: $1"
}

assert_directory() {
    [ -d "$1" ] && [ ! -L "$1" ] ||
        fail_test "expected regular directory: $1"
}

assert_executable() {
    [ -f "$1" ] && [ -x "$1" ] ||
        fail_test "expected executable file: $1"
}

assert_absent() {
    if [ -e "$1" ] || [ -L "$1" ]; then
        fail_test "expected path to be absent: $1"
    fi
}

assert_contains() {
    grep -F -- "$2" "$1" >/dev/null 2>&1 ||
        fail_test "expected $1 to contain: $2"
}

assert_not_contains() {
    if grep -F -- "$2" "$1" >/dev/null 2>&1; then
        fail_test "expected $1 not to contain: $2"
    fi
}

assert_equal() {
    [ "$1" = "$2" ] ||
        fail_test "expected '$2', got '$1'"
}

assert_marker_count() {
    marker_count=$(grep -Fxc "$2" "$1" 2>/dev/null || :)
    [ "$marker_count" -eq "$3" ] ||
        fail_test "expected $3 occurrences of '$2' in $1, got $marker_count"
}

assert_line_count() {
    actual_line_count=$(wc -l <"$1" | tr -d '[:space:]')
    [ "$actual_line_count" -eq "$2" ] ||
        fail_test "expected $2 lines in $1, got $actual_line_count"
}

assert_fixed_count() {
    actual_fixed_count=$(grep -Fxc -- "$2" "$1" 2>/dev/null || :)
    [ "$actual_fixed_count" -eq "$3" ] ||
        fail_test "expected $3 occurrences of '$2' in $1, got $actual_fixed_count"
}

assert_no_install_residue() {
    residue_prefix=$1
    [ ! -e "$residue_prefix/.hardknock-install.lock" ] &&
        [ ! -L "$residue_prefix/.hardknock-install.lock" ] ||
        fail_test "installer lock was not cleaned up"
    for residue_path in "$residue_prefix"/.hardknock-install.*; do
        [ "$residue_path" != "$residue_prefix/.hardknock-install.*" ] ||
            continue
        fail_test "installer transaction was not cleaned up: $residue_path"
    done
    for residue_path in \
        "$residue_prefix"/.hardknock-install-preparing.* \
        "$residue_prefix"/.hardknock-install-lock.* \
        "$residue_prefix"/.hardknock-install-lock-quarantine.*
    do
        case "$residue_path" in
            "$residue_prefix/.hardknock-install-preparing.*"|\
            "$residue_prefix/.hardknock-install-lock.*"|\
            "$residue_prefix/.hardknock-install-lock-quarantine.*")
                continue
                ;;
        esac
        fail_test "installer preparation state was not cleaned up: $residue_path"
    done
}

find_install_transaction() {
    find_transaction_prefix=$1
    FOUND_TRANSACTION=
    for find_transaction_path in \
        "$find_transaction_prefix"/.hardknock-install.*
    do
        [ "$find_transaction_path" != \
            "$find_transaction_prefix/.hardknock-install.*" ] ||
            continue
        [ "$find_transaction_path" != \
            "$find_transaction_prefix/.hardknock-install.lock" ] ||
            continue
        [ -z "$FOUND_TRANSACTION" ] ||
            fail_test "found multiple installer transactions"
        FOUND_TRANSACTION=$find_transaction_path
    done
    [ -n "$FOUND_TRANSACTION" ] ||
        fail_test "expected an interrupted installer transaction"
}

sha256_file() {
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum "$1" | awk '{print $1}'
    elif command -v shasum >/dev/null 2>&1; then
        shasum -a 256 "$1" | awk '{print $1}'
    else
        fail_test "sha256sum or shasum is required"
    fi
}

detect_target() {
    case "$(uname -s)" in
        Linux)
            TEST_SYSTEM=Linux
            test_os=unknown-linux-gnu
            ;;
        Darwin)
            TEST_SYSTEM=Darwin
            test_os=apple-darwin
            ;;
        *) fail_test "unsupported test operating system: $(uname -s)" ;;
    esac
    case "$(uname -m)" in
        x86_64|amd64) test_arch=x86_64 ;;
        arm64|aarch64) test_arch=aarch64 ;;
        *) fail_test "unsupported test architecture: $(uname -m)" ;;
    esac
    TARGET=$test_arch-$test_os
}

inode_of() {
    case "$TEST_SYSTEM" in
        Darwin) stat -f '%i' "$1" ;;
        Linux) stat -c '%i' "$1" ;;
    esac
}

identity_of() {
    case "$TEST_SYSTEM" in
        Darwin) stat -f '%d:%i' "$1" ;;
        Linux) stat -c '%d:%i' "$1" ;;
    esac
}

new_case() {
    CASE_ROOT=$TEST_ROOT/$1
    CASE_HOME=$CASE_ROOT/home
    CASE_PREFIX=$CASE_ROOT/prefix
    CASE_REPOSITORY=$CASE_ROOT/repository
    CASE_TMP=$CASE_ROOT/tmp
    CASE_DATA_HOME=$CASE_ROOT/hardknock-home
    mkdir -p \
        "$CASE_HOME" \
        "$CASE_REPOSITORY" \
        "$CASE_TMP" \
        "$CASE_DATA_HOME"
    printf '%s\n' 'user data' >"$CASE_DATA_HOME/user-data"
}

create_release() {
    fixture_repository=$1
    fixture_version=$2
    fixture_label=$3
    fixture_root=hardknock-$fixture_version-$TARGET
    fixture_parent=$TEST_ROOT/fixture-$fixture_version-$fixture_label
    fixture_directory=$fixture_parent/$fixture_root
    fixture_release_directory=$fixture_repository/v$fixture_version
    fixture_archive=$fixture_release_directory/$fixture_root.tar.gz

    rm -rf "$fixture_parent"
    mkdir -p "$fixture_directory" "$fixture_release_directory"
    {
        printf '%s\n' '#!/bin/sh'
        printf '%s\n' 'case "${1-}" in'
        printf '    --version) printf "%%s\\n" "hardknock %s" ;;\n' \
            "$fixture_version"
        printf '    *) printf "%%s\\n" "hardknock payload %s" ;;\n' \
            "$fixture_label"
        printf '%s\n' 'esac'
    } >"$fixture_directory/hardknock"
    {
        printf '%s\n' '#!/bin/sh'
        printf '%s\n' 'case "${1-}" in'
        printf '    --version) printf "%%s\\n" "hk-effect %s" ;;\n' \
            "$fixture_version"
        printf '    *) printf "%%s\\n" "hk-effect payload %s" ;;\n' \
            "$fixture_label"
        printf '%s\n' 'esac'
    } >"$fixture_directory/hk-effect"
    printf 'Hardknock test license %s %s\n' \
        "$fixture_version" "$fixture_label" >"$fixture_directory/LICENSE"
    printf 'Hardknock test notice %s %s\n' \
        "$fixture_version" "$fixture_label" >"$fixture_directory/NOTICE"
    chmod 0755 \
        "$fixture_directory/hardknock" \
        "$fixture_directory/hk-effect"
    chmod 0644 \
        "$fixture_directory/LICENSE" \
        "$fixture_directory/NOTICE"

    tar -czf "$fixture_archive" -C "$fixture_parent" "$fixture_root"
    fixture_hash=$(sha256_file "$fixture_archive")
    printf '%s  %s\n' "$fixture_hash" "${fixture_archive##*/}" \
        >"$fixture_archive.sha256"
}

rewrite_release_checksum() {
    fixture_hash=$(sha256_file "$fixture_archive")
    printf '%s  %s\n' "$fixture_hash" "${fixture_archive##*/}" \
        >"$fixture_archive.sha256"
}

create_link_release() {
    create_release "$1" "$2" "$3"
    rm -f "$fixture_directory/hk-effect"
    ln -s hardknock "$fixture_directory/hk-effect"
    tar -czf "$fixture_archive" -C "$fixture_parent" "$fixture_root"
    rewrite_release_checksum
}

create_oversized_release() {
    create_release "$1" "$2" "$3"
    dd if=/dev/zero of="$fixture_directory/NOTICE" \
        bs=1048576 count=9 2>/dev/null
    tar -czf "$fixture_archive" -C "$fixture_parent" "$fixture_root"
    rewrite_release_checksum
}

create_hanging_release() {
    create_release "$1" "$2" "$3"
    {
        printf '%s\n' '#!/bin/sh'
        printf '%s\n' 'sleep 20'
        printf '%s\n' 'printf "%s\n" "hardknock should not finish"'
    } >"$fixture_directory/hardknock"
    chmod 0755 "$fixture_directory/hardknock"
    tar -czf "$fixture_archive" -C "$fixture_parent" "$fixture_root"
    rewrite_release_checksum
}

create_traversal_release() {
    fixture_repository=$1
    fixture_version=$2
    fixture_root=hardknock-$fixture_version-$TARGET
    fixture_release_directory=$fixture_repository/v$fixture_version
    fixture_archive=$fixture_release_directory/$fixture_root.tar.gz
    fixture_parent=$TEST_ROOT/traversal-fixture-$fixture_version

    mkdir -p "$fixture_release_directory" "$fixture_parent/inside"
    printf '%s\n' 'must not escape extraction' >"$fixture_parent/escape"
    (
        cd "$fixture_parent/inside"
        tar -P -czf "$fixture_archive" ../escape
    )
    fixture_hash=$(sha256_file "$fixture_archive")
    printf '%s  %s\n' "$fixture_hash" "${fixture_archive##*/}" \
        >"$fixture_archive.sha256"
}

run_installer() {
    HOME=$CASE_HOME \
        SHELL=${CASE_LOGIN_SHELL:-/bin/sh} \
        HARDKNOCK_HOME=$CASE_DATA_HOME \
        TMPDIR=$CASE_TMP \
        HARDKNOCK_INSTALL_PROVENANCE_TIMEOUT_SECONDS=${CASE_PROVENANCE_TIMEOUT_SECONDS:-120} \
        HARDKNOCK_INSTALL_CANDIDATE_TIMEOUT_SECONDS=${CASE_CANDIDATE_TIMEOUT_SECONDS:-30} \
        "$INSTALL_SHELL" "$INSTALLER" "$@"
}

create_failing_mv() {
    CASE_TOOLS=$CASE_ROOT/tools
    CASE_MV_STATE=$CASE_ROOT/mv-state
    mkdir -p "$CASE_TOOLS"
    {
        printf '%s\n' '#!/bin/sh'
        printf '%s\n' 'set -eu'
        printf '%s\n' 'inspect_source=${1-}'
        printf '%s\n' '[ "$inspect_source" != -n ] || inspect_source=${2-}'
        printf '%s\n' 'case "$inspect_source" in'
        printf '%s\n' \
            '    "${HARDKNOCK_TEST_PREFIX:?}"/.hardknock-install-preparing.*|*/phase.next)'
        printf '%s\n' '        exec "${HARDKNOCK_TEST_REAL_MV:?}" "$@"'
        printf '%s\n' '        ;;'
        printf '%s\n' 'esac'
        printf '%s\n' 'state_file=${HARDKNOCK_TEST_MV_STATE:?}'
        printf '%s\n' 'count=0'
        printf '%s\n' 'if [ -f "$state_file" ]; then'
        printf '%s\n' '    IFS= read -r count <"$state_file" || count=0'
        printf '%s\n' 'fi'
        printf '%s\n' 'count=$((count + 1))'
        printf '%s\n' 'printf "%s\n" "$count" >"$state_file"'
        printf '%s\n' \
            'if [ "$count" -eq "${HARDKNOCK_TEST_MV_FAIL_AT:?}" ]; then'
        printf '%s\n' '    exit 73'
        printf '%s\n' 'fi'
        printf '%s\n' 'exec "${HARDKNOCK_TEST_REAL_MV:?}" "$@"'
    } >"$CASE_TOOLS/mv"
    {
        printf '%s\n' '#!/bin/sh'
        printf '%s\n' 'set -eu'
        printf '%s\n' 'state_file=${HARDKNOCK_TEST_MV_STATE:?}'
        printf '%s\n' 'failure_target=$state_file.link-target'
        printf '%s\n' 'if [ -f "$failure_target" ]; then'
        printf '%s\n' '    IFS= read -r target <"$failure_target" || target='
        printf '%s\n' '    [ "${2-}" != "$target" ] || exit 73'
        printf '%s\n' 'fi'
        printf '%s\n' 'count=0'
        printf '%s\n' 'if [ -f "$state_file" ]; then'
        printf '%s\n' '    IFS= read -r count <"$state_file" || count=0'
        printf '%s\n' 'fi'
        printf '%s\n' 'count=$((count + 1))'
        printf '%s\n' 'printf "%s\n" "$count" >"$state_file"'
        printf '%s\n' \
            'if [ "$count" -eq "${HARDKNOCK_TEST_MV_FAIL_AT:?}" ]; then'
        printf '%s\n' '    printf "%s\n" "${2-}" >"$failure_target"'
        printf '%s\n' '    exit 73'
        printf '%s\n' 'fi'
        printf '%s\n' 'exec "${HARDKNOCK_TEST_REAL_LINK:?}" "$@"'
    } >"$CASE_TOOLS/link"
    chmod 0755 "$CASE_TOOLS/mv" "$CASE_TOOLS/link"
}

create_killing_mv() {
    CASE_TOOLS=$CASE_ROOT/tools
    CASE_MV_STATE=$CASE_ROOT/mv-state
    mkdir -p "$CASE_TOOLS"
    {
        printf '%s\n' '#!/bin/sh'
        printf '%s\n' 'set -eu'
        printf '%s\n' 'inspect_source=${1-}'
        printf '%s\n' '[ "$inspect_source" != -n ] || inspect_source=${2-}'
        printf '%s\n' 'case "$inspect_source" in'
        printf '%s\n' \
            '    "${HARDKNOCK_TEST_PREFIX:?}"/.hardknock-install-preparing.*|*/phase.next)'
        printf '%s\n' '        exec "${HARDKNOCK_TEST_REAL_MV:?}" "$@"'
        printf '%s\n' '        ;;'
        printf '%s\n' 'esac'
        printf '%s\n' 'state_file=${HARDKNOCK_TEST_MV_STATE:?}'
        printf '%s\n' 'count=0'
        printf '%s\n' 'if [ -f "$state_file" ]; then'
        printf '%s\n' '    IFS= read -r count <"$state_file" || count=0'
        printf '%s\n' 'fi'
        printf '%s\n' 'count=$((count + 1))'
        printf '%s\n' 'printf "%s\n" "$count" >"$state_file"'
        printf '%s\n' '"${HARDKNOCK_TEST_REAL_MV:?}" "$@"'
        printf '%s\n' \
            'if [ "$count" -eq "${HARDKNOCK_TEST_MV_KILL_AT:?}" ]; then'
        printf '%s\n' '    kill -KILL "$PPID"'
        printf '%s\n' 'fi'
    } >"$CASE_TOOLS/mv"
    {
        printf '%s\n' '#!/bin/sh'
        printf '%s\n' 'set -eu'
        printf '%s\n' 'state_file=${HARDKNOCK_TEST_MV_STATE:?}'
        printf '%s\n' 'count=0'
        printf '%s\n' 'if [ -f "$state_file" ]; then'
        printf '%s\n' '    IFS= read -r count <"$state_file" || count=0'
        printf '%s\n' 'fi'
        printf '%s\n' 'count=$((count + 1))'
        printf '%s\n' 'printf "%s\n" "$count" >"$state_file"'
        printf '%s\n' '"${HARDKNOCK_TEST_REAL_LINK:?}" "$@"'
        printf '%s\n' \
            'if [ "$count" -eq "${HARDKNOCK_TEST_MV_KILL_AT:?}" ]; then'
        printf '%s\n' '    kill -KILL "$PPID"'
        printf '%s\n' 'fi'
    } >"$CASE_TOOLS/link"
    chmod 0755 "$CASE_TOOLS/mv" "$CASE_TOOLS/link"
}

create_racing_mv() {
    CASE_TOOLS=$CASE_ROOT/tools
    CASE_RACE_STATE=$CASE_ROOT/race-state
    mkdir -p "$CASE_TOOLS"
    {
        printf '%s\n' '#!/bin/sh'
        printf '%s\n' 'set -eu'
        printf '%s\n' 'race_source='
        printf '%s\n' 'race_destination='
        printf '%s\n' 'for race_argument in "$@"; do'
        printf '%s\n' '    case "$race_argument" in -*) continue ;; esac'
        printf '%s\n' '    race_source=$race_destination'
        printf '%s\n' '    race_destination=$race_argument'
        printf '%s\n' 'done'
        printf '%s\n' 'if [ ! -e "${HARDKNOCK_TEST_RACE_STATE:?}" ]; then'
        printf '%s\n' '    case "${HARDKNOCK_TEST_RACE_KIND:?}" in'
        printf '%s\n' '        managed)'
        printf '%s\n' \
            '            if [ "$race_source" = "${HARDKNOCK_TEST_PREFIX:?}/bin/hardknock" ]; then'
        printf '%s\n' '                : >"$HARDKNOCK_TEST_RACE_STATE"'
        printf '%s\n' \
            '                "$HARDKNOCK_TEST_REAL_MV" "$race_source" "$race_source.race-original"'
        printf '%s\n' \
            '                printf "%s\\n" "#!/bin/sh" "printf \"%s\\\\n\" concurrent-user-binary" >"$race_source"'
        printf '%s\n' '                chmod 0755 "$race_source"'
        printf '%s\n' '            fi'
        printf '%s\n' '            ;;'
        printf '%s\n' '        profile)'
        printf '%s\n' \
            '            if [ "$race_source" = "${HARDKNOCK_TEST_HOME:?}/.profile" ]; then'
        printf '%s\n' '                case "$race_destination" in'
        printf '%s\n' '                    */profile.displaced)'
        printf '%s\n' '                        : >"$HARDKNOCK_TEST_RACE_STATE"'
        printf '%s\n' \
            '                        "$HARDKNOCK_TEST_REAL_MV" "$race_source" "$race_source.race-original"'
        printf '%s\n' \
            '                        printf "%s\\n" "export CONCURRENT_PROFILE=1" >"$race_source"'
        printf '%s\n' '                        ;;'
        printf '%s\n' '                esac'
        printf '%s\n' '            fi'
        printf '%s\n' '            ;;'
        printf '%s\n' '        *) exit 64 ;;'
        printf '%s\n' '    esac'
        printf '%s\n' 'fi'
        printf '%s\n' 'exec "${HARDKNOCK_TEST_REAL_MV:?}" "$@"'
    } >"$CASE_TOOLS/mv"
    chmod 0755 "$CASE_TOOLS/mv"
}

create_final_placement_collision() {
    CASE_TOOLS=$CASE_ROOT/tools
    CASE_RACE_STATE=$CASE_ROOT/final-placement-race
    mkdir -p "$CASE_TOOLS"
    {
        printf '%s\n' '#!/bin/sh'
        printf '%s\n' 'set -eu'
        printf '%s\n' 'if [ ! -e "${HARDKNOCK_TEST_RACE_STATE:?}" ] &&'
        printf '%s\n' \
            '    [ "${2-}" = "${HARDKNOCK_TEST_PREFIX:?}/bin/hardknock" ]; then'
        printf '%s\n' '    : >"$HARDKNOCK_TEST_RACE_STATE"'
        printf '%s\n' \
            '    printf "%s\\n" "#!/bin/sh" "printf \"%s\\\\n\" final-placement-collision" >"$2"'
        printf '%s\n' '    chmod 0755 "$2"'
        printf '%s\n' 'fi'
        printf '%s\n' 'exec "${HARDKNOCK_TEST_REAL_LINK:?}" "$@"'
    } >"$CASE_TOOLS/link"
    chmod 0755 "$CASE_TOOLS/link"
}

create_cross_filesystem_link_once() {
    CASE_TOOLS=$CASE_ROOT/tools
    CASE_RACE_STATE=$CASE_ROOT/cross-filesystem-link
    mkdir -p "$CASE_TOOLS"
    {
        printf '%s\n' '#!/bin/sh'
        printf '%s\n' 'set -eu'
        printf '%s\n' 'if [ ! -e "${HARDKNOCK_TEST_RACE_STATE:?}" ] &&'
        printf '%s\n' \
            '    [ "${2-}" = "${HARDKNOCK_TEST_PREFIX:?}/bin/hardknock" ]; then'
        printf '%s\n' '    : >"$HARDKNOCK_TEST_RACE_STATE"'
        printf '%s\n' '    exit 18'
        printf '%s\n' 'fi'
        printf '%s\n' 'exec "${HARDKNOCK_TEST_REAL_LINK:?}" "$@"'
    } >"$CASE_TOOLS/link"
    chmod 0755 "$CASE_TOOLS/link"
}

create_phase_publication_error_mv() {
    CASE_TOOLS=$CASE_ROOT/tools
    CASE_PHASE_STATE=$CASE_ROOT/published-phase
    mkdir -p "$CASE_TOOLS"
    {
        printf '%s\n' '#!/bin/sh'
        printf '%s\n' 'set -eu'
        printf '%s\n' 'phase_source=${1-}'
        printf '%s\n' '"${HARDKNOCK_TEST_REAL_MV:?}" "$@"'
        printf '%s\n' 'case "$phase_source" in'
        printf '%s\n' \
            '    */phase.next) cat "${2-}" >"${HARDKNOCK_TEST_PHASE_STATE:?}"; exit 73 ;;'
        printf '%s\n' 'esac'
    } >"$CASE_TOOLS/mv"
    chmod 0755 "$CASE_TOOLS/mv"
}

create_lock_racing_mv() {
    CASE_TOOLS=$CASE_ROOT/tools
    CASE_RACE_STATE=$CASE_ROOT/lock-race
    mkdir -p "$CASE_TOOLS"
    {
        printf '%s\n' '#!/bin/sh'
        printf '%s\n' 'set -eu'
        printf '%s\n' 'race_source=${1-}'
        printf '%s\n' 'race_destination=${2-}'
        printf '%s\n' 'if [ ! -e "${HARDKNOCK_TEST_RACE_STATE:?}" ] &&'
        printf '%s\n' \
            '    [ "$race_source" = "${HARDKNOCK_TEST_PREFIX:?}/.hardknock-install.lock" ]; then'
        printf '%s\n' '    case "$race_destination" in'
        printf '%s\n' '        */captured)'
        printf '%s\n' '            : >"$HARDKNOCK_TEST_RACE_STATE"'
        printf '%s\n' \
            '            "$HARDKNOCK_TEST_REAL_MV" "$race_source" "$race_source.race-original"'
        printf '%s\n' '            {'
        printf '%s\n' '                printf "%s\\n" hardknock-install-lock-v1'
        printf '%s\n' '                printf "pid=%s\\n" "${HARDKNOCK_TEST_LIVE_PID:?}"'
        printf '%s\n' \
            '                printf "prefix=%s\\n" "$HARDKNOCK_TEST_PREFIX"'
        printf '%s\n' '                printf "%s\\n" token=replacement'
        printf '%s\n' '            } >"$race_source"'
        printf '%s\n' '            chmod 0600 "$race_source"'
        printf '%s\n' '            ;;'
        printf '%s\n' '    esac'
        printf '%s\n' 'fi'
        printf '%s\n' 'exec "${HARDKNOCK_TEST_REAL_MV:?}" "$@"'
    } >"$CASE_TOOLS/mv"
    chmod 0755 "$CASE_TOOLS/mv"
}

create_fake_login_shell_lookup() {
    CASE_TOOLS=$CASE_ROOT/tools
    mkdir -p "$CASE_TOOLS"
    {
        printf '%s\n' '#!/bin/sh'
        printf '%s\n' 'printf "%s\\n" "test:x:501:20::/tmp:/bin/zsh"'
    } >"$CASE_TOOLS/getent"
    {
        printf '%s\n' '#!/bin/sh'
        printf '%s\n' 'printf "%s\\n" "UserShell: /bin/zsh"'
    } >"$CASE_TOOLS/dscl"
    chmod 0755 "$CASE_TOOLS/getent" "$CASE_TOOLS/dscl"
}

create_fake_curl() {
    CASE_TOOLS=$CASE_ROOT/tools
    CASE_CURL_LOG=$CASE_ROOT/curl-arguments
    mkdir -p "$CASE_TOOLS"
    : >"$CASE_CURL_LOG"
    {
        printf '%s\n' '#!/bin/sh'
        printf '%s\n' 'set -eu'
        printf '%s\n' 'output='
        printf '%s\n' 'url='
        printf '%s\n' 'expect_output=0'
        printf '%s\n' 'for argument in "$@"; do'
        printf '%s\n' \
            '    printf "%s\n" "$argument" >>"${HARDKNOCK_TEST_CURL_LOG:?}"'
        printf '%s\n' '    if [ "$expect_output" -eq 1 ]; then'
        printf '%s\n' '        output=$argument'
        printf '%s\n' '        expect_output=0'
        printf '%s\n' '        continue'
        printf '%s\n' '    fi'
        printf '%s\n' '    case "$argument" in'
        printf '%s\n' '        --output) expect_output=1 ;;'
        printf '%s\n' '        https://*) url=$argument ;;'
        printf '%s\n' '    esac'
        printf '%s\n' 'done'
        printf '%s\n' '[ -n "$output" ] && [ -n "$url" ]'
        printf '%s\n' 'release_name=${url##*/}'
        printf '%s\n' \
            'source_path=${HARDKNOCK_TEST_CURL_MIRROR:?}/v${HARDKNOCK_TEST_CURL_VERSION:?}/$release_name'
        printf '%s\n' 'cp "$source_path" "$output"'
    } >"$CASE_TOOLS/curl"
    chmod 0755 "$CASE_TOOLS/curl"
}

create_fake_gh() {
    CASE_TOOLS=$CASE_ROOT/tools
    CASE_GH_LOG=$CASE_ROOT/gh-arguments
    CASE_GH_STATE=$CASE_ROOT/gh-state
    CASE_GH_MODE=${1:-success}
    mkdir -p "$CASE_TOOLS" "$CASE_GH_STATE"
    : >"$CASE_GH_LOG"
    cat >"$CASE_TOOLS/gh" <<'FAKE_GH'
#!/bin/sh
set -eu

log=${HARDKNOCK_TEST_GH_LOG:?}
state=${HARDKNOCK_TEST_GH_STATE:?}
repository=openkedge/hardknock
workflow=openkedge/hardknock/.github/workflows/release.yml
custom_predicate=https://openkedge.dev/hardknock/release-publication/v1
slsa_predicate=https://slsa.dev/provenance/v1
api_version='X-GitHub-Api-Version: 2026-03-10'
attestation_lookup_limit=100
expected_attestation_jq='def text: if type == "string" then . else "!" end; if type != "array" then "!" else ("COUNT|" + (length | tostring)), (.[] | (try .verificationResult.statement catch null) as $s | (try $s.predicate catch null) as $p | if (($s | type) == "object" and ($p | type) == "object" and (($p.asset_digests | type) == "object")) then [($s.predicateType | text), ($p | keys | join(",")), ($p.schema | text), ($p.channel | text), ($p.release_tag | text), ($p.candidate_tag | text), ($p.artifact_source_commit | text), ($p.artifact_source_tree | text), ($p.workflow_source_ref | text), ($p.workflow_source_commit | text), ($p.promotion_record_sha256 | text), ($p.asset_digests | to_entries | sort_by(.key) | map([(.key | text), (.value | text)] | join("=")) | join(","))] | join("|") else "!" end) end'
attestation_count_jq='if type == "array" then length else -1 end'
version=${HARDKNOCK_TEST_CURL_VERSION:?}
target=${HARDKNOCK_TEST_TARGET:?}
tag_object=${HARDKNOCK_TEST_TAG_OBJECT:?}
tag_commit=${HARDKNOCK_TEST_TAG_COMMIT:?}
tag_tree=${HARDKNOCK_TEST_TAG_TREE:?}
workflow_commit=${HARDKNOCK_TEST_WORKFLOW_COMMIT:?}
default_branch=${HARDKNOCK_TEST_DEFAULT_BRANCH:-main}
branch_head=${HARDKNOCK_TEST_BRANCH_HEAD:?}
archive_digest=${HARDKNOCK_TEST_ARCHIVE_DIGEST:?}
gh_mode=${HARDKNOCK_TEST_GH_MODE:-success}
tag_mode=${HARDKNOCK_TEST_GH_TAG_MODE:-signed}
attestation_mode=${HARDKNOCK_TEST_GH_ATTESTATION_MODE:-success}
branch_mode=${HARDKNOCK_TEST_GH_BRANCH_MODE:-success}

printf '%s\n' '-- invocation --' >>"$log"
for argument in "$@"; do
    printf '%s\n' "$argument" >>"$log"
done

fail_arguments() {
    printf 'unexpected fake gh arguments: %s\n' "$*" >&2
    exit 90
}

increment() {
    counter_path=$state/$1
    counter=0
    if [ -f "$counter_path" ]; then
        IFS= read -r counter <"$counter_path"
    fi
    counter=$((counter + 1))
    printf '%s\n' "$counter" >"$counter_path"
    COUNTER=$counter
}

if [ "${1-}" = --version ]; then
    [ "$#" -eq 1 ] || fail_arguments "$@"
    printf 'gh version %s (test)\n' \
        "${HARDKNOCK_TEST_GH_VERSION:-2.97.0}"
    exit 0
fi

case "$gh_mode" in
    unavailable) exit 127 ;;
    failure) exit 1 ;;
esac

if [ "${1-}" = api ]; then
    [ "$#" -eq 6 ] ||
        fail_arguments "$@"
    [ "$2" = -H ] &&
        [ "$3" = "$api_version" ] &&
        [ "$5" = --jq ] ||
        fail_arguments "$@"
    endpoint=$4
    jq_filter=$6

    case "$tag_mode" in
        failure) exit 1 ;;
        hang) exec sleep 20 ;;
    esac

    case "$endpoint" in
        "repos/$repository/git/ref/tags/v$version")
            [ "$jq_filter" = \
                '.ref + "|" + .object.type + "|" + .object.sha' ] ||
                fail_arguments "$@"
            increment tag-ref
            resolved_object=$tag_object
            resolved_type=tag
            case "$tag_mode" in
                lightweight) resolved_type=commit ;;
                malformed) resolved_object=not-a-tag-object ;;
                changed_tag)
                    if [ "$COUNTER" -ge 2 ]; then
                        resolved_object=bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb
                    fi
                    ;;
            esac
            printf 'refs/tags/v%s|%s|%s\n' \
                "$version" "$resolved_type" "$resolved_object"
            ;;
        "repos/$repository/git/tags/$tag_object"|\
        "repos/$repository/git/tags/bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb")
            [ "$jq_filter" = \
                '[.tag, (.verification.verified | tostring), .verification.reason, .object.type, .object.sha] | join("|")' ] ||
                fail_arguments "$@"
            verified=true
            reason=valid
            target_type=commit
            tag_name=v$version
            case "$tag_mode" in
                bad_signature)
                    verified=false
                    reason=invalid
                    ;;
                nested) target_type=tag ;;
                wrong_name) tag_name=v0.0.0 ;;
            esac
            printf '%s|%s|%s|%s|%s\n' \
                "$tag_name" "$verified" "$reason" \
                "$target_type" "$tag_commit"
            ;;
        "repos/$repository/git/commits/$tag_commit")
            [ "$jq_filter" = \
                '[.sha, .tree.sha] | join("|")' ] ||
                fail_arguments "$@"
            increment tag-commit
            resolved_tree=$tag_tree
            if [ "$tag_mode" = changed_tree ] &&
                [ "$COUNTER" -ge 2 ]; then
                resolved_tree=2222222222222222222222222222222222222222
            fi
            printf '%s|%s\n' "$tag_commit" "$resolved_tree"
            ;;
        "repos/$repository")
            [ "$jq_filter" = '.default_branch' ] ||
                fail_arguments "$@"
            increment default-branch
            resolved_branch=$default_branch
            if [ "$branch_mode" = changed_default ] &&
                [ "$COUNTER" -ge 2 ]; then
                resolved_branch=trunk
            fi
            printf '%s\n' "$resolved_branch"
            ;;
        "repos/$repository/branches/"*)
            [ "$jq_filter" = '.commit.sha' ] ||
                fail_arguments "$@"
            encoded_branch=${endpoint#"repos/$repository/branches/"}
            expected_encoded_branch=$(printf '%s' "$default_branch" |
                sed 's#/#%2F#g')
            [ "$encoded_branch" = "$expected_encoded_branch" ] ||
                fail_arguments "$@"
            printf '%s\n' "$branch_head"
            ;;
        "repos/$repository/compare/"*)
            [ "$jq_filter" = \
                '[.status, .merge_base_commit.sha, .head_commit.sha] | join("|")' ] ||
                fail_arguments "$@"
            [ "$endpoint" = \
                "repos/$repository/compare/$workflow_commit...$branch_head" ] ||
                fail_arguments "$@"
            if [ "$branch_mode" = ancestry_failure ]; then
                printf 'diverged|3333333333333333333333333333333333333333|%s\n' \
                    "$branch_head"
            else
                printf 'ahead|%s|%s\n' \
                    "$workflow_commit" "$branch_head"
            fi
            ;;
        *) fail_arguments "$@" ;;
    esac
    exit 0
fi

if [ "${1-}" = release ]; then
    case "${2-}" in
        verify)
            [ "$#" -eq 5 ] &&
                [ "$3" = "v$version" ] &&
                [ "$4" = --repo ] &&
                [ "$5" = "$repository" ] ||
                fail_arguments "$@"
            if [ "$gh_mode" = release_failure ]; then
                printf '%s\n' 'private release verifier detail' >&2
                exit 1
            fi
            if [ "$gh_mode" = hang ]; then
                exec sleep 20
            fi
            ;;
        verify-asset)
            [ "$#" -eq 6 ] &&
                [ "$3" = "v$version" ] &&
                [ "${4##*/}" = \
                    "hardknock-$version-$target.tar.gz" ] &&
                [ "$5" = --repo ] &&
                [ "$6" = "$repository" ] ||
                fail_arguments "$@"
            increment release-asset
            if [ "$gh_mode" = release_asset_failure ] ||
                { [ "$gh_mode" = final_asset_failure ] &&
                    [ "$COUNTER" -ge 2 ]; }; then
                printf '%s\n' 'private release asset verifier detail' >&2
                exit 1
            fi
            ;;
        *) fail_arguments "$@" ;;
    esac
    printf '%s\n' 'Verification succeeded!'
    exit 0
fi

emit_binding() {
    emitted_candidate=$1
    emitted_promotion=$2
    emitted_archive_digest=$3
    emitted_predicate_keys=artifact_source_commit,artifact_source_tree,asset_digests,candidate_tag,channel,promotion_record_sha256,release_tag,schema,workflow_source_commit,workflow_source_ref
    emitted_artifact_commit=$tag_commit
    emitted_artifact_tree=$tag_tree
    emitted_release_tag=v$version
    emitted_workflow_ref=refs/heads/$default_branch
    emitted_workflow_commit=$workflow_commit
    other_digest=1111111111111111111111111111111111111111111111111111111111111111
    digest_aarch64_darwin=$other_digest
    digest_aarch64_linux=$other_digest
    digest_x86_64_darwin=$other_digest
    digest_x86_64_linux=$other_digest
    case "$target" in
        aarch64-apple-darwin)
            digest_aarch64_darwin=$emitted_archive_digest
            ;;
        aarch64-unknown-linux-gnu)
            digest_aarch64_linux=$emitted_archive_digest
            ;;
        x86_64-apple-darwin)
            digest_x86_64_darwin=$emitted_archive_digest
            ;;
        x86_64-unknown-linux-gnu)
            digest_x86_64_linux=$emitted_archive_digest
            ;;
        *) exit 91 ;;
    esac
    case "$attestation_mode" in
        bad_predicate_keys)
            emitted_predicate_keys=${emitted_predicate_keys%,workflow_source_ref}
            ;;
        artifact_mismatch)
            emitted_artifact_commit=4444444444444444444444444444444444444444
            ;;
        artifact_tree_mismatch)
            emitted_artifact_tree=5555555555555555555555555555555555555555
            ;;
        wrong_release_tag)
            emitted_release_tag=v0.0.0
            ;;
        workflow_ref_mismatch)
            emitted_workflow_ref=refs/heads/not-default
            ;;
        bad_workflow_commit)
            emitted_workflow_commit=AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA
            ;;
        uppercase_hash)
            other_digest=AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA
            ;;
    esac
    asset_pairs="hardknock-$version-aarch64-apple-darwin.tar.gz=$digest_aarch64_darwin,hardknock-$version-aarch64-apple-darwin.tar.gz.sha256=$other_digest,hardknock-$version-aarch64-unknown-linux-gnu.tar.gz=$digest_aarch64_linux,hardknock-$version-aarch64-unknown-linux-gnu.tar.gz.sha256=$other_digest,hardknock-$version-sbom.cdx.json=$other_digest,hardknock-$version-third-party-licenses.json=$other_digest,hardknock-$version-x86_64-apple-darwin.tar.gz=$digest_x86_64_darwin,hardknock-$version-x86_64-apple-darwin.tar.gz.sha256=$other_digest,hardknock-$version-x86_64-unknown-linux-gnu.tar.gz=$digest_x86_64_linux,hardknock-$version-x86_64-unknown-linux-gnu.tar.gz.sha256=$other_digest,install-hardknock=$other_digest,install-hardknock.sha256=$other_digest"
    if [ "$attestation_mode" = bad_asset_keys ]; then
        asset_pairs=${asset_pairs%,install-hardknock.sha256=*}
    fi
    printf '%s|%s|%s|%s|%s|%s|%s|%s|%s|%s|%s|%s\n' \
        "$custom_predicate" \
        "$emitted_predicate_keys" \
        hardknock-release-publication-v1 \
        stable \
        "$emitted_release_tag" \
        "$emitted_candidate" \
        "$emitted_artifact_commit" \
        "$emitted_artifact_tree" \
        "$emitted_workflow_ref" \
        "$emitted_workflow_commit" \
        "$emitted_promotion" \
        "$asset_pairs"
}

if [ "${1-}" = attestation ]; then
    [ "${2-}" = verify ] &&
        [ "${3##*/}" = "hardknock-$version-$target.tar.gz" ] &&
        [ "${4-}" = --repo ] &&
        [ "${5-}" = "$repository" ] &&
        [ "${6-}" = --limit ] &&
        [ "${7-}" = "$attestation_lookup_limit" ] &&
        [ "${8-}" = --signer-workflow ] &&
        [ "${9-}" = "$workflow" ] &&
        [ "${10-}" = --deny-self-hosted-runners ] &&
        [ "${11-}" = --source-ref ] &&
        [ "${12-}" = "refs/heads/$default_branch" ] ||
        fail_arguments "$@"

    attestation_kind=
    if [ "$#" -eq 18 ] &&
        [ "${13-}" = --predicate-type ] &&
        [ "${14-}" = "$custom_predicate" ] &&
        [ "${15-}" = --format ] &&
        [ "${16-}" = json ] &&
        [ "${17-}" = --jq ] &&
        [ "${18}" = "$expected_attestation_jq" ]; then
        attestation_kind=custom-discovery
    elif [ "$#" -eq 22 ] &&
        [ "${13-}" = --source-digest ] &&
        [ "${14-}" = "$workflow_commit" ] &&
        [ "${15-}" = --signer-digest ] &&
        [ "${16-}" = "$workflow_commit" ] &&
        [ "${17-}" = --predicate-type ] &&
        [ "${18-}" = "$custom_predicate" ] &&
        [ "${19-}" = --format ] &&
        [ "${20-}" = json ] &&
        [ "${21-}" = --jq ] &&
        [ "${22}" = "$expected_attestation_jq" ]; then
        attestation_kind=custom-exact
    elif [ "$#" -eq 22 ] &&
        [ "${13-}" = --source-digest ] &&
        [ "${14-}" = "$workflow_commit" ] &&
        [ "${15-}" = --signer-digest ] &&
        [ "${16-}" = "$workflow_commit" ] &&
        [ "${17-}" = --predicate-type ] &&
        [ "${18-}" = "$slsa_predicate" ] &&
        [ "${19-}" = --format ] &&
        [ "${20-}" = json ] &&
        [ "${21-}" = --jq ] &&
        [ "${22}" = "$attestation_count_jq" ]; then
        attestation_kind=slsa
    else
        fail_arguments "$@"
    fi

    if [ "$gh_mode" = attestation_failure ]; then
        printf '%s\n' 'private attestation verifier detail' >&2
        exit 1
    fi
    if [ "$attestation_kind" = slsa ]; then
        if [ "$attestation_mode" = slsa_failure ]; then
            exit 1
        fi
        if [ "$attestation_mode" = slsa_saturation ]; then
            printf '%s\n' "$attestation_lookup_limit"
        else
            printf '%s\n' 1
        fi
        exit 0
    fi

    increment custom-attestation
    promotion_digest=eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee
    selected_archive_digest=$archive_digest
    case "$attestation_mode" in
        malformed)
            printf '%s\n' 'COUNT|1' '!'
            exit 0
            ;;
        wrong_archive_digest)
            selected_archive_digest=ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff
            ;;
        conflicting)
            if [ "$attestation_kind" = custom-exact ]; then
                promotion_digest=dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd
            fi
            ;;
    esac
    custom_result_count=1
    case "$attestation_mode" in
        multiple)
            custom_result_count=2
            ;;
        duplicate)
            custom_result_count=2
            ;;
        discovery_saturation)
            if [ "$attestation_kind" = custom-discovery ]; then
                custom_result_count=$attestation_lookup_limit
            fi
            ;;
        exact_saturation)
            if [ "$attestation_kind" = custom-exact ]; then
                custom_result_count=$attestation_lookup_limit
            fi
            ;;
    esac
    printf 'COUNT|%s\n' "$custom_result_count"
    custom_result_index=1
    while [ "$custom_result_index" -le "$custom_result_count" ]; do
        custom_candidate="v$version-rc.1"
        if [ "$attestation_mode" = multiple ] &&
            [ "$custom_result_index" -eq 2 ]; then
            custom_candidate="v$version-rc.2"
        fi
        emit_binding "$custom_candidate" \
            "$promotion_digest" "$selected_archive_digest"
        custom_result_index=$((custom_result_index + 1))
    done
    exit 0
fi

fail_arguments "$@"
FAKE_GH
    chmod 0755 "$CASE_TOOLS/gh"
}

run_installer_with_failing_mv() {
    failing_mv_at=$1
    shift
    HOME=$CASE_HOME \
        SHELL=${CASE_LOGIN_SHELL:-/bin/sh} \
        HARDKNOCK_HOME=$CASE_DATA_HOME \
        TMPDIR=$CASE_TMP \
        PATH=$CASE_TOOLS:$ORIGINAL_PATH \
        HARDKNOCK_TEST_MV_STATE=$CASE_MV_STATE \
        HARDKNOCK_TEST_MV_FAIL_AT=$failing_mv_at \
        HARDKNOCK_TEST_REAL_MV=$REAL_MV \
        HARDKNOCK_TEST_REAL_LINK=$REAL_LINK \
        HARDKNOCK_TEST_PREFIX=$CASE_PREFIX \
        "$INSTALL_SHELL" "$INSTALLER" "$@"
}

run_installer_with_killing_mv() {
    killing_mv_at=$1
    shift
    HOME=$CASE_HOME \
        SHELL=${CASE_LOGIN_SHELL:-/bin/sh} \
        HARDKNOCK_HOME=$CASE_DATA_HOME \
        TMPDIR=$CASE_TMP \
        PATH=$CASE_TOOLS:$ORIGINAL_PATH \
        HARDKNOCK_TEST_MV_STATE=$CASE_MV_STATE \
        HARDKNOCK_TEST_MV_KILL_AT=$killing_mv_at \
        HARDKNOCK_TEST_REAL_MV=$REAL_MV \
        HARDKNOCK_TEST_REAL_LINK=$REAL_LINK \
        HARDKNOCK_TEST_PREFIX=$CASE_PREFIX \
        "$INSTALL_SHELL" "$INSTALLER" "$@"
}

run_installer_with_racing_mv() {
    racing_kind=$1
    shift
    HOME=$CASE_HOME \
        SHELL=${CASE_LOGIN_SHELL:-/bin/sh} \
        HARDKNOCK_HOME=$CASE_DATA_HOME \
        TMPDIR=$CASE_TMP \
        PATH=$CASE_TOOLS:$ORIGINAL_PATH \
        HARDKNOCK_TEST_RACE_KIND=$racing_kind \
        HARDKNOCK_TEST_RACE_STATE=$CASE_RACE_STATE \
        HARDKNOCK_TEST_REAL_MV=$REAL_MV \
        HARDKNOCK_TEST_PREFIX=$CASE_PREFIX \
        HARDKNOCK_TEST_HOME=$CASE_HOME \
        "$INSTALL_SHELL" "$INSTALLER" "$@"
}

run_installer_with_final_placement_collision() {
    HOME=$CASE_HOME \
        SHELL=${CASE_LOGIN_SHELL:-/bin/sh} \
        HARDKNOCK_HOME=$CASE_DATA_HOME \
        TMPDIR=$CASE_TMP \
        PATH=$CASE_TOOLS:$ORIGINAL_PATH \
        HARDKNOCK_TEST_RACE_STATE=$CASE_RACE_STATE \
        HARDKNOCK_TEST_REAL_LINK=$REAL_LINK \
        HARDKNOCK_TEST_PREFIX=$CASE_PREFIX \
        "$INSTALL_SHELL" "$INSTALLER" "$@"
}

run_installer_with_cross_filesystem_link() {
    HOME=$CASE_HOME \
        SHELL=${CASE_LOGIN_SHELL:-/bin/sh} \
        HARDKNOCK_HOME=$CASE_DATA_HOME \
        TMPDIR=$CASE_TMP \
        PATH=$CASE_TOOLS:$ORIGINAL_PATH \
        HARDKNOCK_TEST_RACE_STATE=$CASE_RACE_STATE \
        HARDKNOCK_TEST_REAL_LINK=$REAL_LINK \
        HARDKNOCK_TEST_PREFIX=$CASE_PREFIX \
        "$INSTALL_SHELL" "$INSTALLER" "$@"
}

run_installer_with_phase_publication_error() {
    HOME=$CASE_HOME \
        SHELL=${CASE_LOGIN_SHELL:-/bin/sh} \
        HARDKNOCK_HOME=$CASE_DATA_HOME \
        TMPDIR=$CASE_TMP \
        PATH=$CASE_TOOLS:$ORIGINAL_PATH \
        HARDKNOCK_TEST_REAL_MV=$REAL_MV \
        HARDKNOCK_TEST_PHASE_STATE=$CASE_PHASE_STATE \
        "$INSTALL_SHELL" "$INSTALLER" "$@"
}

run_installer_with_lock_racing_mv() {
    HOME=$CASE_HOME \
        SHELL=${CASE_LOGIN_SHELL:-/bin/sh} \
        HARDKNOCK_HOME=$CASE_DATA_HOME \
        TMPDIR=$CASE_TMP \
        PATH=$CASE_TOOLS:$ORIGINAL_PATH \
        HARDKNOCK_TEST_RACE_STATE=$CASE_RACE_STATE \
        HARDKNOCK_TEST_REAL_MV=$REAL_MV \
        HARDKNOCK_TEST_PREFIX=$CASE_PREFIX \
        HARDKNOCK_TEST_LIVE_PID=$$ \
        "$INSTALL_SHELL" "$INSTALLER" "$@"
}

run_installer_with_case_tools() {
    HOME=$CASE_HOME \
        SHELL=${CASE_LOGIN_SHELL:-/bin/sh} \
        HARDKNOCK_HOME=$CASE_DATA_HOME \
        TMPDIR=$CASE_TMP \
        PATH=$CASE_TOOLS:$ORIGINAL_PATH \
        "$INSTALL_SHELL" "$INSTALLER" "$@"
}

run_installer_without_shell() {
    HOME=$CASE_HOME \
        SHELL= \
        HARDKNOCK_HOME=$CASE_DATA_HOME \
        TMPDIR=$CASE_TMP \
        PATH=$CASE_TOOLS:$ORIGINAL_PATH \
        "$INSTALL_SHELL" "$INSTALLER" "$@"
}

run_installer_with_fake_curl() {
    fake_curl_version=$1
    shift
    fake_archive_path=$CASE_REPOSITORY/v$fake_curl_version/hardknock-$fake_curl_version-$TARGET.tar.gz
    fake_archive_digest=$(sha256_file "$fake_archive_path")
    HOME=$CASE_HOME \
        SHELL=${CASE_LOGIN_SHELL:-/bin/sh} \
        HARDKNOCK_HOME=$CASE_DATA_HOME \
        TMPDIR=$CASE_TMP \
        PATH=$CASE_TOOLS:$ORIGINAL_PATH \
        HARDKNOCK_TEST_CURL_LOG=$CASE_CURL_LOG \
        HARDKNOCK_TEST_CURL_MIRROR=$CASE_REPOSITORY \
        HARDKNOCK_TEST_CURL_VERSION=$fake_curl_version \
        HARDKNOCK_TEST_GH_LOG=${CASE_GH_LOG:-$CASE_ROOT/gh-arguments} \
        HARDKNOCK_TEST_GH_STATE=${CASE_GH_STATE:-$CASE_ROOT/gh-state} \
        HARDKNOCK_TEST_GH_MODE=${CASE_GH_MODE:-success} \
        HARDKNOCK_TEST_GH_VERSION=${CASE_GH_VERSION:-2.97.0} \
        HARDKNOCK_TEST_GH_TAG_MODE=${CASE_GH_TAG_MODE:-signed} \
        HARDKNOCK_TEST_GH_ATTESTATION_MODE=${CASE_GH_ATTESTATION_MODE:-success} \
        HARDKNOCK_TEST_GH_BRANCH_MODE=${CASE_GH_BRANCH_MODE:-success} \
        HARDKNOCK_TEST_TAG_OBJECT=${CASE_TAG_OBJECT:-$DEFAULT_TAG_OBJECT} \
        HARDKNOCK_TEST_TAG_COMMIT=${CASE_TAG_COMMIT:-$DEFAULT_TAG_COMMIT} \
        HARDKNOCK_TEST_TAG_TREE=${CASE_TAG_TREE:-$DEFAULT_TAG_TREE} \
        HARDKNOCK_TEST_WORKFLOW_COMMIT=${CASE_WORKFLOW_COMMIT:-$DEFAULT_WORKFLOW_COMMIT} \
        HARDKNOCK_TEST_DEFAULT_BRANCH=${CASE_DEFAULT_BRANCH:-main} \
        HARDKNOCK_TEST_BRANCH_HEAD=${CASE_BRANCH_HEAD:-$DEFAULT_BRANCH_HEAD} \
        HARDKNOCK_TEST_TARGET=$TARGET \
        HARDKNOCK_TEST_ARCHIVE_DIGEST=$fake_archive_digest \
        HARDKNOCK_INSTALL_PROVENANCE_TIMEOUT_SECONDS=${CASE_PROVENANCE_TIMEOUT_SECONDS:-120} \
        HARDKNOCK_INSTALL_CANDIDATE_TIMEOUT_SECONDS=${CASE_CANDIDATE_TIMEOUT_SECONDS:-30} \
        "$INSTALL_SHELL" "$INSTALLER" "$@"
}

expect_official_provenance_failure() {
    expected_failure_version=$1
    expected_failure_message=$2
    if run_installer_with_fake_curl "$expected_failure_version" \
        --version "$expected_failure_version" \
        --prefix "$CASE_PREFIX" \
        --no-modify-path \
        --dry-run \
        >"$CASE_ROOT/output" 2>"$CASE_ROOT/error"; then
        fail_test "invalid official provenance unexpectedly verified"
    fi
    assert_contains "$CASE_ROOT/error" "$expected_failure_message"
    assert_absent "$CASE_PREFIX"
    assert_file "$CASE_DATA_HOME/user-data"
}

detect_target

begin_test "dry-run JSON leaves the filesystem unchanged"
new_case dry-run
CASE_PREFIX="$CASE_ROOT/prefix \"json\""
create_release "$CASE_REPOSITORY" 1.2.3 dry-run
run_installer \
    --version 1.2.3 \
    --prefix "$CASE_PREFIX" \
    --repository "$CASE_REPOSITORY" \
    --dry-run \
    --json \
    >"$CASE_ROOT/output" 2>"$CASE_ROOT/error" ||
    fail_test "dry-run installation failed"
assert_contains "$CASE_ROOT/output" '"ok":true'
assert_contains "$CASE_ROOT/output" \
    '"schema":"hardknock-installer-result-v1"'
assert_contains "$CASE_ROOT/output" '"action":"install"'
assert_contains "$CASE_ROOT/output" '"dry_run":true'
assert_contains "$CASE_ROOT/output" '"changed":true'
assert_contains "$CASE_ROOT/output" '"version":"1.2.3"'
assert_contains "$CASE_ROOT/output" "\"target\":\"$TARGET\""
assert_contains "$CASE_ROOT/output" '"modify_path":true'
assert_contains "$CASE_ROOT/output" 'prefix \"json\"'
assert_contains "$CASE_ROOT/output" \
    '"action":"create"'
assert_contains "$CASE_ROOT/output" \
    'hardknock setup --agent auto --start'
assert_line_count "$CASE_ROOT/output" 1
assert_absent "$CASE_PREFIX"
assert_absent "$CASE_HOME/.profile"
assert_file "$CASE_DATA_HOME/user-data"
pass_test

begin_test "tampered archive fails checksum verification"
new_case tamper
create_release "$CASE_REPOSITORY" 1.2.4 tamper
tampered_archive=$CASE_REPOSITORY/v1.2.4/hardknock-1.2.4-$TARGET.tar.gz
printf '%s\n' 'tampered bytes' >>"$tampered_archive"
if run_installer \
    --version 1.2.4 \
    --prefix "$CASE_PREFIX" \
    --repository "$CASE_REPOSITORY" \
    --no-modify-path \
    >"$CASE_ROOT/output" 2>"$CASE_ROOT/error"; then
    fail_test "tampered archive unexpectedly installed"
fi
assert_contains "$CASE_ROOT/error" 'checksum verification failed'
assert_absent "$CASE_PREFIX"
assert_file "$CASE_DATA_HOME/user-data"
pass_test

begin_test "traversal archive is rejected before extraction"
new_case traversal
create_traversal_release "$CASE_REPOSITORY" 1.2.5
if run_installer \
    --version 1.2.5 \
    --prefix "$CASE_PREFIX" \
    --repository "$CASE_REPOSITORY" \
    --no-modify-path \
    >"$CASE_ROOT/output" 2>"$CASE_ROOT/error"; then
    fail_test "traversal archive unexpectedly installed"
fi
assert_contains "$CASE_ROOT/error" 'unsafe or unexpected member'
assert_absent "$CASE_PREFIX"
assert_file "$CASE_DATA_HOME/user-data"
pass_test

begin_test "install writes managed files and a bounded PATH block"
new_case install
CASE_PREFIX="$CASE_ROOT/prefix with spaces and ' quote"
create_release "$CASE_REPOSITORY" 2.0.0 install
printf '%s\n' 'export KEEP_ME=1' >"$CASE_HOME/.profile"
mkdir -p "$CASE_PREFIX/bin"
printf '%s\n' 'user tool' >"$CASE_PREFIX/bin/user-tool"
run_installer \
    --version v2.0.0 \
    --prefix "$CASE_PREFIX" \
    --repository "$CASE_REPOSITORY" \
    >"$CASE_ROOT/output" 2>"$CASE_ROOT/error" ||
    fail_test "installation failed"
assert_executable "$CASE_PREFIX/bin/hardknock"
assert_executable "$CASE_PREFIX/bin/hk-effect"
assert_file "$CASE_PREFIX/share/doc/hardknock/LICENSE"
assert_file "$CASE_PREFIX/share/doc/hardknock/NOTICE"
assert_file "$CASE_PREFIX/share/hardknock/install-manifest-v1"
assert_file "$CASE_PREFIX/bin/user-tool"
assert_equal "$("$CASE_PREFIX/bin/hardknock" --version)" 'hardknock 2.0.0'
assert_equal "$("$CASE_PREFIX/bin/hk-effect" --version)" 'hk-effect 2.0.0'
assert_contains "$CASE_PREFIX/share/hardknock/install-manifest-v1" \
    'version=2.0.0'
assert_contains "$CASE_HOME/.profile" 'export KEEP_ME=1'
assert_marker_count \
    "$CASE_HOME/.profile" '# >>> hardknock managed PATH >>>' 1
assert_marker_count \
    "$CASE_HOME/.profile" '# <<< hardknock managed PATH <<<' 1
resolved_hardknock=$(
    HOME=$CASE_HOME PATH=/usr/bin:/bin /bin/sh -c \
        '. "$HOME/.profile"; command -v hardknock'
)
assert_equal "$resolved_hardknock" "$CASE_PREFIX/bin/hardknock"
profile_hash_after_install=$(sha256_file "$CASE_HOME/.profile")
hardknock_inode_after_install=$(inode_of "$CASE_PREFIX/bin/hardknock")
run_installer \
    --version 2.0.0 \
    --prefix "$CASE_PREFIX" \
    --repository "$CASE_REPOSITORY" \
    --json \
    >"$CASE_ROOT/repeat-output" 2>"$CASE_ROOT/repeat-error" ||
    fail_test "repeated PATH-managed installation failed"
assert_contains "$CASE_ROOT/repeat-output" '"install_kind":"noop"'
assert_contains "$CASE_ROOT/repeat-output" '"changed":false'
assert_equal \
    "$(sha256_file "$CASE_HOME/.profile")" \
    "$profile_hash_after_install"
assert_equal \
    "$(inode_of "$CASE_PREFIX/bin/hardknock")" \
    "$hardknock_inode_after_install"
assert_marker_count \
    "$CASE_HOME/.profile" '# >>> hardknock managed PATH >>>' 1
assert_file "$CASE_DATA_HOME/user-data"
pass_test

begin_test "zsh installations manage and uninstall the login zprofile"
new_case zsh-profile
create_release "$CASE_REPOSITORY" 2.0.1 zsh-profile
CASE_LOGIN_SHELL=/bin/zsh
printf '%s\n' 'export KEEP_ZPROFILE=1' >"$CASE_HOME/.zprofile"
printf '%s\n' 'export KEEP_PROFILE=1' >"$CASE_HOME/.profile"
run_installer \
    --version 2.0.1 \
    --prefix "$CASE_PREFIX" \
    --repository "$CASE_REPOSITORY" \
    >"$CASE_ROOT/install-output" 2>"$CASE_ROOT/install-error" ||
    fail_test "zsh-profile installation failed"
assert_contains "$CASE_HOME/.zprofile" 'export KEEP_ZPROFILE=1'
assert_marker_count \
    "$CASE_HOME/.zprofile" '# >>> hardknock managed PATH >>>' 1
assert_equal "$(cat "$CASE_HOME/.profile")" 'export KEEP_PROFILE=1'
assert_contains "$CASE_PREFIX/share/hardknock/install-manifest-v1" \
    "path_profile=$CASE_HOME/.zprofile"
resolved_hardknock=$(
    HOME=$CASE_HOME PATH=/usr/bin:/bin /bin/sh -c \
        '. "$HOME/.zprofile"; command -v hardknock'
)
assert_equal "$resolved_hardknock" "$CASE_PREFIX/bin/hardknock"
run_installer \
    --uninstall \
    --prefix "$CASE_PREFIX" \
    >"$CASE_ROOT/uninstall-output" 2>"$CASE_ROOT/uninstall-error" ||
    fail_test "zsh-profile uninstall failed"
assert_contains "$CASE_HOME/.zprofile" 'export KEEP_ZPROFILE=1'
assert_not_contains \
    "$CASE_HOME/.zprofile" '# >>> hardknock managed PATH >>>'
assert_equal "$(cat "$CASE_HOME/.profile")" 'export KEEP_PROFILE=1'
assert_no_install_residue "$CASE_PREFIX"
unset CASE_LOGIN_SHELL
pass_test

begin_test "missing SHELL uses the account login shell"
new_case account-login-shell
create_release "$CASE_REPOSITORY" 2.0.2 account-login-shell
create_fake_login_shell_lookup
printf '%s\n' 'export KEEP_ACCOUNT_ZPROFILE=1' >"$CASE_HOME/.zprofile"
run_installer_without_shell \
    --version 2.0.2 \
    --prefix "$CASE_PREFIX" \
    --repository "$CASE_REPOSITORY" \
    >"$CASE_ROOT/install-output" 2>"$CASE_ROOT/install-error" ||
    fail_test "account login-shell installation failed"
assert_contains "$CASE_HOME/.zprofile" 'export KEEP_ACCOUNT_ZPROFILE=1'
assert_marker_count \
    "$CASE_HOME/.zprofile" '# >>> hardknock managed PATH >>>' 1
assert_absent "$CASE_HOME/.profile"
assert_contains "$CASE_PREFIX/share/hardknock/install-manifest-v1" \
    "path_profile=$CASE_HOME/.zprofile"
pass_test

begin_test "upgrade replaces only the managed release"
new_case upgrade
create_release "$CASE_REPOSITORY" 2.1.0 old
create_release "$CASE_REPOSITORY" 2.2.0 new
printf '%s\n' 'export KEEP_ME=upgrade' >"$CASE_HOME/.profile"
run_installer \
    --version 2.1.0 \
    --prefix "$CASE_PREFIX" \
    --repository "$CASE_REPOSITORY" \
    >"$CASE_ROOT/install-output" 2>"$CASE_ROOT/install-error" ||
    fail_test "initial installation failed"
run_installer \
    --version 2.2.0 \
    --prefix "$CASE_PREFIX" \
    --repository "$CASE_REPOSITORY" \
    >"$CASE_ROOT/upgrade-output" 2>"$CASE_ROOT/upgrade-error" ||
    fail_test "upgrade failed"
assert_equal "$("$CASE_PREFIX/bin/hardknock" --version)" 'hardknock 2.2.0'
assert_equal "$("$CASE_PREFIX/bin/hardknock")" 'hardknock payload new'
assert_equal "$("$CASE_PREFIX/bin/hk-effect")" 'hk-effect payload new'
assert_contains "$CASE_PREFIX/share/doc/hardknock/LICENSE" \
    'Hardknock test license 2.2.0 new'
assert_contains "$CASE_PREFIX/share/hardknock/install-manifest-v1" \
    'version=2.2.0'
assert_marker_count \
    "$CASE_HOME/.profile" '# >>> hardknock managed PATH >>>' 1
assert_marker_count \
    "$CASE_HOME/.profile" '# <<< hardknock managed PATH <<<' 1
assert_contains "$CASE_HOME/.profile" 'export KEEP_ME=upgrade'
assert_file "$CASE_DATA_HOME/user-data"
pass_test

begin_test "unmanaged destination collision is preserved"
new_case collision
create_release "$CASE_REPOSITORY" 2.3.0 collision
mkdir -p "$CASE_PREFIX/bin"
printf '%s\n' 'owned by the user' >"$CASE_PREFIX/bin/hardknock"
if run_installer \
    --version 2.3.0 \
    --prefix "$CASE_PREFIX" \
    --repository "$CASE_REPOSITORY" \
    --no-modify-path \
    >"$CASE_ROOT/output" 2>"$CASE_ROOT/error"; then
    fail_test "unmanaged collision unexpectedly installed"
fi
assert_contains "$CASE_ROOT/error" 'unmanaged file collision'
assert_equal "$(cat "$CASE_PREFIX/bin/hardknock")" 'owned by the user'
assert_absent "$CASE_PREFIX/bin/hk-effect"
assert_absent "$CASE_PREFIX/share/doc/hardknock/LICENSE"
assert_absent "$CASE_PREFIX/share/hardknock/install-manifest-v1"
assert_absent "$CASE_HOME/.profile"
assert_file "$CASE_DATA_HOME/user-data"
pass_test

begin_test "repeated install is idempotent and can skip PATH changes"
new_case repeat-install
create_release "$CASE_REPOSITORY" 2.4.0 repeat
printf '%s\n' 'export PROFILE_UNCHANGED=1' >"$CASE_HOME/.profile"
mkdir -p "$CASE_PREFIX/bin"
printf '%s\n' 'user tool' >"$CASE_PREFIX/bin/user-tool"
run_installer \
    --version 2.4.0 \
    --prefix "$CASE_PREFIX" \
    --repository "$CASE_REPOSITORY" \
    --no-modify-path \
    >"$CASE_ROOT/first-output" 2>"$CASE_ROOT/first-error" ||
    fail_test "first installation failed"
first_hardknock_inode=$(inode_of "$CASE_PREFIX/bin/hardknock")
first_manifest_inode=$(
    inode_of "$CASE_PREFIX/share/hardknock/install-manifest-v1"
)
run_installer \
    --version 2.4.0 \
    --prefix "$CASE_PREFIX" \
    --repository "$CASE_REPOSITORY" \
    --no-modify-path \
    --json \
    >"$CASE_ROOT/second-output" 2>"$CASE_ROOT/second-error" ||
    fail_test "repeated installation failed"
assert_equal "$("$CASE_PREFIX/bin/hardknock" --version)" 'hardknock 2.4.0'
assert_equal \
    "$(inode_of "$CASE_PREFIX/bin/hardknock")" \
    "$first_hardknock_inode"
assert_equal \
    "$(inode_of "$CASE_PREFIX/share/hardknock/install-manifest-v1")" \
    "$first_manifest_inode"
assert_equal "$(cat "$CASE_HOME/.profile")" 'export PROFILE_UNCHANGED=1'
assert_contains "$CASE_PREFIX/share/hardknock/install-manifest-v1" \
    'path_profile=-'
assert_contains "$CASE_ROOT/second-output" '"install_kind":"noop"'
assert_contains "$CASE_ROOT/second-output" '"changed":false'
assert_line_count "$CASE_ROOT/second-output" 1
assert_file "$CASE_PREFIX/bin/user-tool"
assert_file "$CASE_DATA_HOME/user-data"
pass_test

begin_test "uninstall preserves user data and modified files"
new_case uninstall
create_release "$CASE_REPOSITORY" 2.5.0 uninstall
printf '%s\n' 'export KEEP_AFTER_UNINSTALL=1' >"$CASE_HOME/.profile"
run_installer \
    --version 2.5.0 \
    --prefix "$CASE_PREFIX" \
    --repository "$CASE_REPOSITORY" \
    >"$CASE_ROOT/install-output" 2>"$CASE_ROOT/install-error" ||
    fail_test "installation before uninstall failed"
printf '%s\n' 'user tool' >"$CASE_PREFIX/bin/user-tool"
printf '%s\n' 'user-modified license' \
    >"$CASE_PREFIX/share/doc/hardknock/LICENSE"
run_installer \
    --uninstall \
    --prefix "$CASE_PREFIX" \
    --json \
    >"$CASE_ROOT/uninstall-output" 2>"$CASE_ROOT/uninstall-error" ||
    fail_test "uninstall failed"
run_installer \
    --uninstall \
    --prefix "$CASE_PREFIX" \
    --json \
    >"$CASE_ROOT/repeat-output" 2>"$CASE_ROOT/repeat-error" ||
    fail_test "repeated uninstall failed"
assert_absent "$CASE_PREFIX/bin/hardknock"
assert_absent "$CASE_PREFIX/bin/hk-effect"
assert_absent "$CASE_PREFIX/share/doc/hardknock/NOTICE"
assert_absent "$CASE_PREFIX/share/hardknock/install-manifest-v1"
assert_file "$CASE_PREFIX/share/doc/hardknock/LICENSE"
assert_equal \
    "$(cat "$CASE_PREFIX/share/doc/hardknock/LICENSE")" \
    'user-modified license'
assert_file "$CASE_PREFIX/bin/user-tool"
assert_contains "$CASE_HOME/.profile" 'export KEEP_AFTER_UNINSTALL=1'
assert_not_contains "$CASE_HOME/.profile" '# >>> hardknock managed PATH >>>'
assert_not_contains "$CASE_HOME/.profile" '# <<< hardknock managed PATH <<<'
assert_contains "$CASE_ROOT/uninstall-output" '"preserved_modified":true'
assert_contains "$CASE_ROOT/uninstall-output" \
    'preserved modified managed file'
assert_contains "$CASE_ROOT/repeat-output" '"already_absent":true'
assert_line_count "$CASE_ROOT/uninstall-output" 1
assert_line_count "$CASE_ROOT/repeat-output" 1
assert_file "$CASE_DATA_HOME/user-data"
pass_test

begin_test "plain HTTP repository is rejected without a network request"
new_case insecure-http
if run_installer \
    --version 3.0.0 \
    --prefix "$CASE_PREFIX" \
    --repository http://example.invalid/releases \
    --no-modify-path \
    --json \
    >"$CASE_ROOT/output" 2>"$CASE_ROOT/error"; then
    fail_test "plain HTTP repository unexpectedly accepted"
fi
assert_contains "$CASE_ROOT/error" '"ok":false'
assert_contains "$CASE_ROOT/error" 'plain HTTP repositories are not supported'
assert_line_count "$CASE_ROOT/error" 1
assert_absent "$CASE_PREFIX"
assert_file "$CASE_DATA_HOME/user-data"
pass_test

begin_test "musl Linux is rejected before release selection"
new_case musl-linux
CASE_TOOLS=$CASE_ROOT/tools
mkdir -p "$CASE_TOOLS"
{
    printf '%s\n' '#!/bin/sh'
    printf '%s\n' 'case "${1-}" in'
    printf '%s\n' '    -s) printf "%s\n" Linux ;;'
    printf '%s\n' '    -m) printf "%s\n" x86_64 ;;'
    printf '%s\n' '    *) exit 64 ;;'
    printf '%s\n' 'esac'
} >"$CASE_TOOLS/uname"
{
    printf '%s\n' '#!/bin/sh'
    printf '%s\n' 'printf "%s\n" "musl libc 1.2.5"'
} >"$CASE_TOOLS/getconf"
chmod 0755 "$CASE_TOOLS/uname" "$CASE_TOOLS/getconf"
if run_installer_with_case_tools \
    --version 3.0.01 \
    --prefix "$CASE_PREFIX" \
    --repository "$CASE_REPOSITORY" \
    --no-modify-path \
    >"$CASE_ROOT/output" 2>"$CASE_ROOT/error"; then
    fail_test "musl Linux unexpectedly selected a glibc release"
fi
assert_contains "$CASE_ROOT/error" \
    'musl Linux is not supported'
assert_absent "$CASE_PREFIX"
pass_test

begin_test "checksum must name the requested archive"
new_case checksum-name
create_release "$CASE_REPOSITORY" 3.0.1 checksum-name
checksum_archive=$CASE_REPOSITORY/v3.0.1/hardknock-3.0.1-$TARGET.tar.gz
checksum_path=$checksum_archive.sha256
checksum_hash=$(sha256_file "$checksum_archive")
printf '%s  %s\n' "$checksum_hash" wrong-archive.tar.gz >"$checksum_path"
if run_installer \
    --version 3.0.1 \
    --prefix "$CASE_PREFIX" \
    --repository "$CASE_REPOSITORY" \
    --no-modify-path \
    >"$CASE_ROOT/output" 2>"$CASE_ROOT/error"; then
    fail_test "mismatched checksum filename unexpectedly installed"
fi
assert_contains "$CASE_ROOT/error" \
    'checksum file names an unexpected release archive'
assert_absent "$CASE_PREFIX"
assert_file "$CASE_DATA_HOME/user-data"
pass_test

begin_test "symlinked local release file is rejected"
new_case symlink-source
create_release "$CASE_REPOSITORY" 3.0.2 symlink-source
source_archive=$CASE_REPOSITORY/v3.0.2/hardknock-3.0.2-$TARGET.tar.gz
mv "$source_archive" "$source_archive.real"
ln -s "${source_archive##*/}.real" "$source_archive"
if run_installer \
    --version 3.0.2 \
    --prefix "$CASE_PREFIX" \
    --repository "$CASE_REPOSITORY" \
    --no-modify-path \
    >"$CASE_ROOT/output" 2>"$CASE_ROOT/error"; then
    fail_test "symlinked local release unexpectedly installed"
fi
assert_contains "$CASE_ROOT/error" 'release file is missing or unsafe'
assert_absent "$CASE_PREFIX"
assert_file "$CASE_DATA_HOME/user-data"
pass_test

begin_test "archive links are rejected before extraction"
new_case archive-link
create_link_release "$CASE_REPOSITORY" 3.0.3 archive-link
if run_installer \
    --version 3.0.3 \
    --prefix "$CASE_PREFIX" \
    --repository "$CASE_REPOSITORY" \
    --no-modify-path \
    >"$CASE_ROOT/output" 2>"$CASE_ROOT/error"; then
    fail_test "archive containing a symbolic link unexpectedly installed"
fi
assert_contains "$CASE_ROOT/error" \
    'archive contains links, devices, or unsupported member types'
assert_absent "$CASE_PREFIX"
assert_file "$CASE_DATA_HOME/user-data"
pass_test

begin_test "oversized archive member is bounded during extraction"
new_case oversized
create_oversized_release "$CASE_REPOSITORY" 3.0.4 oversized
if run_installer \
    --version 3.0.4 \
    --prefix "$CASE_PREFIX" \
    --repository "$CASE_REPOSITORY" \
    --no-modify-path \
    >"$CASE_ROOT/output" 2>"$CASE_ROOT/error"; then
    fail_test "oversized release member unexpectedly installed"
fi
assert_contains "$CASE_ROOT/error" \
    'cannot safely extract release member: NOTICE'
assert_absent "$CASE_PREFIX"
assert_file "$CASE_DATA_HOME/user-data"
pass_test

begin_test "symbolic link installation prefix is rejected"
new_case symlink-prefix
create_release "$CASE_REPOSITORY" 3.0.5 symlink-prefix
real_prefix=$CASE_ROOT/real-prefix
mkdir "$real_prefix"
ln -s "$real_prefix" "$CASE_PREFIX"
if run_installer \
    --version 3.0.5 \
    --prefix "$CASE_PREFIX" \
    --repository "$CASE_REPOSITORY" \
    --no-modify-path \
    >"$CASE_ROOT/output" 2>"$CASE_ROOT/error"; then
    fail_test "symbolic link prefix unexpectedly accepted"
fi
assert_contains "$CASE_ROOT/error" \
    'installation prefix must not be a symbolic link'
assert_absent "$real_prefix/bin/hardknock"
assert_file "$CASE_DATA_HOME/user-data"
pass_test

begin_test "symbolic link profile is rejected and preserved"
new_case symlink-profile
create_release "$CASE_REPOSITORY" 3.0.6 symlink-profile
profile_target=$CASE_ROOT/profile-target
printf '%s\n' 'user profile target' >"$profile_target"
ln -s "$profile_target" "$CASE_HOME/.profile"
if run_installer \
    --version 3.0.6 \
    --prefix "$CASE_PREFIX" \
    --repository "$CASE_REPOSITORY" \
    >"$CASE_ROOT/output" 2>"$CASE_ROOT/error"; then
    fail_test "symbolic link profile unexpectedly modified"
fi
assert_contains "$CASE_ROOT/error" 'PATH profile must not be a symbolic link'
assert_equal "$(cat "$profile_target")" 'user profile target'
assert_absent "$CASE_PREFIX"
assert_file "$CASE_DATA_HOME/user-data"
pass_test

begin_test "shared-writable installation prefix is rejected"
new_case insecure-prefix
create_release "$CASE_REPOSITORY" 3.0.7 insecure-prefix
mkdir "$CASE_PREFIX"
chmod 0777 "$CASE_PREFIX"
if run_installer \
    --version 3.0.7 \
    --prefix "$CASE_PREFIX" \
    --repository "$CASE_REPOSITORY" \
    --no-modify-path \
    >"$CASE_ROOT/output" 2>"$CASE_ROOT/error"; then
    fail_test "shared-writable prefix unexpectedly accepted"
fi
assert_contains "$CASE_ROOT/error" \
    'writable by another user'
assert_absent "$CASE_PREFIX/bin/hardknock"
assert_file "$CASE_DATA_HOME/user-data"
pass_test

begin_test "a validated live installer lock blocks concurrent mutation"
new_case installer-lock
create_release "$CASE_REPOSITORY" 3.0.8 installer-lock
mkdir "$CASE_PREFIX"
chmod 0700 "$CASE_PREFIX"
{
    printf '%s\n' 'hardknock-install-lock-v1'
    printf 'pid=%s\n' "$$"
    printf 'prefix=%s\n' "$CASE_PREFIX"
    printf '%s\n' 'token=activelock'
} >"$CASE_PREFIX/.hardknock-install.lock"
chmod 0600 "$CASE_PREFIX/.hardknock-install.lock"
if run_installer \
    --version 3.0.8 \
    --prefix "$CASE_PREFIX" \
    --repository "$CASE_REPOSITORY" \
    --no-modify-path \
    >"$CASE_ROOT/output" 2>"$CASE_ROOT/error"; then
    fail_test "installer lock unexpectedly ignored"
fi
assert_contains "$CASE_ROOT/error" \
    'another installer operation is active'
assert_absent "$CASE_PREFIX/bin/hardknock"
assert_file "$CASE_PREFIX/.hardknock-install.lock"
assert_file "$CASE_DATA_HOME/user-data"
pass_test

begin_test "a stale file lock is quarantined and reclaimed"
new_case stale-file-lock
create_release "$CASE_REPOSITORY" 3.0.80 stale-file-lock
mkdir "$CASE_PREFIX"
chmod 0700 "$CASE_PREFIX"
{
    printf '%s\n' 'hardknock-install-lock-v1'
    printf '%s\n' 'pid=99999999'
    printf 'prefix=%s\n' "$CASE_PREFIX"
    printf '%s\n' 'token=stale'
} >"$CASE_PREFIX/.hardknock-install.lock"
chmod 0600 "$CASE_PREFIX/.hardknock-install.lock"
run_installer \
    --version 3.0.80 \
    --prefix "$CASE_PREFIX" \
    --repository "$CASE_REPOSITORY" \
    --no-modify-path \
    >"$CASE_ROOT/output" 2>"$CASE_ROOT/error" ||
    fail_test "stale file-lock recovery failed"
assert_executable "$CASE_PREFIX/bin/hardknock"
assert_no_install_residue "$CASE_PREFIX"
pass_test

begin_test "a stale legacy-directory lock is quarantined and reclaimed"
new_case stale-legacy-lock
create_release "$CASE_REPOSITORY" 3.0.801 stale-legacy-lock
mkdir -p "$CASE_PREFIX/.hardknock-install.lock"
chmod 0700 "$CASE_PREFIX" "$CASE_PREFIX/.hardknock-install.lock"
printf '%s\n' 99999999 >"$CASE_PREFIX/.hardknock-install.lock/pid"
chmod 0600 "$CASE_PREFIX/.hardknock-install.lock/pid"
run_installer \
    --version 3.0.801 \
    --prefix "$CASE_PREFIX" \
    --repository "$CASE_REPOSITORY" \
    --no-modify-path \
    >"$CASE_ROOT/output" 2>"$CASE_ROOT/error" ||
    fail_test "stale legacy-lock recovery failed"
assert_executable "$CASE_PREFIX/bin/hardknock"
assert_no_install_residue "$CASE_PREFIX"
pass_test

begin_test "stale-lock replacement is preserved in quarantine"
new_case stale-lock-race
create_release "$CASE_REPOSITORY" 3.0.802 stale-lock-race
mkdir "$CASE_PREFIX"
chmod 0700 "$CASE_PREFIX"
{
    printf '%s\n' 'hardknock-install-lock-v1'
    printf '%s\n' 'pid=99999999'
    printf 'prefix=%s\n' "$CASE_PREFIX"
    printf '%s\n' 'token=stale'
} >"$CASE_PREFIX/.hardknock-install.lock"
chmod 0600 "$CASE_PREFIX/.hardknock-install.lock"
create_lock_racing_mv
if run_installer_with_lock_racing_mv \
    --version 3.0.802 \
    --prefix "$CASE_PREFIX" \
    --repository "$CASE_REPOSITORY" \
    --no-modify-path \
    >"$CASE_ROOT/output" 2>"$CASE_ROOT/error"; then
    fail_test "stale-lock replacement race unexpectedly succeeded"
fi
assert_contains "$CASE_ROOT/error" \
    'quarantined installer lock does not match the inspected lock'
assert_file "$CASE_PREFIX/.hardknock-install.lock.race-original"
FOUND_QUARANTINE=
for quarantine_path in \
    "$CASE_PREFIX"/.hardknock-install-lock-quarantine.*
do
    [ "$quarantine_path" != \
        "$CASE_PREFIX/.hardknock-install-lock-quarantine.*" ] ||
        continue
    [ -z "$FOUND_QUARANTINE" ] ||
        fail_test "found multiple preserved lock quarantines"
    FOUND_QUARANTINE=$quarantine_path
done
[ -n "$FOUND_QUARANTINE" ] ||
    fail_test "expected a preserved lock quarantine"
assert_file "$FOUND_QUARANTINE/captured"
assert_contains "$FOUND_QUARANTINE/captured" "pid=$$"
quarantine_hash=$(sha256_file "$FOUND_QUARANTINE/captured")
if run_installer \
    --version 3.0.802 \
    --prefix "$CASE_PREFIX" \
    --repository "$CASE_REPOSITORY" \
    --no-modify-path \
    --dry-run \
    >"$CASE_ROOT/dry-output" 2>"$CASE_ROOT/dry-error"; then
    fail_test "dry-run ignored preserved lock quarantine"
fi
assert_contains "$CASE_ROOT/dry-error" \
    'preserved quarantine requires manual review'
assert_equal \
    "$(sha256_file "$FOUND_QUARANTINE/captured")" \
    "$quarantine_hash"
assert_absent "$CASE_PREFIX/bin/hardknock"
pass_test

begin_test "dead lock preparation state is reclaimed automatically"
new_case stale-lock-preparation
create_release "$CASE_REPOSITORY" 3.0.81 stale-lock-preparation
mkdir "$CASE_PREFIX"
chmod 0700 "$CASE_PREFIX"
stale_lock_preparation=$CASE_PREFIX/.hardknock-install-lock.99999999.stale
: >"$stale_lock_preparation"
chmod 0600 "$stale_lock_preparation"
run_installer \
    --version 3.0.81 \
    --prefix "$CASE_PREFIX" \
    --repository "$CASE_REPOSITORY" \
    --no-modify-path \
    >"$CASE_ROOT/output" 2>"$CASE_ROOT/error" ||
    fail_test "stale lock preparation recovery failed"
assert_executable "$CASE_PREFIX/bin/hardknock"
assert_absent "$stale_lock_preparation"
assert_no_install_residue "$CASE_PREFIX"
pass_test

begin_test "same version cannot resolve to changed release contents"
new_case immutable-version
create_release "$CASE_REPOSITORY" 3.0.9 original
run_installer \
    --version 3.0.9 \
    --prefix "$CASE_PREFIX" \
    --repository "$CASE_REPOSITORY" \
    --no-modify-path \
    >"$CASE_ROOT/install-output" 2>"$CASE_ROOT/install-error" ||
    fail_test "initial immutable-version installation failed"
create_release "$CASE_REPOSITORY" 3.0.9 republished
if run_installer \
    --version 3.0.9 \
    --prefix "$CASE_PREFIX" \
    --repository "$CASE_REPOSITORY" \
    --no-modify-path \
    >"$CASE_ROOT/output" 2>"$CASE_ROOT/error"; then
    fail_test "republished same version unexpectedly replaced installation"
fi
assert_contains "$CASE_ROOT/error" \
    'installed version resolves to different release contents'
assert_equal "$("$CASE_PREFIX/bin/hardknock")" \
    'hardknock payload original'
assert_contains "$CASE_PREFIX/share/doc/hardknock/NOTICE" \
    'Hardknock test notice 3.0.9 original'
assert_no_install_residue "$CASE_PREFIX"
assert_file "$CASE_DATA_HOME/user-data"
pass_test

begin_test "failed installation rolls back every managed mutation"
new_case install-rollback
create_release "$CASE_REPOSITORY" 3.1.0 install-rollback
create_failing_mv
if run_installer_with_failing_mv 3 \
    --version 3.1.0 \
    --prefix "$CASE_PREFIX" \
    --repository "$CASE_REPOSITORY" \
    --no-modify-path \
    >"$CASE_ROOT/output" 2>"$CASE_ROOT/error"; then
    fail_test "injected installation failure unexpectedly succeeded"
fi
assert_contains "$CASE_ROOT/error" 'cannot atomically install LICENSE'
assert_absent "$CASE_PREFIX"
assert_no_install_residue "$CASE_PREFIX"
assert_file "$CASE_DATA_HOME/user-data"
pass_test

begin_test "upgrade preserves a concurrent managed-file replacement"
new_case concurrent-managed-file
create_release "$CASE_REPOSITORY" 3.1.20 concurrent-managed-old
create_release "$CASE_REPOSITORY" 3.1.21 concurrent-managed-new
run_installer \
    --version 3.1.20 \
    --prefix "$CASE_PREFIX" \
    --repository "$CASE_REPOSITORY" \
    --no-modify-path \
    >"$CASE_ROOT/install-output" 2>"$CASE_ROOT/install-error" ||
    fail_test "installation before managed-file race failed"
create_racing_mv
if run_installer_with_racing_mv managed \
    --version 3.1.21 \
    --prefix "$CASE_PREFIX" \
    --repository "$CASE_REPOSITORY" \
    --no-modify-path \
    >"$CASE_ROOT/upgrade-output" 2>"$CASE_ROOT/upgrade-error"; then
    fail_test "upgrade overwrote a concurrent managed-file replacement"
fi
assert_contains "$CASE_ROOT/upgrade-error" \
    'cannot atomically install hardknock'
assert_equal \
    "$("$CASE_PREFIX/bin/hardknock")" \
    'concurrent-user-binary'
assert_equal \
    "$("$CASE_PREFIX/bin/hardknock.race-original")" \
    'hardknock payload concurrent-managed-old'
assert_contains "$CASE_PREFIX/share/hardknock/install-manifest-v1" \
    'version=3.1.20'
assert_no_install_residue "$CASE_PREFIX"
pass_test

begin_test "final hard-link placement preserves a concurrent collision"
new_case final-placement-collision
create_release "$CASE_REPOSITORY" 3.1.24 final-placement-old
create_release "$CASE_REPOSITORY" 3.1.25 final-placement-new
run_installer \
    --version 3.1.24 \
    --prefix "$CASE_PREFIX" \
    --repository "$CASE_REPOSITORY" \
    --no-modify-path \
    >"$CASE_ROOT/install-output" 2>"$CASE_ROOT/install-error" ||
    fail_test "installation before final-placement collision failed"
create_final_placement_collision
if run_installer_with_final_placement_collision \
    --version 3.1.25 \
    --prefix "$CASE_PREFIX" \
    --repository "$CASE_REPOSITORY" \
    --no-modify-path \
    >"$CASE_ROOT/upgrade-output" 2>"$CASE_ROOT/upgrade-error"; then
    fail_test "final placement overwrote a concurrent collision"
fi
assert_contains "$CASE_ROOT/upgrade-error" \
    'cannot atomically install hardknock'
assert_equal \
    "$("$CASE_PREFIX/bin/hardknock")" \
    'final-placement-collision'
find_install_transaction "$CASE_PREFIX"
assert_equal \
    "$("$FOUND_TRANSACTION/backup/bin/hardknock" --version)" \
    'hardknock 3.1.24'
pass_test

begin_test "destination-parent staging handles cross-filesystem link failure"
new_case cross-filesystem-placement
create_release "$CASE_REPOSITORY" 3.1.26 cross-filesystem-placement
create_cross_filesystem_link_once
run_installer_with_cross_filesystem_link \
    --version 3.1.26 \
    --prefix "$CASE_PREFIX" \
    --repository "$CASE_REPOSITORY" \
    --no-modify-path \
    >"$CASE_ROOT/install-output" 2>"$CASE_ROOT/install-error" ||
    fail_test "cross-filesystem placement fallback failed"
assert_equal \
    "$("$CASE_PREFIX/bin/hardknock" --version)" \
    'hardknock 3.1.26'
for placement_residue in "$CASE_PREFIX/bin"/.hardknock-place.*; do
    [ "$placement_residue" = \
        "$CASE_PREFIX/bin/.hardknock-place.*" ] ||
        fail_test "cross-filesystem staging residue remained: $placement_residue"
done
assert_no_install_residue "$CASE_PREFIX"
pass_test

begin_test "upgrade preserves a concurrent login-profile replacement"
new_case concurrent-profile
create_release "$CASE_REPOSITORY" 3.1.22 concurrent-profile-old
create_release "$CASE_REPOSITORY" 3.1.23 concurrent-profile-new
printf '%s\n' 'export ORIGINAL_PROFILE=1' >"$CASE_HOME/.profile"
run_installer \
    --version 3.1.22 \
    --prefix "$CASE_PREFIX" \
    --repository "$CASE_REPOSITORY" \
    >"$CASE_ROOT/install-output" 2>"$CASE_ROOT/install-error" ||
    fail_test "installation before profile race failed"
sed "s|^hardknock_bin=.*|hardknock_bin='/stale/hardknock/bin'|" \
    "$CASE_HOME/.profile" >"$CASE_ROOT/stale-profile"
mv "$CASE_ROOT/stale-profile" "$CASE_HOME/.profile"
create_racing_mv
if run_installer_with_racing_mv profile \
    --version 3.1.23 \
    --prefix "$CASE_PREFIX" \
    --repository "$CASE_REPOSITORY" \
    >"$CASE_ROOT/upgrade-output" 2>"$CASE_ROOT/upgrade-error"; then
    fail_test "upgrade overwrote a concurrent PATH profile replacement"
fi
assert_contains "$CASE_ROOT/upgrade-error" \
    'cannot atomically update PATH profile'
assert_equal "$(cat "$CASE_HOME/.profile")" \
    'export CONCURRENT_PROFILE=1'
assert_contains "$CASE_HOME/.profile.race-original" \
    'export ORIGINAL_PROFILE=1'
assert_equal \
    "$("$CASE_PREFIX/bin/hardknock" --version)" \
    'hardknock 3.1.22'
assert_contains "$CASE_PREFIX/share/hardknock/install-manifest-v1" \
    'version=3.1.22'
assert_no_install_residue "$CASE_PREFIX"
pass_test

begin_test "interrupted fresh install is rolled back and retried automatically"
new_case interrupted-fresh
create_release "$CASE_REPOSITORY" 3.1.10 interrupted-fresh
create_killing_mv
if run_installer_with_killing_mv 2 \
    --version 3.1.10 \
    --prefix "$CASE_PREFIX" \
    --repository "$CASE_REPOSITORY" \
    --no-modify-path \
    >"$CASE_ROOT/interrupted-output" \
    2>"$CASE_ROOT/interrupted-error"; then
    fail_test "SIGKILL fresh installation unexpectedly succeeded"
fi
assert_file "$CASE_PREFIX/.hardknock-install.lock"
find_install_transaction "$CASE_PREFIX"
assert_contains "$FOUND_TRANSACTION/metadata" \
    'hardknock-install-transaction-v1'
assert_contains "$FOUND_TRANSACTION/metadata" 'operation=install'
assert_equal "$(cat "$FOUND_TRANSACTION/phase")" active
assert_contains "$FOUND_TRANSACTION/created-directories" \
    'share/hardknock|'
rmdir "$CASE_PREFIX/share/hardknock" ||
    fail_test "could not replace interrupted transaction directory"
mkdir "$CASE_PREFIX/share/hardknock"
chmod 0700 "$CASE_PREFIX/share/hardknock"
replacement_directory_identity=$(identity_of "$CASE_PREFIX/share/hardknock")
run_installer \
    --version 3.1.10 \
    --prefix "$CASE_PREFIX" \
    --repository "$CASE_REPOSITORY" \
    --no-modify-path \
    >"$CASE_ROOT/retry-output" 2>"$CASE_ROOT/retry-error" ||
    fail_test "automatic fresh-install recovery failed"
assert_equal \
    "$("$CASE_PREFIX/bin/hardknock" --version)" \
    'hardknock 3.1.10'
assert_contains "$CASE_PREFIX/share/hardknock/install-manifest-v1" \
    'version=3.1.10'
assert_equal \
    "$(identity_of "$CASE_PREFIX/share/hardknock")" \
    "$replacement_directory_identity"
assert_no_install_residue "$CASE_PREFIX"
pass_test

begin_test "committed phase publication is never rolled back by cleanup"
new_case committed-phase
create_release "$CASE_REPOSITORY" 3.1.101 committed-phase
create_phase_publication_error_mv
if run_installer_with_phase_publication_error \
    --version 3.1.101 \
    --prefix "$CASE_PREFIX" \
    --repository "$CASE_REPOSITORY" \
    --no-modify-path \
    >"$CASE_ROOT/interrupted-output" \
    2>"$CASE_ROOT/interrupted-error"; then
    fail_test "phase-publication error unexpectedly succeeded"
fi
assert_equal "$(cat "$CASE_PHASE_STATE")" committed
assert_equal \
    "$("$CASE_PREFIX/bin/hardknock" --version)" \
    'hardknock 3.1.101'
assert_no_install_residue "$CASE_PREFIX"
run_installer \
    --version 3.1.101 \
    --prefix "$CASE_PREFIX" \
    --repository "$CASE_REPOSITORY" \
    --no-modify-path \
    >"$CASE_ROOT/retry-output" 2>"$CASE_ROOT/retry-error" ||
    fail_test "committed phase recovery failed"
assert_equal \
    "$("$CASE_PREFIX/bin/hardknock" --version)" \
    'hardknock 3.1.101'
assert_no_install_residue "$CASE_PREFIX"
pass_test

begin_test "interrupted upgrade restores the old release before retry"
new_case interrupted-upgrade
create_release "$CASE_REPOSITORY" 3.1.11 interrupted-upgrade-old
create_release "$CASE_REPOSITORY" 3.1.12 interrupted-upgrade-new
run_installer \
    --version 3.1.11 \
    --prefix "$CASE_PREFIX" \
    --repository "$CASE_REPOSITORY" \
    --no-modify-path \
    >"$CASE_ROOT/install-output" 2>"$CASE_ROOT/install-error" ||
    fail_test "installation before interrupted upgrade failed"
create_killing_mv
if run_installer_with_killing_mv 3 \
    --version 3.1.12 \
    --prefix "$CASE_PREFIX" \
    --repository "$CASE_REPOSITORY" \
    --no-modify-path \
    >"$CASE_ROOT/interrupted-output" \
    2>"$CASE_ROOT/interrupted-error"; then
    fail_test "SIGKILL upgrade unexpectedly succeeded"
fi
find_install_transaction "$CASE_PREFIX"
assert_contains "$FOUND_TRANSACTION/metadata" 'operation=install'
interrupted_binary_hash=$(sha256_file "$CASE_PREFIX/bin/hardknock")
interrupted_phase_hash=$(sha256_file "$FOUND_TRANSACTION/phase")
if run_installer \
    --version 3.1.12 \
    --prefix "$CASE_PREFIX" \
    --repository "$CASE_REPOSITORY" \
    --no-modify-path \
    --dry-run \
    --json \
    >"$CASE_ROOT/recovery-output" 2>"$CASE_ROOT/recovery-error"; then
    fail_test "dry-run unexpectedly recovered an interrupted upgrade"
fi
assert_contains "$CASE_ROOT/recovery-error" \
    'dry-run found pending installer recovery'
find_install_transaction "$CASE_PREFIX"
assert_equal \
    "$(sha256_file "$CASE_PREFIX/bin/hardknock")" \
    "$interrupted_binary_hash"
assert_equal \
    "$(sha256_file "$FOUND_TRANSACTION/phase")" \
    "$interrupted_phase_hash"
run_installer \
    --version 3.1.12 \
    --prefix "$CASE_PREFIX" \
    --repository "$CASE_REPOSITORY" \
    --no-modify-path \
    >"$CASE_ROOT/retry-output" 2>"$CASE_ROOT/retry-error" ||
    fail_test "upgrade retry after recovery failed"
assert_equal \
    "$("$CASE_PREFIX/bin/hardknock" --version)" \
    'hardknock 3.1.12'
assert_no_install_residue "$CASE_PREFIX"
pass_test

begin_test "interrupted uninstall restores the installation before retry"
new_case interrupted-uninstall
create_release "$CASE_REPOSITORY" 3.1.13 interrupted-uninstall
run_installer \
    --version 3.1.13 \
    --prefix "$CASE_PREFIX" \
    --repository "$CASE_REPOSITORY" \
    --no-modify-path \
    >"$CASE_ROOT/install-output" 2>"$CASE_ROOT/install-error" ||
    fail_test "installation before interrupted uninstall failed"
create_killing_mv
if run_installer_with_killing_mv 2 \
    --uninstall \
    --prefix "$CASE_PREFIX" \
    >"$CASE_ROOT/interrupted-output" \
    2>"$CASE_ROOT/interrupted-error"; then
    fail_test "SIGKILL uninstall unexpectedly succeeded"
fi
find_install_transaction "$CASE_PREFIX"
assert_contains "$FOUND_TRANSACTION/metadata" 'operation=uninstall'
interrupted_transaction=$FOUND_TRANSACTION
interrupted_phase_hash=$(sha256_file "$FOUND_TRANSACTION/phase")
if run_installer \
    --uninstall \
    --prefix "$CASE_PREFIX" \
    --dry-run \
    --json \
    >"$CASE_ROOT/recovery-output" 2>"$CASE_ROOT/recovery-error"; then
    fail_test "dry-run unexpectedly recovered an interrupted uninstall"
fi
assert_contains "$CASE_ROOT/recovery-error" \
    'dry-run found pending installer recovery'
assert_directory "$interrupted_transaction"
assert_equal \
    "$(sha256_file "$interrupted_transaction/phase")" \
    "$interrupted_phase_hash"
run_installer \
    --uninstall \
    --prefix "$CASE_PREFIX" \
    >"$CASE_ROOT/retry-output" 2>"$CASE_ROOT/retry-error" ||
    fail_test "uninstall retry after recovery failed"
assert_absent "$CASE_PREFIX/bin/hardknock"
assert_absent "$CASE_PREFIX/bin/hk-effect"
assert_absent "$CASE_PREFIX/share/hardknock/install-manifest-v1"
assert_no_install_residue "$CASE_PREFIX"
pass_test

begin_test "uninstall dry-run reports removals without changing files"
new_case uninstall-dry-run
create_release "$CASE_REPOSITORY" 3.1.1 uninstall-dry-run
run_installer \
    --version 3.1.1 \
    --prefix "$CASE_PREFIX" \
    --repository "$CASE_REPOSITORY" \
    --no-modify-path \
    >"$CASE_ROOT/install-output" 2>"$CASE_ROOT/install-error" ||
    fail_test "installation before uninstall dry-run failed"
run_installer \
    --uninstall \
    --prefix "$CASE_PREFIX" \
    --dry-run \
    --json \
    >"$CASE_ROOT/output" 2>"$CASE_ROOT/error" ||
    fail_test "uninstall dry-run failed"
assert_contains "$CASE_ROOT/output" '"action":"uninstall"'
assert_contains "$CASE_ROOT/output" '"dry_run":true'
assert_contains "$CASE_ROOT/output" '"action":"remove"'
assert_executable "$CASE_PREFIX/bin/hardknock"
assert_file "$CASE_PREFIX/share/hardknock/install-manifest-v1"
assert_file "$CASE_DATA_HOME/user-data"
pass_test

begin_test "failed uninstall restores files and PATH profile"
new_case uninstall-rollback
create_release "$CASE_REPOSITORY" 3.1.2 uninstall-rollback
printf '%s\n' 'export KEEP_AFTER_FAILED_UNINSTALL=1' >"$CASE_HOME/.profile"
run_installer \
    --version 3.1.2 \
    --prefix "$CASE_PREFIX" \
    --repository "$CASE_REPOSITORY" \
    >"$CASE_ROOT/install-output" 2>"$CASE_ROOT/install-error" ||
    fail_test "installation before rollback test failed"
profile_hash_before=$(sha256_file "$CASE_HOME/.profile")
manifest_hash_before=$(
    sha256_file "$CASE_PREFIX/share/hardknock/install-manifest-v1"
)
create_failing_mv
if run_installer_with_failing_mv 4 \
    --uninstall \
    --prefix "$CASE_PREFIX" \
    >"$CASE_ROOT/output" 2>"$CASE_ROOT/error"; then
    fail_test "injected uninstall failure unexpectedly succeeded"
fi
assert_contains "$CASE_ROOT/error" 'cannot remove managed hk-effect binary'
assert_executable "$CASE_PREFIX/bin/hardknock"
assert_executable "$CASE_PREFIX/bin/hk-effect"
assert_file "$CASE_PREFIX/share/doc/hardknock/LICENSE"
assert_file "$CASE_PREFIX/share/doc/hardknock/NOTICE"
assert_equal \
    "$(sha256_file "$CASE_HOME/.profile")" \
    "$profile_hash_before"
assert_equal \
    "$(sha256_file "$CASE_PREFIX/share/hardknock/install-manifest-v1")" \
    "$manifest_hash_before"
assert_no_install_residue "$CASE_PREFIX"
assert_file "$CASE_DATA_HOME/user-data"
pass_test

begin_test "file URL mirror remains available for offline agents"
new_case file-url
create_release "$CASE_REPOSITORY" 3.1.3 file-url
run_installer \
    --version 3.1.3 \
    --prefix "$CASE_PREFIX" \
    --repository "file://$CASE_REPOSITORY" \
    --no-modify-path \
    --dry-run \
    --json \
    >"$CASE_ROOT/output" 2>"$CASE_ROOT/error" ||
    fail_test "file URL mirror dry-run failed"
assert_contains "$CASE_ROOT/output" '"source":"local"'
assert_contains "$CASE_ROOT/output" '"status":"checksum_only_local"'
assert_contains "$CASE_ROOT/output" \
    'build provenance was not independently verified'
run_installer \
    --version 3.1.3 \
    --prefix "$CASE_PREFIX" \
    --repository "file://$CASE_REPOSITORY" \
    --no-modify-path \
    --dry-run \
    >"$CASE_ROOT/human-output" 2>"$CASE_ROOT/human-error" ||
    fail_test "file URL mirror human dry-run failed"
assert_contains "$CASE_ROOT/human-error" \
    'build provenance was not independently verified'
assert_absent "$CASE_PREFIX"
assert_file "$CASE_DATA_HOME/user-data"
pass_test

begin_test "HTTPS downloads use hardened curl options without network access"
new_case https-curl
create_release "$CASE_REPOSITORY" 3.1.4 https-curl
create_fake_curl
run_installer_with_fake_curl 3.1.4 \
    --version 3.1.4 \
    --prefix "$CASE_PREFIX" \
    --repository https://releases.example.invalid \
    --no-verify-provenance \
    --no-modify-path \
    --dry-run \
    --json \
    >"$CASE_ROOT/output" 2>"$CASE_ROOT/error" ||
    fail_test "hardened HTTPS dry-run failed"
assert_contains "$CASE_ROOT/output" '"source":"https"'
assert_contains "$CASE_ROOT/output" '"status":"bypassed"'
assert_contains "$CASE_ROOT/output" \
    'build-provenance verification was explicitly bypassed'
assert_contains "$CASE_CURL_LOG" 'disable'
assert_contains "$CASE_CURL_LOG" '=https'
assert_contains "$CASE_CURL_LOG" 'tlsv1.2'
assert_contains "$CASE_CURL_LOG" \
    'https://releases.example.invalid/v3.1.4/'
assert_absent "$CASE_PREFIX"
assert_file "$CASE_DATA_HOME/user-data"
pass_test

begin_test "official HTTPS release provenance verifies with fake gh"
new_case provenance-success
create_release "$CASE_REPOSITORY" 3.1.7 provenance-success
create_fake_curl
create_fake_gh success
CASE_GH_VERSION=2.97.0
run_installer_with_fake_curl 3.1.7 \
    --version 3.1.7 \
    --prefix "$CASE_PREFIX" \
    --no-modify-path \
    --dry-run \
    --json \
    >"$CASE_ROOT/output" 2>"$CASE_ROOT/error" ||
    fail_test "official provenance verification failed"
assert_contains "$CASE_ROOT/output" '"status":"verified"'
assert_contains "$CASE_ROOT/output" '"verified":true'
assert_contains "$CASE_ROOT/output" '"policy":"official-required"'
assert_contains "$CASE_GH_LOG" 'attestation'
assert_contains "$CASE_GH_LOG" 'verify'
assert_contains "$CASE_GH_LOG" 'release'
assert_contains "$CASE_GH_LOG" 'verify-asset'
assert_contains "$CASE_GH_LOG" 'v3.1.7'
assert_contains "$CASE_GH_LOG" "hardknock-3.1.7-$TARGET.tar.gz"
assert_contains "$CASE_GH_LOG" 'openkedge/hardknock'
assert_contains "$CASE_GH_LOG" '--signer-workflow'
assert_contains "$CASE_GH_LOG" \
    'openkedge/hardknock/.github/workflows/release.yml'
assert_contains "$CASE_GH_LOG" '--source-ref'
assert_contains "$CASE_GH_LOG" 'refs/heads/main'
assert_contains "$CASE_GH_LOG" '--source-digest'
assert_contains "$CASE_GH_LOG" '--signer-digest'
assert_contains "$CASE_GH_LOG" "$DEFAULT_WORKFLOW_COMMIT"
assert_contains "$CASE_GH_LOG" '--deny-self-hosted-runners'
assert_contains "$CASE_GH_LOG" \
    'https://openkedge.dev/hardknock/release-publication/v1'
assert_contains "$CASE_GH_LOG" 'https://slsa.dev/provenance/v1'
assert_contains "$CASE_GH_LOG" '--format'
assert_contains "$CASE_GH_LOG" 'json'
assert_contains "$CASE_GH_LOG" '--jq'
assert_contains "$CASE_GH_LOG" '.verificationResult.statement'
assert_contains "$CASE_GH_LOG" \
    'repos/openkedge/hardknock/git/ref/tags/v3.1.7'
assert_contains "$CASE_GH_LOG" \
    'repos/openkedge/hardknock/git/tags/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa'
assert_contains "$CASE_GH_LOG" \
    "repos/openkedge/hardknock/git/commits/$DEFAULT_TAG_COMMIT"
assert_contains "$CASE_GH_LOG" \
    "repos/openkedge/hardknock/compare/$DEFAULT_WORKFLOW_COMMIT...$DEFAULT_BRANCH_HEAD"
assert_contains "$CASE_CURL_LOG" \
    "https://github.com/openkedge/hardknock/releases/download/v3.1.7/hardknock-3.1.7-$TARGET.tar.gz"
run_installer_with_fake_curl 3.1.7 \
    --version 3.1.7 \
    --prefix "$CASE_PREFIX" \
    --no-modify-path \
    --dry-run \
    >"$CASE_ROOT/human-output" 2>"$CASE_ROOT/human-error" ||
    fail_test "official human provenance verification failed"
assert_contains "$CASE_ROOT/human-output" \
    "Verified GitHub build provenance for hardknock-3.1.7-$TARGET.tar.gz"
assert_fixed_count "$CASE_GH_LOG" 'verify-asset' 4
assert_fixed_count "$CASE_GH_LOG" \
    'https://openkedge.dev/hardknock/release-publication/v1' 4
assert_fixed_count "$CASE_GH_LOG" 'https://slsa.dev/provenance/v1' 2
assert_fixed_count "$CASE_GH_LOG" '--limit' 6
assert_fixed_count "$CASE_GH_LOG" '--source-digest' 4
assert_fixed_count "$CASE_GH_LOG" '--signer-digest' 4
assert_absent "$CASE_PREFIX"
assert_file "$CASE_DATA_HOME/user-data"
unset CASE_GH_VERSION
pass_test

begin_test "official provenance rejects an outdated GitHub CLI"
new_case provenance-old-gh
create_release "$CASE_REPOSITORY" 3.1.71 provenance-old-gh
create_fake_curl
create_fake_gh success
CASE_GH_VERSION=2.96.99
if run_installer_with_fake_curl 3.1.71 \
    --version 3.1.71 \
    --prefix "$CASE_PREFIX" \
    --no-modify-path \
    --dry-run \
    >"$CASE_ROOT/output" 2>"$CASE_ROOT/error"; then
    fail_test "outdated GitHub CLI unexpectedly verified provenance"
fi
assert_contains "$CASE_ROOT/error" \
    'GitHub CLI 2.97.0 or newer is required'
assert_contains "$CASE_GH_LOG" '--version'
assert_not_contains "$CASE_GH_LOG" 'verify-asset'
assert_absent "$CASE_PREFIX"
unset CASE_GH_VERSION
pass_test

begin_test "official provenance rejects malformed GitHub CLI versions"
new_case provenance-malformed-gh
create_release "$CASE_REPOSITORY" 3.1.72 provenance-malformed-gh
create_fake_curl
create_fake_gh success
CASE_GH_VERSION=9999999999.93.0
if run_installer_with_fake_curl 3.1.72 \
    --version 3.1.72 \
    --prefix "$CASE_PREFIX" \
    --no-modify-path \
    --dry-run \
    >"$CASE_ROOT/output" 2>"$CASE_ROOT/error"; then
    fail_test "malformed GitHub CLI version unexpectedly accepted"
fi
assert_contains "$CASE_ROOT/error" \
    'GitHub CLI returned an unsupported version format'
assert_not_contains "$CASE_GH_LOG" 'verify-asset'
assert_absent "$CASE_PREFIX"
unset CASE_GH_VERSION
pass_test

begin_test "official provenance rejects malformed stable-tag digests"
new_case provenance-malformed-tag
create_release "$CASE_REPOSITORY" 3.1.73 provenance-malformed-tag
create_fake_curl
create_fake_gh success
CASE_GH_TAG_MODE=malformed
if run_installer_with_fake_curl 3.1.73 \
    --version 3.1.73 \
    --prefix "$CASE_PREFIX" \
    --no-modify-path \
    --dry-run \
    >"$CASE_ROOT/output" 2>"$CASE_ROOT/error"; then
    fail_test "malformed stable-tag digest unexpectedly verified"
fi
assert_contains "$CASE_ROOT/error" \
    'GitHub returned an invalid stable-tag object digest'
assert_contains "$CASE_GH_LOG" \
    'repos/openkedge/hardknock/git/ref/tags/v3.1.73'
assert_not_contains "$CASE_GH_LOG" '--source-digest'
assert_absent "$CASE_PREFIX"
unset CASE_GH_TAG_MODE
pass_test

begin_test "official provenance rejects a lightweight stable tag"
new_case provenance-lightweight-tag
create_release "$CASE_REPOSITORY" 3.1.731 provenance-lightweight-tag
create_fake_curl
create_fake_gh success
CASE_GH_TAG_MODE=lightweight
expect_official_provenance_failure 3.1.731 \
    'official stable tag must be a signed annotated tag'
assert_not_contains "$CASE_GH_LOG" '/git/tags/'
unset CASE_GH_TAG_MODE
pass_test

begin_test "official provenance rejects an invalid stable-tag signature"
new_case provenance-bad-tag-signature
create_release "$CASE_REPOSITORY" 3.1.732 provenance-bad-tag-signature
create_fake_curl
create_fake_gh success
CASE_GH_TAG_MODE=bad_signature
expect_official_provenance_failure 3.1.732 \
    'official stable-tag signature is not valid'
assert_not_contains "$CASE_GH_LOG" '/git/commits/'
unset CASE_GH_TAG_MODE
pass_test

begin_test "official provenance rejects a nested annotated stable tag"
new_case provenance-nested-tag
create_release "$CASE_REPOSITORY" 3.1.733 provenance-nested-tag
create_fake_curl
create_fake_gh success
CASE_GH_TAG_MODE=nested
expect_official_provenance_failure 3.1.733 \
    'official stable tag must point directly to a commit'
assert_not_contains "$CASE_GH_LOG" '/git/commits/'
unset CASE_GH_TAG_MODE
pass_test

begin_test "official provenance fails closed when stable-tag resolution fails"
new_case provenance-tag-resolution-failure
create_release "$CASE_REPOSITORY" 3.1.74 provenance-tag-resolution-failure
create_fake_curl
create_fake_gh success
CASE_GH_TAG_MODE=failure
if run_installer_with_fake_curl 3.1.74 \
    --version 3.1.74 \
    --prefix "$CASE_PREFIX" \
    --no-modify-path \
    --dry-run \
    >"$CASE_ROOT/output" 2>"$CASE_ROOT/error"; then
    fail_test "failed stable-tag resolution unexpectedly verified"
fi
assert_contains "$CASE_ROOT/error" \
    'cannot resolve the official stable tag'
assert_contains "$CASE_GH_LOG" \
    'repos/openkedge/hardknock/git/ref/tags/v3.1.74'
assert_not_contains "$CASE_GH_LOG" '--source-digest'
assert_absent "$CASE_PREFIX"
unset CASE_GH_TAG_MODE
pass_test

begin_test "official stable-tag resolution has a wall-clock timeout"
new_case provenance-tag-resolution-timeout
create_release "$CASE_REPOSITORY" 3.1.75 provenance-tag-resolution-timeout
create_fake_curl
create_fake_gh success
CASE_GH_TAG_MODE=hang
CASE_PROVENANCE_TIMEOUT_SECONDS=1
if run_installer_with_fake_curl 3.1.75 \
    --version 3.1.75 \
    --prefix "$CASE_PREFIX" \
    --no-modify-path \
    --dry-run \
    >"$CASE_ROOT/output" 2>"$CASE_ROOT/error"; then
    fail_test "stable-tag resolution timeout unexpectedly verified"
fi
assert_contains "$CASE_ROOT/error" \
    'official stable-tag resolution timed out'
assert_not_contains "$CASE_GH_LOG" '--source-digest'
assert_absent "$CASE_PREFIX"
unset CASE_GH_TAG_MODE
unset CASE_PROVENANCE_TIMEOUT_SECONDS
pass_test

begin_test "official HTTPS release fails closed on bad provenance"
new_case provenance-failure
create_release "$CASE_REPOSITORY" 3.1.8 provenance-failure
create_fake_curl
create_fake_gh attestation_failure
if run_installer_with_fake_curl 3.1.8 \
    --version 3.1.8 \
    --prefix "$CASE_PREFIX" \
    --no-modify-path \
    --dry-run \
    >"$CASE_ROOT/output" 2>"$CASE_ROOT/error"; then
    fail_test "invalid official provenance unexpectedly installed"
fi
assert_contains "$CASE_ROOT/error" \
    'official release attestation discovery failed'
assert_not_contains "$CASE_ROOT/error" \
    'private attestation verifier detail'
assert_contains "$CASE_GH_LOG" 'openkedge/hardknock'
assert_absent "$CASE_PREFIX"
assert_file "$CASE_DATA_HOME/user-data"
pass_test

begin_test "official provenance accepts duplicate identical custom bindings"
new_case provenance-duplicate-binding
create_release "$CASE_REPOSITORY" 3.1.81 provenance-duplicate-binding
create_fake_curl
create_fake_gh success
CASE_GH_ATTESTATION_MODE=duplicate
CASE_DEFAULT_BRANCH=release/stable
run_installer_with_fake_curl 3.1.81 \
    --version 3.1.81 \
    --prefix "$CASE_PREFIX" \
    --no-modify-path \
    --dry-run \
    --json \
    >"$CASE_ROOT/output" 2>"$CASE_ROOT/error" ||
    fail_test "duplicate identical attestation bindings were rejected"
assert_contains "$CASE_ROOT/output" '"status":"verified"'
assert_contains "$CASE_GH_LOG" 'refs/heads/release/stable'
assert_contains "$CASE_GH_LOG" \
    'repos/openkedge/hardknock/branches/release%2Fstable'
unset CASE_GH_ATTESTATION_MODE
unset CASE_DEFAULT_BRANCH
pass_test

begin_test "official provenance rejects saturated discovery results"
new_case provenance-discovery-saturation
create_release "$CASE_REPOSITORY" 3.1.811 provenance-discovery-saturation
create_fake_curl
create_fake_gh success
CASE_GH_ATTESTATION_MODE=discovery_saturation
expect_official_provenance_failure 3.1.811 \
    'official release attestation discovery lookup reached the configured limit'
assert_fixed_count "$CASE_GH_LOG" '--limit' 1
unset CASE_GH_ATTESTATION_MODE
pass_test

begin_test "official provenance rejects saturated exact-binding results"
new_case provenance-exact-saturation
create_release "$CASE_REPOSITORY" 3.1.812 provenance-exact-saturation
create_fake_curl
create_fake_gh success
CASE_GH_ATTESTATION_MODE=exact_saturation
expect_official_provenance_failure 3.1.812 \
    'exact official release attestation lookup reached the configured limit'
assert_fixed_count "$CASE_GH_LOG" '--limit' 2
unset CASE_GH_ATTESTATION_MODE
pass_test

begin_test "official provenance rejects saturated SLSA results"
new_case provenance-slsa-saturation
create_release "$CASE_REPOSITORY" 3.1.813 provenance-slsa-saturation
create_fake_curl
create_fake_gh success
CASE_GH_ATTESTATION_MODE=slsa_saturation
expect_official_provenance_failure 3.1.813 \
    'official SLSA attestation lookup reached the configured limit'
assert_fixed_count "$CASE_GH_LOG" '--limit' 3
unset CASE_GH_ATTESTATION_MODE
pass_test

begin_test "official provenance rejects malformed custom predicates"
new_case provenance-malformed-predicate
create_release "$CASE_REPOSITORY" 3.1.82 provenance-malformed-predicate
create_fake_curl
create_fake_gh success
CASE_GH_ATTESTATION_MODE=malformed
expect_official_provenance_failure 3.1.82 \
    'official release attestation predicate is malformed'
unset CASE_GH_ATTESTATION_MODE
pass_test

begin_test "official provenance rejects multiple distinct custom bindings"
new_case provenance-multiple-bindings
create_release "$CASE_REPOSITORY" 3.1.83 provenance-multiple-bindings
create_fake_curl
create_fake_gh success
CASE_GH_ATTESTATION_MODE=multiple
expect_official_provenance_failure 3.1.83 \
    'official release attestations contain multiple distinct bindings'
unset CASE_GH_ATTESTATION_MODE
pass_test

begin_test "official provenance rejects a binding that changes on exact verification"
new_case provenance-conflicting-binding
create_release "$CASE_REPOSITORY" 3.1.84 provenance-conflicting-binding
create_fake_curl
create_fake_gh success
CASE_GH_ATTESTATION_MODE=conflicting
expect_official_provenance_failure 3.1.84 \
    'exact official release attestation returned a different binding'
assert_contains "$CASE_GH_LOG" '--signer-digest'
unset CASE_GH_ATTESTATION_MODE
pass_test

begin_test "official provenance requires the exact stable predicate key set"
new_case provenance-predicate-key-set
create_release "$CASE_REPOSITORY" 3.1.85 provenance-predicate-key-set
create_fake_curl
create_fake_gh success
CASE_GH_ATTESTATION_MODE=bad_predicate_keys
expect_official_provenance_failure 3.1.85 \
    'official release attestation predicate does not match the stable release contract'
unset CASE_GH_ATTESTATION_MODE
pass_test

begin_test "official provenance requires the exact twelve-asset key set"
new_case provenance-asset-key-set
create_release "$CASE_REPOSITORY" 3.1.86 provenance-asset-key-set
create_fake_curl
create_fake_gh success
CASE_GH_ATTESTATION_MODE=bad_asset_keys
expect_official_provenance_failure 3.1.86 \
    'official release attestation has an unexpected asset set'
unset CASE_GH_ATTESTATION_MODE
pass_test

begin_test "official provenance requires lowercase asset hashes"
new_case provenance-uppercase-asset-hash
create_release "$CASE_REPOSITORY" 3.1.87 provenance-uppercase-asset-hash
create_fake_curl
create_fake_gh success
CASE_GH_ATTESTATION_MODE=uppercase_hash
expect_official_provenance_failure 3.1.87 \
    'official release attestation has a malformed asset digest'
unset CASE_GH_ATTESTATION_MODE
pass_test

begin_test "official provenance requires the local archive digest"
new_case provenance-wrong-archive-digest
create_release "$CASE_REPOSITORY" 3.1.88 provenance-wrong-archive-digest
create_fake_curl
create_fake_gh success
CASE_GH_ATTESTATION_MODE=wrong_archive_digest
expect_official_provenance_failure 3.1.88 \
    'official release attestation does not bind the downloaded archive'
unset CASE_GH_ATTESTATION_MODE
pass_test

begin_test "official provenance binds the signed tag artifact commit"
new_case provenance-artifact-commit
create_release "$CASE_REPOSITORY" 3.1.89 provenance-artifact-commit
create_fake_curl
create_fake_gh success
CASE_GH_ATTESTATION_MODE=artifact_mismatch
expect_official_provenance_failure 3.1.89 \
    'official release attestation does not bind the signed stable tag'
unset CASE_GH_ATTESTATION_MODE
pass_test

begin_test "official provenance binds the signed tag artifact tree"
new_case provenance-artifact-tree
create_release "$CASE_REPOSITORY" 3.1.891 provenance-artifact-tree
create_fake_curl
create_fake_gh success
CASE_GH_ATTESTATION_MODE=artifact_tree_mismatch
expect_official_provenance_failure 3.1.891 \
    'official release attestation does not bind the signed stable tag'
unset CASE_GH_ATTESTATION_MODE
pass_test

begin_test "official provenance requires the requested stable release tag"
new_case provenance-release-tag
create_release "$CASE_REPOSITORY" 3.1.892 provenance-release-tag
create_fake_curl
create_fake_gh success
CASE_GH_ATTESTATION_MODE=wrong_release_tag
expect_official_provenance_failure 3.1.892 \
    'official release attestation predicate does not match the stable release contract'
unset CASE_GH_ATTESTATION_MODE
pass_test

begin_test "official provenance requires the exact default-branch workflow ref"
new_case provenance-workflow-ref
create_release "$CASE_REPOSITORY" 3.1.893 provenance-workflow-ref
create_fake_curl
create_fake_gh success
CASE_GH_ATTESTATION_MODE=workflow_ref_mismatch
expect_official_provenance_failure 3.1.893 \
    'official release attestation does not bind the default-branch workflow'
unset CASE_GH_ATTESTATION_MODE
pass_test

begin_test "official provenance requires a lowercase workflow source commit"
new_case provenance-workflow-commit
create_release "$CASE_REPOSITORY" 3.1.894 provenance-workflow-commit
create_fake_curl
create_fake_gh success
CASE_GH_ATTESTATION_MODE=bad_workflow_commit
expect_official_provenance_failure 3.1.894 \
    'official release attestation has an invalid workflow source commit'
unset CASE_GH_ATTESTATION_MODE
pass_test

begin_test "official provenance fails closed when SLSA verification fails"
new_case provenance-slsa-failure
create_release "$CASE_REPOSITORY" 3.1.895 provenance-slsa-failure
create_fake_curl
create_fake_gh success
CASE_GH_ATTESTATION_MODE=slsa_failure
expect_official_provenance_failure 3.1.895 \
    'official SLSA provenance verification failed'
assert_contains "$CASE_GH_LOG" 'https://slsa.dev/provenance/v1'
unset CASE_GH_ATTESTATION_MODE
pass_test

begin_test "official HTTPS release fails closed on release verification"
new_case release-verification-failure
create_release "$CASE_REPOSITORY" 3.2.2 release-verification-failure
create_fake_curl
create_fake_gh release_failure
if run_installer_with_fake_curl 3.2.2 \
    --version 3.2.2 \
    --prefix "$CASE_PREFIX" \
    --no-modify-path \
    --dry-run \
    --json \
    >"$CASE_ROOT/output" 2>"$CASE_ROOT/error"; then
    fail_test "invalid official release unexpectedly accepted"
fi
assert_contains "$CASE_ROOT/error" '"ok":false'
assert_contains "$CASE_ROOT/error" \
    'official release verification failed'
assert_not_contains "$CASE_ROOT/error" \
    'private release verifier detail'
assert_not_contains "$CASE_GH_LOG" 'verify-asset'
assert_contains "$CASE_GH_LOG" 'v3.2.2'
assert_not_contains "$CASE_GH_LOG" 'attestation'
assert_absent "$CASE_PREFIX"
assert_file "$CASE_DATA_HOME/user-data"
pass_test

begin_test "official HTTPS release fails closed on initial asset verification"
new_case release-asset-verification-failure
create_release "$CASE_REPOSITORY" 3.2.21 release-asset-verification-failure
create_fake_curl
create_fake_gh release_asset_failure
expect_official_provenance_failure 3.2.21 \
    'official release asset verification failed'
assert_not_contains "$CASE_ROOT/error" \
    'private release asset verifier detail'
assert_not_contains "$CASE_GH_LOG" 'attestation'
pass_test

begin_test "official provenance rejects a promotion outside default-branch history"
new_case provenance-ancestry-failure
create_release "$CASE_REPOSITORY" 3.2.22 provenance-ancestry-failure
create_fake_curl
create_fake_gh success
CASE_GH_BRANCH_MODE=ancestry_failure
expect_official_provenance_failure 3.2.22 \
    'official promotion commit is not in current default-branch history'
unset CASE_GH_BRANCH_MODE
pass_test

begin_test "official provenance rejects a default branch that changes during verification"
new_case provenance-default-branch-change
create_release "$CASE_REPOSITORY" 3.2.23 provenance-default-branch-change
create_fake_curl
create_fake_gh success
CASE_GH_BRANCH_MODE=changed_default
expect_official_provenance_failure 3.2.23 \
    'official default branch changed during provenance verification'
unset CASE_GH_BRANCH_MODE
pass_test

begin_test "official provenance rejects a stable tag that changes during verification"
new_case provenance-tag-change
create_release "$CASE_REPOSITORY" 3.2.24 provenance-tag-change
create_fake_curl
create_fake_gh success
CASE_GH_TAG_MODE=changed_tag
expect_official_provenance_failure 3.2.24 \
    'official stable tag changed during provenance verification'
assert_fixed_count "$CASE_GH_LOG" 'verify-asset' 1
unset CASE_GH_TAG_MODE
pass_test

begin_test "official provenance rejects a stable-tag tree that changes"
new_case provenance-tree-change
create_release "$CASE_REPOSITORY" 3.2.25 provenance-tree-change
create_fake_curl
create_fake_gh success
CASE_GH_TAG_MODE=changed_tree
expect_official_provenance_failure 3.2.25 \
    'official stable tag changed during provenance verification'
assert_fixed_count "$CASE_GH_LOG" 'verify-asset' 1
unset CASE_GH_TAG_MODE
pass_test

begin_test "official provenance reruns final asset verification"
new_case provenance-final-asset-failure
create_release "$CASE_REPOSITORY" 3.2.26 provenance-final-asset-failure
create_fake_curl
create_fake_gh final_asset_failure
expect_official_provenance_failure 3.2.26 \
    'final official release asset verification failed'
assert_fixed_count "$CASE_GH_LOG" 'verify-asset' 2
pass_test

begin_test "official HTTPS provenance verification has a wall-clock timeout"
new_case provenance-timeout
create_release "$CASE_REPOSITORY" 3.2.3 provenance-timeout
create_fake_curl
create_fake_gh hang
CASE_PROVENANCE_TIMEOUT_SECONDS=1
timeout_started=$(date +%s)
if run_installer_with_fake_curl 3.2.3 \
    --version 3.2.3 \
    --prefix "$CASE_PREFIX" \
    --no-modify-path \
    --dry-run \
    --json \
    >"$CASE_ROOT/output" 2>"$CASE_ROOT/error"; then
    fail_test "hung official release verifier unexpectedly accepted"
fi
timeout_elapsed=$(($(date +%s) - timeout_started))
[ "$timeout_elapsed" -le 5 ] ||
    fail_test "provenance timeout took ${timeout_elapsed}s"
assert_contains "$CASE_ROOT/error" \
    'official release verification timed out'
assert_not_contains "$CASE_ROOT/error" 'Verification succeeded!'
assert_absent "$CASE_PREFIX"
assert_file "$CASE_DATA_HOME/user-data"
unset CASE_PROVENANCE_TIMEOUT_SECONDS
pass_test

begin_test "custom HTTPS source requires an explicit provenance bypass"
new_case custom-https-policy
create_release "$CASE_REPOSITORY" 3.2.1 custom-https-policy
create_fake_curl
if run_installer_with_fake_curl 3.2.1 \
    --version 3.2.1 \
    --prefix "$CASE_PREFIX" \
    --repository https://releases.example.invalid \
    --no-modify-path \
    --dry-run \
    >"$CASE_ROOT/output" 2>"$CASE_ROOT/error"; then
    fail_test "custom HTTPS source unexpectedly skipped explicit policy"
fi
assert_contains "$CASE_ROOT/error" \
    'custom HTTPS repositories require --no-verify-provenance'
assert_line_count "$CASE_CURL_LOG" 0
assert_absent "$CASE_PREFIX"
assert_file "$CASE_DATA_HOME/user-data"
pass_test

begin_test "official HTTPS release fails closed when verifier is unavailable"
new_case provenance-unavailable
create_release "$CASE_REPOSITORY" 3.1.9 provenance-unavailable
create_fake_curl
create_fake_gh unavailable
if run_installer_with_fake_curl 3.1.9 \
    --version 3.1.9 \
    --prefix "$CASE_PREFIX" \
    --no-modify-path \
    --dry-run \
    --json \
    >"$CASE_ROOT/output" 2>"$CASE_ROOT/error"; then
    fail_test "unavailable provenance verifier unexpectedly accepted"
fi
assert_contains "$CASE_ROOT/error" '"ok":false'
assert_contains "$CASE_ROOT/error" \
    'gh build-provenance verifier is unavailable'
assert_contains "$CASE_GH_LOG" 'api'
assert_not_contains "$CASE_GH_LOG" 'release'
assert_not_contains "$CASE_GH_LOG" 'attestation'
assert_absent "$CASE_PREFIX"
assert_file "$CASE_DATA_HOME/user-data"
pass_test

begin_test "official HTTPS provenance bypass is explicit and skips gh"
new_case provenance-bypass
create_release "$CASE_REPOSITORY" 3.2.0 provenance-bypass
create_fake_curl
create_fake_gh failure
run_installer_with_fake_curl 3.2.0 \
    --version 3.2.0 \
    --prefix "$CASE_PREFIX" \
    --no-verify-provenance \
    --no-modify-path \
    --dry-run \
    --json \
    >"$CASE_ROOT/output" 2>"$CASE_ROOT/error" ||
    fail_test "explicit provenance bypass failed"
assert_contains "$CASE_ROOT/output" '"status":"bypassed"'
assert_contains "$CASE_ROOT/output" '"verified":false'
assert_contains "$CASE_ROOT/output" \
    'explicitly bypassed with --no-verify-provenance'
run_installer_with_fake_curl 3.2.0 \
    --version 3.2.0 \
    --prefix "$CASE_PREFIX" \
    --no-verify-provenance \
    --no-modify-path \
    --dry-run \
    >"$CASE_ROOT/human-output" 2>"$CASE_ROOT/human-error" ||
    fail_test "human provenance bypass failed"
assert_contains "$CASE_ROOT/human-error" \
    'explicitly bypassed with --no-verify-provenance'
assert_line_count "$CASE_GH_LOG" 0
assert_absent "$CASE_PREFIX"
assert_file "$CASE_DATA_HOME/user-data"
pass_test

begin_test "downloaded candidate binary has a wall-clock timeout"
new_case candidate-timeout
create_hanging_release "$CASE_REPOSITORY" 3.2.4 candidate-timeout
CASE_CANDIDATE_TIMEOUT_SECONDS=1
timeout_started=$(date +%s)
if run_installer \
    --version 3.2.4 \
    --prefix "$CASE_PREFIX" \
    --repository "$CASE_REPOSITORY" \
    --no-modify-path \
    --json \
    >"$CASE_ROOT/output" 2>"$CASE_ROOT/error"; then
    fail_test "hung candidate binary unexpectedly installed"
fi
timeout_elapsed=$(($(date +%s) - timeout_started))
[ "$timeout_elapsed" -le 5 ] ||
    fail_test "candidate timeout took ${timeout_elapsed}s"
assert_contains "$CASE_ROOT/error" \
    'hardknock release binary version check timed out'
assert_absent "$CASE_PREFIX"
assert_file "$CASE_DATA_HOME/user-data"
unset CASE_CANDIDATE_TIMEOUT_SECONDS
pass_test

begin_test "malformed abandoned transaction fails closed"
new_case stale-transaction
create_release "$CASE_REPOSITORY" 3.1.5 stale-transaction
mkdir "$CASE_PREFIX"
chmod 0700 "$CASE_PREFIX"
mkdir "$CASE_PREFIX/.hardknock-install.abandoned"
chmod 0700 "$CASE_PREFIX/.hardknock-install.abandoned"
if run_installer \
    --version 3.1.5 \
    --prefix "$CASE_PREFIX" \
    --repository "$CASE_REPOSITORY" \
    --no-modify-path \
    >"$CASE_ROOT/output" 2>"$CASE_ROOT/error"; then
    fail_test "abandoned transaction unexpectedly ignored"
fi
assert_contains "$CASE_ROOT/error" \
    'installer transaction is missing required recovery state'
assert_absent "$CASE_PREFIX/bin/hardknock"
assert_directory "$CASE_PREFIX/.hardknock-install.abandoned"
assert_file "$CASE_DATA_HOME/user-data"
pass_test

begin_test "default prefix installs beneath HOME without prompts"
new_case default-prefix
create_release "$CASE_REPOSITORY" 3.1.6 default-prefix
default_prefix=$CASE_HOME/.local
run_installer \
    --version 3.1.6 \
    --repository "$CASE_REPOSITORY" \
    --no-modify-path \
    --json \
    >"$CASE_ROOT/output" 2>"$CASE_ROOT/error" ||
    fail_test "default-prefix installation failed"
assert_executable "$default_prefix/bin/hardknock"
assert_executable "$default_prefix/bin/hk-effect"
assert_contains "$CASE_ROOT/output" "\"prefix\":\"$default_prefix\""
assert_absent "$CASE_HOME/.profile"
assert_file "$CASE_DATA_HOME/user-data"
pass_test

printf '1..%s\n' "$TEST_NUMBER"
