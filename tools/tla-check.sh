#!/usr/bin/env bash
set -euo pipefail

usage() {
    cat <<'USAGE'
Usage:
  tools/tla-check.sh [--tlc] [-config FILE] [-workers N] [-deadlock] MODEL.tla
  tools/tla-check.sh conform [-config FILE] -trace FILE.jsonl MODEL.tla

The config defaults to MODEL.cfg beside the module. Explicit paths are relative
to the caller. Model checks run in the module directory. Use -deadlock to
disable deadlock checking, as in TLC. The default is one worker.

Model checks whose inputs match a recorded TLC comparison use everystate 0.4.1.
Other inputs and --tlc use real TLC. Install the pinned native binary at
~/.local/bin/everystate or on PATH. TLC uses Java and the jars in
${FORMAL_TOOLS:-~/.local/opt/fleet-formal}.

Conform reads everystate JSONL observations and prints its JSON report. Use the
existing TLC replay commands for raw simulator captures:
  python3 proofs/fleet/check_proofs.py --only trace
  python3 proofs/store/check_proofs.py --trace-only
Real TLC has no equivalent to the generic conform command.
USAGE
}

fail() {
    printf 'tla-check: %s\n' "$*" >&2
    exit 1
}

find_everystate() {
    local version
    if [[ -x "$HOME/.local/bin/everystate" ]]; then
        everystate="$HOME/.local/bin/everystate"
    else
        everystate=$(type -P everystate) || fail 'everystate 0.4.1 is required (pin 875466de)'
    fi
    [[ $everystate = /* ]] || everystate="$PWD/$everystate"
    version=$("$everystate" --version)
    [[ $version == 'everystate 0.4.1' ]] || fail "expected everystate 0.4.1, got $version"
    printf 'tla-check: using %s\n' "$version" >&2
}

sha256() {
    local checksum
    if command -v sha256sum >/dev/null; then
        checksum=$(sha256sum) || return 1
    else
        checksum=$(shasum -a 256) || return 1
    fi
    printf '%s' "${checksum%% *}"
}

parity_key() {
    local input checksum trace_file
    local files=("$model" "$config") checksums=()
    case "$module" in
        TraceFleetMove.tla)
            files+=("$model_dir/FleetMove.tla")
            trace_file=${TRACE_FILE:-}
            [[ $trace_file ]] || return 1
            ;;
        TraceStoreRetention.tla)
            files+=("$model_dir/StoreRetention.tla")
            trace_file=${TRACE_FILE-traces/retention-trace-green.ndjson}
            ;;
        *) trace_file= ;;
    esac
    if [[ $trace_file ]]; then
        [[ $trace_file = /* ]] || trace_file="$model_dir/$trace_file"
        files+=("$trace_file")
    fi
    for input in "${files[@]}"; do
        [[ -f $input && -r $input ]] || return 1
        checksum=$(sha256 < "$input") || return 1
        checksums+=("$checksum")
    done
    printf '%s\n' "${checksums[@]}" | sha256
}

mode=model
if [[ ${1:-} == conform ]]; then
    mode=conform
    shift
fi

model=
config=
trace=
workers=
skip_deadlock=false
force_tlc=false
while (( $# )); do
    case "$1" in
        -h|--help) usage; exit 0 ;;
        --tlc) force_tlc=true ;;
        -deadlock) skip_deadlock=true ;;
        -config|-trace|-workers)
            [[ ${2:-} && $2 != -* ]] || fail "$1 needs a value"
            case "$1" in
                -config) config=$2 ;;
                -trace) trace=$2 ;;
                -workers) workers=$2 ;;
            esac
            shift
            ;;
        -*) fail "unknown option $1 (see --help)" ;;
        *)
            [[ ! $model ]] || fail 'expected one TLA+ module'
            model=$1
            ;;
    esac
    shift
done

[[ $model == *.tla && -f $model && -r $model ]] ||
    fail 'expected a readable .tla module (see --help)'
model_dir=$(cd -- "$(dirname -- "$model")" && pwd -P)
module=${model##*/}
model="$model_dir/$module"
config=${config:-${model%.tla}.cfg}
[[ $config = /* ]] || config="$PWD/$config"
[[ -f $config && -r $config ]] || fail "missing or unreadable config $config (use -config FILE)"

if [[ $mode == conform ]]; then
    [[ $force_tlc == false && ! $workers && $skip_deadlock == false ]] ||
        fail 'conform requires everystate and accepts only -config and -trace'
    [[ -f $trace && -r $trace ]] || fail 'conform requires a readable -trace FILE'
    [[ $trace = /* ]] || trace="$PWD/$trace"
    find_everystate
    cd -- "$model_dir"
    if report=$("$everystate" conform -json -config "$config" -trace "$trace" "$module"); then
        # A successful result can omit rows that lack both action and state.
        python3 -c '
import json, sys

report = json.load(sys.stdin)
if report["status"] != "conforms" or report["ignored"]:
    sys.exit("tla-check: conform must check every trace observation")
' <<< "$report"
        printf '%s\n' "$report"
    else
        status=$?
        printf '%s\n' "$report"
        exit "$status"
    fi
    exit 0
fi

[[ ! $trace ]] || fail 'use conform for -trace FILE'
workers=${workers:-1}
[[ $workers =~ ^[1-9][0-9]*$ ]] || fail '-workers must be a positive integer'
args=(-config "$config" -workers "$workers")
if [[ $skip_deadlock == true ]]; then
    args+=(-deadlock)
fi
args+=("$module")

if [[ $force_tlc == true ]]; then
    reason='--tlc requested'
else
    reason='inputs have no recorded native parity'
    key=$(parity_key) || key=
    # SHA-256 of the ordered file digests, one per line. Each entry matched
    # TLC's generated and distinct state counts and its invariant verdict.
    case "$module:$key" in
        FleetMove.tla:a8dbae3e47409364d01ae57fb181ec384e89d97e54ca7f1754c9693f35f3e13e|\
        StoreCatchUp.tla:64ef185780289c17c18ceeab3a451bb6f604aea8940b17a03ba624a034a57af3|\
        StoreJanitor.tla:ba51a2129b709b102b2b1178996c763e8cdcfaabda2c49f73cbd20ef16ef3643|\
        StoreJanitor.tla:de1064f516e34ab3f1ac21df0fb342c3345bc5400c7ff2fe0239988db4fcd2eb|\
        StoreRetention.tla:ee22bb0bf4f0fb158a26f5035d79824d58afa74be2db1398fa87c41e7d41291e|\
        TraceFleetMove.tla:4b10d909d0b598c21916d1d1af5518b5aa53ffe6685d7e9b14f338ac487711e2|\
        TraceStoreRetention.tla:408b4becdc43f7113ff6db85f990c3d33086976b870812c150196f0a7aeb6063|\
        TraceStoreRetention.tla:a998559c605c2d927aba8b5abdd368660fbbb147762b8d46ad316ae2e1eacf73|\
        TraceStoreRetention.tla:62e77b96f56739030d286e3b7a88f5d0e30939c07d4f491f3de61051bdfe377f)
            find_everystate
            cd -- "$model_dir"
            exec "$everystate" "${args[@]}"
            ;;
        *)
            ;;
    esac
fi

tools=${FORMAL_TOOLS:-"$HOME/.local/opt/fleet-formal"}
[[ $tools = /* ]] || tools="$PWD/$tools"
jar="$tools/tla2tools-nightly.jar"
[[ -f $jar ]] || jar="$tools/tla2tools.jar"
community="$tools/CommunityModules-deps.jar"
command -v java >/dev/null || fail "real TLC requires Java ($reason)"
[[ -r $jar && -r $community ]] || fail "real TLC requires tla2tools and CommunityModules-deps jars in $tools ($reason)"
printf 'tla-check: using real TLC (%s)\n' "$reason" >&2

tmpdir=${TMPDIR:-/tmp}
[[ $tmpdir = /* ]] || tmpdir="$PWD/$tmpdir"
meta=$(mktemp -d "$tmpdir/hirsel-tlc.XXXXXXXX")
trap 'rm -rf -- "$meta"' EXIT
cd -- "$model_dir"
java -Xmx2g -XX:+UseParallelGC -cp "$jar:$community" tlc2.TLC \
    -noGenerateSpecTE -metadir "$meta" "${args[@]}"
