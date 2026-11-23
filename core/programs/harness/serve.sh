#!/usr/bin/env bash
set +x
set -euo pipefail

here=$(cd -- "$(dirname -- "$0")" && pwd)
mode=${1:---live}
case "$mode" in
    --scripted|--live|--gateway) if (( $# )); then shift; fi ;;
    *) printf '%s\n' 'Use serve.sh --live [message], --scripted or --gateway.' >&2; exit 2 ;;
esac
if [[ $# -gt 1 || ( $# -gt 0 && "$mode" != --live ) ]]; then
    printf '%s\n' 'Only --live accepts a message.' >&2
    exit 2
fi

net='Listen 127.0.0.0/8 0-65535;Connect 127.0.0.0/8 0-65535'
if [[ "$mode" == --scripted ]]; then
    entry=(--entry harness_dev_serve@1/main/scripted)
else
    requested_model=${OPENROUTER_MODEL:-}
    env_file=${OPENROUTER_ENV_FILE:-$HOME/.config/hirsel/openrouter-dev.env}
    if [[ -r "$env_file" ]]; then
        source "$env_file"
    fi
    if [[ -z ${OPENROUTER_API_KEY:-} ]]; then
        printf '%s\n' 'Set OPENROUTER_API_KEY in ~/.config/hirsel/openrouter-dev.env, or run serve.sh --scripted.' >&2
        exit 1
    fi
    export -n OPENROUTER_API_KEY
    export OPENROUTER_MODEL=${requested_model:-${OPENROUTER_MODEL:-deepseek/deepseek-v4.1-flash-1017}}
    net+=';Resolve openrouter.ai;Connect 0.0.0.0/0 443-443;Tls openrouter.ai 443-443 System'
    entry=(--grant 'Env=OPENROUTER_MODEL' --grant 'Secret:api_key=env:OPENROUTER_API_KEY to openrouter.ai')
fi

unset HIRSEL_RECORD
work=$(mktemp -d)
trap 'rm -rf -- "$work"' EXIT
mkdir "$work/data"
"${HIRSEL:-hirsel}" build "$here/serve" --limit 536870912 --dir "$work/data" \
    --grant Stdin --grant Stdout --grant Clock --grant Random --grant Tasks --grant "Net=$net" \
    "${entry[@]}" -o "$work/gateway"

case "$mode" in
    --scripted) node "$here/ts-client/demo.ts" "$work/gateway" --scripted ;;
    --live) OPENROUTER_API_KEY="$OPENROUTER_API_KEY" node "$here/ts-client/demo.ts" "$work/gateway" --live "$@" ;;
    --gateway) OPENROUTER_API_KEY="$OPENROUTER_API_KEY" "$work/gateway" ;;
esac
