#!/usr/bin/env python3
"""Reproducible ASan+LSan, TSan, and Miri sweep across Hirsel core crates.

Target crates:
  - hirsel-pack (lib + pack_export_abi)
  - hirsel-core-rt (lib, with stackful fiber ASan/TSan annotations)
  - hirsel-core-net (lib)
  - hirsel-core-driver (lib pure-Rust/concurrency subsets + cli_dx, mimalloc off)

All runs use `--locked` and never touch `rust-toolchain.toml` or `~/.cargo`.
Exits 75 (`EX_TEMPFAIL`) on `--check-toolchain` or `--defer-if-missing` when
`+nightly`, `rust-src`, or `miri` is absent on the host so standing board runs
can defer cleanly instead of false-failing.
"""

from __future__ import annotations

import argparse
from dataclasses import dataclass
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tempfile
import time

ROOT = Path(__file__).resolve().parents[2]
LLVM_ENGINE = ROOT / "tools/buck2/toolchains/llvm-files/engine/libLLVM.so.23.1"
TARGET_TRIPLE = "x86_64-unknown-linux-gnu"
DEFAULT_CRATES = (
    "hirsel-core-net",
    "hirsel-pack",
    "hirsel-core-rt",
    "hirsel-core-driver",
)
EX_DEFERRED = 75

RESULT_RE = re.compile(
    r"^test result: (?:ok|FAILED)\. "
    r"(?P<passed>\d+) passed; (?P<failed>\d+) failed; (?P<ignored>\d+) ignored; "
    r"\d+ measured; (?P<filtered>\d+) filtered out",
    re.MULTILINE,
)


@dataclass(frozen=True)
class Exclusion:
    pattern: str
    category: str
    reason: str


@dataclass(frozen=True)
class Suite:
    tool: str
    crate: str
    name: str
    package: str
    target_args: tuple[str, ...]
    features: tuple[str, ...] = ()
    no_default_features: bool = False
    filters: tuple[str, ...] = ()
    exclusions: tuple[Exclusion, ...] = ()
    extra_test_args: tuple[str, ...] = ()
    needs_llvm_symlink: bool = False
    detect_leaks: bool = True
    quarantine_mb: int = 16
    cwd_rel: str = "."
    timeout_s: int = 600


