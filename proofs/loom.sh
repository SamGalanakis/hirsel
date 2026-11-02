#!/usr/bin/env bash
# Shared loom-model runs for proofs/*/check.sh: a "pass" verdict needs cargo exit 0 with at
# least one model run (a vacuous pass is a failure), and an expected failure must be a named
# panic — a nonzero exit whose output carries the row's panic text and a FAILED test result
# (a build error, a missing panic or an unexpected panic is "unknown", which never matches).
#
# Table row, |-separated:
#   name | features | preemptions | test-args | bound | panic | expected
#
#   name         run name, matched by the check.sh name-filter
#   features     cargo test args before the test args, e.g. "--test models --features weak"
#   preemptions  the LOOM_MAX_PREEMPTIONS value; empty leaves loom's default
#   test-args    extra args after the features, e.g. "-- --exact every_cut_is_causally_closed"
#   bound        the exact passed-model count a pass run must reach; empty means at least 1
#   panic        fixed string the output must contain for a non-pass verdict (the named panic)
#   expected     "pass", or the failure label the run must reach ("fail", "causality", ...)
#
# loom_run_rows DIR FILTER runs every row of the caller's `loom_runs` whose name matches
# FILTER, prints one line per run, and counts into the globals `total` and `failed`, which
# the caller initialises and reports.
loom_run_rows() {
  local dir="$1" filter="$2" row
  local name features preemptions test_args bound panic expected
  local out code passed verdict result
  for row in "${loom_runs[@]}"; do
    IFS='|' read -r name features preemptions test_args bound panic expected <<<"$row"
    [[ "$name" =~ $filter ]] || continue
    total=$((total + 1))
    local loom_env=(
      "CARGO_TARGET_DIR=${CARGO_TARGET_DIR:-$dir/target}"
      "LOOM_MAX_BRANCHES=1000"
    )
    if [[ -n "$preemptions" ]]; then
      loom_env+=("LOOM_MAX_PREEMPTIONS=$preemptions")
    fi
    set +e
    # shellcheck disable=SC2086
    out="$(cd "$dir" && env "${loom_env[@]}" cargo test --release --locked --offline $features $test_args 2>&1)"
    code=$?
    set -e
    passed="$( (grep -oE '[0-9]+ passed' <<<"$out" || true) | awk '{n += $1} END {print n + 0}')"
    if [[ $code -eq 0 && $passed -gt 0 ]] && [[ -z $bound || $passed -eq $bound ]]; then
      verdict=pass
    elif [[ $expected != pass && $code -ne 0 && -n $panic ]] &&
         grep -Fq "$panic" <<<"$out" && grep -Fq 'test result: FAILED' <<<"$out"; then
      verdict="$expected"
    else
      verdict=unknown
    fi
    if [[ "$verdict" == "$expected" ]]; then
      result="ok ($verdict, $passed models passed)"
    else
      result="FAILED ($verdict)"
    fi
    printf '%-44s %-8s %s\n' "$name" "$expected" "$result"
    if [[ "$result" != ok* ]]; then
      failed=$((failed + 1))
      printf '%s\n' "$out" | tail -40 | sed 's/^/    /'
    fi
  done
}
