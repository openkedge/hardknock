#!/bin/sh
# SPDX-License-Identifier: Apache-2.0

set -eu
set -f
umask 077

PROGRAM_NAME=${0##*/}
DEFAULT_REPOSITORY=https://github.com/openkedge/hardknock/releases/download
OFFICIAL_ATTESTATION_REPOSITORY=openkedge/hardknock
OFFICIAL_RELEASE_WORKFLOW=openkedge/hardknock/.github/workflows/release.yml
RESULT_SCHEMA=hardknock-installer-result-v1
MANIFEST_FORMAT=hardknock-install-manifest-v1
MANIFEST_RELATIVE=share/hardknock/install-manifest-v1
PATH_BEGIN='# >>> hardknock managed PATH >>>'
PATH_END='# <<< hardknock managed PATH <<<'
MAX_ARCHIVE_BYTES=268435456
MAX_CHECKSUM_BYTES=4096
MAX_BINARY_BYTES=134217728
MAX_LEGAL_BYTES=8388608
MAX_PROVENANCE_OUTPUT_BYTES=1048576
MAX_VERSION_OUTPUT_BYTES=4096
PROVENANCE_TIMEOUT_SECONDS=${HARDKNOCK_INSTALL_PROVENANCE_TIMEOUT_SECONDS:-120}
CANDIDATE_TIMEOUT_SECONDS=${HARDKNOCK_INSTALL_CANDIDATE_TIMEOUT_SECONDS:-30}

VERSION=
PREFIX=
REPOSITORY=$DEFAULT_REPOSITORY
NO_MODIFY_PATH=0
DRY_RUN=0
JSON=0
UNINSTALL=0
NO_VERIFY_PROVENANCE=0
REPOSITORY_SET=0
DOWNLOAD_DIR=
TRANSACTION_DIR=
TRANSACTION_ACTIVE=0
LOCK_PATH=
LOCK_HELD=0
PREFIX_CREATED=0
PROFILE_PATH=
PROFILE_STATE=untouched
PROFILE_NEEDS_CHANGE=0
PROFILE_REMOVE=0
NEW_PATH_PROFILE=-
NEW_PATH_PROFILE_CREATED=0
PRESERVED_MODIFIED=0
PROVENANCE_STATUS=not_applicable
PROVENANCE_POLICY=not_applicable
PROVENANCE_VERIFIED=false
PROVENANCE_WARNING=

usage() {
    cat <<EOF
Usage: $PROGRAM_NAME [OPTIONS]

Install a version-pinned Hardknock binary release without a Rust toolchain.

Options:
  --version VERSION       Release version to install, without or with leading v
  --prefix DIRECTORY      Installation prefix (default: \$HOME/.local)
  --repository LOCATION   HTTPS release base URL or absolute local mirror path
  --no-verify-provenance  Explicitly bypass build-provenance verification
  --no-modify-path        Do not add the prefix bin directory to \$HOME/.profile
  --dry-run               Verify the release and print the plan without changes
  --json                  Emit one machine-readable result object
  --uninstall             Remove files owned by the managed installation
  -h, --help              Show this help
EOF
}

json_escape() {
    printf '%s' "$1" | sed 's/\\/\\\\/g; s/"/\\"/g'
}

fail() {
    fail_message=$1
    if [ "$JSON" -eq 1 ]; then
        printf '{"schema":"%s","ok":false,"error":"%s"}\n' \
            "$RESULT_SCHEMA" "$(json_escape "$fail_message")" >&2
    else
        printf '%s: %s\n' "$PROGRAM_NAME" "$fail_message" >&2
    fi
    exit 1
}

validate_no_controls() {
    validate_label=$1
    validate_value=$2
    if printf '%s' "$validate_value" |
        LC_ALL=C grep '[[:cntrl:]]' >/dev/null 2>&1; then
        fail "$validate_label must not contain control characters"
    fi
}

normalize_version() {
    case "$VERSION" in
        v*) VERSION=${VERSION#v} ;;
    esac
    case "$VERSION" in
        ''|*[!0-9A-Za-z.+-]*)
            fail "version must contain only letters, digits, dots, plus signs, and hyphens"
            ;;
    esac
}

validate_absolute_path() {
    validate_path_label=$1
    validate_path_value=$2
    validate_no_controls "$validate_path_label" "$validate_path_value"
    case "$validate_path_value" in
        /*) ;;
        *) fail "$validate_path_label must be an absolute path" ;;
    esac
    case "$validate_path_value" in
        /) ;;
        //*) fail "$validate_path_label must not begin with a double separator" ;;
        */) fail "$validate_path_label must not end with a separator" ;;
    esac
    case "$validate_path_value/" in
        *'/./'*|*'/../'*)
            fail "$validate_path_label must not contain dot path components"
            ;;
    esac
}

validate_prefix() {
    validate_absolute_path "prefix" "$PREFIX"
    [ "$PREFIX" != / ] || fail "prefix must not be the filesystem root"
}

validate_path_chain_no_symlinks() {
    chain_path=$1
    chain_label=$2
    [ ! -L "$chain_path" ] ||
        fail "$chain_label must not be a symbolic link: $chain_path"
}

is_sha256() {
    [ "${#1}" -eq 64 ] || return 1
    case "$1" in
        *[!0-9A-Fa-f]*) return 1 ;;
    esac
    return 0
}

sha256_file() {
    sha256_path=$1
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum "$sha256_path" | awk '{print $1}'
    elif command -v shasum >/dev/null 2>&1; then
        shasum -a 256 "$sha256_path" | awk '{print $1}'
    else
        fail "sha256sum or shasum is required"
    fi
}

file_size() {
    size_path=$1
    size_value=$(wc -c <"$size_path" | tr -d '[:space:]') ||
        fail "cannot determine file size: $size_path"
    case "$size_value" in
        ''|*[!0-9]*) fail "invalid file size reported for: $size_path" ;;
    esac
    printf '%s\n' "$size_value"
}

require_max_size() {
    size_path=$1
    size_limit=$2
    size_label=$3
    size_actual=$(file_size "$size_path")
    [ "$size_actual" -le "$size_limit" ] ||
        fail "$size_label exceeds the supported size limit"
}

validate_timeout() {
    timeout_label=$1
    timeout_value=$2
    case "$timeout_value" in
        ''|*[!0-9]*|0)
            fail "$timeout_label must be a positive whole number of seconds"
            ;;
    esac
}

run_bounded_command() {
    bounded_timeout=$1
    bounded_max_output=$2
    bounded_stdout=$3
    bounded_stderr=$4
    shift 4

    bounded_timeout_marker=$bounded_stdout.timeout
    bounded_complete_marker=$bounded_stdout.complete
    rm -f "$bounded_timeout_marker" "$bounded_complete_marker" ||
        fail "cannot prepare bounded command state"

    case "$HOST_SYSTEM" in
        Darwin) bounded_block_bytes=1024 ;;
        *) bounded_block_bytes=512 ;;
    esac
    bounded_blocks=$(( \
        (bounded_max_output + bounded_block_bytes - 1) /
        bounded_block_bytes
    ))

    (
        ulimit -c 0 2>/dev/null || :
        ulimit -f "$bounded_blocks" 2>/dev/null || exit 97
        exec "$@"
    ) >"$bounded_stdout" 2>"$bounded_stderr" &
    bounded_command_pid=$!

    (
        bounded_sleep_pid=
        trap '
            if [ -n "$bounded_sleep_pid" ]; then
                kill "$bounded_sleep_pid" 2>/dev/null || :
            fi
            exit 0
        ' 1 2 15
        sleep "$bounded_timeout" &
        bounded_sleep_pid=$!
        wait "$bounded_sleep_pid" 2>/dev/null || exit 0
        bounded_sleep_pid=
        if [ ! -f "$bounded_complete_marker" ] &&
            kill -0 "$bounded_command_pid" 2>/dev/null; then
            : >"$bounded_timeout_marker"
            kill -TERM "$bounded_command_pid" 2>/dev/null || :
            sleep 1
            kill -KILL "$bounded_command_pid" 2>/dev/null || :
        fi
    ) &
    bounded_watchdog_pid=$!

    if wait "$bounded_command_pid"; then
        bounded_status=0
    else
        bounded_status=$?
    fi
    : >"$bounded_complete_marker"
    kill "$bounded_watchdog_pid" 2>/dev/null || :
    wait "$bounded_watchdog_pid" 2>/dev/null || :

    chmod 0600 "$bounded_stdout" "$bounded_stderr" ||
        fail "cannot secure bounded command output"
    require_max_size \
        "$bounded_stdout" "$bounded_max_output" "bounded command output"
    require_max_size \
        "$bounded_stderr" "$bounded_max_output" "bounded command error output"

    if [ -f "$bounded_timeout_marker" ]; then
        rm -f \
            "$bounded_timeout_marker" "$bounded_complete_marker" ||
            fail "cannot clean bounded command state"
        return 124
    fi
    rm -f "$bounded_complete_marker" ||
        fail "cannot clean bounded command state"
    return "$bounded_status"
}

detect_target() {
    HOST_SYSTEM=$(uname -s) || fail "cannot detect operating system"
    detected_machine=$(uname -m) || fail "cannot detect machine architecture"
    EFFECTIVE_UID=$(id -u) || fail "cannot determine the effective user"

    case "$HOST_SYSTEM" in
        Linux) detected_os=unknown-linux-gnu ;;
        Darwin) detected_os=apple-darwin ;;
        *) fail "unsupported operating system: $HOST_SYSTEM" ;;
    esac
    case "$detected_machine" in
        x86_64|amd64) detected_arch=x86_64 ;;
        arm64|aarch64) detected_arch=aarch64 ;;
        *) fail "unsupported machine architecture: $detected_machine" ;;
    esac
    TARGET=$detected_arch-$detected_os
}

path_uid() {
    case "$HOST_SYSTEM" in
        Darwin) stat -f '%u' "$1" ;;
        Linux) stat -c '%u' "$1" ;;
        *) return 1 ;;
    esac
}

path_mode() {
    case "$HOST_SYSTEM" in
        Darwin) stat -f '%Lp' "$1" ;;
        Linux) stat -c '%a' "$1" ;;
        *) return 1 ;;
    esac
}

