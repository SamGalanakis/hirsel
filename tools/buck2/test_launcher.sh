#!/bin/sh
# Run one libtest harness (or gate command) as `cargo test` would: from its
# package directory (HIRSEL_TEST_CWD), with an existing CARGO_TARGET_TMPDIR,
# the pinned LLVM as LLVM_SYS_231_PREFIX (its bin first on PATH, for tests that
# run `clang` by name, and its lib on the loader path), and test threads and campaign workers bounded by the action's CPU share (at
# most 4).
set -eu
if [ -n "${HIRSEL_TEST_REPORTER:-}" ] && [ -z "${HIRSEL_TEST_REPORT_ACTIVE:-}" ]; then
  exec python3 "$HIRSEL_TEST_REPORTER" sh "$0" "$@"
fi
# The dedicated Exec laws use no network and need the host's memfd execution policy.
# Other tests run in a network namespace whose only interface is lo, so only
# the test reaches the loopback ports it listens on. Programs on a developer
# host probe new loopback listeners (one connects and sends `GET /` to each),
# which stopped test servers and added foreign requests to what tests
# observed. A pool worker's action sandbox already is such a namespace, and
# forbids nesting another. Elsewhere the launcher enters one through bwrap:
# where AppArmor restricts unprivileged user namespaces, only bwrap's profile
# may bring up lo. The namespace is read from /proc/self/net, which follows
# the calling process; /sys/class/net shows whoever mounted sysfs.
only_lo() {
  [ "$(awk 'NR > 2 { sub(":", "", $1); print $1 }' /proc/self/net/dev)" = lo ]
}
# HIRSEL_TEST_SMALL_TMPFS=BYTES asks for a tmpfs of that size, mounted in the same
# bwrap namespace at HIRSEL_TEST_SMALL_TMPFS_DIR, for laws that need a nearly full
# filesystem. bwrap cannot nest, so such a test runs on the host, never the pool.
if [ -z "${HIRSEL_TEST_NATIVE_EXEC:-}" ] && { ! only_lo || [ -n "${HIRSEL_TEST_SMALL_TMPFS:-}" ]; }; then
  # The marker only stops a second entry: the namespace itself decides.
  if [ -n "${HIRSEL_TEST_NETNS_ENTERED:-}" ]; then
    if ! only_lo; then
      echo "test_launcher.sh: bwrap --unshare-net did not give a namespace with only lo" >&2
      exit 2
    fi
  else
    if ! command -v bwrap >/dev/null 2>&1; then
      echo "test_launcher.sh: tests run in a network namespace with only lo; this one has other interfaces and bwrap (bubblewrap) is not on PATH to enter one" >&2
      exit 2
    fi
    if [ -z "${HIRSEL_TEST_SMALL_TMPFS:-}" ]; then
      HIRSEL_TEST_NETNS_ENTERED=1 exec bwrap --unshare-net --dev-bind / / --die-with-parent sh "$0" "$@"
    fi
    small_tmpfs="$(mktemp -d "${TMPDIR:-/tmp}/hirsel-small-tmpfs.XXXXXX")"
    status=0
    HIRSEL_TEST_NETNS_ENTERED=1 HIRSEL_TEST_SMALL_TMPFS_DIR="$small_tmpfs" bwrap --unshare-net --dev-bind / / --die-with-parent \
      --size "$HIRSEL_TEST_SMALL_TMPFS" --tmpfs "$small_tmpfs" sh "$0" "$@" || status=$?
    rmdir "$small_tmpfs"
    exit "$status"
  fi
