#!/bin/sh
# SPDX-License-Identifier: Apache-2.0

set -eu
set -f
umask 077

PROGRAM_NAME=${0##*/}
DEFAULT_REPOSITORY=https://github.com/openkedge/hardknock/releases/download
OFFICIAL_ATTESTATION_REPOSITORY=openkedge/hardknock
OFFICIAL_RELEASE_WORKFLOW=openkedge/hardknock/.github/workflows/release.yml
OFFICIAL_RELEASE_PREDICATE_TYPE=https://openkedge.dev/hardknock/release-publication/v1
OFFICIAL_SLSA_PREDICATE_TYPE=https://slsa.dev/provenance/v1
GITHUB_API_VERSION=2026-03-10
RESULT_SCHEMA=hardknock-installer-result-v1
MANIFEST_FORMAT=hardknock-install-manifest-v1
TRANSACTION_FORMAT=hardknock-install-transaction-v1
LOCK_FORMAT=hardknock-install-lock-v1
MANIFEST_RELATIVE=share/hardknock/install-manifest-v1
PATH_BEGIN='# >>> hardknock managed PATH >>>'
PATH_END='# <<< hardknock managed PATH <<<'
MAX_ARCHIVE_BYTES=268435456
MAX_CHECKSUM_BYTES=4096
MAX_BINARY_BYTES=134217728
MAX_LEGAL_BYTES=8388608
MAX_PROFILE_BYTES=8388608
MAX_PROVENANCE_OUTPUT_BYTES=1048576
MAX_VERSION_OUTPUT_BYTES=4096
MAX_INSTALLER_STATE_BYTES=16384
MAX_TRANSACTION_TREE_BYTES=65536
# Keep this lookup and saturation threshold aligned with release verification
# automation and user-facing verification guidance.
ATTESTATION_LOOKUP_LIMIT=100
MINIMUM_GH_MAJOR=2
MINIMUM_GH_MINOR=97
MINIMUM_GH_PATCH=0
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
TRANSACTION_COMMITTED=0
TRANSACTION_CLEANUP_ALLOWED=0
TRANSACTION_OPERATION=
TRANSACTION_PROFILE_PATH=-
TRANSACTION_PREVIOUS_PROFILE_PATH=-
TRANSACTION_PREFIX_CREATED=0
LOCK_PATH=
LOCK_HELD=0
LOCK_TOKEN=
PREFIX_CREATED=0
RECOVERED_PREFIX_CREATED=0
RECOVERY_LISTING=
PROFILE_PATH=
PROFILE_STATE=untouched
PROFILE_NEEDS_CHANGE=0
PROFILE_REMOVE=0
PREVIOUS_PROFILE_PATH=
PREVIOUS_PROFILE_STATE=untouched
PREVIOUS_PROFILE_REMOVE=0
NEW_PATH_PROFILE=-
NEW_PATH_PROFILE_CREATED=0
PRESERVED_MODIFIED=0
PROVENANCE_STATUS=not_applicable
PROVENANCE_POLICY=not_applicable
PROVENANCE_VERIFIED=false
PROVENANCE_WARNING=
OLD_HASH_MANIFEST=
OLD_ID_HARDKNOCK=
OLD_ID_EFFECT=
OLD_ID_LICENSE=
OLD_ID_NOTICE=
OLD_ID_MANIFEST=
NEW_HASH_MANIFEST=
PLACEMENT_TEMPORARY=
STABLE_TAG_OBJECT=
STABLE_TAG_COMMIT=
STABLE_TAG_TREE=
OFFICIAL_DEFAULT_BRANCH=
OFFICIAL_WORKFLOW_SOURCE_REF=
OFFICIAL_WORKFLOW_SOURCE_COMMIT=
ARCHIVE_PROVENANCE_SHA256=
CUSTOM_ATTESTATION_JQ=
ATTESTATION_COUNT_JQ='if type == "array" then length else -1 end'
DISTINCT_ATTESTATION_BINDING=

usage() {
    cat <<EOF
Usage: $PROGRAM_NAME [OPTIONS]

Install a version-pinned Hardknock binary release without a Rust toolchain.

Options:
  --version VERSION       Release version to install, without or with leading v
  --prefix DIRECTORY      Installation prefix (default: \$HOME/.local)
  --repository LOCATION   HTTPS release base URL or absolute local mirror path
  --no-verify-provenance  Explicitly bypass build-provenance verification
  --no-modify-path        Do not add the prefix bin directory to a login profile
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

is_git_commit_oid() {
    [ "${#1}" -eq 40 ] || return 1
    case "$1" in
        *[!0-9A-Fa-f]*) return 1 ;;
    esac
}

is_lower_git_oid() {
    [ "${#1}" -eq 40 ] || return 1
    case "$1" in
        *[!0-9a-f]*) return 1 ;;
    esac
}

is_lower_sha256() {
    [ "${#1}" -eq 64 ] || return 1
    case "$1" in
        *[!0-9a-f]*) return 1 ;;
    esac
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
        Linux)
            detected_libc=$(getconf GNU_LIBC_VERSION 2>/dev/null || :)
            case "$detected_libc" in
                'glibc '[0-9]*|'GNU libc '[0-9]*)
                    detected_os=unknown-linux-gnu
                    ;;
                *musl*|*MUSL*)
                    fail "musl Linux is not supported; Hardknock release binaries require glibc"
                    ;;
                *)
                    if command -v ldd >/dev/null 2>&1; then
                        detected_libc=$(ldd --version 2>&1 || :)
                    fi
                    case "$detected_libc" in
                        *musl*|*MUSL*)
                            fail "musl Linux is not supported; Hardknock release binaries require glibc"
                            ;;
                        *'GNU C Library'*|*'GNU libc'*|*GLIBC*|*glibc*)
                            detected_os=unknown-linux-gnu
                            ;;
                        *)
                            fail "unsupported Linux C library; Hardknock release binaries require glibc"
                            ;;
                    esac
                    ;;
            esac
            ;;
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

normalize_decimal_component() {
    decimal_component=$1
    case "$decimal_component" in
        ''|*[!0-9]*)
            return 1
            ;;
    esac
    [ "${#decimal_component}" -le 9 ] || return 1
    decimal_component=$(
        printf '%s\n' "$decimal_component" |
            sed 's/^0*//'
    )
    [ -n "$decimal_component" ] || decimal_component=0
    printf '%s\n' "$decimal_component"
}

require_supported_gh_version() {
    gh_version_stdout=$DOWNLOAD_DIR/gh-version-output
    gh_version_stderr=$DOWNLOAD_DIR/gh-version-errors
    if run_bounded_command \
        "$PROVENANCE_TIMEOUT_SECONDS" "$MAX_VERSION_OUTPUT_BYTES" \
        "$gh_version_stdout" "$gh_version_stderr" \
        gh --version; then
        :
    else
        gh_version_status=$?
        if [ "$gh_version_status" -eq 124 ]; then
            fail "GitHub CLI version detection timed out"
        fi
        fail "cannot determine the GitHub CLI version"
    fi

    IFS= read -r gh_version_line <"$gh_version_stdout" ||
        fail "GitHub CLI returned an empty version"
    validate_no_controls "GitHub CLI version" "$gh_version_line"
    case "$gh_version_line" in
        'gh version '[0-9]*)
            gh_version=${gh_version_line#gh version }
            gh_version=${gh_version%% *}
            ;;
        *)
            fail "GitHub CLI returned an unsupported version format"
            ;;
    esac
    gh_major=${gh_version%%.*}
    gh_remainder=${gh_version#*.}
    [ "$gh_remainder" != "$gh_version" ] ||
        fail "GitHub CLI returned an unsupported version format"
    gh_minor=${gh_remainder%%.*}
    gh_patch=${gh_remainder#*.}
    [ "$gh_patch" != "$gh_remainder" ] ||
        fail "GitHub CLI returned an unsupported version format"
    case "$gh_patch" in
        *.*) fail "GitHub CLI returned an unsupported version format" ;;
    esac
    gh_major=$(normalize_decimal_component "$gh_major") ||
        fail "GitHub CLI returned an unsupported version format"
    gh_minor=$(normalize_decimal_component "$gh_minor") ||
        fail "GitHub CLI returned an unsupported version format"
    gh_patch=$(normalize_decimal_component "$gh_patch") ||
        fail "GitHub CLI returned an unsupported version format"

    if [ "$gh_major" -lt "$MINIMUM_GH_MAJOR" ] ||
        { [ "$gh_major" -eq "$MINIMUM_GH_MAJOR" ] &&
            [ "$gh_minor" -lt "$MINIMUM_GH_MINOR" ]; } ||
        { [ "$gh_major" -eq "$MINIMUM_GH_MAJOR" ] &&
            [ "$gh_minor" -eq "$MINIMUM_GH_MINOR" ] &&
            [ "$gh_patch" -lt "$MINIMUM_GH_PATCH" ]; }; then
        fail "GitHub CLI 2.97.0 or newer is required for official provenance verification"
    fi
}

read_single_provenance_line() {
    provenance_line_path=$1
    provenance_line_label=$2
    SINGLE_PROVENANCE_LINE=
    provenance_line_count=0
    while IFS= read -r provenance_line ||
        [ -n "$provenance_line" ]; do
        provenance_line_count=$((provenance_line_count + 1))
        [ "$provenance_line_count" -eq 1 ] ||
            fail "GitHub returned ambiguous $provenance_line_label data"
        validate_no_controls "$provenance_line_label" "$provenance_line"
        [ -n "$provenance_line" ] ||
            fail "GitHub returned empty $provenance_line_label data"
        SINGLE_PROVENANCE_LINE=$provenance_line
    done <"$provenance_line_path"
    [ "$provenance_line_count" -eq 1 ] ||
        fail "GitHub returned empty $provenance_line_label data"
}

run_provenance_gh() {
    provenance_gh_stdout=$1
    provenance_gh_stderr=$2
    provenance_gh_output_limit=$3
    provenance_gh_timeout_error=$4
    provenance_gh_failure_error=$5
    shift 5

    if run_bounded_command \
        "$PROVENANCE_TIMEOUT_SECONDS" "$provenance_gh_output_limit" \
        "$provenance_gh_stdout" "$provenance_gh_stderr" \
        "$@"; then
        return
    else
        provenance_gh_status=$?
    fi
    if [ "$provenance_gh_status" -eq 124 ]; then
        fail "$provenance_gh_timeout_error"
    fi
    if [ "$provenance_gh_status" -eq 127 ]; then
        fail "gh build-provenance verifier is unavailable"
    fi
    fail "$provenance_gh_failure_error"
}

resolve_signed_stable_tag() {
    stable_tag_phase=$1
    stable_tag_name=v$VERSION

    stable_ref_stdout=$DOWNLOAD_DIR/stable-tag-$stable_tag_phase-ref-output
    stable_ref_stderr=$DOWNLOAD_DIR/stable-tag-$stable_tag_phase-ref-errors
    run_provenance_gh \
        "$stable_ref_stdout" "$stable_ref_stderr" \
        "$MAX_VERSION_OUTPUT_BYTES" \
        "official stable-tag resolution timed out" \
        "cannot resolve the official stable tag" \
        gh api \
            -H "X-GitHub-Api-Version: $GITHUB_API_VERSION" \
            "repos/$OFFICIAL_ATTESTATION_REPOSITORY/git/ref/tags/$stable_tag_name" \
            --jq '.ref + "|" + .object.type + "|" + .object.sha'
    read_single_provenance_line "$stable_ref_stdout" "stable-tag reference"
    IFS='|' read -r \
        resolved_tag_ref resolved_tag_type resolved_tag_object \
        resolved_tag_extra <<EOF
$SINGLE_PROVENANCE_LINE
EOF
    [ "$resolved_tag_ref|$resolved_tag_type|$resolved_tag_object" = \
        "$SINGLE_PROVENANCE_LINE" ] &&
        [ -z "$resolved_tag_extra" ] ||
        fail "GitHub returned malformed stable-tag reference data"
    [ "$resolved_tag_ref" = "refs/tags/$stable_tag_name" ] ||
        fail "official stable-tag reference does not match the requested release"
    [ "$resolved_tag_type" = tag ] ||
        fail "official stable tag must be a signed annotated tag"
    is_lower_git_oid "$resolved_tag_object" ||
        fail "GitHub returned an invalid stable-tag object digest"

    stable_object_stdout=$DOWNLOAD_DIR/stable-tag-$stable_tag_phase-object-output
    stable_object_stderr=$DOWNLOAD_DIR/stable-tag-$stable_tag_phase-object-errors
    run_provenance_gh \
        "$stable_object_stdout" "$stable_object_stderr" \
        "$MAX_VERSION_OUTPUT_BYTES" \
        "official stable-tag signature resolution timed out" \
        "cannot verify the official stable-tag signature" \
        gh api \
            -H "X-GitHub-Api-Version: $GITHUB_API_VERSION" \
            "repos/$OFFICIAL_ATTESTATION_REPOSITORY/git/tags/$resolved_tag_object" \
            --jq '[.tag, (.verification.verified | tostring), .verification.reason, .object.type, .object.sha] | join("|")'
    read_single_provenance_line "$stable_object_stdout" "stable-tag signature"
    IFS='|' read -r \
        resolved_tag_name resolved_tag_verified resolved_tag_reason \
        resolved_target_type resolved_target_commit resolved_tag_extra <<EOF
$SINGLE_PROVENANCE_LINE
EOF
    [ "$resolved_tag_name|$resolved_tag_verified|$resolved_tag_reason|$resolved_target_type|$resolved_target_commit" = \
        "$SINGLE_PROVENANCE_LINE" ] &&
        [ -z "$resolved_tag_extra" ] ||
        fail "GitHub returned malformed stable-tag signature data"
    [ "$resolved_tag_name" = "$stable_tag_name" ] ||
        fail "official annotated tag names a different release"
    [ "$resolved_tag_verified" = true ] &&
        [ "$resolved_tag_reason" = valid ] ||
        fail "official stable-tag signature is not valid"
    [ "$resolved_target_type" = commit ] ||
        fail "official stable tag must point directly to a commit"
    is_lower_git_oid "$resolved_target_commit" ||
        fail "GitHub returned an invalid stable-tag commit digest"

    stable_commit_stdout=$DOWNLOAD_DIR/stable-tag-$stable_tag_phase-commit-output
    stable_commit_stderr=$DOWNLOAD_DIR/stable-tag-$stable_tag_phase-commit-errors
    run_provenance_gh \
        "$stable_commit_stdout" "$stable_commit_stderr" \
        "$MAX_VERSION_OUTPUT_BYTES" \
        "official stable-tag commit resolution timed out" \
        "cannot resolve the official stable-tag commit tree" \
        gh api \
            -H "X-GitHub-Api-Version: $GITHUB_API_VERSION" \
            "repos/$OFFICIAL_ATTESTATION_REPOSITORY/git/commits/$resolved_target_commit" \
            --jq '[.sha, .tree.sha] | join("|")'
    read_single_provenance_line "$stable_commit_stdout" "stable-tag commit"
    IFS='|' read -r \
        resolved_commit resolved_tree resolved_commit_extra <<EOF
$SINGLE_PROVENANCE_LINE
EOF
    [ "$resolved_commit|$resolved_tree" = "$SINGLE_PROVENANCE_LINE" ] &&
        [ -z "$resolved_commit_extra" ] ||
        fail "GitHub returned malformed stable-tag commit data"
    [ "$resolved_commit" = "$resolved_target_commit" ] ||
        fail "official stable-tag commit resolution returned a different commit"
    is_lower_git_oid "$resolved_tree" ||
        fail "GitHub returned an invalid stable-tag tree digest"

    STABLE_TAG_OBJECT=$resolved_tag_object
    STABLE_TAG_COMMIT=$resolved_commit
    STABLE_TAG_TREE=$resolved_tree
}

