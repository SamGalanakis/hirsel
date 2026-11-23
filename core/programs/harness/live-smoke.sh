#!/usr/bin/env bash
set +x
set -euo pipefail

env_file=${OPENROUTER_ENV_FILE:-$HOME/.config/hirsel/openrouter-dev.env}
if [[ -r "$env_file" ]]; then
    source "$env_file"
fi
if [[ -z ${OPENROUTER_API_KEY:-} ]]; then
    printf 'SKIP: OpenRouter key unavailable\n'
    exit 0
fi
export -n OPENROUTER_API_KEY
unset HIRSEL_RECORD

here=$(cd -- "$(dirname -- "$0")" && pwd)
work=$(mktemp -d)
trap 'rm -rf -- "$work"' EXIT
mkdir "$work/data"
"${HIRSEL:-hirsel}" build "$here/live-smoke" --limit 268435456 --dir "$work/data" \
    --grant Stdout --grant Clock --grant Random --grant Tasks \
    --grant 'Net=Listen 127.0.0.0/8 0-65535;Connect 127.0.0.0/8 0-65535;Resolve openrouter.ai;Connect 0.0.0.0/0 443-443;Tls openrouter.ai 443-443 System' \
    --grant 'Secret:api_key=env:OPENROUTER_API_KEY to openrouter.ai' -o "$work/gateway-smoke"

OPENROUTER_API_KEY="$OPENROUTER_API_KEY" "$work/gateway-smoke"