fi
unset HIRSEL_TEST_NETNS_ENTERED
root="$(pwd)"
# Keep SDK copies on the executor's original scratch filesystem, before the
# recording tests select their ancestry-safe /dev/shm TMPDIR.
native_inputs_tmp="${TMPDIR:-/tmp}"
scratch_owner="$(realpath "${HIRSEL_SCRATCH_OWNER:-$root/tools/buck2/scratch.py}")"
scratch=""
action_cache=""
private_native_inputs=""
cleanup_scratch() {
  status=$?
  keep_scratch=""
  if [ "$status" -ne 0 ]; then
    keep_scratch="${HIRSEL_KEEP_TEST_SCRATCH:-}"
  fi
  for owned in "$private_native_inputs" "$action_cache" "$scratch"; do
    [ -n "$owned" ] || continue
    if python3 "$scratch_owner" release "$owned" '' "$$" "$keep_scratch"; then
      [ -z "$keep_scratch" ] || printf 'Kept failed test scratch at %s\n' "$owned" >&2
    fi
  done
}
trap cleanup_scratch 0
trap 'exit 129' HUP
trap 'exit 130' INT
trap 'exit 143' TERM
if [ -n "${HIRSEL_TEST_POOL:-}" ]; then
  # A pool action's work directory is a bind mount of a subdirectory. The
  # action's private /dev tmpfs is mounted at its root: scratch directories
  # and TMPDIR live there.
  action_root=/dev/shm
  scratch="$(python3 "$scratch_owner" create "$action_root" hirsel-test- "$$" '')"
  mkdir -p "$scratch/target-tmpdir" "$(dirname "$HIRSEL_TEST_TMPDIR")"
  rm -rf "$HIRSEL_TEST_TMPDIR"
  ln -s "$scratch/target-tmpdir" "$HIRSEL_TEST_TMPDIR"
else
  # On this host, scratch lives outside the project: Buck2 watches the project
  # tree and stops at a file name that is not UTF-8, which native launch tests
  # create on purpose. One directory per checkout, linked in atomically, since
  # host tests run concurrently. Leases retain the target until the last
  # launcher exits. Replace a pre-link directory by rename before removing it.
  action_root="$native_inputs_tmp"
  scratch="$(python3 "$scratch_owner" create "$action_root" hirsel-test- "$$" '' "$(printf %s "$root" | cksum | cut -d' ' -f1)")"
  mkdir -p "$scratch" "$(dirname "$HIRSEL_TEST_TMPDIR")"
  until [ "$(readlink "$HIRSEL_TEST_TMPDIR" || true)" = "$scratch" ]; do
    if [ -d "$HIRSEL_TEST_TMPDIR" ] && [ ! -L "$HIRSEL_TEST_TMPDIR" ] &&
      mv -T "$HIRSEL_TEST_TMPDIR" "$HIRSEL_TEST_TMPDIR.stale.$$" 2>/dev/null; then
      rm -rf "$HIRSEL_TEST_TMPDIR.stale.$$"
    fi
    ln -sfn "$scratch" "$HIRSEL_TEST_TMPDIR.$$"
    mv -T "$HIRSEL_TEST_TMPDIR.$$" "$HIRSEL_TEST_TMPDIR" 2>/dev/null || rm -f "$HIRSEL_TEST_TMPDIR.$$"
  done
fi
# Put even anonymous tempfile directories under an owned action directory.
# The shared checkout symlink above only serves compile-time CARGO_TARGET_TMPDIR.
action_cache="$(python3 "$scratch_owner" create "$action_root" hirsel-action- "$$" '')"
native_input_materializer=""
export TMPDIR="$action_cache/tmp"
export XDG_CACHE_HOME="$action_cache/cache"
export XDG_DATA_HOME="$action_cache/data"
mkdir -m 700 "$TMPDIR" "$XDG_CACHE_HOME"
if [ -n "${LLVM_SYS_231_PREFIX:-}" ]; then
  LLVM_SYS_231_PREFIX="$(cd "$LLVM_SYS_231_PREFIX" && pwd)"
  export LLVM_SYS_231_PREFIX
fi
if [ -n "${HIRSEL_NATIVE_ENVIRONMENT:-}" ]; then
  # The shared CAS inode's ctime may change when another action creates or
  # removes a hard link. Detach every tool/shared-library file before strict
  # native hashing; preserve private aliases and the existing input bytes.
  native_input_materializer="$(realpath "${HIRSEL_PRIVATE_NATIVE_INPUTS:?missing native input materializer}")"
  unset HIRSEL_PRIVATE_NATIVE_INPUTS
  # Each row of tools/buck2/native-toolchain/environment.tsv is a variable, the
  # toolchain whose tree HIRSEL_TOOLCHAIN_<TARGET> names, and a path inside it.
  trees=""
  while IFS="$(printf '\t')" read -r variable target path; do
    case "$variable" in "" | "#"*) continue ;; esac
    name="HIRSEL_TOOLCHAIN_$(printf '%s' "$target" | tr '[:lower:]-' '[:upper:]_')"
    eval "tree=\${$name}"
    export "$variable=$(cd "$tree" && pwd)/$path"
    trees="$trees $name"
  done < "$HIRSEL_NATIVE_ENVIRONMENT"
  unset HIRSEL_NATIVE_ENVIRONMENT $trees