resolve_official_default_branch() {
    default_branch_phase=$1
    default_branch_stdout=$DOWNLOAD_DIR/default-branch-$default_branch_phase-output
    default_branch_stderr=$DOWNLOAD_DIR/default-branch-$default_branch_phase-errors
    run_provenance_gh \
        "$default_branch_stdout" "$default_branch_stderr" \
        "$MAX_VERSION_OUTPUT_BYTES" \
        "official default-branch resolution timed out" \
        "cannot resolve the official default branch" \
        gh api \
            -H "X-GitHub-Api-Version: $GITHUB_API_VERSION" \
            "repos/$OFFICIAL_ATTESTATION_REPOSITORY" \
            --jq '.default_branch'
    read_single_provenance_line "$default_branch_stdout" "default-branch"
    [ "${#SINGLE_PROVENANCE_LINE}" -le 255 ] ||
        fail "GitHub returned an invalid default-branch name"
    case "$SINGLE_PROVENANCE_LINE" in
        refs/*|/*|*/|*'..'*|*'@{'*|*.lock|*' '*|*~*|*^*|*:*|*\?*|*\**|*\[*|*\\*)
            fail "GitHub returned an invalid default-branch name"
            ;;
    esac
    RESOLVED_DEFAULT_BRANCH=$SINGLE_PROVENANCE_LINE
}

url_encode_path_segment() {
    LC_ALL=C printf '%s' "$1" |
        od -An -v -tu1 |
        awk '{
            for (field = 1; field <= NF; field++) {
                byte = $field + 0
                if ((byte >= 48 && byte <= 57) ||
                    (byte >= 65 && byte <= 90) ||
                    (byte >= 97 && byte <= 122) ||
                    byte == 45 || byte == 46 ||
                    byte == 95 || byte == 126) {
                    printf "%c", byte
                } else {
                    printf "%%%02X", byte
                }
            }
        } END { printf "\n" }'
}

build_custom_attestation_jq() {
    CUSTOM_ATTESTATION_JQ='def text: if type == "string" then . else "!" end; if type != "array" then "!" else ("COUNT|" + (length | tostring)), (.[] | (try .verificationResult.statement catch null) as $s | (try $s.predicate catch null) as $p | if (($s | type) == "object" and ($p | type) == "object" and (($p.asset_digests | type) == "object")) then [($s.predicateType | text), ($p | keys | join(",")), ($p.schema | text), ($p.channel | text), ($p.release_tag | text), ($p.candidate_tag | text), ($p.artifact_source_commit | text), ($p.artifact_source_tree | text), ($p.workflow_source_ref | text), ($p.workflow_source_commit | text), ($p.promotion_record_sha256 | text), ($p.asset_digests | to_entries | sort_by(.key) | map([(.key | text), (.value | text)] | join("=")) | join(","))] | join("|") else "!" end) end'
}

load_distinct_attestation_binding() {
    attestation_binding_path=$1
    attestation_binding_label=$2
    DISTINCT_ATTESTATION_BINDING=
    attestation_result_count=
    attestation_binding_count=0
    attestation_output_lines=0
    while IFS= read -r attestation_binding_line ||
        [ -n "$attestation_binding_line" ]; do
        attestation_output_lines=$((attestation_output_lines + 1))
        validate_no_controls \
            "custom release attestation binding" "$attestation_binding_line"
        [ -n "$attestation_binding_line" ] ||
            fail "official release attestation returned an empty binding"
        if [ "$attestation_output_lines" -eq 1 ]; then
            case "$attestation_binding_line" in
                COUNT\|*)
                    attestation_result_count=${attestation_binding_line#COUNT|}
                    ;;
                *)
                    fail "$attestation_binding_label returned a malformed result count"
                    ;;
            esac
            case "$attestation_result_count" in
                ''|*[!0-9]*|0)
                    fail "$attestation_binding_label returned an invalid result count"
                    ;;
            esac
            [ "$attestation_result_count" -lt \
                "$ATTESTATION_LOOKUP_LIMIT" ] ||
                fail "$attestation_binding_label lookup reached the configured limit"
            continue
        fi
        attestation_binding_count=$((attestation_binding_count + 1))
        if [ -z "$DISTINCT_ATTESTATION_BINDING" ]; then
            DISTINCT_ATTESTATION_BINDING=$attestation_binding_line
        elif [ "$DISTINCT_ATTESTATION_BINDING" != \
            "$attestation_binding_line" ]; then
            fail "official release attestations contain multiple distinct bindings"
        fi
    done <"$attestation_binding_path"
    [ -n "$attestation_result_count" ] &&
        [ "$attestation_binding_count" -eq "$attestation_result_count" ] ||
        fail "$attestation_binding_label returned an inconsistent result count"
    [ "$attestation_binding_count" -gt 0 ] &&
        [ -n "$DISTINCT_ATTESTATION_BINDING" ] ||
        fail "official release attestation did not contain a binding"
}

validate_attestation_result_count() {
    attestation_count_path=$1
    attestation_count_label=$2
    read_single_provenance_line \
        "$attestation_count_path" "$attestation_count_label result count"
    attestation_result_count=$SINGLE_PROVENANCE_LINE
    case "$attestation_result_count" in
        ''|*[!0-9]*|0)
            fail "$attestation_count_label returned an invalid result count"
            ;;
    esac
    [ "$attestation_result_count" -lt "$ATTESTATION_LOOKUP_LIMIT" ] ||
        fail "$attestation_count_label lookup reached the configured limit"
}

parse_asset_digest_pair() {
    asset_pair=$1
    expected_asset_name=$2
    case "$asset_pair" in
        "$expected_asset_name="*)
            PARSED_ASSET_DIGEST=${asset_pair#*=}
            ;;
        *)
            fail "official release attestation has an unexpected asset set"
            ;;
    esac
    is_lower_sha256 "$PARSED_ASSET_DIGEST" ||
        fail "official release attestation has a malformed asset digest"
}

validate_custom_attestation_binding() {
    custom_binding=$1
    custom_binding_fields=$(printf '%s\n' "$custom_binding" |
        awk -F '|' 'NR == 1 { print NF }')
    [ "$custom_binding_fields" -eq 12 ] ||
        fail "official release attestation predicate is malformed"
    IFS='|' read -r \
        binding_predicate_type binding_predicate_keys \
        binding_schema binding_channel binding_release_tag \
        binding_candidate_tag binding_artifact_commit \
        binding_artifact_tree binding_workflow_ref \
        binding_workflow_commit binding_promotion_record \
        binding_asset_pairs <<EOF
$custom_binding
EOF

    expected_predicate_keys=artifact_source_commit,artifact_source_tree,asset_digests,candidate_tag,channel,promotion_record_sha256,release_tag,schema,workflow_source_commit,workflow_source_ref
    [ "$binding_predicate_type" = "$OFFICIAL_RELEASE_PREDICATE_TYPE" ] &&
        [ "$binding_predicate_keys" = "$expected_predicate_keys" ] &&
        [ "$binding_schema" = hardknock-release-publication-v1 ] &&
        [ "$binding_channel" = stable ] &&
        [ "$binding_release_tag" = "v$VERSION" ] ||
        fail "official release attestation predicate does not match the stable release contract"

    candidate_prefix=v$VERSION-rc.
    case "$binding_candidate_tag" in
        "$candidate_prefix"*) candidate_number=${binding_candidate_tag#"$candidate_prefix"} ;;
        *) fail "official release attestation has an invalid candidate tag" ;;
    esac
    case "$candidate_number" in
        ''|*[!0-9]*|0|0*)
            fail "official release attestation has an invalid candidate tag"
            ;;
    esac

    [ "$binding_artifact_commit" = "$STABLE_TAG_COMMIT" ] &&
        [ "$binding_artifact_tree" = "$STABLE_TAG_TREE" ] ||
        fail "official release attestation does not bind the signed stable tag"
    [ "$binding_workflow_ref" = "$OFFICIAL_WORKFLOW_SOURCE_REF" ] ||
        fail "official release attestation does not bind the default-branch workflow"
    is_lower_git_oid "$binding_workflow_commit" ||
        fail "official release attestation has an invalid workflow source commit"
    is_lower_sha256 "$binding_promotion_record" ||
        fail "official release attestation has an invalid promotion record digest"

    asset_pair_fields=$(printf '%s\n' "$binding_asset_pairs" |
        awk -F ',' 'NR == 1 { print NF }')
    [ "$asset_pair_fields" -eq 12 ] ||
        fail "official release attestation has an unexpected asset set"
    IFS=',' read -r \
        asset_aarch64_darwin asset_aarch64_darwin_checksum \
        asset_aarch64_linux asset_aarch64_linux_checksum \
        asset_sbom asset_third_party \
        asset_x86_64_darwin asset_x86_64_darwin_checksum \
        asset_x86_64_linux asset_x86_64_linux_checksum \
        asset_installer asset_installer_checksum <<EOF
$binding_asset_pairs
EOF

    parse_asset_digest_pair \
        "$asset_aarch64_darwin" \
        "hardknock-$VERSION-aarch64-apple-darwin.tar.gz"
    digest_aarch64_darwin=$PARSED_ASSET_DIGEST
    parse_asset_digest_pair \
        "$asset_aarch64_darwin_checksum" \
        "hardknock-$VERSION-aarch64-apple-darwin.tar.gz.sha256"
    parse_asset_digest_pair \
        "$asset_aarch64_linux" \
        "hardknock-$VERSION-aarch64-unknown-linux-gnu.tar.gz"
    digest_aarch64_linux=$PARSED_ASSET_DIGEST
    parse_asset_digest_pair \
        "$asset_aarch64_linux_checksum" \
        "hardknock-$VERSION-aarch64-unknown-linux-gnu.tar.gz.sha256"
    parse_asset_digest_pair \
        "$asset_sbom" "hardknock-$VERSION-sbom.cdx.json"
    parse_asset_digest_pair \
        "$asset_third_party" \
        "hardknock-$VERSION-third-party-licenses.json"
    parse_asset_digest_pair \
        "$asset_x86_64_darwin" \
        "hardknock-$VERSION-x86_64-apple-darwin.tar.gz"
    digest_x86_64_darwin=$PARSED_ASSET_DIGEST
    parse_asset_digest_pair \
        "$asset_x86_64_darwin_checksum" \
        "hardknock-$VERSION-x86_64-apple-darwin.tar.gz.sha256"
    parse_asset_digest_pair \
        "$asset_x86_64_linux" \
        "hardknock-$VERSION-x86_64-unknown-linux-gnu.tar.gz"
    digest_x86_64_linux=$PARSED_ASSET_DIGEST
    parse_asset_digest_pair \
        "$asset_x86_64_linux_checksum" \
        "hardknock-$VERSION-x86_64-unknown-linux-gnu.tar.gz.sha256"
    parse_asset_digest_pair "$asset_installer" "install-hardknock"
    parse_asset_digest_pair \
        "$asset_installer_checksum" "install-hardknock.sha256"

    case "$ARCHIVE_NAME" in
        "hardknock-$VERSION-aarch64-apple-darwin.tar.gz")
            bound_archive_digest=$digest_aarch64_darwin
            ;;
        "hardknock-$VERSION-aarch64-unknown-linux-gnu.tar.gz")
            bound_archive_digest=$digest_aarch64_linux
            ;;
        "hardknock-$VERSION-x86_64-apple-darwin.tar.gz")
            bound_archive_digest=$digest_x86_64_darwin
            ;;
        "hardknock-$VERSION-x86_64-unknown-linux-gnu.tar.gz")
            bound_archive_digest=$digest_x86_64_linux
            ;;
        *)
            fail "installer selected an unsupported provenance asset"
            ;;
    esac
    [ "$bound_archive_digest" = "$ARCHIVE_PROVENANCE_SHA256" ] ||
        fail "official release attestation does not bind the downloaded archive"

    OFFICIAL_WORKFLOW_SOURCE_COMMIT=$binding_workflow_commit
}

confirm_promotion_in_default_branch() {
    resolve_official_default_branch final
    [ "$RESOLVED_DEFAULT_BRANCH" = "$OFFICIAL_DEFAULT_BRANCH" ] ||
        fail "official default branch changed during provenance verification"

    encoded_default_branch=$(url_encode_path_segment "$OFFICIAL_DEFAULT_BRANCH") ||
        fail "cannot encode the official default-branch name"
    [ -n "$encoded_default_branch" ] ||
        fail "cannot encode the official default-branch name"

    branch_head_stdout=$DOWNLOAD_DIR/default-branch-head-output
    branch_head_stderr=$DOWNLOAD_DIR/default-branch-head-errors
    run_provenance_gh \
        "$branch_head_stdout" "$branch_head_stderr" \
        "$MAX_VERSION_OUTPUT_BYTES" \
        "official default-branch head resolution timed out" \
        "cannot resolve the official default-branch head" \
        gh api \
            -H "X-GitHub-Api-Version: $GITHUB_API_VERSION" \
            "repos/$OFFICIAL_ATTESTATION_REPOSITORY/branches/$encoded_default_branch" \
            --jq '.commit.sha'
    read_single_provenance_line "$branch_head_stdout" "default-branch head"
    default_branch_head=$SINGLE_PROVENANCE_LINE
    is_lower_git_oid "$default_branch_head" ||
        fail "GitHub returned an invalid default-branch head digest"

    branch_compare_stdout=$DOWNLOAD_DIR/default-branch-comparison-output
    branch_compare_stderr=$DOWNLOAD_DIR/default-branch-comparison-errors
    run_provenance_gh \
        "$branch_compare_stdout" "$branch_compare_stderr" \
        "$MAX_VERSION_OUTPUT_BYTES" \
        "official promotion ancestry verification timed out" \
        "cannot verify official promotion ancestry" \
        gh api \
            -H "X-GitHub-Api-Version: $GITHUB_API_VERSION" \
            "repos/$OFFICIAL_ATTESTATION_REPOSITORY/compare/$OFFICIAL_WORKFLOW_SOURCE_COMMIT...$default_branch_head" \
            --jq '[.status, .merge_base_commit.sha, .head_commit.sha] | join("|")'
    read_single_provenance_line \
        "$branch_compare_stdout" "promotion ancestry"
    IFS='|' read -r \
        comparison_status comparison_merge_base comparison_head \
        comparison_extra <<EOF
$SINGLE_PROVENANCE_LINE
EOF
    [ "$comparison_status|$comparison_merge_base|$comparison_head" = \
        "$SINGLE_PROVENANCE_LINE" ] &&
        [ -z "$comparison_extra" ] ||
        fail "GitHub returned malformed promotion ancestry data"
    case "$comparison_status" in
        ahead|identical) ;;
        *) fail "official promotion commit is not in current default-branch history" ;;
    esac
    [ "$comparison_merge_base" = "$OFFICIAL_WORKFLOW_SOURCE_COMMIT" ] &&
        [ "$comparison_head" = "$default_branch_head" ] ||
        fail "official promotion commit is not in current default-branch history"
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

