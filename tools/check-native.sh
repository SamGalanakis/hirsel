#!/usr/bin/env bash
# Native gate: LLVM emission, the runtime and its C harness, native differential, Lean and
# TLA+ model checks. Run by the full gate and on demand; native changes run native --full --filter
# for the programs they touch.
# Buck2 runs native tests, C harnesses and toolchain checks on the pool, with
# pinned tools as inputs. The Lean and TLA+ model checks still use the invoking host.
set -Eeuo pipefail
# Four test threads and campaign workers unless the caller says otherwise.
export CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-4}" HIRSEL_JOBS="${HIRSEL_JOBS:-4}" RUST_TEST_THREADS="${RUST_TEST_THREADS:-4}"
cd "$(dirname "$0")/.."
stage="arguments"
stage_start=$SECONDS
gate_start=$SECONDS
fail_stage() {
  local status=$?
  echo "check-native.sh: ${stage} failed (exit ${status}, $((SECONDS - stage_start)) s)" >&2
  exit "$status"
}
trap fail_stage ERR
# Every stage runs, so one red stage cannot hide the rest; the gate fails after the last.
failed_stages=()
first_status=0
run_stage() {
  stage=$1
  shift
  stage_start=$SECONDS
  echo "check-native.sh: ${stage}"
  local status
  # A subshell outside any `||`, so errexit still stops the stage's own
  # commands; the ERR trap is off while it runs.
  trap - ERR
  set +e
  (set -e; "$@")
  status=$?
  set -e
  trap fail_stage ERR
  if (( status != 0 )); then
    echo "check-native.sh: ${stage} failed (exit ${status}, $((SECONDS - stage_start)) s)" >&2
    failed_stages+=("$stage")
    (( first_status != 0 )) || first_status=$status
  else
    echo "check-native.sh: ${stage} passed ($((SECONDS - stage_start)) s)"
  fi
}
if (( $# > 0 )); then echo "usage: tools/check-native.sh" >&2; exit 2; fi
# Every stage is in the full gate: its campaign targets exist under hirsel.full.
config=(-c hirsel.full=true ${HIRSEL_PRIORITY:+-c "hirsel.priority=$HIRSEL_PRIORITY"})
export HIRSEL_BUCK2_CONFIG="${config[*]}"
buck2() { local command=$1; shift; python3 tools/buck2/event_logs.py "$command" "${config[@]}" "$@"; }
release=(--target-platforms //tools/buck2:release)
libtest() { local argument; for argument in "$@"; do printf -- '--test-arg=%s\n' "$argument"; done; }
mapfile -t runtime < <(buck2 uquery 'kind("_rust_test", attrregexfilter(name, "^hirsel-rt-", //...))')
run_stage "runtime tests" buck2 test "${runtime[@]}" --
run_stage "runtime C harness" buck2 test //:gate-native-runtime-c --
run_stage "library C probes" buck2 test //:gate-native-library-c --
run_stage "layout C probes" buck2 test //:hirsel-lir-test-it -- $(libtest layout:: --ignored)
run_stage "LLVM toolchain pin" buck2 test //:gate-native-toolchain-pin --
# They link AArch64 programs with the cross gcc and run them under qemu-user.
run_stage "LLVM backend tests" buck2 test //:hirsel-llvm-unittest //:hirsel-llvm-test-it -- $(libtest --include-ignored)
run_stage "LLVM scalar library matrix against the interpreter's kernels" \
  buck2 test //:hirsel-pipeline-tests-test-front-shard-1 -- $(libtest llvm_scalar_library:: --include-ignored)
run_stage "Lean RC model audit" proofs/rc/audit.sh
run_stage "Lean RC model vectors" proofs/rc/check-vectors.sh
run_stage "TLA+ ordered-mode model" proofs/ordered/check.sh
mapfile -t driver < <(buck2 uquery 'attrregexfilter(name, "^hirsel-driver-test-it(-shard-[0-9]+)?$", //...)')
run_stage "native stages end to end: full matrix on both targets and 22 link partitions" \
  buck2 test "${release[@]}" "${driver[@]}" -- \
  $(libtest native:: --include-ignored --test-threads=4)
run_stage "native smoke with real Io record and replay" buck2 test //:gate-native-smoke --
# Host execution proves production Ordered widths 1/2/4/8; the pool executor
# otherwise clamps wider cells to its two CPUs.
run_stage "Ordered qualification: native trace, immutable-input replay and topology" \
  buck2 test -c hirsel.tests=host //:hirsel-cli-test-cli-shard-1 -- $(libtest ordered_qualification:: runtime50_ordered:: --ignored --test-threads=1)
run_stage "native executables under Valgrind memcheck" buck2 test //:gate-native-sanitize --
if (( ${#failed_stages[@]} > 0 )); then
  printf 'check-native.sh: %d failing stages: %s\n' "${#failed_stages[@]}" "$(IFS=';'; echo "${failed_stages[*]}")" >&2
  echo "check-native.sh: failed ($((SECONDS - gate_start)) s)" >&2
  exit "$first_status"
fi
echo "check-native.sh: passed ($((SECONDS - gate_start)) s)"
