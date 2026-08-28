#!/bin/sh
set -eu
attempt=${HK_ATTEMPT:-1}
if [ "$attempt" -le "${HK_FAILURES:-0}" ]; then
  echo 'HK_SIGNATURE transient_command_failure'
  exit "${HK_FAILURE_EXIT:-17}"
fi
token=${HK_TOKEN_STATE-$(cat token)}
if [ "$token" != VALID_TOKEN ]; then
  echo 'HK_SIGNATURE stale_credential'
  exit 21
fi
if [ "$(cat generation)" != "$(cat plan-generation)" ]; then
  echo 'HK_SIGNATURE configuration_stale'
  exit 22
fi
if [ "$(cat input-generation)" != "$(cat generation)" ]; then
  echo 'HK_SIGNATURE stale_input'
  exit 23
fi
if [ "$(cat dependency)" = down ] && [ "$attempt" -lt 2 ]; then
  echo 'HK_SIGNATURE transient_command_failure'
  exit 17
fi
printf '%s\n' success > result
echo operation_succeeded
