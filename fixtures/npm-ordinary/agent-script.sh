#!/bin/sh
# SPDX-License-Identifier: Apache-2.0
set -eu
if [ "${1:-run}" = run ]; then
    strategy=baseline
    if [ -f .hardknock/context.md ]; then
        while read -r directive lesson action; do
            if [ "$directive" = HARDKNOCK_RECOMMEND ]; then
                printf 'RETRIEVED %s\n' "$lesson"
                if [ "$action" = './agent-script.sh alternative' ] && [ "$strategy" = baseline ] && [ ! -f ignore-experience ]; then
                    strategy=alternative
                    printf 'APPLIED %s\n' "$lesson"
                else
                    printf 'IGNORED %s\n' "$lesson"
                fi
            fi
        done < .hardknock/context.md
    fi
    exec "$0" "$strategy"
fi
case "${1:-}" in
    baseline)
        echo 'ACTION shell npm install'
        printf '{"simulated":true}\n' > package-lock.json
        ;;
    alternative)
        echo 'ACTION shell pnpm install'
        touch pnpm-attempted
        ;;
    *) exit 2 ;;
esac