fi
# Listing and empty shards never execute native work, so do not copy an SDK.
materialize_native_inputs() {
  [ -n "$native_input_materializer" ] || return 0
  for argument in "$@"; do
    case "$argument" in --list | --help | -h) return 0 ;; esac
  done
  private_native_inputs="$(python3 "$scratch_owner" create "$native_inputs_tmp" hirsel-native-inputs- "$$" '')"
  LLVM_SYS_231_PREFIX="$(python3 "$native_input_materializer" "$LLVM_SYS_231_PREFIX" "$private_native_inputs/llvm")"
  export LLVM_SYS_231_PREFIX
  export PATH="$LLVM_SYS_231_PREFIX/bin:$PATH"
  export LD_LIBRARY_PATH="$LLVM_SYS_231_PREFIX/lib${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
}
if [ -n "${LLVM_SYS_231_PREFIX:-}" ]; then
  export PATH="$LLVM_SYS_231_PREFIX/bin:${PATH:-/usr/bin:/bin}"
  export LD_LIBRARY_PATH="$LLVM_SYS_231_PREFIX/lib${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
fi
# defs.bzl passes the executor's effective CPU reservation explicitly: a
# worker's cgroup files need not be visible inside its action sandbox. Never
# infer a pool reservation from the worker host's CPU count.
jobs="${HIRSEL_TEST_CPUS:-}"
case "$jobs" in
  "")
    if [ -n "${HIRSEL_TEST_POOL:-}" ]; then
      echo "test_launcher.sh: pool action requires HIRSEL_TEST_CPUS" >&2
      exit 2
    fi
    jobs="$(nproc 2>/dev/null || echo 1)" ;;
  *[!0-9]* | 0) echo "test_launcher.sh: invalid HIRSEL_TEST_CPUS=$jobs" >&2; exit 2 ;;
esac
# A visible, tighter cgroup quota and caller settings can only reduce it.
[ "$jobs" -le 4 ] || jobs=4
cgroup="$(sed -n 's/^0:://p' /proc/self/cgroup 2>/dev/null)" || cgroup=""
while :; do
  if read -r quota period 2>/dev/null < "/sys/fs/cgroup${cgroup%/}/cpu.max" && [ "$quota" != max ]; then
    cpus=$((quota / period))
    [ "$cpus" -ge 1 ] || cpus=1
    [ "$cpus" -ge "$jobs" ] || jobs=$cpus
  fi
  [ -n "$cgroup" ] && [ "$cgroup" != / ] || break
  cgroup="${cgroup%/*}"
done
# The run's memory grant (defs.bzl) caps its data before anything runs, so
# scripts and the native programs a test launches are capped too; hirsel
# binaries set the same limit again and name it when they reach it. The limit
# is soft: only a sanitizer's child lifts it (AddressSanitizer maps terabytes
# of shadow address space), and the pool's reservation still bounds that run.
if [ -n "${HIRSEL_MEMORY_LIMIT:-}" ]; then
  case "$HIRSEL_MEMORY_LIMIT" in
    *K) limit_kb="${HIRSEL_MEMORY_LIMIT%K}" ;;
    *M) limit_kb="$((${HIRSEL_MEMORY_LIMIT%M} * 1024))" ;;
    *G) limit_kb="$((${HIRSEL_MEMORY_LIMIT%G} * 1024 * 1024))" ;;
    *) limit_kb="$((HIRSEL_MEMORY_LIMIT / 1024))" ;;
  esac
  ulimit -S -d "$limit_kb"