path_identity() {
    case "$HOST_SYSTEM" in
        Darwin) stat -f '%d:%i' "$1" ;;
        Linux) stat -c '%d:%i' "$1" ;;
        *) return 1 ;;
    esac
}

is_path_identity() {
    identity_value=$1
    case "$identity_value" in
        *:*)
            identity_device=${identity_value%%:*}
            identity_inode=${identity_value#*:}
            ;;
        *)
            return 1
            ;;
    esac
    case "$identity_device:$identity_inode" in
        *:*:*) return 1 ;;
    esac
    case "$identity_device" in
        ''|*[!0-9]*) return 1 ;;
    esac
    case "$identity_inode" in
        ''|*[!0-9]*) return 1 ;;
    esac
}

move_no_replace() {
    move_source=$1
    move_destination=$2
    move_destination_directory=${move_destination%/*}
    move_source_directory=${move_source%/*}
    [ -f "$move_source" ] && [ ! -L "$move_source" ] || return 1
    [ -d "$move_destination_directory" ] &&
        [ ! -L "$move_destination_directory" ] || return 1
    if [ -e "$move_destination" ] || [ -L "$move_destination" ]; then
        return 1
    fi
    move_source_identity=$(path_identity "$move_source") || return 1
    move_source_hash=$(sha256_file "$move_source") || return 1
    move_source_mode=$(path_mode "$move_source") || return 1
    durability_barrier "$move_source" || return 1

    if link "$move_source" "$move_destination" 2>/dev/null; then
        [ -f "$move_destination" ] &&
            [ ! -L "$move_destination" ] &&
            [ "$(path_identity "$move_destination")" = \
                "$move_source_identity" ] &&
            [ "$(sha256_file "$move_destination")" = "$move_source_hash" ] &&
            [ "$(path_mode "$move_destination")" = "$move_source_mode" ] ||
            return 1
        rm -f "$move_source" || return 1
        durability_barrier "$move_destination_directory" || return 1
        if [ "$move_source_directory" != "$move_destination_directory" ]; then
            durability_barrier "$move_source_directory" || return 1
        fi
        return 0
    fi

    [ ! -e "$move_destination" ] &&
        [ ! -L "$move_destination" ] || return 1
    move_staging=$(
        mktemp "$move_destination_directory/.hardknock-place.XXXXXX"
    ) || move_staging=
    [ -n "$move_staging" ] || return 1
    PLACEMENT_TEMPORARY=$move_staging
    if ! cp -p "$move_source" "$move_staging" 2>/dev/null; then
        rm -f "$move_staging" 2>/dev/null || :
        PLACEMENT_TEMPORARY=
        return 1
    fi
    move_staging_identity=$(path_identity "$move_staging") || {
        rm -f "$move_staging" 2>/dev/null || :
        PLACEMENT_TEMPORARY=
        return 1
    }
    if [ "$(path_identity "$move_source")" != "$move_source_identity" ] ||
        [ "$(sha256_file "$move_source")" != "$move_source_hash" ] ||
        [ "$(path_mode "$move_source")" != "$move_source_mode" ] ||
        [ "$(sha256_file "$move_staging")" != "$move_source_hash" ] ||
        [ "$(path_mode "$move_staging")" != "$move_source_mode" ]; then
        rm -f "$move_staging" 2>/dev/null || :
        PLACEMENT_TEMPORARY=
        return 1
    fi
    durability_barrier "$move_staging" || {
        rm -f "$move_staging" 2>/dev/null || :
        PLACEMENT_TEMPORARY=
        return 1
    }
    if ! link "$move_staging" "$move_destination" 2>/dev/null; then
        rm -f "$move_staging" 2>/dev/null || :
        PLACEMENT_TEMPORARY=
        return 1
    fi
    if [ ! -f "$move_destination" ] ||
        [ -L "$move_destination" ] ||
        [ "$(path_identity "$move_destination")" != "$move_staging_identity" ] ||
        [ "$(sha256_file "$move_destination")" != "$move_source_hash" ] ||
        [ "$(path_mode "$move_destination")" != "$move_source_mode" ]; then
        rm -f "$move_staging" 2>/dev/null || :
        PLACEMENT_TEMPORARY=
        return 1
    fi
    rm -f "$move_staging" || return 1
    PLACEMENT_TEMPORARY=
    durability_barrier "$move_destination_directory" || return 1
    [ "$(path_identity "$move_source")" = "$move_source_identity" ] &&
        [ "$(sha256_file "$move_source")" = "$move_source_hash" ] &&
        [ "$(path_mode "$move_source")" = "$move_source_mode" ] ||
        return 1
    rm -f "$move_source" || return 1
    durability_barrier "$move_source_directory"
}

verify_captured_file() {
    captured_path=$1
    captured_identity=$2
    captured_hash=$3
    captured_limit=$4
    captured_label=$5
    assert_secure_file "$captured_path" "$captured_label"
    require_max_size "$captured_path" "$captured_limit" "$captured_label"
    [ "$(path_identity "$captured_path")" = "$captured_identity" ] &&
        [ "$(sha256_file "$captured_path")" = "$captured_hash" ]
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
    require_supported_gh_version

    ARCHIVE_PROVENANCE_SHA256=$(sha256_file "$provenance_archive") ||
        fail "cannot calculate the downloaded archive digest for provenance verification"
    is_lower_sha256 "$ARCHIVE_PROVENANCE_SHA256" ||
        fail "cannot calculate a canonical downloaded archive digest"

    # The signed stable tag binds the artifact source. The publication
    # workflow remains a workflow_dispatch run from the current default branch.
    resolve_signed_stable_tag initial
    initial_stable_tag_object=$STABLE_TAG_OBJECT
    initial_stable_tag_commit=$STABLE_TAG_COMMIT
    initial_stable_tag_tree=$STABLE_TAG_TREE

    release_stdout=$DOWNLOAD_DIR/release-verification-output
    release_stderr=$DOWNLOAD_DIR/release-verification-errors
    run_provenance_gh \
        "$release_stdout" "$release_stderr" \
        "$MAX_PROVENANCE_OUTPUT_BYTES" \
        "official release verification timed out" \
        "official release verification failed" \
        gh release verify "v$VERSION" \
            --repo "$OFFICIAL_ATTESTATION_REPOSITORY"

    release_asset_stdout=$DOWNLOAD_DIR/release-asset-verification-output
    release_asset_stderr=$DOWNLOAD_DIR/release-asset-verification-errors
    run_provenance_gh \
        "$release_asset_stdout" "$release_asset_stderr" \
        "$MAX_PROVENANCE_OUTPUT_BYTES" \
        "official release asset verification timed out" \
        "official release asset verification failed" \
        gh release verify-asset "v$VERSION" "$provenance_archive" \
            --repo "$OFFICIAL_ATTESTATION_REPOSITORY"

    resolve_official_default_branch initial
    OFFICIAL_DEFAULT_BRANCH=$RESOLVED_DEFAULT_BRANCH
    OFFICIAL_WORKFLOW_SOURCE_REF=refs/heads/$OFFICIAL_DEFAULT_BRANCH
    build_custom_attestation_jq

    custom_discovery_stdout=$DOWNLOAD_DIR/custom-attestation-discovery-output
    custom_discovery_stderr=$DOWNLOAD_DIR/custom-attestation-discovery-errors
    run_provenance_gh \
        "$custom_discovery_stdout" "$custom_discovery_stderr" \
        "$MAX_PROVENANCE_OUTPUT_BYTES" \
        "official release attestation discovery timed out" \
        "official release attestation discovery failed" \
        gh attestation verify "$provenance_archive" \
            --repo "$OFFICIAL_ATTESTATION_REPOSITORY" \
            --limit "$ATTESTATION_LOOKUP_LIMIT" \
            --signer-workflow "$OFFICIAL_RELEASE_WORKFLOW" \
            --deny-self-hosted-runners \
            --source-ref "$OFFICIAL_WORKFLOW_SOURCE_REF" \
            --predicate-type "$OFFICIAL_RELEASE_PREDICATE_TYPE" \
            --format json \
            --jq "$CUSTOM_ATTESTATION_JQ"
    load_distinct_attestation_binding \
        "$custom_discovery_stdout" "official release attestation discovery"
    discovered_attestation_binding=$DISTINCT_ATTESTATION_BINDING
    validate_custom_attestation_binding "$discovered_attestation_binding"

    # Pin both the source repository commit and the reusable workflow signer
    # commit learned from the exact custom release predicate.
    custom_exact_stdout=$DOWNLOAD_DIR/custom-attestation-exact-output
    custom_exact_stderr=$DOWNLOAD_DIR/custom-attestation-exact-errors
    run_provenance_gh \
        "$custom_exact_stdout" "$custom_exact_stderr" \
        "$MAX_PROVENANCE_OUTPUT_BYTES" \
        "exact official release attestation verification timed out" \
        "exact official release attestation verification failed" \
        gh attestation verify "$provenance_archive" \
            --repo "$OFFICIAL_ATTESTATION_REPOSITORY" \
            --limit "$ATTESTATION_LOOKUP_LIMIT" \
            --signer-workflow "$OFFICIAL_RELEASE_WORKFLOW" \
            --deny-self-hosted-runners \
            --source-ref "$OFFICIAL_WORKFLOW_SOURCE_REF" \
            --source-digest "$OFFICIAL_WORKFLOW_SOURCE_COMMIT" \
            --signer-digest "$OFFICIAL_WORKFLOW_SOURCE_COMMIT" \
            --predicate-type "$OFFICIAL_RELEASE_PREDICATE_TYPE" \
            --format json \
            --jq "$CUSTOM_ATTESTATION_JQ"
    load_distinct_attestation_binding \
        "$custom_exact_stdout" "exact official release attestation"
    [ "$DISTINCT_ATTESTATION_BINDING" = \
        "$discovered_attestation_binding" ] ||
        fail "exact official release attestation returned a different binding"

    slsa_stdout=$DOWNLOAD_DIR/slsa-attestation-output
    slsa_stderr=$DOWNLOAD_DIR/slsa-attestation-errors
    run_provenance_gh \
        "$slsa_stdout" "$slsa_stderr" \
        "$MAX_PROVENANCE_OUTPUT_BYTES" \
        "official SLSA provenance verification timed out" \
        "official SLSA provenance verification failed" \
        gh attestation verify "$provenance_archive" \
            --repo "$OFFICIAL_ATTESTATION_REPOSITORY" \
            --limit "$ATTESTATION_LOOKUP_LIMIT" \
            --signer-workflow "$OFFICIAL_RELEASE_WORKFLOW" \
            --deny-self-hosted-runners \
            --source-ref "$OFFICIAL_WORKFLOW_SOURCE_REF" \
            --source-digest "$OFFICIAL_WORKFLOW_SOURCE_COMMIT" \
            --signer-digest "$OFFICIAL_WORKFLOW_SOURCE_COMMIT" \
            --predicate-type "$OFFICIAL_SLSA_PREDICATE_TYPE" \
            --format json \
            --jq "$ATTESTATION_COUNT_JQ"
    validate_attestation_result_count \
        "$slsa_stdout" "official SLSA attestation"

    confirm_promotion_in_default_branch

    resolve_signed_stable_tag final
    [ "$STABLE_TAG_OBJECT" = "$initial_stable_tag_object" ] &&
        [ "$STABLE_TAG_COMMIT" = "$initial_stable_tag_commit" ] &&
        [ "$STABLE_TAG_TREE" = "$initial_stable_tag_tree" ] ||
        fail "official stable tag changed during provenance verification"

    final_archive_digest=$(sha256_file "$provenance_archive") ||
        fail "cannot recalculate the downloaded archive digest"
    [ "$final_archive_digest" = "$ARCHIVE_PROVENANCE_SHA256" ] ||
        fail "downloaded archive changed during provenance verification"

    final_release_asset_stdout=$DOWNLOAD_DIR/final-release-asset-verification-output
    final_release_asset_stderr=$DOWNLOAD_DIR/final-release-asset-verification-errors
    run_provenance_gh \
        "$final_release_asset_stdout" "$final_release_asset_stderr" \
        "$MAX_PROVENANCE_OUTPUT_BYTES" \
        "final official release asset verification timed out" \
        "final official release asset verification failed" \
        gh release verify-asset "v$VERSION" "$provenance_archive" \
            --repo "$OFFICIAL_ATTESTATION_REPOSITORY"
    final_archive_digest=$(sha256_file "$provenance_archive") ||
        fail "cannot recalculate the downloaded archive digest"
    [ "$final_archive_digest" = "$ARCHIVE_PROVENANCE_SHA256" ] ||
        fail "downloaded archive changed during provenance verification"

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
    OLD_HASH_MANIFEST=
    OLD_ID_HARDKNOCK=
    OLD_ID_EFFECT=
    OLD_ID_LICENSE=
    OLD_ID_NOTICE=
    OLD_ID_MANIFEST=
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

validate_managed_profile_path() {
    managed_profile_label=$1
    managed_profile_path=$2
    [ "$managed_profile_path" != - ] || return 0
    validate_absolute_path "$managed_profile_label" "$managed_profile_path"
    [ -n "${HOME:-}" ] ||
        fail "HOME is required to validate the managed PATH profile"
    validate_absolute_path "HOME" "$HOME"
    case "$managed_profile_path" in
        "$HOME/.profile"|"$HOME/.zprofile") ;;
        *) fail "$managed_profile_label references an unexpected PATH profile" ;;
    esac
}

select_login_profile() {
    selected_shell=${SHELL:-}
    if [ -z "$selected_shell" ]; then
        selected_account=$(id -un 2>/dev/null || :)
        if [ -n "$selected_account" ]; then
            case "$HOST_SYSTEM" in
                Linux)
                    if command -v getent >/dev/null 2>&1; then
                        selected_shell=$(
                            getent passwd "$selected_account" 2>/dev/null |
                                awk -F: 'NR == 1 { print $7 }'
                        )
                    fi
                    ;;
                Darwin)
                    if command -v dscl >/dev/null 2>&1; then
                        selected_shell=$(
                            dscl . -read "/Users/$selected_account" UserShell \
                                2>/dev/null |
                                awk 'NR == 1 { print $2 }'
                        )
                    fi
                    ;;
            esac
        fi
    fi
    if [ -n "$selected_shell" ]; then
        validate_no_controls "login shell" "$selected_shell"
    fi
    case "${selected_shell##*/}" in
        zsh) SELECTED_PROFILE_PATH=$HOME/.zprofile ;;
        *) SELECTED_PROFILE_PATH=$HOME/.profile ;;
    esac
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
    OLD_HASH_MANIFEST=$(sha256_file "$MANIFEST_PATH") ||
        fail "cannot hash managed manifest"
    OLD_ID_MANIFEST=$(path_identity "$MANIFEST_PATH") ||
        fail "cannot identify managed manifest"

    validate_no_controls "managed PATH profile" "$OLD_PATH_PROFILE"
    validate_managed_profile_path "managed manifest" "$OLD_PATH_PROFILE"
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
    if [ "$STATE_HARDKNOCK" = present ]; then
        OLD_ID_HARDKNOCK=$(path_identity "$PREFIX/bin/hardknock") ||
            fail "cannot identify existing managed hardknock"
    fi
    if [ "$STATE_EFFECT" = present ]; then
        OLD_ID_EFFECT=$(path_identity "$PREFIX/bin/hk-effect") ||
            fail "cannot identify existing managed hk-effect"
    fi
    if [ "$STATE_LICENSE" = present ]; then
        OLD_ID_LICENSE=$(
            path_identity "$PREFIX/share/doc/hardknock/LICENSE"
        ) || fail "cannot identify existing managed LICENSE"
    fi
    if [ "$STATE_NOTICE" = present ]; then
        OLD_ID_NOTICE=$(
            path_identity "$PREFIX/share/doc/hardknock/NOTICE"
        ) || fail "cannot identify existing managed NOTICE"
    fi
}

