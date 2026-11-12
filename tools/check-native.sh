#!/usr/bin/env bash
# Native support checks: runtime/C harnesses, layout, LLVM and concurrency models.
# tools/main-runs runs the retained reference/native/Rust component matrix.
# Buck2 runs native tests, C harnesses and toolchain checks on the pool with
# pinned inputs. The TLA+ model check uses the invoking host.
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
filtered() { tools/test-filter "$@"; }
mapfile -t runtime < <(buck2 uquery 'kind("_rust_test", attrregexfilter(name, "^hirsel-rt-", //...))')
run_stage "runtime tests" buck2 test "${runtime[@]}" --
run_stage "runtime C harness" buck2 test //:gate-native-runtime-c --
run_stage "library C probes" buck2 test //:gate-native-library-c --
run_stage "layout C probes" filtered //:hirsel-lir-test-it -- layout::c_probes_agree_on_both_targets --exact --include-ignored
run_stage "LLVM toolchain pin" buck2 test //:gate-native-toolchain-pin --
# They link AArch64 programs with the cross gcc and run them under qemu-user.
run_stage "LLVM backend tests" buck2 test //:hirsel-llvm-unittest //:hirsel-llvm-test-it -- $(libtest --include-ignored)
run_stage "TLA+ ordered-mode model" proofs/ordered/check.sh
run_stage "free-mode GenMC models" buck2 test //:gate-free-genmc --
if (( ${#failed_stages[@]} > 0 )); then
  printf 'check-native.sh: %d failing stages: %s\n' "${#failed_stages[@]}" "$(IFS=';'; echo "${failed_stages[*]}")" >&2
  echo "check-native.sh: failed ($((SECONDS - gate_start)) s)" >&2
  exit "$first_status"
fi
echo "check-native.sh: passed ($((SECONDS - gate_start)) s)"