fi
RUST_TEST_THREADS="${RUST_TEST_THREADS:-$jobs}" HIRSEL_JOBS="${HIRSEL_JOBS:-$jobs}"
[ "$RUST_TEST_THREADS" -le "$jobs" ] || RUST_TEST_THREADS=$jobs
[ "$HIRSEL_JOBS" -le "$jobs" ] || HIRSEL_JOBS=$jobs
export RUST_TEST_THREADS HIRSEL_JOBS
harness="$root/$1"
shift
# Value-taking options from the pinned libtest --help. Keep values quoted,
# even when they contain spaces or look like flags.
takes_value() {
  case "$1" in
    --logfile | --test-threads | --skip | --color | --format | --shuffle-seed | -Z) return 0 ;;
    *) return 1 ;;
  esac
}
# libtest's command-line setting overrides RUST_TEST_THREADS. Bound both
# spellings too, preserving all other arguments and literal filters after --.
remaining=$#
literal=false
unset requested_logfile ignore_mode
while [ "$remaining" -gt 0 ]; do
  argument=$1; shift; remaining=$((remaining - 1))
  if "$literal"; then set -- "$@" "$argument"; continue; fi
  case "$argument" in
    --) literal=true ;;
    --logfile | --logfile=*)
      if [ -n "${HIRSEL_TEST_FILTER_CASES:-}" ]; then
        [ "${requested_logfile+x}" != x ] || { echo "duplicate --logfile" >&2; exit 2; }
        if [ "$argument" = --logfile ]; then
          [ "$remaining" -gt 0 ] || { echo "$argument requires a value" >&2; exit 2; }
          requested_logfile=$1; shift; remaining=$((remaining - 1))
        else
          requested_logfile=${argument#*=}
        fi
        continue
      fi ;;
    --ignored | --include-ignored)
      # A target that bakes an ignore mode keeps it; libtest refuses a second.
      # Filtered execution checks the completed bodies against the dealt names.
      if [ -n "${HIRSEL_TEST_FILTER_CASES:-}" ]; then
        [ "${ignore_mode+x}" != x ] || continue
        ignore_mode=$argument
      fi ;;
    --test-threads | --test-threads=*)
      case "$argument" in
        --test-threads)
          [ "$remaining" -gt 0 ] || { echo "$argument requires a value" >&2; exit 2; }
          threads=$1; shift; remaining=$((remaining - 1)) ;;
        *) threads=${argument#*=} ;;
      esac
      case "$threads" in
        "" | *[!0-9]* | 0) echo "test_launcher.sh: invalid --test-threads=$threads" >&2; exit 2 ;;
      esac
      [ "$threads" -le "$jobs" ] || threads=$jobs
      set -- "$@" --test-threads "$threads"
      continue ;;
  esac
  set -- "$@" "$argument"
  if takes_value "$argument" && [ "$remaining" -gt 0 ]; then
    value=$1; shift; remaining=$((remaining - 1)); set -- "$@" "$value"
  fi
done
if [ -n "${HIRSEL_TEST_BIN_EXES:-}" ]; then
  # The harness names the package's binaries (CARGO_BIN_EXE_<name>) under
  # HIRSEL_TEST_BIN_EXE_DIR, so its compile never waits for them
  # (tools/buck2/defs.bzl). Link those names to this run's binaries. Shards on
  # the host share the directory: each replaces a link by an atomic rename.
  mkdir -p "$HIRSEL_TEST_BIN_EXE_DIR"
  for entry in $HIRSEL_TEST_BIN_EXES; do
    link="$HIRSEL_TEST_BIN_EXE_DIR/${entry%%=*}"
    ln -s "$root/${entry#*=}" "$link.$$"
    mv -f "$link.$$" "$link"
  done