inspect_profile() {
    PROFILE_STATE=absent
    [ ! -L "$PROFILE_PATH" ] ||
        fail "PATH profile must not be a symbolic link: $PROFILE_PATH"
    if [ ! -e "$PROFILE_PATH" ]; then
        return
    fi
    assert_secure_file "$PROFILE_PATH" "PATH profile"
    require_max_size "$PROFILE_PATH" "$MAX_PROFILE_BYTES" "PATH profile"
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
    PREVIOUS_PROFILE_PATH=
    PREVIOUS_PROFILE_STATE=untouched
    PREVIOUS_PROFILE_REMOVE=0
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
    select_login_profile

    if [ "$MANIFEST_PRESENT" -eq 1 ] &&
        [ "$OLD_PATH_PROFILE" != - ] &&
        [ "$OLD_PATH_PROFILE" != "$SELECTED_PROFILE_PATH" ]; then
        PREVIOUS_PROFILE_PATH=$OLD_PATH_PROFILE
        PROFILE_PATH=$PREVIOUS_PROFILE_PATH
        inspect_profile
        PREVIOUS_PROFILE_STATE=$PROFILE_STATE
        case "$PREVIOUS_PROFILE_STATE" in
            managed) PREVIOUS_PROFILE_REMOVE=1 ;;
            absent|unmanaged) PREVIOUS_PROFILE_REMOVE=0 ;;
        esac
    fi

    PROFILE_PATH=$SELECTED_PROFILE_PATH
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
    profile_slot=${1:-profile}
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
    record_profile_file_intent "$profile_slot" "$profile_temporary" || {
        rm -f "$profile_temporary"
        return 1
    }
    commit_profile_candidate "$profile_slot" "$profile_temporary"
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
    profile_slot=$1
    profile_remove_path=$2
    profile_was_created=$3
    PROFILE_PATH=$profile_remove_path
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
    if [ "$profile_was_created" -eq 1 ] &&
        [ ! -s "$profile_temporary" ]; then
        rm -f "$profile_temporary"
        record_profile_absent_intent "$profile_slot" ||
            fail "cannot journal PATH profile removal"
        commit_profile_candidate "$profile_slot" - ||
            fail "PATH profile changed while removing the managed block"
    else
        record_profile_file_intent "$profile_slot" "$profile_temporary" || {
            rm -f "$profile_temporary"
            fail "cannot journal PATH profile update"
        }
        commit_profile_candidate "$profile_slot" "$profile_temporary" ||
            fail "PATH profile changed while updating the managed block"
    fi
}

durability_barrier() {
    barrier_path=$1
    if [ "$HOST_SYSTEM" = Linux ] &&
        sync -f "$barrier_path" >/dev/null 2>&1; then
        return 0
    fi
    sync >/dev/null 2>&1
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
        durability_barrier "$prefix_parent" ||
            fail "cannot make installation prefix creation durable"
        PREFIX_CREATED=1
    fi
    assert_secure_directory "$PREFIX" "installation prefix"
}

load_lock_metadata() {
    lock_metadata_path=${1:-$LOCK_PATH}
    assert_secure_file "$lock_metadata_path" "installer lock"
    require_max_size \
        "$lock_metadata_path" "$MAX_INSTALLER_STATE_BYTES" \
        "installer lock metadata"
    lock_line_number=0
    lock_seen_pid=0
    lock_seen_prefix=0
    lock_seen_token=0
    LOCK_OWNER_PID=
    LOCK_OWNER_TOKEN=
    while IFS= read -r lock_line || [ -n "$lock_line" ]; do
        lock_line_number=$((lock_line_number + 1))
        validate_no_controls "installer lock metadata" "$lock_line"
        if [ "$lock_line_number" -eq 1 ]; then
            [ "$lock_line" = "$LOCK_FORMAT" ] ||
                fail "installer lock has an unsupported format"
            continue
        fi
        case "$lock_line" in
            pid=*)
                [ "$lock_seen_pid" -eq 0 ] ||
                    fail "installer lock contains duplicate owner metadata"
                LOCK_OWNER_PID=${lock_line#pid=}
                lock_seen_pid=1
                ;;
            prefix=*)
                [ "$lock_seen_prefix" -eq 0 ] ||
                    fail "installer lock contains duplicate prefix metadata"
                lock_prefix=${lock_line#prefix=}
                lock_seen_prefix=1
                ;;
            token=*)
                [ "$lock_seen_token" -eq 0 ] ||
                    fail "installer lock contains duplicate token metadata"
                LOCK_OWNER_TOKEN=${lock_line#token=}
                lock_seen_token=1
                ;;
            *)
                fail "installer lock contains unknown metadata"
                ;;
        esac
    done <"$lock_metadata_path"
    [ "$lock_line_number" -eq 4 ] &&
        [ "$lock_seen_pid" -eq 1 ] &&
        [ "$lock_seen_prefix" -eq 1 ] &&
        [ "$lock_seen_token" -eq 1 ] ||
        fail "installer lock metadata is incomplete"
    case "$LOCK_OWNER_PID" in
        ''|*[!0-9]*) fail "installer lock contains an invalid owner process" ;;
    esac
    [ "${#LOCK_OWNER_PID}" -le 10 ] &&
        [ "$LOCK_OWNER_PID" -gt 0 ] ||
        fail "installer lock contains an invalid owner process"
    [ "$lock_prefix" = "$PREFIX" ] ||
        fail "installer lock belongs to a different installation prefix"
    case "$LOCK_OWNER_TOKEN" in
        ''|*[!0-9A-Za-z]*)
            fail "installer lock contains an invalid ownership token"
            ;;
    esac
    [ "${#LOCK_OWNER_TOKEN}" -le 64 ] ||
        fail "installer lock contains an invalid ownership token"
}

load_legacy_lock_metadata() {
    legacy_lock_path=${1:-$LOCK_PATH}
    assert_secure_directory "$legacy_lock_path" "legacy installer lock"
    legacy_pid_path=$legacy_lock_path/pid
    assert_secure_file "$legacy_pid_path" "legacy installer lock owner"
    require_max_size \
        "$legacy_pid_path" 32 "legacy installer lock owner"
    IFS= read -r LOCK_OWNER_PID <"$legacy_pid_path" ||
        fail "legacy installer lock owner is empty"
    case "$LOCK_OWNER_PID" in
        ''|*[!0-9]*) fail "legacy installer lock owner is invalid" ;;
    esac
    [ "${#LOCK_OWNER_PID}" -le 10 ] &&
        [ "$LOCK_OWNER_PID" -gt 0 ] ||
        fail "legacy installer lock owner is invalid"
    set +f
    for legacy_lock_entry in "$legacy_lock_path"/*; do
        if [ "$legacy_lock_entry" = "$legacy_lock_path/*" ] &&
            [ ! -e "$legacy_lock_entry" ] &&
            [ ! -L "$legacy_lock_entry" ]; then
            continue
        fi
        [ "$legacy_lock_entry" = "$legacy_pid_path" ] ||
            fail "legacy installer lock contains an unexpected entry"
    done
    set -f
}

check_no_preserved_lock_quarantines() {
    set +f
    for preserved_quarantine in \
        "$PREFIX"/.hardknock-install-lock-quarantine.*
    do
        if [ "$preserved_quarantine" = \
            "$PREFIX/.hardknock-install-lock-quarantine.*" ] &&
            [ ! -e "$preserved_quarantine" ] &&
            [ ! -L "$preserved_quarantine" ]; then
            continue
        fi
        set -f
        fail "installer lock recovery is ambiguous; preserved quarantine requires manual review: $preserved_quarantine"
    done
    set -f
}

recover_stale_lock_preparations() {
    recovered_lock_preparation=0
    set +f
    for lock_preparation in "$PREFIX"/.hardknock-install-lock.*.*; do
        if [ "$lock_preparation" = \
            "$PREFIX/.hardknock-install-lock.*.*" ] &&
            [ ! -e "$lock_preparation" ] &&
            [ ! -L "$lock_preparation" ]; then
            continue
        fi
        lock_preparation_name=${lock_preparation##*/}
        lock_preparation_owner_and_token=${lock_preparation_name#.hardknock-install-lock.}
        lock_preparation_owner=${lock_preparation_owner_and_token%%.*}
        lock_preparation_token=${lock_preparation_owner_and_token#*.}
        case "$lock_preparation_owner" in
            ''|*[!0-9]*)
                fail "installer lock preparation has an invalid owner"
                ;;
        esac
        [ "${#lock_preparation_owner}" -le 10 ] &&
            [ "$lock_preparation_owner" -gt 0 ] ||
            fail "installer lock preparation has an invalid owner"
        case "$lock_preparation_token" in
            ''|*[!0-9A-Za-z]*)
                fail "installer lock preparation has an invalid token"
                ;;
        esac
        [ "${#lock_preparation_token}" -le 64 ] ||
            fail "installer lock preparation has an invalid token"
        if kill -0 "$lock_preparation_owner" 2>/dev/null; then
            fail "another installer operation is preparing a lock: $lock_preparation"
        fi
        assert_secure_file \
            "$lock_preparation" "stale installer lock preparation"
        require_max_size \
            "$lock_preparation" "$MAX_INSTALLER_STATE_BYTES" \
            "stale installer lock preparation"
        rm -f "$lock_preparation" ||
            fail "cannot remove stale installer lock preparation"
        recovered_lock_preparation=1
    done
    set -f
    if [ "$recovered_lock_preparation" -eq 1 ]; then
        durability_barrier "$PREFIX" ||
            fail "cannot make installer lock preparation recovery durable"
    fi
}

recover_stale_lock() {
    if [ ! -e "$LOCK_PATH" ] && [ ! -L "$LOCK_PATH" ]; then
        return 0
    fi
    [ ! -L "$LOCK_PATH" ] ||
        fail "installer lock must not be a symbolic link: $LOCK_PATH"
    if [ -f "$LOCK_PATH" ]; then
        stale_lock_identity=$(path_identity "$LOCK_PATH") ||
            fail "cannot identify stale installer lock"
        stale_lock_hash=$(sha256_file "$LOCK_PATH") ||
            fail "cannot hash stale installer lock"
        load_lock_metadata
        lock_kind=file
        stale_lock_owner=$LOCK_OWNER_PID
        stale_lock_token=$LOCK_OWNER_TOKEN
        [ "$(path_identity "$LOCK_PATH")" = "$stale_lock_identity" ] &&
            [ "$(sha256_file "$LOCK_PATH")" = "$stale_lock_hash" ] ||
            fail "installer lock changed while it was being inspected"
    elif [ -d "$LOCK_PATH" ]; then
        stale_lock_identity=$(path_identity "$LOCK_PATH") ||
            fail "cannot identify stale legacy installer lock"
        stale_legacy_pid_identity=$(path_identity "$LOCK_PATH/pid") ||
            fail "cannot identify stale legacy installer lock owner"
        stale_legacy_pid_hash=$(sha256_file "$LOCK_PATH/pid") ||
            fail "cannot hash stale legacy installer lock owner"
        load_legacy_lock_metadata
        lock_kind=legacy
        stale_lock_owner=$LOCK_OWNER_PID
        stale_lock_token=
        [ "$(path_identity "$LOCK_PATH")" = "$stale_lock_identity" ] &&
            [ "$(path_identity "$LOCK_PATH/pid")" = \
                "$stale_legacy_pid_identity" ] &&
            [ "$(sha256_file "$LOCK_PATH/pid")" = \
                "$stale_legacy_pid_hash" ] ||
            fail "legacy installer lock changed while it was being inspected"
    else
        fail "installer lock is not a supported file: $LOCK_PATH"
    fi
    if kill -0 "$stale_lock_owner" 2>/dev/null; then
        fail "another installer operation is active: $LOCK_PATH"
    fi

    stale_lock_quarantine=$(
        mktemp -d \
            "$PREFIX/.hardknock-install-lock-quarantine.$$.XXXXXX"
    ) || fail "cannot create installer lock quarantine"
    chmod 0700 "$stale_lock_quarantine" ||
        fail "cannot secure installer lock quarantine"
    stale_lock_capture=$stale_lock_quarantine/captured
    if ! mv "$LOCK_PATH" "$stale_lock_capture"; then
        rmdir "$stale_lock_quarantine" 2>/dev/null || :
        fail "installer lock changed before it could be quarantined"
    fi
    durability_barrier "$PREFIX" ||
        fail "cannot make installer lock quarantine durable"

    if [ "$lock_kind" = file ]; then
        [ -f "$stale_lock_capture" ] &&
            [ ! -L "$stale_lock_capture" ] &&
            [ "$(path_identity "$stale_lock_capture")" = \
                "$stale_lock_identity" ] &&
            [ "$(sha256_file "$stale_lock_capture")" = \
                "$stale_lock_hash" ] ||
            fail "quarantined installer lock does not match the inspected lock"
        load_lock_metadata "$stale_lock_capture"
        [ "$LOCK_OWNER_PID" = "$stale_lock_owner" ] &&
            [ "$LOCK_OWNER_TOKEN" = "$stale_lock_token" ] ||
            fail "quarantined installer lock metadata changed"
        if kill -0 "$LOCK_OWNER_PID" 2>/dev/null; then
            fail "quarantined installer lock owner became active; preserving quarantine"
        fi
        rm -f "$stale_lock_capture" ||
            fail "cannot remove quarantined stale installer lock"
    else
        [ -d "$stale_lock_capture" ] &&
            [ ! -L "$stale_lock_capture" ] &&
            [ "$(path_identity "$stale_lock_capture")" = \
                "$stale_lock_identity" ] &&
            [ "$(path_identity "$stale_lock_capture/pid")" = \
                "$stale_legacy_pid_identity" ] &&
            [ "$(sha256_file "$stale_lock_capture/pid")" = \
                "$stale_legacy_pid_hash" ] ||
            fail "quarantined legacy installer lock does not match the inspected lock"
        load_legacy_lock_metadata "$stale_lock_capture"
        [ "$LOCK_OWNER_PID" = "$stale_lock_owner" ] ||
            fail "quarantined legacy installer lock metadata changed"
        if kill -0 "$LOCK_OWNER_PID" 2>/dev/null; then
            fail "quarantined legacy installer lock owner became active; preserving quarantine"
        fi
        rm -f "$stale_lock_capture/pid" &&
            rmdir "$stale_lock_capture" ||
            fail "cannot remove quarantined stale legacy installer lock"
    fi
    rmdir "$stale_lock_quarantine" ||
        fail "cannot remove installer lock quarantine"
    durability_barrier "$PREFIX" ||
        fail "cannot make stale installer lock recovery durable"
}