SUITES: tuple[Suite, ...] = (
    # ---------------- ASan + LSan ----------------
    Suite(
        tool="asan",
        crate="hirsel-core-net",
        name="hirsel-core-net::lib",
        package="hirsel-core-net",
        target_args=("--lib",),
        extra_test_args=("--test-threads=1",),
        exclusions=(
            Exclusion(
                "a_ring_dropped_with_notifications_owed_keeps_their_bytes",
                "intentional-leak",
                "Intentionally tests sendzc Table::drop leaking kernel-owned DMA buffers.",
            ),
            Exclusion(
                "replaced_watch_leaves_one_poll",
                "sanitizer-timing-artifact",
                "io_uring task_work timing race sensitive to ASan instrumentation overhead.",
            ),
        ),
    ),
    Suite(
        tool="asan",
        crate="hirsel-pack",
        name="hirsel-pack::lib",
        package="hirsel-pack",
        target_args=("--lib",),
        features=("hirsel-core-rt/sanitizer-asan",),
    ),
    Suite(
        tool="asan",
        crate="hirsel-pack",
        name="hirsel-pack::pack_export_abi",
        package="hirsel-pack",
        target_args=("--test", "pack_export_abi"),
        features=("hirsel-core-rt/sanitizer-asan",),
        needs_llvm_symlink=True,
    ),
    Suite(
        tool="asan",
        crate="hirsel-core-rt",
        name="hirsel-core-rt::lib (LSan on leak-free core unsafe suites)",
        package="hirsel-core-rt",
        target_args=("--lib",),
        features=("sanitizer-asan",),
        filters=(
            "isolate_tests::",
            "causes_tests::",
            "region_tests::",
            "counted_tests::",
            "compact_tests::",
            "steps::",
            "limit::bound_tests::",
            "limit::refund_tests::",
            "peak::",
            "retention::",
            "embed_stop_tests::room_check_prevents_guard_hit_and_growth_fault_in_lock",
        ),
        extra_test_args=("--test-threads=1",),
        detect_leaks=True,
    ),
    Suite(
        tool="asan",
        crate="hirsel-core-rt",
        name="hirsel-core-rt::lib (full runtime + fiber sweep, LSan off for intentional Box::leak/stop tests)",
        package="hirsel-core-rt",
        target_args=("--lib",),
        features=("sanitizer-asan",),
        detect_leaks=False,
        exclusions=(
            Exclusion(
                "host_file_tests::run_stop_skips_host_file_cleanup",
                "sanitizer-stderr-artifact",
                "ASan writes __asan_handle_no_return warning to stderr before _exit.",
            ),
            Exclusion(
                "fiber::stop_tests::host_stops_have_exact_names_and_skip_cleanup",
                "sanitizer-stderr-artifact",
                "ASan writes __asan_handle_no_return warning to stderr before _exit.",
            ),
            Exclusion(
                "allocator::stop_tests::fail15_native_allocator_refusal_is_a_reported_run_stop_without_cleanup",
                "sanitizer-rlimit-artifact",
                "RLIMIT_AS=0 fails ASan internal malloc before try_allocate_raw.",
            ),
            Exclusion(
                "task_heap::tests::emptied_dedicated_pages_stay_resident_within_the_peak_until_idle",
                "sanitizer-rss-vma-artifact",
                "RSS delta invalidated by ASan shadow memory and quarantine.",
            ),
            Exclusion(
                "embed_stop_tests::room_check_prevents_guard_hit_and_growth_fault_in_lock",
                "parallel-global-counter",
                "Checks process-global fiber::guard_hits()==0; covered in single-threaded LSan suite.",
            ),
            Exclusion(
                "task_heap::tests::task_heap_vma_count_stays_logarithmic_under_small_and_large_churn",
                "sanitizer-rss-vma-artifact",
                "VMA count inflated by ASan shadow mappings.",
            ),
            Exclusion(
                "task_heap::tests::task_heap_vma_count_stays_logarithmic_at_hundreds_of_mb",
                "sanitizer-rss-vma-artifact",
                "VMA count inflated by ASan shadow mappings.",
            ),
            Exclusion(
                "tests::repeated_sequence_growth_keeps_peak_rss_bounded",
                "sanitizer-rss-vma-artifact",
                "Peak RSS assertion invalidated by ASan quarantine.",
            ),
            Exclusion(
                "tests::short_idle_wakes_keep_a_busy_reader_stack",
                "sanitizer-rss-vma-artifact",
                "Timing/RSS stack residency artifact under ASan.",
            ),
            Exclusion(
                "units::memory::tests::shared_retention_cross_stock_eviction_and_idle_purge",
                "sanitizer-rss-vma-artifact",
                "ASan stack frame expansion invalidates exact fiber::COMMIT retention bound.",
            ),
            Exclusion(
                "tasks_tests::guarded_stacks_report_102",
                "sanitizer-signal-artifact",
                "ASan intercepts intentional stack-guard SIGSEGV in child process.",
            ),
            Exclusion(
                "units::plan::tests::plan_derivation_and_envelope_refusal",
                "sanitizer-rlimit-artifact",
                "RLIMIT_NPROC child spawn interacts with ASan thread runtime.",
            ),
        ),
    ),
    Suite(
        tool="asan",
        crate="hirsel-core-driver",
        name="hirsel-core-driver::cli_dx",
        package="hirsel-core-driver",
        target_args=("--test", "cli_dx"),
        no_default_features=True,
        needs_llvm_symlink=True,
        cwd_rel="crates/hirsel-core-driver",
    ),
    Suite(
        tool="asan",
        crate="hirsel-core-driver",
        name="hirsel-core-driver::lib (store, resident, code_store, manifests, cli_tools)",
        package="hirsel-core-driver",
        target_args=("--lib",),
        no_default_features=True,
        needs_llvm_symlink=True,
        filters=(
            "store::",
            "resident::",
            "code_store::",
            "image_index::",
            "release_manifest::",
            "source_delta::",
            "cli_tools::",
            "describe::",
            "reuse::",
        ),
        exclusions=(
            Exclusion(
                "store::collect::tests::concurrent_parts_are_whole_or_miss_during_eviction",
                "host-tool-dep",
                "Requires Buck2-provisioned clang/runtime fixture paths.",
            ),
            Exclusion(
                "store::tests::opening_removes_leftover_temporaries_and_keeps_live_ones",
                "host-tool-dep",
                "Requires Buck2-provisioned store fixture paths.",
            ),
            Exclusion(
                "code_admission::code_store::m8_bounds_and_releases_every_reservation",
                "intentional-leak",
                "Intentionally calls std::mem::forget(reservation) at code_store.rs:957 to simulate crash recovery.",
            ),
        ),
    ),
    # ---------------- TSan ----------------
    Suite(
        tool="tsan",
        crate="hirsel-core-net",
        name="hirsel-core-net::lib",
        package="hirsel-core-net",
        target_args=("--lib",),
        extra_test_args=("--test-threads=1",),
        exclusions=(
            Exclusion(
                "replaced_watch_leaves_one_poll",
                "sanitizer-timing-artifact",
                "io_uring task_work timing race sensitive to TSan instrumentation.",
            ),
            Exclusion(
                "reactor::tests::udp_tests::first_receive_into_a_fresh_pool_takes_no_page_faults",
                "sanitizer-rss-vma-artifact",
                "TSan shadow page initialization triggers minor page faults on first touch.",
            ),
        ),
    ),
    Suite(
        tool="tsan",
        crate="hirsel-pack",
        name="hirsel-pack::lib",
        package="hirsel-pack",
        target_args=("--lib",),
        features=("hirsel-core-rt/sanitizer-tsan",),
    ),
    Suite(
        tool="tsan",
        crate="hirsel-pack",
        name="hirsel-pack::pack_export_abi",
        package="hirsel-pack",
        target_args=("--test", "pack_export_abi"),
        features=("hirsel-core-rt/sanitizer-tsan",),
        needs_llvm_symlink=True,
    ),
    Suite(
        tool="tsan",
        crate="hirsel-core-rt",
        name="hirsel-core-rt::lib (full runtime + fiber TSan sweep)",
        package="hirsel-core-rt",
        target_args=("--lib",),
        features=("sanitizer-tsan",),
        exclusions=(
            Exclusion(
                "embed_stop_tests::room_check_prevents_guard_hit_and_growth_fault_in_lock",
                "sanitizer-stack-frame-artifact",
                "TSan frame instrumentation shifts &buf vs fiber::room() stack pointer.",
            ),
            Exclusion(
                "task_heap::tests::multi_unit_task_heaps_keep_vma_count_bounded_across_many_units",
                "sanitizer-rss-vma-artifact",
                "64-unit 256 MiB VMA test exceeds TSan shadow memory/runtime envelope.",
            ),
            Exclusion(
                "task_heap::tests::task_heap_vma_count_stays_logarithmic_under_small_and_large_churn",
                "sanitizer-rss-vma-artifact",
                "VMA count inflated by TSan shadow mappings.",
            ),
            Exclusion(
                "task_heap::tests::task_heap_vma_count_stays_logarithmic_at_hundreds_of_mb",
                "sanitizer-rss-vma-artifact",
                "VMA count inflated by TSan shadow mappings.",
            ),
            Exclusion(
                "task_heap::tests::emptied_dedicated_pages_stay_resident_within_the_peak_until_idle",
                "sanitizer-rss-vma-artifact",
                "RSS delta invalidated by TSan runtime.",
            ),
            Exclusion(
                "tests::repeated_sequence_growth_keeps_peak_rss_bounded",
                "sanitizer-rss-vma-artifact",
                "Peak RSS assertion invalidated by TSan shadow memory.",
            ),
            Exclusion(
                "tests::short_idle_wakes_keep_a_busy_reader_stack",
                "sanitizer-rss-vma-artifact",
                "Timing/RSS stack residency artifact under TSan.",
            ),
            Exclusion(
                "allocator::stop_tests::fail15_native_allocator_refusal_is_a_reported_run_stop_without_cleanup",
                "sanitizer-rlimit-artifact",
                "RLIMIT_AS=0 fails TSan internal allocator before try_allocate_raw.",
            ),
            Exclusion(
                "fiber::stop_tests::host_stops_have_exact_names_and_skip_cleanup",
                "sanitizer-stderr-artifact",
                "TSan runtime diagnostics break exact stderr equality assertion.",
            ),
            Exclusion(
                "units::memory::tests::batched_refill_coalescing_and_cross_unit_stack_scrub",
                "sanitizer-rss-vma-artifact",
                "Cross-unit stack scrub timing/RSS assertion invalidated under TSan.",
            ),
            Exclusion(
                "units::memory::tests::shared_retention_cross_stock_eviction_and_idle_purge",
                "sanitizer-rss-vma-artifact",
                "TSan stack frame expansion invalidates exact fiber::COMMIT retention bound.",
            ),
            Exclusion(
                "provider_s0_tests::s0_os_callbacks_by_identity_match_the_pointer_calls",
                "sanitizer-timing-artifact",
                "Compares wall-clock sleep/cancel callback event logs for exact equality across two runs under TSan instrumentation.",
            ),
            Exclusion(
                "tasks_tests::guarded_stacks_report_102",
                "sanitizer-signal-artifact",
                "TSan exits 66 on intentional stack-guard SIGSEGV before Hirsel handler.",
            ),
        ),
    ),
    Suite(
        tool="tsan",
        crate="hirsel-core-driver",
        name="hirsel-core-driver::cli_dx",
        package="hirsel-core-driver",
        target_args=("--test", "cli_dx"),
        no_default_features=True,
        needs_llvm_symlink=True,
        cwd_rel="crates/hirsel-core-driver",
    ),
    Suite(
        tool="tsan",
        crate="hirsel-core-driver",
        name="hirsel-core-driver::lib (concurrent store, resident, code_store)",
        package="hirsel-core-driver",
        target_args=("--lib",),
        no_default_features=True,
        needs_llvm_symlink=True,
        filters=("store::", "resident::", "code_store::"),
    ),
    # ---------------- Miri (-Zmiri-strict-provenance) ----------------
    Suite(
        tool="miri",
        crate="hirsel-core-net",
        name="hirsel-core-net::lib (pure-Rust grant/class/address/rule suites)",
        package="hirsel-core-net",
        target_args=("--lib",),
        extra_test_args=("--test-threads=1",),
        timeout_s=1800,
        exclusions=(
            Exclusion("reactor::", "syscall-ffi", "io_uring/epoll/socket syscalls unsupported by Miri."),
            Exclusion("agreement_registry::", "ffi", "Calls ring C FFI (OPENSSL_cpuid_setup)."),
            Exclusion("p256_key::", "ffi", "Calls ring C FFI."),
            Exclusion("srtp_cipher::", "ffi", "Calls ring C FFI."),
            Exclusion("key_agreement::", "ffi", "Calls ring C FFI."),
            Exclusion("key_wrap::", "ffi", "Calls ring C FFI and spawns Python verifier."),
            Exclusion("secret_aead::", "ffi", "Calls ring C FFI."),
            Exclusion("packet_cipher::", "ffi", "Calls ring C FFI."),
            Exclusion("packet_sframe::", "ffi", "Calls ring C FFI."),
            Exclusion("tls_kdf::", "ffi", "Calls ring C FFI / OpenSSL verifier."),
            Exclusion("tests::write_half_shutdown_does_not_close_read_owner", "syscall-ffi", "Calls libc::shutdown on socketpair."),
            Exclusion("tests::exec_narrowing_matches_both_grants_and_charges_the_result", "miri-combinatorial-timeout", "20,956 pure-safe BTreeMap intersections (>10 min under Miri; covered by ASan+TSan)."),
        ),
    ),
    Suite(
        tool="miri",
        crate="hirsel-core-rt",
        name="hirsel-core-rt::lib (pure-Rust unsafe modules: isolate, counted, peak, limit, region, steps, retention, mem_sampler)",
        package="hirsel-core-rt",
        target_args=("--lib",),
        timeout_s=1800,
        filters=(
            "steps::",
            "retention::",
            "mem_sampler::",
            "isolate_tests::",
            "counted_tests::",
            "peak::",
            "region_tests::",
            "limit::bound_tests::a_frame_charge_returns_to_its_limit_after_a_scope_outranks_the_body_record",
            "limit::bound_tests::payer_record_pointer_preserves_full_slot_provenance",
            "limit::bound_tests::charged_parent_boundary_and_outward_growth_keep_exact_room",
            "limit::bound_tests::forwarded_lexical_parent_uses_the_unchanged_general_union",
        ),
        exclusions=(
            Exclusion("region_tests::region_cache_retires_backing_after_two_smaller_uses", "miri-combinatorial-timeout", "Allocates and memsets 8 MiB region chunks across 11 iterations under Miri; covered by ASan+LSan and TSan."),
            Exclusion("hirsel-stack/fiber/units/tasks/io_uring/tls/contained", "fiber-asm-syscall-ffi", "Naked assembly stack switches, sigaction/mprotect fault handlers, io_uring, ring/wasmtime FFI are unsupported by Miri."),
        ),
    ),
    Suite(
        tool="miri",
        crate="hirsel-core-driver",
        name="hirsel-core-driver::lib (pure in-memory index/manifest/reuse/encoding suites)",
        package="hirsel-core-driver",
        target_args=("--lib",),
        no_default_features=True,
        timeout_s=1800,
        filters=(
            "image_index::tests::queries_encode_as_their_stored_keys",
            "release_manifest::tests::executable_is_named_not_truncated",
            "reuse::tests::",
            "encoding_laws::",
        ),
        exclusions=(
            Exclusion("native::/pack/llvm/cranelift/launcher/store/tempfile", "jit-ffi-subprocess", "Invokes libLLVM C++ FFI, Cranelift JIT execution, clang/lld, host subprocesses, or host filesystem tempdirs unsupported under isolated Miri."),
        ),
    ),
)