mode_is_group_or_world_writable() {
    mode_value=$1
    case "$mode_value" in
        ''|*[!0-7]*) return 0 ;;
    esac
    mode_world=${mode_value#${mode_value%?}}
    mode_without_world=${mode_value%?}
    mode_group=${mode_without_world#${mode_without_world%?}}
    case "$mode_group" in
        2|3|6|7) return 0 ;;
    esac
    case "$mode_world" in
        2|3|6|7) return 0 ;;
    esac
    return 1
}

assert_owned_path() {
    owned_path=$1
    owned_label=$2
    owned_uid=$(path_uid "$owned_path") ||
        fail "cannot inspect ownership for $owned_label: $owned_path"
    [ "$owned_uid" = "$EFFECTIVE_UID" ] ||
        fail "$owned_label is not owned by the effective user: $owned_path"
}

assert_private_permissions() {
    private_path=$1
    private_label=$2
    private_mode=$(path_mode "$private_path") ||
        fail "cannot inspect permissions for $private_label: $private_path"
    if mode_is_group_or_world_writable "$private_mode"; then
        fail "$private_label is writable by another user: $private_path"
    fi
}

assert_secure_directory() {
    secure_directory=$1
    secure_label=$2
    [ ! -L "$secure_directory" ] ||
        fail "$secure_label must not be a symbolic link: $secure_directory"
    [ -d "$secure_directory" ] ||
        fail "$secure_label is not a directory: $secure_directory"
    assert_owned_path "$secure_directory" "$secure_label"
    assert_private_permissions "$secure_directory" "$secure_label"
}

assert_secure_file() {
    secure_file=$1
    secure_label=$2
    [ ! -L "$secure_file" ] ||
        fail "$secure_label must not be a symbolic link: $secure_file"
    [ -f "$secure_file" ] ||
        fail "$secure_label is not a regular file: $secure_file"
    assert_owned_path "$secure_file" "$secure_label"
    assert_private_permissions "$secure_file" "$secure_label"
}