publish_lock() {
    lock_temporary=$(
        mktemp "$PREFIX/.hardknock-install-lock.$$.XXXXXX"
    ) ||
        fail "cannot prepare installer lock"
    lock_temporary_name=${lock_temporary##*/}
    LOCK_TOKEN=${lock_temporary_name#.hardknock-install-lock.$$.}
    case "$LOCK_TOKEN" in
        ''|*[!0-9A-Za-z]*)
            rm -f "$lock_temporary"
            fail "cannot create a valid installer lock token"
            ;;
    esac
    {
        printf '%s\n' "$LOCK_FORMAT"
        printf 'pid=%s\n' "$$"
        printf 'prefix=%s\n' "$PREFIX"
        printf 'token=%s\n' "$LOCK_TOKEN"
    } >"$lock_temporary" || {
        rm -f "$lock_temporary"
        fail "cannot write installer lock metadata"
    }
    chmod 0600 "$lock_temporary" || {
        rm -f "$lock_temporary"
        fail "cannot secure installer lock metadata"
    }
    durability_barrier "$lock_temporary" || {
        rm -f "$lock_temporary"
        fail "cannot make installer lock metadata durable"
    }
    if ! ln "$lock_temporary" "$LOCK_PATH" 2>/dev/null; then
        rm -f "$lock_temporary"
        fail "another installer operation became active: $LOCK_PATH"
    fi
    LOCK_HASH=$(sha256_file "$LOCK_PATH") || {
        rm -f "$lock_temporary"
        fail "cannot bind installer lock ownership"
    }
    rm -f "$lock_temporary" ||
        fail "cannot remove installer lock preparation file"
    durability_barrier "$PREFIX" ||
        fail "cannot make installer lock acquisition durable"
    LOCK_HELD=1
}

release_lock() {
    [ "$LOCK_HELD" -eq 1 ] || return 0
    [ -f "$LOCK_PATH" ] && [ ! -L "$LOCK_PATH" ] || return 1
    release_lock_hash=$(sha256_file "$LOCK_PATH") || return 1
    [ "$release_lock_hash" = "$LOCK_HASH" ] || return 1
    rm -f "$LOCK_PATH" || return 1
    durability_barrier "$PREFIX" || return 1
    LOCK_HELD=0
    LOCK_TOKEN=
    LOCK_HASH=
}

set_transaction_paths() {
    STAGE_DIRECTORY=$TRANSACTION_DIR/stage
    BACKUP_DIRECTORY=$TRANSACTION_DIR/backup
    PLACED_DIRECTORY=$TRANSACTION_DIR/placed
    CREATED_DIRECTORIES_FILE=$TRANSACTION_DIR/created-directories
    TRANSACTION_METADATA_FILE=$TRANSACTION_DIR/metadata
    TRANSACTION_PHASE_FILE=$TRANSACTION_DIR/phase
}

validate_transaction_name() {
    validate_transaction_path=$1
    validate_transaction_kind=$2
    validate_transaction_name=${validate_transaction_path##*/}
    case "$validate_transaction_kind:$validate_transaction_name" in
        active:.hardknock-install.*)
            validate_transaction_token=${validate_transaction_name#.hardknock-install.}
            ;;
        preparing:.hardknock-install-preparing.*)
            validate_transaction_token=${validate_transaction_name#.hardknock-install-preparing.}
            ;;
        *)
            fail "installer recovery found an invalid transaction name"
            ;;
    esac
    case "$validate_transaction_token" in
        ''|*[!0-9A-Za-z]*)
            fail "installer recovery found an invalid transaction token"
            ;;
    esac
    [ "${#validate_transaction_token}" -le 64 ] ||
        fail "installer recovery found an invalid transaction token"
}

validate_transaction_tree() {
    validate_tree_path=$1
    assert_secure_directory "$validate_tree_path" "installer transaction"
    RECOVERY_LISTING=$(
        mktemp "${TMPDIR:-/tmp}/hardknock-transaction-list.XXXXXX"
    ) || fail "cannot create installer recovery workspace"
    find "$validate_tree_path" -print >"$RECOVERY_LISTING" ||
        fail "cannot inspect installer transaction"
    chmod 0600 "$RECOVERY_LISTING" ||
        fail "cannot secure installer recovery workspace"
    require_max_size \
        "$RECOVERY_LISTING" "$MAX_TRANSACTION_TREE_BYTES" \
        "installer transaction tree"
    if ! awk -v root="$validate_tree_path" '
        BEGIN {
            allowed["stage"] = 1
            allowed["stage/bin"] = 1
            allowed["stage/bin/hardknock"] = 1
            allowed["stage/bin/hk-effect"] = 1
            allowed["stage/share"] = 1
            allowed["stage/share/doc"] = 1
            allowed["stage/share/doc/hardknock"] = 1
            allowed["stage/share/doc/hardknock/LICENSE"] = 1
            allowed["stage/share/doc/hardknock/NOTICE"] = 1
            allowed["stage/share/hardknock"] = 1
            allowed["stage/share/hardknock/install-manifest-v1"] = 1
            allowed["backup"] = 1
            allowed["backup/bin"] = 1
            allowed["backup/bin/hardknock"] = 1
            allowed["backup/bin/hk-effect"] = 1
            allowed["backup/share"] = 1
            allowed["backup/share/doc"] = 1
            allowed["backup/share/doc/hardknock"] = 1
            allowed["backup/share/doc/hardknock/LICENSE"] = 1
            allowed["backup/share/doc/hardknock/NOTICE"] = 1
            allowed["backup/share/hardknock"] = 1
            allowed["backup/share/hardknock/install-manifest-v1"] = 1
            allowed["placed"] = 1
            allowed["placed/bin"] = 1
            allowed["placed/bin/hardknock"] = 1
            allowed["placed/bin/hk-effect"] = 1
            allowed["placed/share"] = 1
            allowed["placed/share/doc"] = 1
            allowed["placed/share/doc/hardknock"] = 1
            allowed["placed/share/doc/hardknock/LICENSE"] = 1
            allowed["placed/share/doc/hardknock/NOTICE"] = 1
            allowed["placed/share/hardknock"] = 1
            allowed["placed/share/hardknock/install-manifest-v1"] = 1
            allowed["created-directories"] = 1
            allowed["metadata"] = 1
            allowed["phase"] = 1
            allowed["phase.next"] = 1
            allowed["profile.backup"] = 1
            allowed["profile.absent"] = 1
            allowed["profile.changed"] = 1
            allowed["profile.applied"] = 1
            allowed["profile.expected"] = 1
            allowed["profile.displaced"] = 1
            allowed["previous-profile.backup"] = 1
            allowed["previous-profile.absent"] = 1
            allowed["previous-profile.changed"] = 1
            allowed["previous-profile.applied"] = 1
            allowed["previous-profile.expected"] = 1
            allowed["previous-profile.displaced"] = 1
        }
        NR == 1 {
            if ($0 != root) {
                exit 1
            }
            next
        }
        {
            relative = substr($0, length(root) + 2)
            if (!(relative in allowed)) {
                exit 1
            }
        }
        END {
            if (NR == 0) {
                exit 1
            }
        }
    ' "$RECOVERY_LISTING"; then
        fail "installer transaction contains an unexpected entry"
    fi
    rm -f "$RECOVERY_LISTING" ||
        fail "cannot clean installer recovery workspace"
    RECOVERY_LISTING=

    for transaction_directory in \
        "$validate_tree_path/stage" \
        "$validate_tree_path/stage/bin" \
        "$validate_tree_path/stage/share" \
        "$validate_tree_path/stage/share/doc" \
        "$validate_tree_path/stage/share/doc/hardknock" \
        "$validate_tree_path/stage/share/hardknock" \
        "$validate_tree_path/backup" \
        "$validate_tree_path/backup/bin" \
        "$validate_tree_path/backup/share" \
        "$validate_tree_path/backup/share/doc" \
        "$validate_tree_path/backup/share/doc/hardknock" \
        "$validate_tree_path/backup/share/hardknock" \
        "$validate_tree_path/placed" \
        "$validate_tree_path/placed/bin" \
        "$validate_tree_path/placed/share" \
        "$validate_tree_path/placed/share/doc" \
        "$validate_tree_path/placed/share/doc/hardknock" \
        "$validate_tree_path/placed/share/hardknock"
    do
        if [ -e "$transaction_directory" ] ||
            [ -L "$transaction_directory" ]; then
            assert_secure_directory \
                "$transaction_directory" "installer transaction directory"
        fi
    done

    for transaction_file in \
        "$validate_tree_path/stage/bin/hardknock" \
        "$validate_tree_path/stage/bin/hk-effect" \
        "$validate_tree_path/stage/share/doc/hardknock/LICENSE" \
        "$validate_tree_path/stage/share/doc/hardknock/NOTICE" \
        "$validate_tree_path/stage/share/hardknock/install-manifest-v1" \
        "$validate_tree_path/backup/bin/hardknock" \
        "$validate_tree_path/backup/bin/hk-effect" \
        "$validate_tree_path/backup/share/doc/hardknock/LICENSE" \
        "$validate_tree_path/backup/share/doc/hardknock/NOTICE" \
        "$validate_tree_path/backup/share/hardknock/install-manifest-v1" \
        "$validate_tree_path/placed/bin/hardknock" \
        "$validate_tree_path/placed/bin/hk-effect" \
        "$validate_tree_path/placed/share/doc/hardknock/LICENSE" \
        "$validate_tree_path/placed/share/doc/hardknock/NOTICE" \
        "$validate_tree_path/placed/share/hardknock/install-manifest-v1" \
        "$validate_tree_path/created-directories" \
        "$validate_tree_path/metadata" \
        "$validate_tree_path/phase" \
        "$validate_tree_path/phase.next" \
        "$validate_tree_path/profile.backup" \
        "$validate_tree_path/profile.absent" \
        "$validate_tree_path/profile.changed" \
        "$validate_tree_path/profile.applied" \
        "$validate_tree_path/profile.expected" \
        "$validate_tree_path/profile.displaced" \
        "$validate_tree_path/previous-profile.backup" \
        "$validate_tree_path/previous-profile.absent" \
        "$validate_tree_path/previous-profile.changed" \
        "$validate_tree_path/previous-profile.applied" \
        "$validate_tree_path/previous-profile.expected" \
        "$validate_tree_path/previous-profile.displaced"
    do
        if [ -e "$transaction_file" ] || [ -L "$transaction_file" ]; then
            assert_secure_file \
                "$transaction_file" "installer transaction file"
            case "$transaction_file" in
                */stage/bin/*|*/backup/bin/*)
                    require_max_size \
                        "$transaction_file" "$MAX_BINARY_BYTES" \
                        "installer transaction binary"
                    ;;
                */profile.backup|*/profile.displaced|\
                */previous-profile.backup|*/previous-profile.displaced)
                    require_max_size \
                        "$transaction_file" "$MAX_PROFILE_BYTES" \
                        "installer transaction profile backup"
                    ;;
                */stage/share/doc/*|*/backup/share/doc/*)
                    require_max_size \
                        "$transaction_file" "$MAX_LEGAL_BYTES" \
                        "installer transaction legal file"
                    ;;
                *)
                    require_max_size \
                        "$transaction_file" "$MAX_INSTALLER_STATE_BYTES" \
                        "installer transaction metadata"
                    ;;
            esac
        fi
    done
}

load_transaction_metadata() {
    assert_secure_file "$TRANSACTION_METADATA_FILE" "installer transaction metadata"
    require_max_size \
        "$TRANSACTION_METADATA_FILE" "$MAX_INSTALLER_STATE_BYTES" \
        "installer transaction metadata"
    transaction_line_number=0
    transaction_seen_operation=0
    transaction_seen_prefix=0
    transaction_seen_profile=0
    transaction_seen_previous_profile=0
    transaction_seen_prefix_created=0
    while IFS= read -r transaction_line ||
        [ -n "$transaction_line" ]; do
        transaction_line_number=$((transaction_line_number + 1))
        validate_no_controls "installer transaction metadata" "$transaction_line"
        if [ "$transaction_line_number" -eq 1 ]; then
            [ "$transaction_line" = "$TRANSACTION_FORMAT" ] ||
                fail "installer transaction has an unsupported format"
            continue
        fi
        case "$transaction_line" in
            operation=*)
                [ "$transaction_seen_operation" -eq 0 ] ||
                    fail "installer transaction contains duplicate operation metadata"
                TRANSACTION_OPERATION=${transaction_line#operation=}
                transaction_seen_operation=1
                ;;
            prefix=*)
                [ "$transaction_seen_prefix" -eq 0 ] ||
                    fail "installer transaction contains duplicate prefix metadata"
                transaction_prefix=${transaction_line#prefix=}
                transaction_seen_prefix=1
                ;;
            profile_path=*)
                [ "$transaction_seen_profile" -eq 0 ] ||
                    fail "installer transaction contains duplicate profile metadata"
                TRANSACTION_PROFILE_PATH=${transaction_line#profile_path=}
                transaction_seen_profile=1
                ;;
            previous_profile_path=*)
                [ "$transaction_seen_previous_profile" -eq 0 ] ||
                    fail "installer transaction contains duplicate previous profile metadata"
                TRANSACTION_PREVIOUS_PROFILE_PATH=${transaction_line#previous_profile_path=}
                transaction_seen_previous_profile=1
                ;;
            prefix_created=0|prefix_created=1)
                [ "$transaction_seen_prefix_created" -eq 0 ] ||
                    fail "installer transaction contains duplicate prefix creation metadata"
                TRANSACTION_PREFIX_CREATED=${transaction_line#prefix_created=}
                transaction_seen_prefix_created=1
                ;;
            *)
                fail "installer transaction contains unknown metadata"
                ;;
        esac
    done <"$TRANSACTION_METADATA_FILE"
    [ "$transaction_line_number" -eq 6 ] &&
        [ "$transaction_seen_operation" -eq 1 ] &&
        [ "$transaction_seen_prefix" -eq 1 ] &&
        [ "$transaction_seen_profile" -eq 1 ] &&
        [ "$transaction_seen_previous_profile" -eq 1 ] &&
        [ "$transaction_seen_prefix_created" -eq 1 ] ||
        fail "installer transaction metadata is incomplete"
    case "$TRANSACTION_OPERATION" in
        install|uninstall) ;;
        *) fail "installer transaction contains an invalid operation" ;;
    esac
    [ "$transaction_prefix" = "$PREFIX" ] ||
        fail "installer transaction belongs to a different installation prefix"
    validate_managed_profile_path \
        "installer transaction" "$TRANSACTION_PROFILE_PATH"
    validate_managed_profile_path \
        "installer transaction" "$TRANSACTION_PREVIOUS_PROFILE_PATH"
    if [ "$TRANSACTION_PROFILE_PATH" != - ] &&
        [ "$TRANSACTION_PROFILE_PATH" = "$TRANSACTION_PREVIOUS_PROFILE_PATH" ]; then
        fail "installer transaction repeats the same PATH profile"
    fi
    if [ "$TRANSACTION_OPERATION" = uninstall ] &&
        [ "$TRANSACTION_PREFIX_CREATED" -ne 0 ]; then
        fail "uninstall transaction contains invalid prefix creation metadata"
    fi
}

load_transaction_phase() {
    assert_secure_file "$TRANSACTION_PHASE_FILE" "installer transaction phase"
    require_max_size \
        "$TRANSACTION_PHASE_FILE" 32 "installer transaction phase"
    transaction_phase_line_count=0
    TRANSACTION_PHASE=
    while IFS= read -r transaction_phase_line ||
        [ -n "$transaction_phase_line" ]; do
        transaction_phase_line_count=$((transaction_phase_line_count + 1))
        [ "$transaction_phase_line_count" -eq 1 ] ||
            fail "installer transaction phase contains extra data"
        TRANSACTION_PHASE=$transaction_phase_line
    done <"$TRANSACTION_PHASE_FILE"
    case "$TRANSACTION_PHASE" in
        active|committed) ;;
        *) fail "installer transaction contains an invalid phase" ;;
    esac
}

durable_transaction_phase_is_committed() {
    [ -n "${TRANSACTION_DIR:-}" ] &&
        [ -d "$TRANSACTION_DIR" ] &&
        [ ! -L "$TRANSACTION_DIR" ] || return 1
    durable_phase_path=$TRANSACTION_DIR/phase
    [ -f "$durable_phase_path" ] &&
        [ ! -L "$durable_phase_path" ] || return 1
    durable_phase_size=$(wc -c <"$durable_phase_path" 2>/dev/null |
        tr -d '[:space:]') || return 1
    case "$durable_phase_size" in
        ''|*[!0-9]*) return 1 ;;
    esac
    [ "$durable_phase_size" -le 32 ] || return 1
    durable_phase_lines=0
    durable_phase_value=
    while IFS= read -r durable_phase_line ||
        [ -n "$durable_phase_line" ]; do
        durable_phase_lines=$((durable_phase_lines + 1))
        [ "$durable_phase_lines" -eq 1 ] || return 1
        durable_phase_value=$durable_phase_line
    done <"$durable_phase_path"
    [ "$durable_phase_lines" -eq 1 ] &&
        [ "$durable_phase_value" = committed ]
}

validate_created_directories() {
    assert_secure_file \
        "$CREATED_DIRECTORIES_FILE" "installer directory rollback journal"
    require_max_size \
        "$CREATED_DIRECTORIES_FILE" "$MAX_INSTALLER_STATE_BYTES" \
        "installer directory rollback journal"
    created_seen_bin=0
    created_seen_share=0
    created_seen_doc=0
    created_seen_doc_hardknock=0
    created_seen_hardknock=0
    while IFS= read -r created_record ||
        [ -n "$created_record" ]; do
        validate_no_controls \
            "installer directory rollback entry" "$created_record"
        case "$created_record" in
            *'|'*)
                created_directory=${created_record%%|*}
                created_identity=${created_record#*|}
                ;;
            *)
                return 1
                ;;
        esac
        case "$created_identity" in
            *'|'*) return 1 ;;
        esac
        is_path_identity "$created_identity" || return 1
        case "$created_directory" in
            bin)
                [ "$created_seen_bin" -eq 0 ] || return 1
                created_seen_bin=1
                ;;
            share)
                [ "$created_seen_share" -eq 0 ] || return 1
                created_seen_share=1
                ;;
            share/doc)
                [ "$created_seen_doc" -eq 0 ] || return 1
                created_seen_doc=1
                ;;
            share/doc/hardknock)
                [ "$created_seen_doc_hardknock" -eq 0 ] || return 1
                created_seen_doc_hardknock=1
                ;;
            share/hardknock)
                [ "$created_seen_hardknock" -eq 0 ] || return 1
                created_seen_hardknock=1
                ;;
            *)
                return 1
                ;;
        esac
    done <"$CREATED_DIRECTORIES_FILE"
}

validate_preparing_transaction() {
    preparing_path=$1
    validate_transaction_name "$preparing_path" preparing
    validate_transaction_tree "$preparing_path"
    for preparing_mutation in \
        "$preparing_path/stage/bin" \
        "$preparing_path/stage/share" \
        "$preparing_path/backup/bin" \
        "$preparing_path/backup/share" \
        "$preparing_path/placed/bin" \
        "$preparing_path/placed/share" \
        "$preparing_path/phase.next" \
        "$preparing_path/profile.backup" \
        "$preparing_path/profile.absent" \
        "$preparing_path/profile.changed" \
        "$preparing_path/profile.applied" \
        "$preparing_path/profile.expected" \
        "$preparing_path/profile.displaced" \
        "$preparing_path/previous-profile.backup" \
        "$preparing_path/previous-profile.absent" \
        "$preparing_path/previous-profile.changed" \
        "$preparing_path/previous-profile.applied" \
        "$preparing_path/previous-profile.expected" \
        "$preparing_path/previous-profile.displaced"
    do
        if [ -e "$preparing_mutation" ] ||
            [ -L "$preparing_mutation" ]; then
            fail "preparing installer transaction contains mutation state"
        fi
    done
    if [ -e "$preparing_path/created-directories" ] &&
        [ -s "$preparing_path/created-directories" ]; then
        fail "preparing installer transaction contains directory mutations"
    fi
}

remove_recovered_transaction() {
    remove_recovery_path=$1
    case "$remove_recovery_path" in
        "$PREFIX"/.hardknock-install.*|\
        "$PREFIX"/.hardknock-install-preparing.*)
            ;;
        *)
            fail "refusing to remove an invalid installer transaction path"
            ;;
    esac
    rm -rf "$remove_recovery_path" ||
        fail "cannot remove recovered installer transaction"
    durability_barrier "$PREFIX" ||
        fail "cannot make installer transaction recovery durable"
}

recover_interrupted_transactions() {
    recovery_transaction=
    recovery_kind=
    recovery_count=0
    set +f
    for recovery_candidate in \
        "$PREFIX"/.hardknock-install.* \
        "$PREFIX"/.hardknock-install-preparing.*
    do
        if { [ "$recovery_candidate" = "$PREFIX/.hardknock-install.*" ] ||
            [ "$recovery_candidate" = "$PREFIX/.hardknock-install-preparing.*" ]; } &&
            [ ! -e "$recovery_candidate" ] &&
            [ ! -L "$recovery_candidate" ]; then
            continue
        fi
        [ "$recovery_candidate" != "$LOCK_PATH" ] || continue
        recovery_count=$((recovery_count + 1))
        [ "$recovery_count" -eq 1 ] ||
            fail "multiple interrupted installer transactions require manual review"
        recovery_transaction=$recovery_candidate
        case "${recovery_candidate##*/}" in
            .hardknock-install-preparing.*) recovery_kind=preparing ;;
            .hardknock-install.*) recovery_kind=active ;;
            *) fail "installer recovery found an unexpected reserved path" ;;
        esac
    done
    set -f
    [ "$recovery_count" -eq 0 ] && return 0

    if [ "$recovery_kind" = preparing ]; then
        validate_preparing_transaction "$recovery_transaction"
        remove_recovered_transaction "$recovery_transaction"
        return
    fi

    validate_transaction_name "$recovery_transaction" active
    validate_transaction_tree "$recovery_transaction"
    TRANSACTION_DIR=$recovery_transaction
    set_transaction_paths
    [ -d "$STAGE_DIRECTORY" ] &&
        [ -d "$BACKUP_DIRECTORY" ] &&
        [ -d "$PLACED_DIRECTORY" ] &&
        [ -f "$CREATED_DIRECTORIES_FILE" ] ||
        fail "installer transaction is missing required recovery state"
    load_transaction_metadata
    load_transaction_phase
    validate_created_directories ||
        fail "installer transaction contains an invalid directory rollback journal"
    if [ -e "$TRANSACTION_DIR/phase.next" ]; then
        require_max_size \
            "$TRANSACTION_DIR/phase.next" 32 \
            "installer transaction pending phase"
        pending_phase=$(cat "$TRANSACTION_DIR/phase.next") ||
            fail "cannot read installer transaction pending phase"
        case "$pending_phase" in
            active|committed) ;;
            *) fail "installer transaction contains an invalid pending phase" ;;
        esac
    fi

    if [ "$TRANSACTION_PHASE" = committed ]; then
        TRANSACTION_ACTIVE=0
        TRANSACTION_COMMITTED=1
        TRANSACTION_CLEANUP_ALLOWED=1
        remove_recovered_transaction "$TRANSACTION_DIR"
        TRANSACTION_DIR=
        TRANSACTION_CLEANUP_ALLOWED=0
        return
    fi

    TRANSACTION_ACTIVE=1
    TRANSACTION_COMMITTED=0
    TRANSACTION_CLEANUP_ALLOWED=1
    if rollback_transaction; then
        TRANSACTION_ACTIVE=0
        if [ "$TRANSACTION_PREFIX_CREATED" -eq 1 ]; then
            RECOVERED_PREFIX_CREATED=1
        fi
        remove_recovered_transaction "$TRANSACTION_DIR"
        TRANSACTION_DIR=
        TRANSACTION_CLEANUP_ALLOWED=0
    else
        TRANSACTION_ACTIVE=0
        TRANSACTION_CLEANUP_ALLOWED=0
        PREFIX_CREATED=0
        fail "automatic installer recovery could not safely restore the previous installation"
    fi
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
    check_no_preserved_lock_quarantines
    recover_stale_lock_preparations
    recover_stale_lock
    publish_lock
    recover_interrupted_transactions
}

