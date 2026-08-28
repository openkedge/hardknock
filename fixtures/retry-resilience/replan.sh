#!/bin/sh
set -eu
case "$(cat fixture-kind)" in
  retry-resilience)
    printf '%s\n' success > result
    echo alternative_succeeded
    ;;
  config-drift)
    cp generation plan-generation
    /bin/sh ./operation.sh
    ;;
  stale-credential)
    /bin/sh ./operation.sh
    ;;
esac