fi
cd "$HIRSEL_TEST_CWD"
# Only the selector can exempt an intentional empty partition from body counts.
report_empty_partition() {
  if [ -n "${HIRSEL_TEST_REPORT_ACTIVE:-}" ] && ! "$discovery"; then
    printf 'HIRSEL_TEST_EMPTY_PARTITION %s\n' "$test_binary"
  fi
}
# Only Rust harness runs use the libtest event protocol. Discovery and help
# retain their own formats, and scripts keep their original argument surface.
run_harness() (
  case "$harness" in
    *.py) exec python3 "$harness" "$@" ;;
  esac
  if [ -n "$test_binary" ] && [ -n "${HIRSEL_TEST_REPORT_ACTIVE:-}" ]; then
    for argument in "$@"; do
      case "$argument" in --list | --help | -h) "$harness" "$@"; exit $? ;; esac
    done
    remaining=$#
    literal=false
    set -- "$@" -Zunstable-options --format=json
    while [ "$remaining" -gt 0 ]; do
      argument=$1; shift; remaining=$((remaining - 1))
      if "$literal"; then set -- "$@" "$argument"; continue; fi
      case "$argument" in
        --) literal=true; set -- "$@" -- ;;
        --format=* | --quiet | -q | -Zunstable-options) ;;
        -Z)
          [ "$remaining" -gt 0 ] || { echo "$argument requires a value" >&2; exit 2; }
          value=$1; shift; remaining=$((remaining - 1))
          [ "$value" = unstable-options ] || set -- "$@" -Z "$value" ;;
        --format)
          [ "$remaining" -gt 0 ] || { echo "$argument requires a value" >&2; exit 2; }
          shift; remaining=$((remaining - 1)) ;;
        *)
          set -- "$@" "$argument"
          if takes_value "$argument" && [ "$remaining" -gt 0 ]; then
            value=$1; shift; remaining=$((remaining - 1)); set -- "$@" "$value"
          fi ;;
      esac
    done
    export RUSTC_BOOTSTRAP=1
  fi
  if [ -z "${HIRSEL_TEST_FILTER_CASES:-}" ]; then
    "$harness" "$@"
    exit $?
  fi
  # libtest writes results directly to this file, even with --nocapture.
  expected="$(printf '%s\n' "$HIRSEL_TEST_FILTER_CASES" |
    awk -v target="$TEST_TARGET" '$1 == target {print $2}' | LC_ALL=C sort)"
  unset HIRSEL_TEST_FILTER_CASES
  logfile="$action_cache/libtest-results"
  status=0
  "$harness" --logfile "$logfile" "$@" || status=$?
  if [ "${requested_logfile+x}" = x ] && [ -f "$logfile" ]; then
    cat "$logfile" > "$requested_logfile"
  fi
  [ "$status" -eq 0 ] || exit "$status"
  completed="$(sed -n 's/^ok //p' "$logfile" | LC_ALL=C sort)"
  if [ -z "$expected" ] || [ "$completed" != "$expected" ]; then
    printf 'test_launcher.sh: %s expected bodies:\n%s\ncompleted bodies:\n%s\n' \
      "$TEST_TARGET" "$expected" "$completed" >&2
    exit 2
  fi
)
test_binary="${HIRSEL_TEST_BINARY:-}"
unset HIRSEL_TEST_TMPDIR HIRSEL_TEST_CWD HIRSEL_TEST_BIN_EXES HIRSEL_TEST_BIN_EXE_DIR HIRSEL_TEST_POOL
unset HIRSEL_TEST_BINARY
set -f # Rust case names and declared patterns are words, never shell globs.
list_cases() (
  remaining=$#
  set -- "$@" --list --format terse --color never
  literal=false
  exact=false
  while [ "$remaining" -gt 0 ]; do
    argument=$1; shift; remaining=$((remaining - 1))
    if "$literal"; then set -- "$@" "$argument"; continue; fi
    case "$argument" in
      --) literal=true; set -- "$@" -- ;;
      --exact)
        if ! "$exact"; then set -- "$@" --exact; exact=true; fi ;;
      --list | --quiet | -q | --format=* | --color=* | --logfile=*) ;;
      --format | --color | --logfile)
        [ "$remaining" -gt 0 ] || { echo "$argument requires a value" >&2; exit 2; }
        shift; remaining=$((remaining - 1)) ;;
      *)
        set -- "$@" "$argument"
        if takes_value "$argument"; then
          [ "$remaining" -gt 0 ] || { echo "$argument requires a value" >&2; exit 2; }
          value=$1; shift; remaining=$((remaining - 1)); set -- "$@" "$value"
        fi ;;
    esac
  done
  listing="$("$harness" "$@")" || exit $?
  printf '%s\n' "$listing" | sed -n 's/: test$//p' | LC_ALL=C sort
)
# --ignored can legitimately select only host-disabled bodies. Distinguish
# that skip from a name absent from the binary, using libtest's full listing.
selection_exists() (
  remaining=$#
  literal=false
  while [ "$remaining" -gt 0 ]; do
    argument=$1; shift; remaining=$((remaining - 1))
    if "$literal"; then set -- "$@" "$argument"; continue; fi
    case "$argument" in
      --) literal=true ;;
      --ignored) continue ;;
    esac
    set -- "$@" "$argument"
    if takes_value "$argument" && [ "$remaining" -gt 0 ]; then
      value=$1; shift; remaining=$((remaining - 1)); set -- "$@" "$value"
    fi
  done
  [ -n "$(list_cases "$@")" ]
)
# Discover positive selectors before deciding which shard owns them. A stale
# selector fails on every shard, rather than becoming an empty exact filter.
filtered=false
restricted=false
skip=false
literal=false
discovery=false
for argument in "$@"; do
  if "$skip"; then skip=false; continue; fi
  if "$literal"; then filtered=true; continue; fi
  case "$argument" in
    --) literal=true ;;
    --list | --help | -h) discovery=true ;;
    --skip) restricted=true; skip=true ;;
    --skip=*) restricted=true ;;
    -*) if takes_value "$argument"; then skip=true; fi ;;
    *) filtered=true ;;
  esac