recover_before_snapshot() {
    [ -e "$PREFIX" ] || [ -L "$PREFIX" ] || return 0
    acquire_lock 0
    release_lock || fail "cannot release installer recovery lock"
    if [ "$RECOVERED_PREFIX_CREATED" -eq 1 ]; then
        rmdir "$PREFIX" 2>/dev/null || :
        RECOVERED_PREFIX_CREATED=0
        PREFIX_CREATED=0
    fi
}

assert_dry_run_recovery_clean() {
    [ -e "$PREFIX" ] || [ -L "$PREFIX" ] || return 0
    check_no_preserved_lock_quarantines
    if [ -e "$LOCK_PATH" ] || [ -L "$LOCK_PATH" ]; then
        fail "dry-run found pending installer recovery; rerun without --dry-run to recover it"
    fi
    set +f
    for dry_run_recovery in \
        "$PREFIX"/.hardknock-install.* \
        "$PREFIX"/.hardknock-install-preparing.*
    do
        if { [ "$dry_run_recovery" = "$PREFIX/.hardknock-install.*" ] ||
            [ "$dry_run_recovery" = "$PREFIX/.hardknock-install-preparing.*" ]; } &&
            [ ! -e "$dry_run_recovery" ] &&
            [ ! -L "$dry_run_recovery" ]; then
            continue
        fi
        set -f
        fail "dry-run found pending installer recovery; rerun without --dry-run to recover it"
    done
    set -f
}

begin_transaction() {
    transaction_operation=$1
    transaction_preparing=$(
        mktemp -d "$PREFIX/.hardknock-install-preparing.XXXXXX"
    ) || fail "cannot create installation transaction under prefix"
    TRANSACTION_DIR=$transaction_preparing
    set_transaction_paths
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
    chmod 0600 "$CREATED_DIRECTORIES_FILE" ||
        fail "cannot secure directory rollback journal"

    TRANSACTION_OPERATION=$transaction_operation
    TRANSACTION_PROFILE_PATH=${PROFILE_PATH:--}
    TRANSACTION_PREVIOUS_PROFILE_PATH=${PREVIOUS_PROFILE_PATH:--}
    TRANSACTION_PREFIX_CREATED=$PREFIX_CREATED
    {
        printf '%s\n' "$TRANSACTION_FORMAT"
        printf 'operation=%s\n' "$TRANSACTION_OPERATION"
        printf 'prefix=%s\n' "$PREFIX"
        printf 'profile_path=%s\n' "$TRANSACTION_PROFILE_PATH"
        printf 'previous_profile_path=%s\n' \
            "$TRANSACTION_PREVIOUS_PROFILE_PATH"
        printf 'prefix_created=%s\n' "$TRANSACTION_PREFIX_CREATED"
    } >"$TRANSACTION_METADATA_FILE" ||
        fail "cannot write installation transaction metadata"
    printf '%s\n' active >"$TRANSACTION_PHASE_FILE" ||
        fail "cannot write installation transaction phase"
    chmod 0600 "$TRANSACTION_METADATA_FILE" "$TRANSACTION_PHASE_FILE" ||
        fail "cannot secure installation transaction metadata"
    durability_barrier "$TRANSACTION_DIR" ||
        fail "cannot make installation transaction metadata durable"

    transaction_suffix=${transaction_preparing##*.hardknock-install-preparing.}
    transaction_active=$PREFIX/.hardknock-install.$transaction_suffix
    mv "$transaction_preparing" "$transaction_active" ||
        fail "cannot publish installation transaction"
    durability_barrier "$PREFIX" ||
        fail "cannot make installation transaction publication durable"
    TRANSACTION_DIR=$transaction_active
    set_transaction_paths
    TRANSACTION_ACTIVE=1
    TRANSACTION_COMMITTED=0
    TRANSACTION_CLEANUP_ALLOWED=1
}

ensure_managed_directory() {
    ensure_directory=$1
    if [ -e "$ensure_directory" ] || [ -L "$ensure_directory" ]; then
        assert_secure_directory "$ensure_directory" "installation directory"
        return
    fi
    ensure_relative=${ensure_directory#"$PREFIX"/}
    case "$ensure_relative" in
        bin|share|share/doc|share/doc/hardknock|share/hardknock) ;;
        *) fail "refusing to create an unexpected installation directory" ;;
    esac
    mkdir "$ensure_directory" ||
        fail "cannot create installation directory: $ensure_directory"
    chmod 0700 "$ensure_directory" ||
        fail "cannot secure installation directory: $ensure_directory"
    ensure_identity=$(path_identity "$ensure_directory") ||
        fail "cannot identify created installation directory"
    printf '%s|%s\n' "$ensure_relative" "$ensure_identity" \
        >>"$CREATED_DIRECTORIES_FILE" ||
        fail "cannot journal created installation directory"
    durability_barrier "$CREATED_DIRECTORIES_FILE" ||
        fail "cannot make directory rollback journal durable"
    durability_barrier "${ensure_directory%/*}" ||
        fail "cannot make installation directory creation durable"
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
    NEW_HASH_MANIFEST=$(
        sha256_file "$STAGE_DIRECTORY/$MANIFEST_RELATIVE"
    ) || fail "cannot hash staged managed installation manifest"
}

load_managed_expectation() {
    expectation_relative=$1
    case "$expectation_relative" in
        bin/hardknock)
            if [ "${TRANSACTION_OPERATION:-}" = uninstall ]; then
                EXPECTED_STATE=present
            else
                EXPECTED_STATE=$STATE_HARDKNOCK
            fi
            EXPECTED_HASH=$OLD_HASH_HARDKNOCK
            EXPECTED_IDENTITY=$OLD_ID_HARDKNOCK
            EXPECTED_LIMIT=$MAX_BINARY_BYTES
            ;;
        bin/hk-effect)
            if [ "${TRANSACTION_OPERATION:-}" = uninstall ]; then
                EXPECTED_STATE=present
            else
                EXPECTED_STATE=$STATE_EFFECT
            fi
            EXPECTED_HASH=$OLD_HASH_EFFECT
            EXPECTED_IDENTITY=$OLD_ID_EFFECT
            EXPECTED_LIMIT=$MAX_BINARY_BYTES
            ;;
        share/doc/hardknock/LICENSE)
            if [ "${TRANSACTION_OPERATION:-}" = uninstall ]; then
                EXPECTED_STATE=present
            else
                EXPECTED_STATE=$STATE_LICENSE
            fi
            EXPECTED_HASH=$OLD_HASH_LICENSE
            EXPECTED_IDENTITY=$OLD_ID_LICENSE
            EXPECTED_LIMIT=$MAX_LEGAL_BYTES
            ;;
        share/doc/hardknock/NOTICE)
            if [ "${TRANSACTION_OPERATION:-}" = uninstall ]; then
                EXPECTED_STATE=present
            else
                EXPECTED_STATE=$STATE_NOTICE
            fi
            EXPECTED_HASH=$OLD_HASH_NOTICE
            EXPECTED_IDENTITY=$OLD_ID_NOTICE
            EXPECTED_LIMIT=$MAX_LEGAL_BYTES
            ;;
        "$MANIFEST_RELATIVE")
            if [ "$MANIFEST_PRESENT" -eq 1 ]; then
                EXPECTED_STATE=present
            else
                EXPECTED_STATE=absent
            fi
            EXPECTED_HASH=$OLD_HASH_MANIFEST
            EXPECTED_IDENTITY=$OLD_ID_MANIFEST
            EXPECTED_LIMIT=$MAX_INSTALLER_STATE_BYTES
            ;;
        *) return 1 ;;
    esac
}

