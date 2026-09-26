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
    chmod 0755 "$CASE_TOOLS/mv"
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
    CASE_GH_MODE=${1:-success}
    mkdir -p "$CASE_TOOLS"
    : >"$CASE_GH_LOG"
    {
        printf '%s\n' '#!/bin/sh'
        printf '%s\n' 'set -eu'
        printf '%s\n' \
            'printf "%s\n" "-- invocation --" >>"${HARDKNOCK_TEST_GH_LOG:?}"'
        printf '%s\n' 'for argument in "$@"; do'
        printf '%s\n' \
            '    printf "%s\n" "$argument" >>"${HARDKNOCK_TEST_GH_LOG:?}"'
        printf '%s\n' 'done'
        printf '%s\n' 'case "${HARDKNOCK_TEST_GH_MODE:?}" in'
        printf '%s\n' '    success)'
        printf '%s\n' \
            '        printf "%s\n" "Loaded digest sha256:test"'
        printf '%s\n' \
            '        printf "%s\n" "Verification succeeded!"'
        printf '%s\n' '        ;;'
        printf '%s\n' '    release_failure)'
        printf '%s\n' '        if [ "${1-}" = release ]; then'
        printf '%s\n' \
            '            printf "%s\n" "private release verifier detail" >&2'
        printf '%s\n' '            exit 1'
        printf '%s\n' '        fi'
        printf '%s\n' '        ;;'
        printf '%s\n' '    attestation_failure)'
        printf '%s\n' '        if [ "${1-}" = attestation ]; then'
        printf '%s\n' \
            '            printf "%s\n" "private attestation verifier detail" >&2'
        printf '%s\n' '            exit 1'
        printf '%s\n' '        fi'
        printf '%s\n' '        ;;'
        printf '%s\n' '    failure)'
        printf '%s\n' \
            '        printf "%s\n" "attestation did not verify" >&2'
        printf '%s\n' '        exit 1'
        printf '%s\n' '        ;;'
        printf '%s\n' '    hang)'
        printf '%s\n' '        sleep 20'
        printf '%s\n' '        ;;'
        printf '%s\n' '    unavailable)'
        printf '%s\n' '        exit 127'
        printf '%s\n' '        ;;'
        printf '%s\n' '    *) exit 64 ;;'
        printf '%s\n' 'esac'
    } >"$CASE_TOOLS/gh"
    chmod 0755 "$CASE_TOOLS/gh"
}

run_installer_with_failing_mv() {
    failing_mv_at=$1
    shift
    HOME=$CASE_HOME \
        HARDKNOCK_HOME=$CASE_DATA_HOME \
        TMPDIR=$CASE_TMP \
        PATH=$CASE_TOOLS:$ORIGINAL_PATH \
        HARDKNOCK_TEST_MV_STATE=$CASE_MV_STATE \
        HARDKNOCK_TEST_MV_FAIL_AT=$failing_mv_at \
        HARDKNOCK_TEST_REAL_MV=$REAL_MV \
        "$INSTALL_SHELL" "$INSTALLER" "$@"
}

run_installer_with_fake_curl() {
    fake_curl_version=$1
    shift
    HOME=$CASE_HOME \
        HARDKNOCK_HOME=$CASE_DATA_HOME \
        TMPDIR=$CASE_TMP \
        PATH=$CASE_TOOLS:$ORIGINAL_PATH \
        HARDKNOCK_TEST_CURL_LOG=$CASE_CURL_LOG \
        HARDKNOCK_TEST_CURL_MIRROR=$CASE_REPOSITORY \
        HARDKNOCK_TEST_CURL_VERSION=$fake_curl_version \
        HARDKNOCK_TEST_GH_LOG=${CASE_GH_LOG:-$CASE_ROOT/gh-arguments} \
        HARDKNOCK_TEST_GH_MODE=${CASE_GH_MODE:-success} \
        HARDKNOCK_INSTALL_PROVENANCE_TIMEOUT_SECONDS=${CASE_PROVENANCE_TIMEOUT_SECONDS:-120} \
        HARDKNOCK_INSTALL_CANDIDATE_TIMEOUT_SECONDS=${CASE_CANDIDATE_TIMEOUT_SECONDS:-30} \
        "$INSTALL_SHELL" "$INSTALLER" "$@"
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

begin_test "concurrent or stale installer lock blocks mutation"
new_case installer-lock
create_release "$CASE_REPOSITORY" 3.0.8 installer-lock
mkdir "$CASE_PREFIX"
chmod 0700 "$CASE_PREFIX"
mkdir "$CASE_PREFIX/.hardknock-install.lock"
chmod 0700 "$CASE_PREFIX/.hardknock-install.lock"
printf '%s\n' 424242 >"$CASE_PREFIX/.hardknock-install.lock/pid"
if run_installer \
    --version 3.0.8 \
    --prefix "$CASE_PREFIX" \
    --repository "$CASE_REPOSITORY" \
    --no-modify-path \
    >"$CASE_ROOT/output" 2>"$CASE_ROOT/error"; then
    fail_test "installer lock unexpectedly ignored"
fi
assert_contains "$CASE_ROOT/error" \
    'another installer operation is active or left a stale lock'
assert_absent "$CASE_PREFIX/bin/hardknock"
assert_file "$CASE_PREFIX/.hardknock-install.lock/pid"
assert_file "$CASE_DATA_HOME/user-data"
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
if run_installer_with_failing_mv 3 \
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
assert_absent "$CASE_PREFIX"
assert_file "$CASE_DATA_HOME/user-data"
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
    'official build-provenance verification failed'
assert_not_contains "$CASE_ROOT/error" \
    'private attestation verifier detail'
assert_contains "$CASE_GH_LOG" 'openkedge/hardknock'
assert_absent "$CASE_PREFIX"
assert_file "$CASE_DATA_HOME/user-data"
pass_test

begin_test "official HTTPS release fails closed on asset or tag mismatch"
new_case release-asset-failure
create_release "$CASE_REPOSITORY" 3.2.2 release-asset-failure
create_fake_curl
create_fake_gh release_failure
if run_installer_with_fake_curl 3.2.2 \
    --version 3.2.2 \
    --prefix "$CASE_PREFIX" \
    --no-modify-path \
    --dry-run \
    --json \
    >"$CASE_ROOT/output" 2>"$CASE_ROOT/error"; then
    fail_test "mismatched official release asset unexpectedly accepted"
fi
assert_contains "$CASE_ROOT/error" '"ok":false'
assert_contains "$CASE_ROOT/error" \
    'official release asset verification failed'
assert_not_contains "$CASE_ROOT/error" \
    'private release verifier detail'
assert_contains "$CASE_GH_LOG" 'verify-asset'
assert_contains "$CASE_GH_LOG" 'v3.2.2'
assert_not_contains "$CASE_GH_LOG" 'attestation'
assert_absent "$CASE_PREFIX"
assert_file "$CASE_DATA_HOME/user-data"
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
    'official release asset verification timed out'
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
assert_contains "$CASE_GH_LOG" 'release'
assert_contains "$CASE_GH_LOG" 'verify-asset'
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

begin_test "abandoned transaction requires repair before mutation"
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
    'interrupted installer transaction requires repair'
assert_absent "$CASE_PREFIX/bin/hardknock"
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