done
if "$discovery" && [ -n "${HIRSEL_TEST_FILTER_CASES:-}" ]; then
  echo "test_launcher.sh: filtered execution requires test bodies" >&2
  exit 2
fi
if "$discovery" && [ -z "${HIRSEL_TEST_HOST_CASES:-}" ] && [ -z "${HIRSEL_TEST_DEDICATED_CASE:-}" ]; then run_harness "$@"; exit $?; fi
if [ -z "${HIRSEL_TEST_HOST_CASES:-}" ] && [ -z "${HIRSEL_TEST_DEDICATED_CASE:-}" ]; then
  if [ -n "$test_binary" ] && { "$filtered" || "$restricted"; }; then
    cases="$(list_cases "$@")"
    if [ -z "$cases" ] && ! selection_exists "$@"; then
      printf 'test_launcher.sh: selector [%s] matched no test bodies\n' "$*" >&2
      exit 2
    fi
  fi
  materialize_native_inputs "$@"; run_harness "$@"; exit $?
fi
cases="$(list_cases "$@")"
if [ -z "$cases" ] && { "$filtered" || "$restricted"; } && ! "$discovery" && ! selection_exists "$@"; then
  printf 'test_launcher.sh: selector [%s] matched no test bodies\n' "$*" >&2
  exit 2
fi
if [ -n "${HIRSEL_TEST_DEDICATED_CASE:-}" ]; then
  if ! "$discovery" && [ -z "$(list_cases --exact "$HIRSEL_TEST_DEDICATED_CASE")" ]; then
    printf 'test_launcher.sh: selector [%s] matched no test bodies\n' "$HIRSEL_TEST_DEDICATED_CASE" >&2
    exit 2
  fi
  cases="$(printf '%s\n' "$cases" | awk -v selected="$HIRSEL_TEST_DEDICATED_CASE" '$0 == selected')"
  unset HIRSEL_TEST_HOST_CASES
fi
unset HIRSEL_TEST_DEDICATED_CASE
# Host requirements remain physical ownership under positive filters as well.
if [ -n "${HIRSEL_TEST_HOST_CASES:-}" ]; then
  cases="$(printf '%s\n' "$cases" | awk -v isolated="$HIRSEL_TEST_HOST_CASES" '
    BEGIN { n = split(isolated, names, " "); for (i = 1; i <= n; i++) excluded[names[i]] = 1 }
    !($0 in excluded)')"
fi
unset HIRSEL_TEST_HOST_CASES
if [ -z "$cases" ] && "$filtered" && ! "$discovery"; then
  printf 'test_launcher.sh: selector [%s] matched no test bodies on %s\n' "$*" "$test_binary" >&2
  exit 2
fi
# The caller's positive filters already shaped the listing. Replace them with
# the exact selected names, preserving flags and paired values. In particular,
# an empty partition must use an empty exact filter: no filters would run all.
remaining=$#
if [ -n "$cases" ]; then
  # shellcheck disable=SC2086 # Rust case names contain no spaces; globbing is off.
  set -- "$@" --exact $cases
else
  report_empty_partition
  set -- "$@" --exact ""
fi
literal=false
while [ "$remaining" -gt 0 ]; do
  argument=$1; shift; remaining=$((remaining - 1))
  if "$literal"; then continue; fi
  case "$argument" in
    --) literal=true ;;
    --exact) ;;
    -*)
      set -- "$@" "$argument"
      if takes_value "$argument"; then
        [ "$remaining" -gt 0 ] || { echo "$argument requires a value" >&2; exit 2; }
        value=$1; shift; remaining=$((remaining - 1)); set -- "$@" "$value"
      fi ;;
  esac
done
[ -z "$cases" ] || materialize_native_inputs "$@"
run_harness "$@"