MIRI_CRATE_EXCLUSIONS: dict[str, tuple[Exclusion, ...]] = {
    "hirsel-pack": (
        Exclusion(
            "hirsel-pack lib, pack_export_abi, and pack_held",
            "jit-ffi",
            "The pack harnesses exercise native code in executable mappings, including JIT compilation and fiber signal recovery.",
        ),
    ),
}


def check_toolchain(tools: tuple[str, ...]) -> tuple[bool, str]:
    res = subprocess.run(
        ["cargo", "+nightly", "--version"], capture_output=True, text=True
    )
    if res.returncode != 0:
        return False, "nightly rustc/cargo toolchain is not installed"
    if "tsan" in tools:
        sysroot = subprocess.check_output(
            ["rustc", "+nightly", "--print", "sysroot"], text=True
        ).strip()
        if not (Path(sysroot) / "lib/rustlib/src/rust/library").is_dir():
            return False, "nightly rust-src component missing (required for TSan -Zbuild-std)"
    if "miri" in tools:
        miri = subprocess.run(
            ["cargo", "+nightly", "miri", "--version"], capture_output=True, text=True
        )
        if miri.returncode != 0:
            return False, "nightly miri component is not installed"
    return True, res.stdout.strip()


def symlink_llvm_beside(binary: Path) -> None:
    if not LLVM_ENGINE.is_file():
        return
    lib_dir = binary.parent / "lib"
    lib_dir.mkdir(parents=True, exist_ok=True)
    link = lib_dir / LLVM_ENGINE.name
    if link.is_symlink() or link.exists():
        link.unlink()
    link.symlink_to(LLVM_ENGINE)


