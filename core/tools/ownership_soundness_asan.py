#!/usr/bin/env python3
"""AddressSanitizer arm of the ownership soundness fuzzer.

Builds `hirsel-core-rt` as an ASan-instrumented static archive with the nightly toolchain setup
of tools/sanitizers/run.py (same toolchain check, flags, feature, target directory and runtime
options) and links generated native programs against it. The native link takes its arguments
from the C driver, so a wrapper appends rustc's own ASan runtime to them.
"""
import functools
import importlib.util
import os
from pathlib import Path
import shlex
import subprocess
import sys

ROOT = Path(__file__).resolve().parents[2]
EX_DEFERRED = 75
RUNTIME_LIBRARY = 'librustc-nightly_rt.asan.a'
# The options of tools/sanitizers/run.py with leak detection on.
OPTIONS = ('detect_leaks=1:detect_stack_use_after_return=0:quarantine_size_mb=16:'
           'abort_on_error=1:halt_on_error=1:allocator_may_return_null=1')
DIAGNOSTICS = ('AddressSanitizer', 'LeakSanitizer', 'UndefinedBehaviorSanitizer', 'ThreadSanitizer',
               'MemorySanitizer')


@functools.cache
def sweep():
    """tools/sanitizers/run.py, the owner of the pinned sanitizer toolchain setup."""
    spec = importlib.util.spec_from_file_location('hirsel_sanitizer_sweep', ROOT / 'tools/sanitizers/run.py')
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


def sanitizer_runtime() -> Path | None:
    """rustc's ASan runtime for the sweep's target, or None when the nightly lacks it."""
    found = subprocess.run(['rustc', '+nightly', '--print', 'sysroot'], capture_output=True, text=True)
    if found.returncode != 0:
        return None
    library = Path(found.stdout.strip()) / 'lib/rustlib' / sweep().TARGET_TRIPLE / 'lib' / RUNTIME_LIBRARY
    return library if library.is_file() else None


def toolchain() -> tuple[bool, str]:
    """Whether the nightly sanitizer components are present, and what is missing when not."""
    try:
        ready, info = sweep().check_toolchain(('asan',))
    except OSError as error:
        return False, f'cargo is unavailable: {error}'
    if not ready:
        return False, info
    if sanitizer_runtime() is None:
        return False, f'nightly {RUNTIME_LIBRARY} is not installed for {sweep().TARGET_TRIPLE}'
    return True, info


def build_runtime(target_dir: Path, jobs: int) -> Path:
    """Build the ASan runtime archive beside the sanitizer sweep's own artifacts."""
    env = os.environ | {'RUSTFLAGS': '-Zsanitizer=address', 'CARGO_TARGET_DIR': str(target_dir / 'san-asan')}
    command = ['cargo', '+nightly', 'build', '--locked', '-j', str(jobs), '--target', sweep().TARGET_TRIPLE,
               '-p', 'hirsel-core-rt', '--features', 'sanitizer-asan', '--lib']
    built = subprocess.run(command, cwd=ROOT, env=env, capture_output=True, text=True)
    archive = target_dir / 'san-asan' / sweep().TARGET_TRIPLE / 'debug/libhirsel_core_rt.a'
    if built.returncode != 0 or not archive.is_file():
        raise RuntimeError(f'ASan runtime build failed:\n{built.stderr[-3000:]}')
    return archive


# Positive control only: a constructor that frees one block twice when PLANT_ENV is set. The
# allocation goes through ASan's interceptors, so a live runtime reports it whatever the object's
# own instrumentation.
PLANT_ENV = 'HIRSEL_ASAN_PLANT'
PLANT_SOURCE = f"""#include <stdlib.h>
__attribute__((constructor)) static void hirsel_asan_plant(void) {{
  if (!getenv("{PLANT_ENV}")) return;
  volatile char *block = malloc(16);
  free((void *)block);
  free((void *)block);
}}
"""


def plant_object(cc: Path | None, directory: Path) -> Path:
    """Compile the positive control's double-free constructor."""
    directory.mkdir(parents=True, exist_ok=True)
    source = directory / 'asan-plant.c'
    source.write_text(PLANT_SOURCE)
    obj = directory / 'asan-plant.o'
    subprocess.run([str(cc or 'cc'), '-c', '-fPIC', '-O0', str(source), '-o', str(obj)], check=True)
    return obj


def linker_wrapper(cc: Path | None, directory: Path, plant: Path | None = None) -> Path:
    """A C driver that adds rustc's ASan runtime (and the control's plant) to every link it describes."""
    directory.mkdir(parents=True, exist_ok=True)
    wrapper = directory / 'asan-cc'
    runtime = sanitizer_runtime()
    extra = f' {shlex.quote(str(plant))}' if plant else ''
    wrapper.write_text(
        '#!/bin/sh\n'
        f'exec {shlex.quote(str(cc or "cc"))} "$@"{extra} -Wl,--whole-archive {shlex.quote(str(runtime))} '
        '-Wl,--no-whole-archive -lpthread -lrt -lm -ldl\n')
    wrapper.chmod(0o755)
    return wrapper


def environment() -> dict:
    return {'ASAN_OPTIONS': OPTIONS}


def instrumented(binary: Path) -> bool:
    """True when the linked program carries a live ASan runtime (it answers `help=1`)."""
    probe = subprocess.run([str(binary), '0'], capture_output=True, env=os.environ | {'ASAN_OPTIONS': 'help=1'},
                           timeout=120)
    return b'AddressSanitizer' in probe.stderr


def diagnostic(stderr: str) -> str | None:
    """The sanitizer report in a program's stderr, from its first line."""
    for name in DIAGNOSTICS:
        at = stderr.find(name)
        if at >= 0:
            start = stderr.rfind('\n', 0, at) + 1
            return stderr[start:start + 1200]
    return None
