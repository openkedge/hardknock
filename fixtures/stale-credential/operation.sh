#!/bin/sh
set -eu
kind=$(cat fixture-kind)
attempt=${HK_ATTEMPT:-1}
if [ "$attempt" -le "${HK_FAILURES:-0}" ]; then
  echo 'HK_SIGNATURE transient_command_failure'
  exit "${HK_FAILURE_EXIT:-17}"
fi
case "$kind" in
  retry-resilience)
    delay=${HK_DELAY_MS:-0}
    echo "logical_delay_ms=$delay"
    if [ "$delay" -ge 2000 ]; then
      echo 'HK_SIGNATURE retry_exhaustion'
      exit 20
    fi
    ;;
  stale-credential)
    token=${HK_TOKEN_STATE:-$(cat token)}
    if [ "$token" != VALID_TOKEN ]; then
      echo 'HK_SIGNATURE stale_credential'
      exit 21
    fi
    ;;
  config-drift)
    if [ "$(cat generation)" != "$(cat plan-generation)" ]; then
      echo 'HK_SIGNATURE configuration_stale'
      exit 22
    fi
    ;;
esac
printf '%s\n' success > result
echo operation_succeeded