def build_test_binary(suite: Suite, target_dir: Path, env: dict[str, str]) -> Path:
    cmd = ["cargo", "+nightly", "test", "--locked", "--no-run", "--message-format=json"]
    if suite.tool == "tsan":
        cmd.append("-Zbuild-std")
    cmd += ["--target", TARGET_TRIPLE, "-p", suite.package]
    if suite.no_default_features:
        cmd.append("--no-default-features")
    if suite.features:
        cmd += ["--features", ",".join(suite.features)]
    cmd += list(suite.target_args)
    build_env = env | {"CARGO_TARGET_DIR": str(target_dir / f"san-{suite.tool}")}
    proc = subprocess.run(
        cmd, cwd=ROOT, env=build_env, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True
    )
    if proc.returncode != 0:
        raise RuntimeError(
            f"build failed for {suite.name} ({suite.tool}):\n{proc.stderr[-3000:]}"
        )
    executable = None
    for line in proc.stdout.splitlines():
        if not line.startswith("{"):
            continue
        msg = json.loads(line)
        if msg.get("reason") == "compiler-artifact" and msg.get("executable"):
            if msg.get("profile", {}).get("test"):
                executable = Path(msg["executable"])
    if executable is None:
        raise RuntimeError(f"no test executable found in cargo output for {suite.name}")
    if suite.needs_llvm_symlink:
        symlink_llvm_beside(executable)
    return executable