validate_repository() {
    validate_no_controls "repository" "$REPOSITORY"
    case "$REPOSITORY" in
        https://*)
            case "$REPOSITORY" in
                *'?'*|*'#'*) fail "HTTPS repository must not contain a query or fragment" ;;
            esac
            repository_authority=${REPOSITORY#https://}
            repository_authority=${repository_authority%%/*}
            [ -n "$repository_authority" ] ||
                fail "HTTPS repository must include a host"
            case "$repository_authority" in
                *@*) fail "HTTPS repository must not contain user credentials" ;;
            esac
            SOURCE_KIND=https
            REPOSITORY=${REPOSITORY%/}
            if [ "$REPOSITORY" = "$DEFAULT_REPOSITORY" ]; then
                PROVENANCE_POLICY=official-required
            else
                PROVENANCE_POLICY=custom-https-explicit-bypass
                [ "$NO_VERIFY_PROVENANCE" -eq 1 ] ||
                    fail "custom HTTPS repositories require --no-verify-provenance because official build attestations cannot authenticate a custom release source"
            fi
            ;;
        http://*)
            fail "plain HTTP repositories are not supported; use HTTPS"
            ;;
        file://*)
            LOCAL_REPOSITORY=${REPOSITORY#file://}
            validate_absolute_path "local repository" "$LOCAL_REPOSITORY"
            validate_path_chain_no_symlinks "$LOCAL_REPOSITORY" "local repository"
            [ -d "$LOCAL_REPOSITORY" ] && [ ! -L "$LOCAL_REPOSITORY" ] ||
                fail "local repository is not a regular directory: $LOCAL_REPOSITORY"
            SOURCE_KIND=local
            PROVENANCE_POLICY=local-checksum-only
            ;;
        /*)
            LOCAL_REPOSITORY=$REPOSITORY
            validate_absolute_path "local repository" "$LOCAL_REPOSITORY"
            validate_path_chain_no_symlinks "$LOCAL_REPOSITORY" "local repository"
            [ -d "$LOCAL_REPOSITORY" ] && [ ! -L "$LOCAL_REPOSITORY" ] ||
                fail "local repository is not a regular directory: $LOCAL_REPOSITORY"
            SOURCE_KIND=local
            PROVENANCE_POLICY=local-checksum-only
            ;;
        *)
            fail "repository must be an HTTPS URL or absolute local mirror path"
            ;;
    esac
}

verify_build_provenance() {
    provenance_archive=$1

    if [ "$SOURCE_KIND" = local ]; then
        PROVENANCE_STATUS=checksum_only_local
        PROVENANCE_VERIFIED=false
        PROVENANCE_WARNING="local mirror used checksum verification only; build provenance was not independently verified"
        return
    fi

    if [ "$NO_VERIFY_PROVENANCE" -eq 1 ]; then
        PROVENANCE_STATUS=bypassed
        PROVENANCE_VERIFIED=false
        PROVENANCE_WARNING="build-provenance verification was explicitly bypassed with --no-verify-provenance"
        return
    fi

    command -v gh >/dev/null 2>&1 ||
        fail "gh is required to verify official build provenance; install GitHub CLI or explicitly use --no-verify-provenance"

    release_stdout=$DOWNLOAD_DIR/release-verification-output
    release_stderr=$DOWNLOAD_DIR/release-verification-errors
    if run_bounded_command \
        "$PROVENANCE_TIMEOUT_SECONDS" "$MAX_PROVENANCE_OUTPUT_BYTES" \
        "$release_stdout" "$release_stderr" \
        gh release verify-asset "v$VERSION" "$provenance_archive" \
            --repo "$OFFICIAL_ATTESTATION_REPOSITORY"; then
        :
    else
        release_exit=$?
        if [ "$release_exit" -eq 124 ]; then
            fail "official release asset verification timed out"
        fi
        if [ "$release_exit" -eq 127 ]; then
            fail "gh build-provenance verifier is unavailable"
        fi
        fail "official release asset verification failed"
    fi

    provenance_stdout=$DOWNLOAD_DIR/provenance-output
    provenance_stderr=$DOWNLOAD_DIR/provenance-errors
    if run_bounded_command \
        "$PROVENANCE_TIMEOUT_SECONDS" "$MAX_PROVENANCE_OUTPUT_BYTES" \
        "$provenance_stdout" "$provenance_stderr" \
        gh attestation verify "$provenance_archive" \
            --repo "$OFFICIAL_ATTESTATION_REPOSITORY" \
            --signer-workflow "$OFFICIAL_RELEASE_WORKFLOW"; then
        :
    else
        provenance_exit=$?
        if [ "$provenance_exit" -eq 124 ]; then
            fail "official build-provenance verification timed out"
        fi
        if [ "$provenance_exit" -eq 127 ]; then
            fail "gh build-provenance verifier is unavailable"
        fi
        fail "official build-provenance verification failed"
    fi

    PROVENANCE_STATUS=verified
    PROVENANCE_VERIFIED=true
    PROVENANCE_WARNING=
}

fetch_release_file() {
    fetch_name=$1
    fetch_destination=$2
    if [ "$SOURCE_KIND" = local ]; then
        fetch_source=$LOCAL_REPOSITORY/v$VERSION/$fetch_name
        [ -f "$fetch_source" ] && [ ! -L "$fetch_source" ] ||
            fail "release file is missing or unsafe: $fetch_name"
        cp "$fetch_source" "$fetch_destination" ||
            fail "cannot copy release file: $fetch_name"
    else
        fetch_url=${REPOSITORY%/}/v$VERSION/$fetch_name
        command -v curl >/dev/null 2>&1 || fail "curl is required"
        if ! curl --disable \
            --fail \
            --location \
            --silent \
            --show-error \
            --proto '=https' \
            --proto-redir '=https' \
            --tlsv1.2 \
            --connect-timeout 20 \
            --max-time 300 \
            --retry 2 \
            --output "$fetch_destination" \
            "$fetch_url" \
            2>"$DOWNLOAD_DIR/curl-errors"; then
            fail "cannot download release file: $fetch_name"
        fi
    fi
    chmod 0600 "$fetch_destination" ||
        fail "cannot secure downloaded release file: $fetch_name"
}

verify_checksum() {
    checksum_file=$1
    checksum_archive=$2
    checksum_expected_name=$3
    require_max_size "$checksum_file" "$MAX_CHECKSUM_BYTES" "checksum file"
    checksum_content=$(cat "$checksum_file") ||
        fail "cannot read checksum file"
    validate_no_controls "checksum record" "$checksum_content"
    set -- $checksum_content
    [ "$#" -eq 2 ] ||
        fail "checksum file must contain one SHA-256 and archive name"
    expected_hash=$1
    checksum_record_name=$2
    case "$checksum_record_name" in
        \*) checksum_record_name=${checksum_record_name#\*} ;;
    esac
    is_sha256 "$expected_hash" ||
        fail "checksum file does not contain a valid SHA-256"
    [ "$checksum_record_name" = "$checksum_expected_name" ] ||
        fail "checksum file names an unexpected release archive"
    actual_hash=$(sha256_file "$checksum_archive") ||
        fail "cannot calculate archive checksum"
    expected_hash=$(printf '%s' "$expected_hash" | tr 'A-F' 'a-f')
    actual_hash=$(printf '%s' "$actual_hash" | tr 'A-F' 'a-f')
    [ "$actual_hash" = "$expected_hash" ] ||
        fail "release archive checksum verification failed"
}

validate_archive() {
    archive_path=$1
    archive_root=$2
    archive_names=$DOWNLOAD_DIR/archive-names
    archive_verbose=$DOWNLOAD_DIR/archive-verbose
    archive_errors=$DOWNLOAD_DIR/archive-errors

    require_max_size "$archive_path" "$MAX_ARCHIVE_BYTES" "release archive"
    if ! LC_ALL=C tar -tzf "$archive_path" \
        >"$archive_names" 2>"$archive_errors"; then
        fail "release archive is not a readable gzip-compressed tar"
    fi
    require_max_size "$archive_names" "$MAX_CHECKSUM_BYTES" "archive member list"

    archive_root_count=0
    archive_hardknock_count=0
    archive_effect_count=0
    archive_license_count=0
    archive_notice_count=0
    archive_member_count=0
    while IFS= read -r archive_member || [ -n "$archive_member" ]; do
        archive_member_count=$((archive_member_count + 1))
        case "$archive_member" in
            "$archive_root"|"$archive_root/")
                archive_root_count=$((archive_root_count + 1))
                ;;
            "$archive_root/hardknock")
                archive_hardknock_count=$((archive_hardknock_count + 1))
                ;;
            "$archive_root/hk-effect")
                archive_effect_count=$((archive_effect_count + 1))
                ;;
            "$archive_root/LICENSE")
                archive_license_count=$((archive_license_count + 1))
                ;;
            "$archive_root/NOTICE")
                archive_notice_count=$((archive_notice_count + 1))
                ;;
            *)
                fail "release archive contains an unsafe or unexpected member"
                ;;
        esac
    done <"$archive_names"

    [ "$archive_member_count" -eq 5 ] &&
        [ "$archive_root_count" -eq 1 ] &&
        [ "$archive_hardknock_count" -eq 1 ] &&
        [ "$archive_effect_count" -eq 1 ] &&
        [ "$archive_license_count" -eq 1 ] &&
        [ "$archive_notice_count" -eq 1 ] ||
        fail "release archive must contain each managed member exactly once"

    if ! LC_ALL=C tar -tvzf "$archive_path" \
        >"$archive_verbose" 2>>"$archive_errors"; then
        fail "release archive metadata cannot be inspected"
    fi
    awk '
        {
            type = substr($0, 1, 1)
            if (type == "d") {
                directories += 1
            } else if (type == "-") {
                files += 1
            } else {
                invalid += 1
            }
        }
        END {
            if (directories != 1 || files != 4 || invalid != 0) {
                exit 1
            }
        }
    ' "$archive_verbose" ||
        fail "release archive contains links, devices, or unsupported member types"
}

extract_member() {
    extract_archive_path=$1
    extract_member_name=$2
    extract_destination=$3
    extract_limit=$4
    extract_label=$5
    case "$HOST_SYSTEM" in
        Darwin) extract_block_bytes=1024 ;;
        *) extract_block_bytes=512 ;;
    esac
    extract_blocks=$(( (extract_limit + extract_block_bytes - 1) / extract_block_bytes ))
    if ! (
        ulimit -c 0 2>/dev/null || :
        ulimit -f "$extract_blocks" 2>/dev/null || exit 97
        exec tar -xzOf "$extract_archive_path" "$extract_member_name"
    ) >"$extract_destination" 2>"$DOWNLOAD_DIR/extract-errors"; then
        rm -f "$extract_destination"
        fail "cannot safely extract release member: $extract_label"
    fi
    require_max_size "$extract_destination" "$extract_limit" "$extract_label"
}

extract_archive() {
    archive_path=$1
    archive_root=$2
    EXTRACT_DIRECTORY=$DOWNLOAD_DIR/extracted
    EXTRACTED_ROOT=$EXTRACT_DIRECTORY/$archive_root
    mkdir -p "$EXTRACTED_ROOT" ||
        fail "cannot create private extraction directory"
    chmod 0700 "$EXTRACT_DIRECTORY" "$EXTRACTED_ROOT" ||
        fail "cannot secure extraction directory"

    extract_member \
        "$archive_path" "$archive_root/hardknock" \
        "$EXTRACTED_ROOT/hardknock" "$MAX_BINARY_BYTES" hardknock
    extract_member \
        "$archive_path" "$archive_root/hk-effect" \
        "$EXTRACTED_ROOT/hk-effect" "$MAX_BINARY_BYTES" hk-effect
    extract_member \
        "$archive_path" "$archive_root/LICENSE" \
        "$EXTRACTED_ROOT/LICENSE" "$MAX_LEGAL_BYTES" LICENSE
    extract_member \
        "$archive_path" "$archive_root/NOTICE" \
        "$EXTRACTED_ROOT/NOTICE" "$MAX_LEGAL_BYTES" NOTICE

    [ -s "$EXTRACTED_ROOT/hardknock" ] ||
        fail "hardknock release binary is empty"
    [ -s "$EXTRACTED_ROOT/hk-effect" ] ||
        fail "hk-effect release binary is empty"
    chmod 0755 "$EXTRACTED_ROOT/hardknock" "$EXTRACTED_ROOT/hk-effect" ||
        fail "cannot set extracted binary permissions"
    chmod 0644 "$EXTRACTED_ROOT/LICENSE" "$EXTRACTED_ROOT/NOTICE" ||
        fail "cannot set extracted legal file permissions"
}

calculate_release_hashes() {
    NEW_HASH_HARDKNOCK=$(sha256_file "$EXTRACTED_ROOT/hardknock")
    NEW_HASH_EFFECT=$(sha256_file "$EXTRACTED_ROOT/hk-effect")
    NEW_HASH_LICENSE=$(sha256_file "$EXTRACTED_ROOT/LICENSE")
    NEW_HASH_NOTICE=$(sha256_file "$EXTRACTED_ROOT/NOTICE")
}

verify_release_binaries() {
    command -v env >/dev/null 2>&1 || fail "env is required"
    verify_home=$DOWNLOAD_DIR/verify-home
    mkdir -p \
        "$verify_home/home" \
        "$verify_home/hardknock" \
        "$verify_home/config" \
        "$verify_home/cache" \
        "$verify_home/data" \
        "$verify_home/tmp" ||
        fail "cannot create isolated release verification home"

    hardknock_version_output=$DOWNLOAD_DIR/hardknock-version-output
    hardknock_version_errors=$DOWNLOAD_DIR/hardknock-version-errors
    if run_bounded_command \
        "$CANDIDATE_TIMEOUT_SECONDS" "$MAX_VERSION_OUTPUT_BYTES" \
        "$hardknock_version_output" "$hardknock_version_errors" \
        env -i \
            HOME="$verify_home/home" \
            HARDKNOCK_HOME="$verify_home/hardknock" \
            XDG_CONFIG_HOME="$verify_home/config" \
            XDG_CACHE_HOME="$verify_home/cache" \
            XDG_DATA_HOME="$verify_home/data" \
            TMPDIR="$verify_home/tmp" \
            PATH=/usr/bin:/bin \
            LC_ALL=C \
            "$EXTRACTED_ROOT/hardknock" --version; then
        :
    else
        hardknock_status=$?
        if [ "$hardknock_status" -eq 124 ]; then
            fail "hardknock release binary version check timed out"
        fi
        fail "hardknock release binary cannot report its version"
    fi
    hardknock_version=$(cat "$hardknock_version_output") ||
        fail "cannot read hardknock release binary version"

    effect_version_output=$DOWNLOAD_DIR/hk-effect-version-output
    effect_version_errors=$DOWNLOAD_DIR/hk-effect-version-errors
    if run_bounded_command \
        "$CANDIDATE_TIMEOUT_SECONDS" "$MAX_VERSION_OUTPUT_BYTES" \
        "$effect_version_output" "$effect_version_errors" \
        env -i \
            HOME="$verify_home/home" \
            HARDKNOCK_HOME="$verify_home/hardknock" \
            XDG_CONFIG_HOME="$verify_home/config" \
            XDG_CACHE_HOME="$verify_home/cache" \
            XDG_DATA_HOME="$verify_home/data" \
            TMPDIR="$verify_home/tmp" \
            PATH=/usr/bin:/bin \
            LC_ALL=C \
            "$EXTRACTED_ROOT/hk-effect" --version; then
        :
    else
        effect_status=$?
        if [ "$effect_status" -eq 124 ]; then
            fail "hk-effect release binary version check timed out"
        fi
        fail "hk-effect release binary cannot report its version"
    fi
    effect_version=$(cat "$effect_version_output") ||
        fail "cannot read hk-effect release binary version"
    [ "$hardknock_version" = "hardknock $VERSION" ] ||
        fail "hardknock binary version does not match requested version"
    [ "$effect_version" = "hk-effect $VERSION" ] ||
        fail "hk-effect binary version does not match requested version"
}

reset_manifest_state() {
    MANIFEST_PRESENT=0
    OLD_VERSION=
    OLD_TARGET=
    OLD_PATH_PROFILE=-
    OLD_PATH_PROFILE_CREATED=0
    OLD_HASH_HARDKNOCK=
    OLD_HASH_EFFECT=
    OLD_HASH_LICENSE=
    OLD_HASH_NOTICE=
}

preflight_directories() {
    validate_path_chain_no_symlinks "$PREFIX" "installation prefix"
    for managed_directory in \
        "$PREFIX" \
        "$PREFIX/bin" \
        "$PREFIX/share" \
        "$PREFIX/share/doc" \
        "$PREFIX/share/doc/hardknock" \
        "$PREFIX/share/hardknock"
    do
        if [ -e "$managed_directory" ] || [ -L "$managed_directory" ]; then
            assert_secure_directory "$managed_directory" "installation directory"
        fi
    done
}

load_manifest() {
    reset_manifest_state
    if [ ! -e "$MANIFEST_PATH" ] && [ ! -L "$MANIFEST_PATH" ]; then
        return
    fi
    assert_secure_file "$MANIFEST_PATH" "managed manifest"

    manifest_line_number=0
    manifest_seen_version=0
    manifest_seen_target=0
    manifest_seen_profile=0
    manifest_seen_profile_created=0
    manifest_seen_hardknock=0
    manifest_seen_effect=0
    manifest_seen_license=0
    manifest_seen_notice=0
    while IFS= read -r manifest_line || [ -n "$manifest_line" ]; do
        manifest_line_number=$((manifest_line_number + 1))
        validate_no_controls "managed manifest entry" "$manifest_line"
        if [ "$manifest_line_number" -eq 1 ]; then
            [ "$manifest_line" = "$MANIFEST_FORMAT" ] ||
                fail "managed manifest has an unsupported format"
            continue
        fi
        case "$manifest_line" in
            version=*)
                [ "$manifest_seen_version" -eq 0 ] ||
                    fail "managed manifest contains duplicate version metadata"
                OLD_VERSION=${manifest_line#version=}
                manifest_seen_version=1
                ;;
            target=*)
                [ "$manifest_seen_target" -eq 0 ] ||
                    fail "managed manifest contains duplicate target metadata"
                OLD_TARGET=${manifest_line#target=}
                manifest_seen_target=1
                ;;
            path_profile=*)
                [ "$manifest_seen_profile" -eq 0 ] ||
                    fail "managed manifest contains duplicate PATH metadata"
                OLD_PATH_PROFILE=${manifest_line#path_profile=}
                manifest_seen_profile=1
                ;;
            path_profile_created=0|path_profile_created=1)
                [ "$manifest_seen_profile_created" -eq 0 ] ||
                    fail "managed manifest contains duplicate PATH creation metadata"
                OLD_PATH_PROFILE_CREATED=${manifest_line#path_profile_created=}
                manifest_seen_profile_created=1
                ;;
            "file=bin/hardknock|"*)
                [ "$manifest_seen_hardknock" -eq 0 ] ||
                    fail "managed manifest contains duplicate hardknock entries"
                OLD_HASH_HARDKNOCK=${manifest_line#file=bin/hardknock|}
                manifest_seen_hardknock=1
                ;;
            "file=bin/hk-effect|"*)
                [ "$manifest_seen_effect" -eq 0 ] ||
                    fail "managed manifest contains duplicate hk-effect entries"
                OLD_HASH_EFFECT=${manifest_line#file=bin/hk-effect|}
                manifest_seen_effect=1
                ;;
            "file=share/doc/hardknock/LICENSE|"*)
                [ "$manifest_seen_license" -eq 0 ] ||
                    fail "managed manifest contains duplicate LICENSE entries"
                OLD_HASH_LICENSE=${manifest_line#file=share/doc/hardknock/LICENSE|}
                manifest_seen_license=1
                ;;
            "file=share/doc/hardknock/NOTICE|"*)
                [ "$manifest_seen_notice" -eq 0 ] ||
                    fail "managed manifest contains duplicate NOTICE entries"
                OLD_HASH_NOTICE=${manifest_line#file=share/doc/hardknock/NOTICE|}
                manifest_seen_notice=1
                ;;
            *)
                fail "managed manifest contains an unknown entry"
                ;;
        esac
    done <"$MANIFEST_PATH"

    [ "$manifest_line_number" -gt 1 ] &&
        [ "$manifest_seen_version" -eq 1 ] &&
        [ "$manifest_seen_target" -eq 1 ] &&
        [ "$manifest_seen_profile" -eq 1 ] &&
        [ "$manifest_seen_profile_created" -eq 1 ] &&
        [ "$manifest_seen_hardknock" -eq 1 ] &&
        [ "$manifest_seen_effect" -eq 1 ] &&
        [ "$manifest_seen_license" -eq 1 ] &&
        [ "$manifest_seen_notice" -eq 1 ] ||
        fail "managed manifest is incomplete"

    case "$OLD_VERSION" in
        ''|*[!0-9A-Za-z.+-]*)
            fail "managed manifest contains an invalid version"
            ;;
    esac
    case "$OLD_TARGET" in
        x86_64-unknown-linux-gnu|aarch64-unknown-linux-gnu|\
        x86_64-apple-darwin|aarch64-apple-darwin)
            ;;
        *) fail "managed manifest contains an invalid target" ;;
    esac
    is_sha256 "$OLD_HASH_HARDKNOCK" &&
        is_sha256 "$OLD_HASH_EFFECT" &&
        is_sha256 "$OLD_HASH_LICENSE" &&
        is_sha256 "$OLD_HASH_NOTICE" ||
        fail "managed manifest contains an invalid file checksum"
    OLD_HASH_HARDKNOCK=$(printf '%s' "$OLD_HASH_HARDKNOCK" | tr 'A-F' 'a-f')
    OLD_HASH_EFFECT=$(printf '%s' "$OLD_HASH_EFFECT" | tr 'A-F' 'a-f')
    OLD_HASH_LICENSE=$(printf '%s' "$OLD_HASH_LICENSE" | tr 'A-F' 'a-f')
    OLD_HASH_NOTICE=$(printf '%s' "$OLD_HASH_NOTICE" | tr 'A-F' 'a-f')

    validate_no_controls "managed PATH profile" "$OLD_PATH_PROFILE"
    if [ "$OLD_PATH_PROFILE" != - ]; then
        validate_absolute_path "managed PATH profile" "$OLD_PATH_PROFILE"
        [ -n "${HOME:-}" ] ||
            fail "HOME is required to validate the managed PATH profile"
        validate_absolute_path "HOME" "$HOME"
        [ "$OLD_PATH_PROFILE" = "$HOME/.profile" ] ||
            fail "managed manifest references an unexpected PATH profile"
    fi
    MANIFEST_PRESENT=1
}

preflight_managed_file() {
    preflight_relative=$1
    preflight_old_hash=$2
    preflight_destination=$PREFIX/$preflight_relative
    if [ ! -e "$preflight_destination" ] &&
        [ ! -L "$preflight_destination" ]; then
        printf '%s\n' absent
        return
    fi
    assert_secure_file "$preflight_destination" "managed installation file"
    [ "$MANIFEST_PRESENT" -eq 1 ] ||
        fail "unmanaged file collision at $preflight_destination"
    preflight_actual_hash=$(sha256_file "$preflight_destination") ||
        fail "cannot hash existing managed file: $preflight_destination"
    [ "$preflight_actual_hash" = "$preflight_old_hash" ] ||
        fail "existing managed file was modified; refusing to overwrite: $preflight_destination"
    printf '%s\n' present
}

preflight_installation() {
    STATE_HARDKNOCK=$(
        preflight_managed_file bin/hardknock "$OLD_HASH_HARDKNOCK"
    )
    STATE_EFFECT=$(
        preflight_managed_file bin/hk-effect "$OLD_HASH_EFFECT"
    )
    STATE_LICENSE=$(
        preflight_managed_file \
            share/doc/hardknock/LICENSE "$OLD_HASH_LICENSE"
    )
    STATE_NOTICE=$(
        preflight_managed_file \
            share/doc/hardknock/NOTICE "$OLD_HASH_NOTICE"
    )
}

inspect_profile() {
    PROFILE_STATE=absent
    [ ! -L "$PROFILE_PATH" ] ||
        fail "PATH profile must not be a symbolic link: $PROFILE_PATH"
    if [ ! -e "$PROFILE_PATH" ]; then
        return
    fi
    assert_secure_file "$PROFILE_PATH" "PATH profile"
    PROFILE_STATE=$(
        awk -v begin="$PATH_BEGIN" -v end="$PATH_END" '
            $0 == begin {
                if (inside || seen_begin) {
                    invalid = 1
                }
                inside = 1
                seen_begin = 1
                next
            }
            $0 == end {
                if (!inside || seen_end) {
                    invalid = 1
                }
                inside = 0
                seen_end = 1
                next
            }
            END {
                if (inside || seen_begin != seen_end || invalid) {
                    exit 2
                }
                if (seen_begin) {
                    print "managed"
                } else {
                    print "unmanaged"
                }
            }
        ' "$PROFILE_PATH"
    ) || fail "PATH profile contains malformed Hardknock markers"
}

shell_quote() {
    printf "'"
    printf '%s' "$1" | sed "s/'/'\\\\''/g"
    printf "'"
}

write_expected_path_block() {
    expected_destination=$1
    expected_quoted_bin=$(shell_quote "$PREFIX/bin")
    {
        printf '%s\n' "$PATH_BEGIN"
        printf 'hardknock_bin=%s\n' "$expected_quoted_bin"
        printf '%s\n' 'case ":${PATH}:" in'
        printf '%s\n' '    *":${hardknock_bin}:"*) ;;'
        printf '%s\n' '    *) PATH="${hardknock_bin}:${PATH}" ;;'
        printf '%s\n' 'esac'
        printf '%s\n' 'export PATH'
        printf '%s\n' 'unset hardknock_bin'
        printf '%s\n' "$PATH_END"
    } >"$expected_destination"
}

profile_block_is_current() {
    profile_expected=$DOWNLOAD_DIR/profile-expected
    profile_actual=$DOWNLOAD_DIR/profile-actual
    write_expected_path_block "$profile_expected"
    awk -v begin="$PATH_BEGIN" -v end="$PATH_END" '
        $0 == begin {
            copying = 1
        }
        copying {
            print
        }
        $0 == end && copying {
            exit
        }
    ' "$PROFILE_PATH" >"$profile_actual"
    cmp -s "$profile_expected" "$profile_actual"
}

prepare_path_plan() {
    NEW_PATH_PROFILE=-
    NEW_PATH_PROFILE_CREATED=0
    PROFILE_PATH=
    PROFILE_STATE=untouched
    PROFILE_NEEDS_CHANGE=0
    if [ "$NO_MODIFY_PATH" -eq 1 ]; then
        if [ "$MANIFEST_PRESENT" -eq 1 ] && [ "$OLD_PATH_PROFILE" != - ]; then
            NEW_PATH_PROFILE=$OLD_PATH_PROFILE
            NEW_PATH_PROFILE_CREATED=$OLD_PATH_PROFILE_CREATED
        fi
        return
    fi

    [ -n "${HOME:-}" ] || fail "HOME is required unless --no-modify-path is used"
    validate_absolute_path "HOME" "$HOME"
    validate_path_chain_no_symlinks "$HOME" "HOME"
    assert_secure_directory "$HOME" "HOME"
    PROFILE_PATH=$HOME/.profile
    inspect_profile
    if [ "$PROFILE_STATE" = managed ]; then
        [ "$MANIFEST_PRESENT" -eq 1 ] &&
            [ "$OLD_PATH_PROFILE" = "$PROFILE_PATH" ] ||
            fail "PATH profile contains an unmanaged Hardknock marker block"
    fi

    NEW_PATH_PROFILE=$PROFILE_PATH
    if [ "$MANIFEST_PRESENT" -eq 1 ] &&
        [ "$OLD_PATH_PROFILE" = "$PROFILE_PATH" ]; then
        NEW_PATH_PROFILE_CREATED=$OLD_PATH_PROFILE_CREATED
    elif [ "$PROFILE_STATE" = absent ]; then
        NEW_PATH_PROFILE_CREATED=1
    fi

    case "$PROFILE_STATE" in
        managed)
            if ! profile_block_is_current; then
                PROFILE_NEEDS_CHANGE=1
            fi
            ;;
        absent|unmanaged)
            PROFILE_NEEDS_CHANGE=1
            ;;
    esac
}

strip_path_block() {
    strip_input=$1
    strip_output=$2
    awk -v begin="$PATH_BEGIN" -v end="$PATH_END" '
        $0 == begin {
            inside = 1
            next
        }
        $0 == end {
            inside = 0
            next
        }
        !inside {
            print
        }
        END {
            if (inside) {
                exit 1
            }
        }
    ' "$strip_input" >"$strip_output"
}

write_path_block() {
    profile_directory=${PROFILE_PATH%/*}
    profile_temporary=$(mktemp "$profile_directory/.hardknock-profile.XXXXXX") ||
        return 1
    if [ -e "$PROFILE_PATH" ]; then
        cp -p "$PROFILE_PATH" "$profile_temporary" || {
            rm -f "$profile_temporary"
            return 1
        }
        strip_path_block "$PROFILE_PATH" "$profile_temporary" || {
            rm -f "$profile_temporary"
            return 1
        }
    else
        : >"$profile_temporary"
        chmod 0644 "$profile_temporary" || {
            rm -f "$profile_temporary"
            return 1
        }
    fi

    if [ -s "$profile_temporary" ]; then
        profile_last_line=$(tail -n 1 "$profile_temporary")
        if [ -n "$profile_last_line" ]; then
            printf '\n' >>"$profile_temporary" || {
                rm -f "$profile_temporary"
                return 1
            }
        fi
    fi
    write_expected_path_block "$DOWNLOAD_DIR/profile-new-block" || {
        rm -f "$profile_temporary"
        return 1
    }
    cat "$DOWNLOAD_DIR/profile-new-block" >>"$profile_temporary" || {
        rm -f "$profile_temporary"
        return 1
    }
    mv "$profile_temporary" "$PROFILE_PATH"
}

prepare_uninstall_profile() {
    PROFILE_PATH=
    PROFILE_STATE=untouched
    PROFILE_REMOVE=0
    [ "$OLD_PATH_PROFILE" != - ] || return 0
    PROFILE_PATH=$OLD_PATH_PROFILE
    validate_path_chain_no_symlinks "$PROFILE_PATH" "managed PATH profile"
    inspect_profile
    case "$PROFILE_STATE" in
        managed) PROFILE_REMOVE=1 ;;
        absent|unmanaged) PROFILE_REMOVE=0 ;;
    esac
}

remove_path_block() {
    [ "$PROFILE_REMOVE" -eq 1 ] || return 0
    profile_directory=${PROFILE_PATH%/*}
    profile_temporary=$(mktemp "$profile_directory/.hardknock-profile.XXXXXX") ||
        fail "cannot create temporary PATH profile"
    cp -p "$PROFILE_PATH" "$profile_temporary" || {
        rm -f "$profile_temporary"
        fail "cannot preserve PATH profile metadata"
    }
    strip_path_block "$PROFILE_PATH" "$profile_temporary" || {
        rm -f "$profile_temporary"
        fail "cannot remove managed PATH marker block"
    }
    if [ "$OLD_PATH_PROFILE_CREATED" -eq 1 ] &&
        [ ! -s "$profile_temporary" ]; then
        rm -f "$profile_temporary"
        rm -f "$PROFILE_PATH" ||
            fail "cannot remove installer-created PATH profile"
    else
        mv "$profile_temporary" "$PROFILE_PATH" ||
            fail "cannot atomically update PATH profile"
    fi
}

check_no_install_lock() {
    if [ -e "$LOCK_PATH" ] || [ -L "$LOCK_PATH" ]; then
        fail "another installer operation is active or left a stale lock: $LOCK_PATH"
    fi
}

check_no_stale_transactions() {
    stale_transaction_found=0
    set +f
    for stale_transaction in "$PREFIX"/.hardknock-install.*; do
        if [ "$stale_transaction" = "$PREFIX/.hardknock-install.*" ] &&
            [ ! -e "$stale_transaction" ] &&
            [ ! -L "$stale_transaction" ]; then
            continue
        fi
        if [ "$stale_transaction" = "$LOCK_PATH" ]; then
            continue
        fi
        stale_transaction_found=1
        break
    done
    set -f
    [ "$stale_transaction_found" -eq 0 ] ||
        fail "an interrupted installer transaction requires repair under: $PREFIX"
}

prepare_prefix_for_mutation() {
    if [ ! -e "$PREFIX" ] && [ ! -L "$PREFIX" ]; then
        prefix_parent=${PREFIX%/*}
        [ -n "$prefix_parent" ] || prefix_parent=/
        [ -d "$prefix_parent" ] && [ ! -L "$prefix_parent" ] ||
            fail "prefix parent must already be a regular directory: $prefix_parent"
        mkdir "$PREFIX" ||
            fail "cannot create installation prefix: $PREFIX"
        chmod 0700 "$PREFIX" ||
            fail "cannot secure installation prefix: $PREFIX"
        PREFIX_CREATED=1
    fi
    assert_secure_directory "$PREFIX" "installation prefix"
}

acquire_lock() {
    allow_prefix_creation=$1
    if [ ! -e "$PREFIX" ] && [ ! -L "$PREFIX" ]; then
        [ "$allow_prefix_creation" -eq 1 ] ||
            fail "managed installation disappeared before it could be locked"
        prepare_prefix_for_mutation
    else
        assert_secure_directory "$PREFIX" "installation prefix"
    fi
    check_no_install_lock
    check_no_stale_transactions
    mkdir "$LOCK_PATH" ||
        fail "cannot acquire installer lock: $LOCK_PATH"
    LOCK_HELD=1
    chmod 0700 "$LOCK_PATH" ||
        fail "cannot secure installer lock"
    printf '%s\n' "$$" >"$LOCK_PATH/pid" ||
        fail "cannot record installer lock owner"
    chmod 0600 "$LOCK_PATH/pid" ||
        fail "cannot secure installer lock owner record"
}

release_lock() {
    [ "$LOCK_HELD" -eq 1 ] || return 0
    rm -f "$LOCK_PATH/pid" || return 1
    rmdir "$LOCK_PATH" || return 1
    LOCK_HELD=0
}

begin_transaction() {
    TRANSACTION_DIR=$(mktemp -d "$PREFIX/.hardknock-install.XXXXXX") ||
        fail "cannot create installation transaction under prefix"
    STAGE_DIRECTORY=$TRANSACTION_DIR/stage
    BACKUP_DIRECTORY=$TRANSACTION_DIR/backup
    PLACED_DIRECTORY=$TRANSACTION_DIR/placed
    CREATED_DIRECTORIES_FILE=$TRANSACTION_DIR/created-directories
    mkdir -p \
        "$STAGE_DIRECTORY" \
        "$BACKUP_DIRECTORY" \
        "$PLACED_DIRECTORY" ||
        fail "cannot prepare installation transaction"
    chmod 0700 \
        "$TRANSACTION_DIR" \
        "$STAGE_DIRECTORY" \
        "$BACKUP_DIRECTORY" \
        "$PLACED_DIRECTORY" ||
        fail "cannot secure installation transaction"
    : >"$CREATED_DIRECTORIES_FILE" ||
        fail "cannot create directory rollback journal"
    TRANSACTION_ACTIVE=1
}

ensure_managed_directory() {
    ensure_directory=$1
    if [ -e "$ensure_directory" ] || [ -L "$ensure_directory" ]; then
        assert_secure_directory "$ensure_directory" "installation directory"
        return
    fi
    printf '%s\n' "$ensure_directory" >>"$CREATED_DIRECTORIES_FILE" ||
        fail "cannot journal created installation directory"
    mkdir "$ensure_directory" ||
        fail "cannot create installation directory: $ensure_directory"
    chmod 0700 "$ensure_directory" ||
        fail "cannot secure installation directory: $ensure_directory"
}

ensure_installation_directories() {
    ensure_managed_directory "$PREFIX/bin"
    ensure_managed_directory "$PREFIX/share"
    ensure_managed_directory "$PREFIX/share/doc"
    ensure_managed_directory "$PREFIX/share/doc/hardknock"
    ensure_managed_directory "$PREFIX/share/hardknock"
}

write_new_manifest() {
    manifest_destination=$1
    {
        printf '%s\n' "$MANIFEST_FORMAT"
        printf 'version=%s\n' "$VERSION"
        printf 'target=%s\n' "$TARGET"
        printf 'path_profile=%s\n' "$NEW_PATH_PROFILE"
        printf 'path_profile_created=%s\n' "$NEW_PATH_PROFILE_CREATED"
        printf 'file=bin/hardknock|%s\n' "$NEW_HASH_HARDKNOCK"
        printf 'file=bin/hk-effect|%s\n' "$NEW_HASH_EFFECT"
        printf 'file=share/doc/hardknock/LICENSE|%s\n' "$NEW_HASH_LICENSE"
        printf 'file=share/doc/hardknock/NOTICE|%s\n' "$NEW_HASH_NOTICE"
    } >"$manifest_destination" || return 1
    chmod 0644 "$manifest_destination"
}

stage_installation() {
    mkdir -p \
        "$STAGE_DIRECTORY/bin" \
        "$STAGE_DIRECTORY/share/doc/hardknock" \
        "$STAGE_DIRECTORY/share/hardknock" ||
        fail "cannot prepare staged installation"
    cp "$EXTRACTED_ROOT/hardknock" "$STAGE_DIRECTORY/bin/hardknock" &&
        cp "$EXTRACTED_ROOT/hk-effect" "$STAGE_DIRECTORY/bin/hk-effect" &&
        cp "$EXTRACTED_ROOT/LICENSE" \
            "$STAGE_DIRECTORY/share/doc/hardknock/LICENSE" &&
        cp "$EXTRACTED_ROOT/NOTICE" \
            "$STAGE_DIRECTORY/share/doc/hardknock/NOTICE" ||
        fail "cannot stage release files"
    chmod 0755 \
        "$STAGE_DIRECTORY/bin/hardknock" \
        "$STAGE_DIRECTORY/bin/hk-effect" ||
        fail "cannot set staged binary permissions"
    chmod 0644 \
        "$STAGE_DIRECTORY/share/doc/hardknock/LICENSE" \
        "$STAGE_DIRECTORY/share/doc/hardknock/NOTICE" ||
        fail "cannot set staged legal file permissions"

    [ "$(sha256_file "$STAGE_DIRECTORY/bin/hardknock")" = "$NEW_HASH_HARDKNOCK" ] &&
        [ "$(sha256_file "$STAGE_DIRECTORY/bin/hk-effect")" = "$NEW_HASH_EFFECT" ] &&
        [ "$(sha256_file "$STAGE_DIRECTORY/share/doc/hardknock/LICENSE")" = "$NEW_HASH_LICENSE" ] &&
        [ "$(sha256_file "$STAGE_DIRECTORY/share/doc/hardknock/NOTICE")" = "$NEW_HASH_NOTICE" ] ||
        fail "staged release files changed after verification"
    write_new_manifest "$STAGE_DIRECTORY/$MANIFEST_RELATIVE" ||
        fail "cannot stage managed installation manifest"
}

replace_managed_file() {
    replace_relative=$1
    replace_source=$STAGE_DIRECTORY/$replace_relative
    replace_destination=$PREFIX/$replace_relative
    replace_backup=$BACKUP_DIRECTORY/$replace_relative
    replace_marker=$PLACED_DIRECTORY/$replace_relative
    mkdir -p "${replace_backup%/*}" "${replace_marker%/*}" || return 1
    if [ -e "$replace_destination" ] || [ -L "$replace_destination" ]; then
        mv "$replace_destination" "$replace_backup" || return 1
    fi
    : >"$replace_marker" || return 1
    mv "$replace_source" "$replace_destination"
}

journal_profile_change() {
    [ -n "$PROFILE_PATH" ] || return 1
    if [ -e "$PROFILE_PATH" ]; then
        cp -p "$PROFILE_PATH" "$TRANSACTION_DIR/profile.backup" ||
            return 1
    else
        : >"$TRANSACTION_DIR/profile.absent" || return 1
    fi
    : >"$TRANSACTION_DIR/profile.changed"
}

rollback_managed_file() {
    rollback_relative=$1
    rollback_destination=$PREFIX/$rollback_relative
    rollback_backup=$BACKUP_DIRECTORY/$rollback_relative
    rollback_marker=$PLACED_DIRECTORY/$rollback_relative
    if [ -e "$rollback_backup" ] || [ -L "$rollback_backup" ]; then
        rm -f "$rollback_destination" 2>/dev/null || return 1
        mkdir -p "${rollback_destination%/*}" 2>/dev/null || return 1
        mv "$rollback_backup" "$rollback_destination" 2>/dev/null ||
            return 1
    elif [ -e "$rollback_marker" ] &&
        [ ! -e "$STAGE_DIRECTORY/$rollback_relative" ]; then
        rm -f "$rollback_destination" 2>/dev/null || return 1
    fi
    return 0
}

rollback_profile_change() {
    [ -e "$TRANSACTION_DIR/profile.changed" ] || return 0
    if [ -f "$TRANSACTION_DIR/profile.backup" ]; then
        rollback_profile_directory=${PROFILE_PATH%/*}
        rollback_profile_temporary=$(
            mktemp "$rollback_profile_directory/.hardknock-profile.XXXXXX"
        ) || rollback_profile_temporary=
        [ -n "$rollback_profile_temporary" ] || return 1
        if ! cp -p "$TRANSACTION_DIR/profile.backup" \
            "$rollback_profile_temporary" 2>/dev/null ||
            ! mv "$rollback_profile_temporary" "$PROFILE_PATH" \
                2>/dev/null; then
            rm -f "$rollback_profile_temporary" 2>/dev/null || :
            return 1
        fi
    elif [ -e "$TRANSACTION_DIR/profile.absent" ]; then
        rm -f "$PROFILE_PATH" 2>/dev/null || return 1
    fi
    return 0
}

rollback_created_directories() {
    [ -f "$CREATED_DIRECTORIES_FILE" ] || return 0
    awk '{ paths[NR] = $0 } END { for (i = NR; i > 0; i--) print paths[i] }' \
        "$CREATED_DIRECTORIES_FILE" |
        while IFS= read -r rollback_directory; do
            rmdir "$rollback_directory" 2>/dev/null || :
        done
}

rollback_transaction() {
    [ -n "$TRANSACTION_DIR" ] && [ -d "$TRANSACTION_DIR" ] || return 0
    rollback_failed=0
    rollback_profile_change || rollback_failed=1
    rollback_managed_file "$MANIFEST_RELATIVE" || rollback_failed=1
    rollback_managed_file share/doc/hardknock/NOTICE || rollback_failed=1
    rollback_managed_file share/doc/hardknock/LICENSE || rollback_failed=1
    rollback_managed_file bin/hk-effect || rollback_failed=1
    rollback_managed_file bin/hardknock || rollback_failed=1
    rollback_created_directories
    [ "$rollback_failed" -eq 0 ]
}

finish_transaction() {
    TRANSACTION_ACTIVE=0
    rm -rf "$TRANSACTION_DIR" ||
        fail "cannot remove completed installation transaction"
    TRANSACTION_DIR=
}

cleanup() {
    cleanup_status=$?
    trap - 0 1 2 3 15
    rollback_complete=1
    if [ "${TRANSACTION_ACTIVE:-0}" -eq 1 ]; then
        rollback_transaction || rollback_complete=0
    fi
    if [ -n "${TRANSACTION_DIR:-}" ] &&
        [ -d "$TRANSACTION_DIR" ] &&
        [ "$TRANSACTION_DIR" != / ]; then
        if [ "$rollback_complete" -eq 1 ]; then
            rm -rf "$TRANSACTION_DIR"
        else
            printf '%s: rollback incomplete; preserved recovery data at %s\n' \
                "$PROGRAM_NAME" "$TRANSACTION_DIR" >&2
            PREFIX_CREATED=0
        fi
    fi
    release_lock 2>/dev/null || :
    if [ "${PREFIX_CREATED:-0}" -eq 1 ] &&
        [ -n "${PREFIX:-}" ] &&
        [ "$PREFIX" != / ]; then
        rmdir "$PREFIX" 2>/dev/null || :
    fi
    if [ -n "${DOWNLOAD_DIR:-}" ] &&
        [ -d "$DOWNLOAD_DIR" ] &&
        [ "$DOWNLOAD_DIR" != / ]; then
        rm -rf "$DOWNLOAD_DIR"
    fi
    exit "$cleanup_status"
}

determine_install_plan() {
    if [ "$MANIFEST_PRESENT" -eq 1 ] &&
        [ "$OLD_VERSION" = "$VERSION" ] &&
        [ "$OLD_TARGET" = "$TARGET" ]; then
        [ "$OLD_HASH_HARDKNOCK" = "$NEW_HASH_HARDKNOCK" ] &&
            [ "$OLD_HASH_EFFECT" = "$NEW_HASH_EFFECT" ] &&
            [ "$OLD_HASH_LICENSE" = "$NEW_HASH_LICENSE" ] &&
            [ "$OLD_HASH_NOTICE" = "$NEW_HASH_NOTICE" ] ||
            fail "installed version resolves to different release contents; use a new version"
    fi

    INSTALL_CHANGED=1
    if [ "$MANIFEST_PRESENT" -eq 0 ]; then
        INSTALL_KIND=fresh
    elif [ "$OLD_VERSION" != "$VERSION" ] ||
        [ "$OLD_TARGET" != "$TARGET" ]; then
        INSTALL_KIND=upgrade
    elif [ "$STATE_HARDKNOCK" = present ] &&
        [ "$STATE_EFFECT" = present ] &&
        [ "$STATE_LICENSE" = present ] &&
        [ "$STATE_NOTICE" = present ] &&
        [ "$PROFILE_NEEDS_CHANGE" -eq 0 ] &&
        [ "$NEW_PATH_PROFILE" = "$OLD_PATH_PROFILE" ] &&
        [ "$NEW_PATH_PROFILE_CREATED" -eq "$OLD_PATH_PROFILE_CREATED" ]; then
        INSTALL_KIND=noop
        INSTALL_CHANGED=0
    else
        INSTALL_KIND=repair
    fi

    if [ "$INSTALL_CHANGED" -eq 0 ]; then
        ACTION_HARDKNOCK=none
        ACTION_EFFECT=none
        ACTION_LICENSE=none
        ACTION_NOTICE=none
        ACTION_MANIFEST=none
    else
        [ "$STATE_HARDKNOCK" = present ] &&
            ACTION_HARDKNOCK=replace || ACTION_HARDKNOCK=create
        [ "$STATE_EFFECT" = present ] &&
            ACTION_EFFECT=replace || ACTION_EFFECT=create
        [ "$STATE_LICENSE" = present ] &&
            ACTION_LICENSE=replace || ACTION_LICENSE=create
        [ "$STATE_NOTICE" = present ] &&
            ACTION_NOTICE=replace || ACTION_NOTICE=create
        [ "$MANIFEST_PRESENT" -eq 1 ] &&
            ACTION_MANIFEST=replace || ACTION_MANIFEST=create
    fi

    if [ "$NO_MODIFY_PATH" -eq 1 ] ||
        [ "$PROFILE_NEEDS_CHANGE" -eq 0 ]; then
        ACTION_PROFILE=none
    elif [ "$PROFILE_STATE" = absent ]; then
        ACTION_PROFILE=create
    else
        ACTION_PROFILE=update
    fi
}

apply_installation() {
    begin_transaction
    ensure_installation_directories
    stage_installation
    replace_managed_file bin/hardknock ||
        fail "cannot atomically install hardknock"
    replace_managed_file bin/hk-effect ||
        fail "cannot atomically install hk-effect"
    replace_managed_file share/doc/hardknock/LICENSE ||
        fail "cannot atomically install LICENSE"
    replace_managed_file share/doc/hardknock/NOTICE ||
        fail "cannot atomically install NOTICE"
    replace_managed_file "$MANIFEST_RELATIVE" ||
        fail "cannot atomically install managed manifest"

    if [ "$PROFILE_NEEDS_CHANGE" -eq 1 ]; then
        journal_profile_change ||
            fail "cannot journal PATH profile update"
        write_path_block ||
            fail "cannot atomically update PATH profile"
    fi

    finish_transaction
    PREFIX_CREATED=0
}

classify_uninstall_file() {
    classify_relative=$1
    classify_expected_hash=$2
    classify_destination=$PREFIX/$classify_relative
    if [ ! -e "$classify_destination" ] &&
        [ ! -L "$classify_destination" ]; then
        printf '%s\n' absent
        return
    fi
    if [ -L "$classify_destination" ] ||
        [ ! -f "$classify_destination" ]; then
        printf '%s\n' preserve_unsafe
        return
    fi
    classify_uid=$(path_uid "$classify_destination" 2>/dev/null || :)
    classify_mode=$(path_mode "$classify_destination" 2>/dev/null || :)
    if [ "$classify_uid" != "$EFFECTIVE_UID" ] ||
        mode_is_group_or_world_writable "$classify_mode"; then
        printf '%s\n' preserve_unsafe
        return
    fi
    classify_actual_hash=$(sha256_file "$classify_destination") ||
        fail "cannot hash managed file during uninstall: $classify_destination"
    if [ "$classify_actual_hash" != "$classify_expected_hash" ]; then
        printf '%s\n' preserve_modified
        return
    fi
    printf '%s\n' remove
}

prepare_uninstall_plan() {
    STATUS_HARDKNOCK=$(
        classify_uninstall_file bin/hardknock "$OLD_HASH_HARDKNOCK"
    )
    STATUS_EFFECT=$(
        classify_uninstall_file bin/hk-effect "$OLD_HASH_EFFECT"
    )
    STATUS_LICENSE=$(
        classify_uninstall_file \
            share/doc/hardknock/LICENSE "$OLD_HASH_LICENSE"
    )
    STATUS_NOTICE=$(
        classify_uninstall_file \
            share/doc/hardknock/NOTICE "$OLD_HASH_NOTICE"
    )
    PRESERVED_MODIFIED=0
    for uninstall_status in \
        "$STATUS_HARDKNOCK" \
        "$STATUS_EFFECT" \
        "$STATUS_LICENSE" \
        "$STATUS_NOTICE"
    do
        case "$uninstall_status" in
            preserve_*) PRESERVED_MODIFIED=1 ;;
        esac
    done
    prepare_uninstall_profile
}

transaction_remove_managed_file() {
    remove_relative=$1
    remove_status=$2
    [ "$remove_status" = remove ] || return 0
    remove_destination=$PREFIX/$remove_relative
    remove_backup=$BACKUP_DIRECTORY/$remove_relative
    mkdir -p "${remove_backup%/*}" || return 1
    mv "$remove_destination" "$remove_backup"
}

apply_uninstall() {
    begin_transaction
    if [ "$PROFILE_REMOVE" -eq 1 ]; then
        journal_profile_change ||
            fail "cannot journal PATH profile removal"
        remove_path_block
    fi
    transaction_remove_managed_file bin/hardknock "$STATUS_HARDKNOCK" ||
        fail "cannot remove managed hardknock binary"
    transaction_remove_managed_file bin/hk-effect "$STATUS_EFFECT" ||
        fail "cannot remove managed hk-effect binary"
    transaction_remove_managed_file \
        share/doc/hardknock/LICENSE "$STATUS_LICENSE" ||
        fail "cannot remove managed LICENSE"
    transaction_remove_managed_file \
        share/doc/hardknock/NOTICE "$STATUS_NOTICE" ||
        fail "cannot remove managed NOTICE"

    manifest_backup=$BACKUP_DIRECTORY/$MANIFEST_RELATIVE
    mkdir -p "${manifest_backup%/*}" ||
        fail "cannot prepare managed manifest removal"
    mv "$MANIFEST_PATH" "$manifest_backup" ||
        fail "cannot remove managed installation manifest"
    finish_transaction
}

json_install_change() {
    json_change_path=$1
    json_change_action=$2
    printf '{"path":"%s","action":"%s"}' \
        "$(json_escape "$json_change_path")" "$json_change_action"
}

emit_install_json() {
    emit_dry_run=$1
    if [ "$NEW_PATH_PROFILE" = - ]; then
        emit_profile=null
    else
        emit_profile="\"$(json_escape "$NEW_PATH_PROFILE")\""
    fi
    if [ "$MANIFEST_PRESENT" -eq 1 ]; then
        emit_previous_version="\"$(json_escape "$OLD_VERSION")\""
    else
        emit_previous_version=null
    fi
    [ "$INSTALL_CHANGED" -eq 1 ] &&
        emit_changed=true || emit_changed=false
    [ "$NO_MODIFY_PATH" -eq 0 ] &&
        emit_modify_path=true || emit_modify_path=false

    printf '{'
    printf '"schema":"%s","ok":true,"action":"install",' "$RESULT_SCHEMA"
    printf '"install_kind":"%s","dry_run":%s,"changed":%s,' \
        "$INSTALL_KIND" "$emit_dry_run" "$emit_changed"
    printf '"version":"%s","previous_version":%s,"target":"%s",' \
        "$(json_escape "$VERSION")" "$emit_previous_version" "$TARGET"
    printf '"prefix":"%s","source":"%s","archive":"%s",' \
        "$(json_escape "$PREFIX")" "$SOURCE_KIND" "$ARCHIVE_NAME"
    printf '"integrity":"sha256",'
    printf '"provenance":{"status":"%s","verified":%s,"policy":"%s"},' \
        "$PROVENANCE_STATUS" "$PROVENANCE_VERIFIED" "$PROVENANCE_POLICY"
    printf '"modify_path":%s,"managed_profile":%s,' \
        "$emit_modify_path" "$emit_profile"
    printf '"changes":['
    json_install_change "$PREFIX/bin/hardknock" "$ACTION_HARDKNOCK"
    printf ','
    json_install_change "$PREFIX/bin/hk-effect" "$ACTION_EFFECT"
    printf ','
    json_install_change \
        "$PREFIX/share/doc/hardknock/LICENSE" "$ACTION_LICENSE"
    printf ','
    json_install_change \
        "$PREFIX/share/doc/hardknock/NOTICE" "$ACTION_NOTICE"
    printf ','
    json_install_change "$MANIFEST_PATH" "$ACTION_MANIFEST"
    if [ "$NO_MODIFY_PATH" -eq 0 ]; then
        printf ','
        json_install_change "$PROFILE_PATH" "$ACTION_PROFILE"
    fi
    printf '],"warnings":['
    if [ -n "$PROVENANCE_WARNING" ]; then
        printf '"%s"' "$(json_escape "$PROVENANCE_WARNING")"
    fi
    printf '],"next_actions":['
    printf '"%s","%s"' \
        "$(json_escape "$PREFIX/bin/hardknock setup --agent auto --start")" \
        "$(json_escape "$PREFIX/bin/hardknock doctor --strict")"
    printf '],"hardknock_home_preserved":true}\n'
}

print_provenance_human() {
    case "$PROVENANCE_STATUS" in
        verified)
            printf 'Verified GitHub build provenance for %s\n' "$ARCHIVE_NAME"
            ;;
        bypassed|checksum_only_local)
            printf '%s: warning: %s\n' \
                "$PROGRAM_NAME" "$PROVENANCE_WARNING" >&2
            ;;
    esac
}

emit_uninstall_warning_json() {
    warning_path=$1
    warning_status=$2
    case "$warning_status" in
        preserve_modified)
            warning_message="preserved modified managed file: $warning_path"
            ;;
        preserve_unsafe)
            warning_message="preserved unsafe or ambiguously owned managed path: $warning_path"
            ;;
        *) return 0 ;;
    esac
    if [ "$WARNING_SEPARATOR" -eq 1 ]; then
        printf ','
    fi
    printf '"%s"' "$(json_escape "$warning_message")"
    WARNING_SEPARATOR=1
}

emit_uninstall_change_json() {
    uninstall_change_path=$1
    uninstall_change_status=$2
    uninstall_emit_dry_run=$3
    if [ "$uninstall_change_status" = remove ] &&
        [ "$uninstall_emit_dry_run" = false ]; then
        uninstall_change_status=removed
    fi
    printf '{"path":"%s","action":"%s"}' \
        "$(json_escape "$uninstall_change_path")" \
        "$uninstall_change_status"
}

emit_uninstall_json() {
    emit_dry_run=$1
    emit_already_absent=$2
    if [ "$emit_already_absent" = true ]; then
        printf '{'
        printf '"schema":"%s","ok":true,"action":"uninstall",' "$RESULT_SCHEMA"
        printf '"dry_run":%s,"changed":false,"prefix":"%s",' \
            "$emit_dry_run" "$(json_escape "$PREFIX")"
        printf '"already_absent":true,"changes":[],"warnings":[],"next_actions":[],'
        printf '"hardknock_home_preserved":true}\n'
        return
    fi

    [ "$PRESERVED_MODIFIED" -eq 1 ] &&
        emit_preserved=true || emit_preserved=false
    printf '{'
    printf '"schema":"%s","ok":true,"action":"uninstall",' "$RESULT_SCHEMA"
    printf '"dry_run":%s,"changed":true,"prefix":"%s",' \
        "$emit_dry_run" "$(json_escape "$PREFIX")"
    printf '"already_absent":false,"preserved_modified":%s,"changes":[' \
        "$emit_preserved"
    emit_uninstall_change_json \
        "$PREFIX/bin/hardknock" "$STATUS_HARDKNOCK" "$emit_dry_run"
    printf ','
    emit_uninstall_change_json \
        "$PREFIX/bin/hk-effect" "$STATUS_EFFECT" "$emit_dry_run"
    printf ','
    emit_uninstall_change_json \
        "$PREFIX/share/doc/hardknock/LICENSE" \
        "$STATUS_LICENSE" "$emit_dry_run"
    printf ','
    emit_uninstall_change_json \
        "$PREFIX/share/doc/hardknock/NOTICE" \
        "$STATUS_NOTICE" "$emit_dry_run"
    printf ','
    if [ "$emit_dry_run" = true ]; then
        manifest_action=remove
    else
        manifest_action=removed
    fi
    emit_uninstall_change_json "$MANIFEST_PATH" "$manifest_action" true
    if [ "$OLD_PATH_PROFILE" != - ]; then
        printf ','
        if [ "$PROFILE_REMOVE" -eq 1 ]; then
            if [ "$emit_dry_run" = true ]; then
                profile_action=remove_managed_block
            else
                profile_action=removed_managed_block
            fi
        else
            profile_action=none
        fi
        emit_uninstall_change_json \
            "$OLD_PATH_PROFILE" "$profile_action" true
    fi
    printf '],"warnings":['
    WARNING_SEPARATOR=0
    emit_uninstall_warning_json \
        "$PREFIX/bin/hardknock" "$STATUS_HARDKNOCK"
    emit_uninstall_warning_json \
        "$PREFIX/bin/hk-effect" "$STATUS_EFFECT"
    emit_uninstall_warning_json \
        "$PREFIX/share/doc/hardknock/LICENSE" "$STATUS_LICENSE"
    emit_uninstall_warning_json \
        "$PREFIX/share/doc/hardknock/NOTICE" "$STATUS_NOTICE"
    printf '],"next_actions":['
    if [ "$PRESERVED_MODIFIED" -eq 1 ]; then
        printf '"Review preserved files before reinstalling into this prefix."'
    fi
    printf '],"hardknock_home_preserved":true}\n'
}

print_uninstall_warnings() {
    for warning_record in \
        "bin/hardknock|$STATUS_HARDKNOCK" \
        "bin/hk-effect|$STATUS_EFFECT" \
        "share/doc/hardknock/LICENSE|$STATUS_LICENSE" \
        "share/doc/hardknock/NOTICE|$STATUS_NOTICE"
    do
        warning_relative=${warning_record%%|*}
        warning_status=${warning_record#*|}
        case "$warning_status" in
            preserve_modified)
                printf '%s: preserving modified managed file: %s/%s\n' \
                    "$PROGRAM_NAME" "$PREFIX" "$warning_relative" >&2
                ;;
            preserve_unsafe)
                printf '%s: preserving unsafe managed path: %s/%s\n' \
                    "$PROGRAM_NAME" "$PREFIX" "$warning_relative" >&2
                ;;
        esac
    done
}

snapshot_installation() {
    preflight_directories
    load_manifest
    preflight_installation
}

snapshot_uninstall() {
    preflight_directories
    load_manifest
    if [ "$MANIFEST_PRESENT" -eq 1 ]; then
        prepare_uninstall_plan
    fi
}

run_uninstall() {
    if [ "$DRY_RUN" -eq 1 ]; then
        check_no_install_lock
        snapshot_uninstall
        if [ "$MANIFEST_PRESENT" -eq 0 ]; then
            if [ "$JSON" -eq 1 ]; then
                emit_uninstall_json true true
            else
                printf 'No managed Hardknock installation at %s\n' "$PREFIX"
            fi
            return
        fi
        if [ "$JSON" -eq 1 ]; then
            emit_uninstall_json true false
        else
            printf 'Would uninstall managed Hardknock files from %s\n' "$PREFIX"
            print_uninstall_warnings
        fi
        return
    fi

    if [ ! -e "$PREFIX" ] && [ ! -L "$PREFIX" ]; then
        if [ "$JSON" -eq 1 ]; then
            emit_uninstall_json false true
        fi
        return
    fi
    acquire_lock 0
    snapshot_uninstall
    if [ "$MANIFEST_PRESENT" -eq 0 ]; then
        release_lock || fail "cannot release installer lock"
        if [ "$JSON" -eq 1 ]; then
            emit_uninstall_json false true
        fi
        return
    fi

    apply_uninstall
    release_lock || fail "cannot release installer lock"
    if [ "$JSON" -eq 1 ]; then
        emit_uninstall_json false false
    else
        printf 'Uninstalled managed Hardknock files from %s\n' "$PREFIX"
        print_uninstall_warnings
    fi
}

SEEN_VERSION=0
SEEN_PREFIX=0
SEEN_REPOSITORY=0
SEEN_NO_MODIFY_PATH=0
SEEN_DRY_RUN=0
SEEN_JSON=0
SEEN_UNINSTALL=0
SEEN_NO_VERIFY_PROVENANCE=0
while [ "$#" -gt 0 ]; do
    case "$1" in
        --version)
            [ "$SEEN_VERSION" -eq 0 ] ||
                fail "--version may be specified only once"
            [ "$#" -ge 2 ] || fail "--version requires a value"
            VERSION=$2
            SEEN_VERSION=1
            shift 2
            ;;
        --prefix)
            [ "$SEEN_PREFIX" -eq 0 ] ||
                fail "--prefix may be specified only once"
            [ "$#" -ge 2 ] || fail "--prefix requires a value"
            PREFIX=$2
            SEEN_PREFIX=1
            shift 2
            ;;
        --repository)
            [ "$SEEN_REPOSITORY" -eq 0 ] ||
                fail "--repository may be specified only once"
            [ "$#" -ge 2 ] || fail "--repository requires a value"
            REPOSITORY=$2
            REPOSITORY_SET=1
            SEEN_REPOSITORY=1
            shift 2
            ;;
        --no-modify-path)
            [ "$SEEN_NO_MODIFY_PATH" -eq 0 ] ||
                fail "--no-modify-path may be specified only once"
            NO_MODIFY_PATH=1
            SEEN_NO_MODIFY_PATH=1
            shift
            ;;
        --no-verify-provenance)
            [ "$SEEN_NO_VERIFY_PROVENANCE" -eq 0 ] ||
                fail "--no-verify-provenance may be specified only once"
            NO_VERIFY_PROVENANCE=1
            SEEN_NO_VERIFY_PROVENANCE=1
            shift
            ;;
        --dry-run)
            [ "$SEEN_DRY_RUN" -eq 0 ] ||
                fail "--dry-run may be specified only once"
            DRY_RUN=1
            SEEN_DRY_RUN=1
            shift
            ;;
        --json)
            [ "$SEEN_JSON" -eq 0 ] ||
                fail "--json may be specified only once"
            JSON=1
            SEEN_JSON=1
            shift
            ;;
        --uninstall)
            [ "$SEEN_UNINSTALL" -eq 0 ] ||
                fail "--uninstall may be specified only once"
            UNINSTALL=1
            SEEN_UNINSTALL=1
            shift
            ;;
        -h|--help)
            usage
            exit 0
            ;;
        *)
            fail "unknown option: $1"
            ;;
    esac
done

if [ -z "$PREFIX" ]; then
    [ -n "${HOME:-}" ] || fail "HOME is required when --prefix is omitted"
    validate_absolute_path "HOME" "$HOME"
    PREFIX=$HOME/.local
fi
validate_prefix
detect_target
validate_timeout \
    "build-provenance verification timeout" "$PROVENANCE_TIMEOUT_SECONDS"
validate_timeout \
    "candidate binary verification timeout" "$CANDIDATE_TIMEOUT_SECONDS"
validate_path_chain_no_symlinks "$PREFIX" "installation prefix"
MANIFEST_PATH=$PREFIX/$MANIFEST_RELATIVE
LOCK_PATH=$PREFIX/.hardknock-install.lock

trap cleanup 0
trap 'exit 1' 1 2 3 15

if [ "$UNINSTALL" -eq 1 ]; then
    [ "$SEEN_VERSION" -eq 0 ] ||
        fail "--version cannot be combined with --uninstall"
    [ "$REPOSITORY_SET" -eq 0 ] ||
        fail "--repository cannot be combined with --uninstall"
    [ "$NO_MODIFY_PATH" -eq 0 ] ||
        fail "--no-modify-path cannot be combined with --uninstall"
    [ "$NO_VERIFY_PROVENANCE" -eq 0 ] ||
        fail "--no-verify-provenance cannot be combined with --uninstall"
    run_uninstall
    exit 0
fi

[ -n "$VERSION" ] || fail "--version is required for installation"
normalize_version
validate_repository
check_no_install_lock
snapshot_installation

DOWNLOAD_DIR=$(mktemp -d "${TMPDIR:-/tmp}/hardknock-install.XXXXXX") ||
    fail "cannot create temporary download directory"
chmod 0700 "$DOWNLOAD_DIR" ||
    fail "cannot secure temporary download directory"
ARCHIVE_ROOT=hardknock-$VERSION-$TARGET
ARCHIVE_NAME=$ARCHIVE_ROOT.tar.gz
CHECKSUM_NAME=$ARCHIVE_NAME.sha256
ARCHIVE_PATH=$DOWNLOAD_DIR/$ARCHIVE_NAME
CHECKSUM_PATH=$DOWNLOAD_DIR/$CHECKSUM_NAME

fetch_release_file "$CHECKSUM_NAME" "$CHECKSUM_PATH"
fetch_release_file "$ARCHIVE_NAME" "$ARCHIVE_PATH"
verify_checksum "$CHECKSUM_PATH" "$ARCHIVE_PATH" "$ARCHIVE_NAME"
verify_build_provenance "$ARCHIVE_PATH"
validate_archive "$ARCHIVE_PATH" "$ARCHIVE_ROOT"
extract_archive "$ARCHIVE_PATH" "$ARCHIVE_ROOT"
calculate_release_hashes
verify_release_binaries

if [ "$DRY_RUN" -eq 1 ]; then
    check_no_install_lock
    snapshot_installation
    prepare_path_plan
    determine_install_plan
    if [ "$JSON" -eq 1 ]; then
        emit_install_json true
    else
        printf 'Would install Hardknock %s for %s into %s (%s)\n' \
            "$VERSION" "$TARGET" "$PREFIX" "$INSTALL_KIND"
        print_provenance_human
    fi
    exit 0
fi

acquire_lock 1
snapshot_installation
prepare_path_plan
determine_install_plan
if [ "$INSTALL_CHANGED" -eq 1 ]; then
    apply_installation
fi
release_lock || fail "cannot release installer lock"

if [ "$JSON" -eq 1 ]; then
    emit_install_json false
elif [ "$INSTALL_CHANGED" -eq 0 ]; then
    printf 'Hardknock %s is already installed at %s\n' "$VERSION" "$PREFIX"
else
    printf 'Installed Hardknock %s for %s into %s (%s)\n' \
        "$VERSION" "$TARGET" "$PREFIX" "$INSTALL_KIND"
    print_provenance_human
    if [ "$ACTION_PROFILE" != none ]; then
        printf 'Updated managed PATH block in %s\n' "$PROFILE_PATH"
    fi
fi