clear_file_intent() {
    clear_marker=$1
    rm -f "$clear_marker" || return 1
    durability_barrier "${clear_marker%/*}"
}

replace_managed_file() {
    replace_relative=$1
    replace_source=$STAGE_DIRECTORY/$replace_relative
    replace_destination=$PREFIX/$replace_relative
    replace_backup=$BACKUP_DIRECTORY/$replace_relative
    replace_marker=$PLACED_DIRECTORY/$replace_relative
    mkdir -p "${replace_backup%/*}" "${replace_marker%/*}" || return 1
    replace_hash=$(sha256_file "$replace_source") || return 1
    printf 'file=%s\n' "$replace_hash" >"$replace_marker" || return 1
    chmod 0600 "$replace_marker" || return 1
    durability_barrier "$TRANSACTION_DIR" || return 1
    load_managed_expectation "$replace_relative" || return 1
    if [ "$EXPECTED_STATE" = present ]; then
        if [ ! -e "$replace_destination" ] &&
            [ ! -L "$replace_destination" ]; then
            clear_file_intent "$replace_marker" || return 1
            return 1
        fi
        mv "$replace_destination" "$replace_backup" || return 1
        durability_barrier "${replace_destination%/*}" || return 1
        if ! verify_captured_file \
            "$replace_backup" \
            "$EXPECTED_IDENTITY" \
            "$EXPECTED_HASH" \
            "$EXPECTED_LIMIT" \
            "managed installation file"; then
            if move_no_replace "$replace_backup" "$replace_destination"; then
                durability_barrier "${replace_destination%/*}" || return 1
                clear_file_intent "$replace_marker" || return 1
            fi
            return 1
        fi
    elif [ -e "$replace_destination" ] || [ -L "$replace_destination" ]; then
        clear_file_intent "$replace_marker" || return 1
        return 1
    fi
    if ! move_no_replace "$replace_source" "$replace_destination"; then
        if [ "$EXPECTED_STATE" = absent ]; then
            clear_file_intent "$replace_marker" || return 1
        fi
        return 1
    fi
    durability_barrier "${replace_destination%/*}"
}

journal_profile_change() {
    profile_slot=$1
    profile_journal_path=$2
    [ "$profile_slot" = profile ] ||
        [ "$profile_slot" = previous-profile ] ||
        return 1
    validate_managed_profile_path \
        "installer transaction profile" "$profile_journal_path"
    profile_journal_prefix=$TRANSACTION_DIR/$profile_slot
    if [ -e "$profile_journal_path" ]; then
        assert_secure_file "$profile_journal_path" "PATH profile"
        require_max_size \
            "$profile_journal_path" "$MAX_PROFILE_BYTES" "PATH profile"
        profile_expected_identity=$(path_identity "$profile_journal_path") ||
            return 1
        cp -p "$profile_journal_path" "$profile_journal_prefix.backup" ||
            return 1
        profile_expected_hash=$(
            sha256_file "$profile_journal_prefix.backup"
        ) || return 1
        [ "$(path_identity "$profile_journal_path")" = \
            "$profile_expected_identity" ] &&
            [ "$(sha256_file "$profile_journal_path")" = \
                "$profile_expected_hash" ] ||
            return 1
        printf '%s\n' "$profile_expected_identity" \
            >"$profile_journal_prefix.expected" || return 1
    else
        : >"$profile_journal_prefix.absent" || return 1
    fi
    durability_barrier "$TRANSACTION_DIR"
}

record_profile_file_intent() {
    profile_slot=$1
    profile_candidate=$2
    require_max_size "$profile_candidate" "$MAX_PROFILE_BYTES" "PATH profile"
    profile_candidate_hash=$(sha256_file "$profile_candidate") || return 1
    profile_journal_prefix=$TRANSACTION_DIR/$profile_slot
    printf 'file=%s\n' "$profile_candidate_hash" \
        >"$profile_journal_prefix.applied" || return 1
    : >"$profile_journal_prefix.changed" || return 1
    chmod 0600 \
        "$profile_journal_prefix.applied" \
        "$profile_journal_prefix.changed" || return 1
    durability_barrier "$TRANSACTION_DIR"
}

record_profile_absent_intent() {
    profile_slot=$1
    profile_journal_prefix=$TRANSACTION_DIR/$profile_slot
    printf '%s\n' absent >"$profile_journal_prefix.applied" || return 1
    : >"$profile_journal_prefix.changed" || return 1
    chmod 0600 \
        "$profile_journal_prefix.applied" \
        "$profile_journal_prefix.changed" || return 1
    durability_barrier "$TRANSACTION_DIR"
}

clear_profile_intent() {
    clear_profile_slot=$1
    clear_profile_prefix=$TRANSACTION_DIR/$clear_profile_slot
    rm -f \
        "$clear_profile_prefix.applied" \
        "$clear_profile_prefix.changed" || return 1
    durability_barrier "$TRANSACTION_DIR"
}