def run_suite(suite: Suite, target_dir: Path, tmp_dir: Path, seeds: int) -> dict:
    started = time.monotonic()
    base_env = os.environ.copy()
    base_env["TMPDIR"] = str(tmp_dir)
    base_env["HIRSEL_MEMORY_LIMIT"] = "64T"
    base_env["HIRSEL_CHECK_SEEDS"] = str(seeds)
    run_cwd = ROOT / suite.cwd_rel

    if suite.tool == "asan":
        base_env["RUSTFLAGS"] = "-Zsanitizer=address"
        leaks = "1" if suite.detect_leaks else "0"
        base_env["ASAN_OPTIONS"] = (
            f"detect_leaks={leaks}:detect_stack_use_after_return=0:"
            f"quarantine_size_mb={suite.quarantine_mb}:"
            "abort_on_error=1:halt_on_error=1:allocator_may_return_null=1"
        )
    elif suite.tool == "tsan":
        base_env["RUSTFLAGS"] = "-Zsanitizer=thread"
        base_env["TSAN_OPTIONS"] = (
            "halt_on_error=1:second_deadlock_stack=1:allocator_may_return_null=1"
        )
    elif suite.tool == "miri":
        base_env["CARGO_TARGET_DIR"] = str(target_dir / "san-miri")
        base_env["MIRIFLAGS"] = "-Zmiri-strict-provenance"
    else:
        raise ValueError(f"unknown tool: {suite.tool}")

    try:
        if suite.tool == "miri":
            cmd = ["cargo", "+nightly", "miri", "test", "--locked", "-p", suite.package]
            if suite.no_default_features:
                cmd.append("--no-default-features")
            cmd += list(suite.target_args) + ["--"]
        else:
            cmd = [str(build_test_binary(suite, target_dir, base_env))]
        cmd += [*suite.filters, *suite.extra_test_args]
        for ex in suite.exclusions:
            if ex.category not in ("fiber-asm-syscall-ffi", "jit-ffi-subprocess"):
                cmd += ["--skip", ex.pattern]
        proc = subprocess.run(
            cmd, cwd=run_cwd, env=base_env, stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
            text=True, timeout=suite.timeout_s,
        )
        output = proc.stdout
        returncode = proc.returncode
    except subprocess.TimeoutExpired as error:
        output = (error.stdout or b"").decode(errors="replace")
        output += f"\nSuite timed out after {suite.timeout_s}s.\n"
        returncode = 124
    except (OSError, RuntimeError) as error:
        output = str(error)
        returncode = 1

    wall_s = round(time.monotonic() - started, 2)
    # A failing subprocess fixture can print its own libtest footer before the
    # parent harness reports its totals.
    summary = None
    for match in RESULT_RE.finditer(output):
        summary = match
    counts = (
        {key: int(value) for key, value in summary.groupdict().items()}
        if summary is not None
        else dict.fromkeys(("passed", "failed", "ignored", "filtered"))
    )
    ok = returncode == 0 and bool(counts["passed"]) and counts["failed"] == 0
    return {
        "tool": suite.tool,
        "crate": suite.crate,
        "name": suite.name,
        "ok": ok,
        "returncode": returncode,
        **counts,
        "filters": list(suite.filters),
        "excluded_rules": [
            {"pattern": ex.pattern, "category": ex.category, "reason": ex.reason}
            for ex in suite.exclusions
        ],
        "wall_s": wall_s,
        "output": output if not ok else "",
    }


