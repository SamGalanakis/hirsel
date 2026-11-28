#!/bin/sh
# Build both services from the checkout containing this script.
set -eu
driver=$(realpath "$1")
runtime=$(realpath "$2")
mkdir -p "$3"
out=$(realpath "$3")
repo=$(CDPATH='' cd -- "$(dirname "$0")/../.." && pwd)
cd "$repo"
PATH="$repo/tools/buck2/toolchains/llvm-files/bin:$PATH"
export PATH
build() {
    name=$1
    entry=$2
    shift 2
    "$driver" build core/programs/registry --libraries core --runtime "$runtime" --cc cc \
        --entry "hirsel.ai/registry@1/$entry" --limit 4294967296 --grant Compiler \
        --grant Clock --grant Random --grant Tasks --dir /etc/hirsel/registry \
        --output "$out/$name" "$@"
}
build writer main/static_main \
    --grant 'Net=Listen 127.0.0.1/32 8111-8111;Connect 0.0.0.0/0 1-65535 public;Connect ::/0 1-65535 public;Resolve *;Tls * 1-65535 System' \
    --grant 'Env=AWS_ACCESS_KEY_ID' --grant 'Secret:bucket_secret=env:AWS_SECRET_ACCESS_KEY kdf' \
    --grant 'Secret:release_signer=file:/run/registry-keys/log sign(ed25519)' \
    --grant 'Secret:status_signer=file:/run/registry-keys/status sign(ed25519)'
build discovery discovery/main --dir /data/projection --grant Stderr \
    --grant 'Net=Listen 127.0.0.1/32 8112-8113;Connect 0.0.0.0/0 443-443 public;Connect ::/0 443-443 public;Resolve pkg.hirsel.ai;Tls pkg.hirsel.ai 443-443 System'
install -m 755 "$driver" "$out/hirsel"
strip "$out/hirsel"
cp -r deploy/pkg/.dockerignore deploy/pkg/config deploy/pkg/Dockerfile deploy/pkg/fly.toml deploy/pkg/nginx.conf deploy/pkg/start.py "$out/"