commit_profile_candidate() {
    commit_profile_slot=$1
    commit_profile_candidate_path=$2
    commit_profile_prefix=$TRANSACTION_DIR/$commit_profile_slot
    commit_profile_displaced=$commit_profile_prefix.displaced
    commit_profile_directory=${PROFILE_PATH%/*}

    if [ -f "$commit_profile_prefix.backup" ]; then
        [ -e "$PROFILE_PATH" ] && [ ! -L "$PROFILE_PATH" ] || return 1
        IFS= read -r commit_profile_identity \
            <"$commit_profile_prefix.expected" || return 1
        commit_profile_hash=$(
            sha256_file "$commit_profile_prefix.backup"
        ) || return 1
        mv "$PROFILE_PATH" "$commit_profile_displaced" || return 1
        durability_barrier "$commit_profile_directory" || return 1
        if ! verify_captured_file \
            "$commit_profile_displaced" \
            "$commit_profile_identity" \
            "$commit_profile_hash" \
            "$MAX_PROFILE_BYTES" \
            "PATH profile"; then
            if move_no_replace \
                "$commit_profile_displaced" "$PROFILE_PATH"; then
                durability_barrier "$commit_profile_directory" || return 1
                clear_profile_intent "$commit_profile_slot" || return 1
            fi
            return 1
        fi
    else
        [ -f "$commit_profile_prefix.absent" ] || return 1
        if [ -e "$PROFILE_PATH" ] || [ -L "$PROFILE_PATH" ]; then
            clear_profile_intent "$commit_profile_slot" || return 1
            return 1
        fi
    fi

    if [ "$commit_profile_candidate_path" = - ]; then
        durability_barrier "$commit_profile_directory"
        return
    fi
    if ! move_no_replace "$commit_profile_candidate_path" "$PROFILE_PATH"; then
        rm -f "$commit_profile_candidate_path" 2>/dev/null || :
        if [ ! -e "$commit_profile_displaced" ] &&
            [ ! -L "$commit_profile_displaced" ]; then
            clear_profile_intent "$commit_profile_slot" || return 1
        fi
        return 1
    fi
    durability_barrier "$commit_profile_directory"
}

verify_profile_intent() {
    verify_profile_slot=$1
    verify_profile_path=$2
    verify_profile_prefix=$TRANSACTION_DIR/$verify_profile_slot
    [ -e "$verify_profile_prefix.changed" ] || return 0
    load_file_intent "$verify_profile_prefix.applied" || return 1
    case "$FILE_INTENT" in
        absent)
            [ ! -e "$verify_profile_path" ] &&
                [ ! -L "$verify_profile_path" ]
            ;;
        file)
            rollback_secure_regular_file \
                "$verify_profile_path" "$MAX_PROFILE_BYTES" &&
                [ "$(sha256_file "$verify_profile_path")" = \
                    "$FILE_INTENT_HASH" ]
            ;;
        *) return 1 ;;
    esac
}

load_file_intent() {
    intent_path=$1
    [ -f "$intent_path" ] && [ ! -L "$intent_path" ] || return 1
    intent_size=$(wc -c <"$intent_path" 2>/dev/null |
        tr -d '[:space:]') || return 1
    case "$intent_size" in
        ''|*[!0-9]*) return 1 ;;
    esac
    [ "$intent_size" -le "$MAX_INSTALLER_STATE_BYTES" ] || return 1
    IFS= read -r FILE_INTENT <"$intent_path" || return 1
    case "$FILE_INTENT" in
        absent) FILE_INTENT_HASH= ;;
        file=*)
            FILE_INTENT_HASH=${FILE_INTENT#file=}
            is_sha256 "$FILE_INTENT_HASH" || return 1
            FILE_INTENT=file
            ;;
        *) return 1 ;;
    esac
}

rollback_secure_regular_file() {
    rollback_secure_path=$1
    rollback_secure_limit=$2
    [ -f "$rollback_secure_path" ] &&
        [ ! -L "$rollback_secure_path" ] || return 1
    rollback_secure_uid=$(path_uid "$rollback_secure_path" 2>/dev/null) ||
        return 1
    [ "$rollback_secure_uid" = "$EFFECTIVE_UID" ] || return 1
    rollback_secure_mode=$(path_mode "$rollback_secure_path" 2>/dev/null) ||
        return 1
    mode_is_group_or_world_writable "$rollback_secure_mode" && return 1
    rollback_secure_size=$(wc -c <"$rollback_secure_path" 2>/dev/null |
        tr -d '[:space:]') || return 1
    case "$rollback_secure_size" in
        ''|*[!0-9]*) return 1 ;;
    esac
    [ "$rollback_secure_size" -le "$rollback_secure_limit" ]
}

rollback_profile_path_is_valid() {
    rollback_validate_profile=$1
    [ -n "${HOME:-}" ] || return 1
    case "$rollback_validate_profile" in
        "$HOME/.profile"|"$HOME/.zprofile") return 0 ;;
        *) return 1 ;;
    esac
}

restore_backup_file() {
    restore_backup=$1
    restore_destination=$2
    restore_expected_current=$3
    restore_directory=${restore_destination%/*}
    mkdir -p "$restore_directory" 2>/dev/null || return 1
    restore_temporary=$(
        mktemp "$restore_directory/.hardknock-rollback.XXXXXX"
    ) || restore_temporary=
    [ -n "$restore_temporary" ] || return 1
    if ! cp -p "$restore_backup" "$restore_temporary" 2>/dev/null; then
        rm -f "$restore_temporary" 2>/dev/null || :
        return 1
    fi
    restore_displaced=$restore_temporary.displaced
    if [ -e "$restore_destination" ] || [ -L "$restore_destination" ]; then
        [ "$restore_expected_current" != absent ] || {
            rm -f "$restore_temporary" 2>/dev/null || :
            return 1
        }
        restore_current_identity=$(path_identity "$restore_destination") ||
            return 1
        mv "$restore_destination" "$restore_displaced" 2>/dev/null ||
            return 1
        if ! verify_captured_file \
            "$restore_displaced" \
            "$restore_current_identity" \
            "$restore_expected_current" \
            "$MAX_BINARY_BYTES" \
            "rollback destination"; then
            move_no_replace "$restore_displaced" "$restore_destination" ||
                return 1
            rm -f "$restore_temporary" 2>/dev/null || :
            return 1
        fi
    elif [ "$restore_expected_current" != absent ]; then
        rm -f "$restore_temporary" 2>/dev/null || :
        return 1
    fi
    if ! move_no_replace "$restore_temporary" "$restore_destination"; then
        if [ -e "$restore_displaced" ] ||
            [ -L "$restore_displaced" ]; then
            move_no_replace "$restore_displaced" "$restore_destination" ||
                return 1
        fi
        rm -f "$restore_temporary" 2>/dev/null || :
        return 1
    fi
    rm -f "$restore_displaced" 2>/dev/null || :
    durability_barrier "$restore_directory"
}

remove_expected_file() {
    remove_expected_path=$1
    remove_expected_hash=$2
    remove_expected_limit=$3
    remove_expected_directory=${remove_expected_path%/*}
    remove_expected_identity=$(path_identity "$remove_expected_path") ||
        return 1
    remove_expected_capture=$(
        mktemp "$remove_expected_directory/.hardknock-remove.XXXXXX"
    ) || remove_expected_capture=
    [ -n "$remove_expected_capture" ] || return 1
    rm -f "$remove_expected_capture" || return 1
    mv "$remove_expected_path" "$remove_expected_capture" || return 1
    if ! verify_captured_file \
        "$remove_expected_capture" \
        "$remove_expected_identity" \
        "$remove_expected_hash" \
        "$remove_expected_limit" \
        "rollback removal"; then
        move_no_replace "$remove_expected_capture" "$remove_expected_path" ||
            return 1
        return 1
    fi
    rm -f "$remove_expected_capture" || return 1
    durability_barrier "$remove_expected_directory"
}

rollback_managed_file() {
    rollback_relative=$1
    rollback_destination=$PREFIX/$rollback_relative
    rollback_backup=$BACKUP_DIRECTORY/$rollback_relative
    rollback_marker=$PLACED_DIRECTORY/$rollback_relative
    FILE_INTENT=
    FILE_INTENT_HASH=
    if [ -e "$rollback_marker" ]; then
        load_file_intent "$rollback_marker" || return 1
    fi
    if [ -e "$rollback_backup" ] || [ -L "$rollback_backup" ]; then
        rollback_secure_regular_file \
            "$rollback_backup" "$MAX_BINARY_BYTES" || return 1
        rollback_backup_hash=$(sha256_file "$rollback_backup") || return 1
        if [ -e "$rollback_destination" ] ||
            [ -L "$rollback_destination" ]; then
            rollback_secure_regular_file \
                "$rollback_destination" "$MAX_BINARY_BYTES" || return 1
            rollback_destination_hash=$(
                sha256_file "$rollback_destination"
            ) || return 1
            if [ "$rollback_destination_hash" = "$rollback_backup_hash" ]; then
                return 0
            fi
            [ "$FILE_INTENT" = file ] &&
                [ "$rollback_destination_hash" = "$FILE_INTENT_HASH" ] ||
                return 1
        fi
        if [ -e "$rollback_destination" ] ||
            [ -L "$rollback_destination" ]; then
            rollback_expected_current=$FILE_INTENT_HASH
        else
            rollback_expected_current=absent
        fi
        restore_backup_file \
            "$rollback_backup" \
            "$rollback_destination" \
            "$rollback_expected_current" ||
            return 1
    elif [ "$FILE_INTENT" = file ]; then
        rollback_source=$STAGE_DIRECTORY/$rollback_relative
        if [ -e "$rollback_source" ] || [ -L "$rollback_source" ]; then
            rollback_secure_regular_file \
                "$rollback_source" "$MAX_BINARY_BYTES" || return 1
            [ "$(sha256_file "$rollback_source")" = "$FILE_INTENT_HASH" ] ||
                return 1
        fi
        if [ -e "$rollback_destination" ] ||
            [ -L "$rollback_destination" ]; then
            rollback_secure_regular_file \
                "$rollback_destination" "$MAX_BINARY_BYTES" || return 1
            rollback_destination_hash=$(
                sha256_file "$rollback_destination"
            ) || return 1
            [ "$rollback_destination_hash" = "$FILE_INTENT_HASH" ] ||
                return 1
            remove_expected_file \
                "$rollback_destination" \
                "$FILE_INTENT_HASH" \
                "$MAX_BINARY_BYTES" ||
                return 1
        fi
    elif [ -n "$FILE_INTENT" ] && [ "$FILE_INTENT" != absent ]; then
        return 1
    fi
    return 0
}

rollback_profile_change() {
    rollback_profile_slot=$1
    rollback_profile_path=$2
    rollback_profile_prefix=$TRANSACTION_DIR/$rollback_profile_slot
    [ -e "$rollback_profile_prefix.changed" ] || return 0
    [ "$rollback_profile_path" != - ] || return 1
    rollback_profile_path_is_valid "$rollback_profile_path" || return 1
    load_file_intent "$rollback_profile_prefix.applied" || return 1

    if [ -f "$rollback_profile_prefix.backup" ]; then
        rollback_profile_backup=$rollback_profile_prefix.backup
        rollback_profile_displaced=$rollback_profile_prefix.displaced
        rollback_secure_regular_file \
            "$rollback_profile_backup" "$MAX_PROFILE_BYTES" || return 1
        rollback_profile_backup_hash=$(
            sha256_file "$rollback_profile_backup"
        ) || return 1
        if { [ -e "$rollback_profile_displaced" ] ||
            [ -L "$rollback_profile_displaced" ]; } &&
            { [ ! -e "$rollback_profile_path" ] &&
                [ ! -L "$rollback_profile_path" ]; }; then
            IFS= read -r rollback_profile_identity \
                <"$rollback_profile_prefix.expected" || return 1
            verify_captured_file \
                "$rollback_profile_displaced" \
                "$rollback_profile_identity" \
                "$rollback_profile_backup_hash" \
                "$MAX_PROFILE_BYTES" \
                "displaced PATH profile" ||
                return 1
            move_no_replace \
                "$rollback_profile_displaced" "$rollback_profile_path" ||
                return 1
            durability_barrier "${rollback_profile_path%/*}" ||
                return 1
            return 0
        fi
        if [ -e "$rollback_profile_path" ] ||
            [ -L "$rollback_profile_path" ]; then
            rollback_secure_regular_file \
                "$rollback_profile_path" "$MAX_PROFILE_BYTES" || return 1
            rollback_profile_hash=$(
                sha256_file "$rollback_profile_path"
            ) || return 1
            if [ "$rollback_profile_hash" = \
                "$rollback_profile_backup_hash" ]; then
                return 0
            fi
            [ "$FILE_INTENT" = file ] &&
                [ "$rollback_profile_hash" = "$FILE_INTENT_HASH" ] ||
                return 1
        else
            [ "$FILE_INTENT" = absent ] || return 1
        fi
        if [ -e "$rollback_profile_path" ] ||
            [ -L "$rollback_profile_path" ]; then
            rollback_profile_expected_current=$FILE_INTENT_HASH
        else
            rollback_profile_expected_current=absent
        fi
        restore_backup_file \
            "$rollback_profile_backup" \
            "$rollback_profile_path" \
            "$rollback_profile_expected_current" ||
            return 1
    elif [ -e "$rollback_profile_prefix.absent" ]; then
        if [ ! -e "$rollback_profile_path" ] &&
            [ ! -L "$rollback_profile_path" ]; then
            return 0
        fi
        [ "$FILE_INTENT" = file ] || return 1
        rollback_secure_regular_file \
            "$rollback_profile_path" "$MAX_PROFILE_BYTES" || return 1
        rollback_profile_hash=$(sha256_file "$rollback_profile_path") ||
            return 1
        [ "$rollback_profile_hash" = "$FILE_INTENT_HASH" ] || return 1
        remove_expected_file \
            "$rollback_profile_path" \
            "$FILE_INTENT_HASH" \
            "$MAX_PROFILE_BYTES" ||
            return 1
    else
        return 1
    fi
    return 0
}

rollback_created_directories() {
    [ -f "$CREATED_DIRECTORIES_FILE" ] || return 0
    awk '{ paths[NR] = $0 } END { for (i = NR; i > 0; i--) print paths[i] }' \
        "$CREATED_DIRECTORIES_FILE" |
        while IFS= read -r rollback_record; do
            rollback_relative=${rollback_record%%|*}
            rollback_identity=${rollback_record#*|}
            rollback_directory=$PREFIX/$rollback_relative
            if [ -d "$rollback_directory" ] &&
                [ ! -L "$rollback_directory" ] &&
                [ "$(path_identity "$rollback_directory" 2>/dev/null || :)" = \
                    "$rollback_identity" ]; then
                rmdir "$rollback_directory" 2>/dev/null || :
            fi
        done
}

rollback_transaction() {
    [ -n "$TRANSACTION_DIR" ] && [ -d "$TRANSACTION_DIR" ] || return 0
    rollback_failed=0
    rollback_profile_change \
        profile "$TRANSACTION_PROFILE_PATH" || rollback_failed=1
    rollback_profile_change \
        previous-profile "$TRANSACTION_PREVIOUS_PROFILE_PATH" ||
        rollback_failed=1
    rollback_managed_file "$MANIFEST_RELATIVE" || rollback_failed=1
    rollback_managed_file share/doc/hardknock/NOTICE || rollback_failed=1
    rollback_managed_file share/doc/hardknock/LICENSE || rollback_failed=1
    rollback_managed_file bin/hk-effect || rollback_failed=1
    rollback_managed_file bin/hardknock || rollback_failed=1
    rollback_created_directories
    [ "$rollback_failed" -eq 0 ]
}

write_transaction_phase() {
    new_transaction_phase=$1
    printf '%s\n' "$new_transaction_phase" \
        >"$TRANSACTION_DIR/phase.next" || return 1
    chmod 0600 "$TRANSACTION_DIR/phase.next" || return 1
    durability_barrier "$TRANSACTION_DIR/phase.next" || return 1
    mv "$TRANSACTION_DIR/phase.next" "$TRANSACTION_PHASE_FILE" ||
        return 1
    durability_barrier "$TRANSACTION_DIR"
}

finish_transaction() {
    durability_barrier "$PREFIX" ||
        fail "cannot make installation changes durable"
    write_transaction_phase committed ||
        fail "cannot commit installation transaction"
    TRANSACTION_COMMITTED=1
    TRANSACTION_ACTIVE=0
    rm -rf "$TRANSACTION_DIR" ||
        fail "cannot remove completed installation transaction"
    durability_barrier "$PREFIX" ||
        fail "cannot make installation transaction cleanup durable"
    TRANSACTION_DIR=
    TRANSACTION_COMMITTED=0
    TRANSACTION_CLEANUP_ALLOWED=0
}

cleanup() {
    cleanup_status=$?
    trap - 0 1 2 3 15
    rollback_complete=1
    if [ "${TRANSACTION_ACTIVE:-0}" -eq 1 ] &&
        [ "${TRANSACTION_COMMITTED:-0}" -eq 0 ]; then
        if durable_transaction_phase_is_committed; then
            TRANSACTION_COMMITTED=1
            TRANSACTION_ACTIVE=0
        else
            rollback_transaction || rollback_complete=0
        fi
    fi
    if [ -n "${TRANSACTION_DIR:-}" ] &&
        [ -d "$TRANSACTION_DIR" ] &&
        [ "$TRANSACTION_DIR" != / ] &&
        [ "${TRANSACTION_CLEANUP_ALLOWED:-0}" -eq 1 ]; then
        if [ "$rollback_complete" -eq 1 ]; then
            rm -rf "$TRANSACTION_DIR"
        else
            printf '%s: rollback incomplete; preserved recovery data at %s\n' \
                "$PROGRAM_NAME" "$TRANSACTION_DIR" >&2
            PREFIX_CREATED=0
        fi
    fi
    if [ -n "${RECOVERY_LISTING:-}" ]; then
        rm -f "$RECOVERY_LISTING" 2>/dev/null || :
        RECOVERY_LISTING=
    fi
    if [ -n "${PLACEMENT_TEMPORARY:-}" ] &&
        [ "$PLACEMENT_TEMPORARY" != / ]; then
        rm -f "$PLACEMENT_TEMPORARY" 2>/dev/null || :
        PLACEMENT_TEMPORARY=
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

verify_applied_file() {
    verify_relative=$1
    verify_hash=$2
    verify_limit=$3
    verify_destination=$PREFIX/$verify_relative
    rollback_secure_regular_file "$verify_destination" "$verify_limit" &&
        [ "$(sha256_file "$verify_destination")" = "$verify_hash" ]
}

verify_installation_commit() {
    verify_applied_file \
        bin/hardknock "$NEW_HASH_HARDKNOCK" "$MAX_BINARY_BYTES" &&
        verify_applied_file \
            bin/hk-effect "$NEW_HASH_EFFECT" "$MAX_BINARY_BYTES" &&
        verify_applied_file \
            share/doc/hardknock/LICENSE "$NEW_HASH_LICENSE" "$MAX_LEGAL_BYTES" &&
        verify_applied_file \
            share/doc/hardknock/NOTICE "$NEW_HASH_NOTICE" "$MAX_LEGAL_BYTES" &&
        verify_applied_file \
            "$MANIFEST_RELATIVE" \
            "$NEW_HASH_MANIFEST" \
            "$MAX_INSTALLER_STATE_BYTES" ||
        return 1
    if [ "$PROFILE_NEEDS_CHANGE" -eq 1 ]; then
        verify_profile_intent profile "$NEW_PATH_PROFILE" || return 1
    fi
    if [ "$PREVIOUS_PROFILE_REMOVE" -eq 1 ]; then
        verify_profile_intent \
            previous-profile "$PREVIOUS_PROFILE_PATH" || return 1
    fi
}

apply_installation() {
    begin_transaction install
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

    if [ "$PREVIOUS_PROFILE_REMOVE" -eq 1 ]; then
        journal_profile_change \
            previous-profile "$PREVIOUS_PROFILE_PATH" ||
            fail "cannot journal previous PATH profile update"
        remove_path_block \
            previous-profile \
            "$PREVIOUS_PROFILE_PATH" \
            "$OLD_PATH_PROFILE_CREATED"
    fi
    if [ "$PROFILE_NEEDS_CHANGE" -eq 1 ]; then
        PROFILE_PATH=$NEW_PATH_PROFILE
        journal_profile_change profile "$PROFILE_PATH" ||
            fail "cannot journal PATH profile update"
        write_path_block profile ||
            fail "cannot atomically update PATH profile"
    fi

    verify_installation_commit ||
        fail "managed installation changed before transaction commit"
    finish_transaction
    PROFILE_PATH=$NEW_PATH_PROFILE
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
    if [ "$STATUS_HARDKNOCK" = remove ]; then
        OLD_ID_HARDKNOCK=$(path_identity "$PREFIX/bin/hardknock") ||
            fail "cannot identify managed hardknock during uninstall"
    fi
    if [ "$STATUS_EFFECT" = remove ]; then
        OLD_ID_EFFECT=$(path_identity "$PREFIX/bin/hk-effect") ||
            fail "cannot identify managed hk-effect during uninstall"
    fi
    if [ "$STATUS_LICENSE" = remove ]; then
        OLD_ID_LICENSE=$(
            path_identity "$PREFIX/share/doc/hardknock/LICENSE"
        ) || fail "cannot identify managed LICENSE during uninstall"
    fi
    if [ "$STATUS_NOTICE" = remove ]; then
        OLD_ID_NOTICE=$(
            path_identity "$PREFIX/share/doc/hardknock/NOTICE"
        ) || fail "cannot identify managed NOTICE during uninstall"
    fi
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
    remove_marker=$PLACED_DIRECTORY/$remove_relative
    mkdir -p "${remove_backup%/*}" "${remove_marker%/*}" || return 1
    printf '%s\n' absent >"$remove_marker" || return 1
    chmod 0600 "$remove_marker" || return 1
    durability_barrier "$TRANSACTION_DIR" || return 1
    load_managed_expectation "$remove_relative" || return 1
    [ "$EXPECTED_STATE" = present ] || {
        clear_file_intent "$remove_marker" || return 1
        return 1
    }
    if [ ! -e "$remove_destination" ] &&
        [ ! -L "$remove_destination" ]; then
        clear_file_intent "$remove_marker" || return 1
        return 1
    fi
    mv "$remove_destination" "$remove_backup" || return 1
    durability_barrier "${remove_destination%/*}" || return 1
    if ! verify_captured_file \
        "$remove_backup" \
        "$EXPECTED_IDENTITY" \
        "$EXPECTED_HASH" \
        "$EXPECTED_LIMIT" \
        "managed installation file"; then
        if move_no_replace "$remove_backup" "$remove_destination"; then
            durability_barrier "${remove_destination%/*}" || return 1
            clear_file_intent "$remove_marker" || return 1
        fi
        return 1
    fi
}

verify_uninstall_removal() {
    verify_remove_relative=$1
    verify_remove_status=$2
    [ "$verify_remove_status" = remove ] || return 0
    verify_remove_path=$PREFIX/$verify_remove_relative
    [ ! -e "$verify_remove_path" ] && [ ! -L "$verify_remove_path" ]
}

apply_uninstall() {
    PREVIOUS_PROFILE_PATH=
    begin_transaction uninstall
    if [ "$PROFILE_REMOVE" -eq 1 ]; then
        journal_profile_change profile "$PROFILE_PATH" ||
            fail "cannot journal PATH profile removal"
        remove_path_block \
            profile "$PROFILE_PATH" "$OLD_PATH_PROFILE_CREATED"
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

    transaction_remove_managed_file \
        "$MANIFEST_RELATIVE" remove ||
        fail "cannot remove managed installation manifest"
    verify_uninstall_removal bin/hardknock "$STATUS_HARDKNOCK" &&
        verify_uninstall_removal bin/hk-effect "$STATUS_EFFECT" &&
        verify_uninstall_removal \
            share/doc/hardknock/LICENSE "$STATUS_LICENSE" &&
        verify_uninstall_removal \
            share/doc/hardknock/NOTICE "$STATUS_NOTICE" &&
        verify_uninstall_removal "$MANIFEST_RELATIVE" remove ||
        fail "managed installation changed before uninstall commit"
    if [ "$PROFILE_REMOVE" -eq 1 ]; then
        verify_profile_intent profile "$PROFILE_PATH" ||
            fail "PATH profile changed before uninstall commit"
    fi
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
        assert_dry_run_recovery_clean
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
if [ "$DRY_RUN" -eq 1 ]; then
    assert_dry_run_recovery_clean
else
    recover_before_snapshot
fi
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