def list_exclusions() -> None:
    for suite in SUITES:
        if not suite.exclusions:
            continue
        print(f"[{suite.tool}] {suite.name}")
        for ex in suite.exclusions:
            print(f"  - {ex.pattern} ({ex.category}): {ex.reason}")
    for crate, items in MIRI_CRATE_EXCLUSIONS.items():
        print(f"[miri] {crate}")
        for ex in items:
            print(f"  - {ex.pattern} ({ex.category}): {ex.reason}")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("mode", choices=("asan", "tsan", "miri", "all"))
    parser.add_argument("--crates", default=",".join(DEFAULT_CRATES))
    parser.add_argument("--target-dir", type=lambda path: Path(path).resolve(), default=ROOT / "target")
    parser.add_argument("--seeds", type=int, default=16)
    parser.add_argument("--json", type=Path, dest="json_out")
    parser.add_argument("--check-toolchain", action="store_true")
    parser.add_argument("--defer-if-missing", action="store_true")
    parser.add_argument("--clean-target", action="store_true")
    parser.add_argument("--list-exclusions", action="store_true")
    args = parser.parse_args()

    if args.list_exclusions:
        list_exclusions()
        return 0

    tools = ("asan", "tsan", "miri") if args.mode == "all" else (args.mode,)
    crates = tuple(c.strip() for c in args.crates.split(",") if c.strip())
    if not crates or any(crate not in DEFAULT_CRATES for crate in crates):
        parser.error(f"--crates expects a list drawn from {', '.join(DEFAULT_CRATES)}")

    ready, info = check_toolchain(tools)
    if args.check_toolchain:
        print(info if ready else f"missing: {info}")
        return 0 if ready else EX_DEFERRED
    if not ready:
        print(f"sanitizers deferred: {info}", file=sys.stderr)
        return EX_DEFERRED if args.defer_if_missing else 2

    selected = [s for s in SUITES if s.tool in tools and s.crate in crates]
    results = []
    overall_ok = True
    tmp_root = Path(tempfile.mkdtemp(prefix="hirsel-san-"))
    try:
        for suite in selected:
            print(f"==> [{suite.tool}] {suite.name}", flush=True)
            res = run_suite(suite, args.target_dir, tmp_root, args.seeds)
            results.append(res)
            status = "PASS" if res["ok"] else "FAIL"
            print(
                f"    {status} passed={res['passed']} failed={res['failed']} "
                f"ignored={res['ignored']} filtered={res['filtered']} "
                f"wall={res['wall_s']:.2f}s",
                flush=True,
            )
            if not res["ok"]:
                overall_ok = False
                print(res["output"], file=sys.stderr)
    finally:
        shutil.rmtree(tmp_root, ignore_errors=True)
        if args.clean_target:
            for tool in tools:
                shutil.rmtree(args.target_dir / f"san-{tool}", ignore_errors=True)

    summary = {
        "ok": overall_ok,
        "tools": list(tools),
        "crates": list(crates),
        "seeds": args.seeds,
        "results": results,
        "crate_exclusions": {
            crate: [
                {"pattern": e.pattern, "category": e.category, "reason": e.reason}
                for e in items
            ]
            for crate, items in MIRI_CRATE_EXCLUSIONS.items()
            if "miri" in tools and crate in crates
        },
    }
    if args.json_out:
        args.json_out.parent.mkdir(parents=True, exist_ok=True)
        args.json_out.write_text(json.dumps(summary, indent=2) + "\n")
    return 0 if overall_ok else 1


if __name__ == "__main__":
    raise SystemExit(main())
