#!/usr/bin/env python3
"""Measurement pins survive a later build that replaces Buck's live outputs.

Bug law: a lane recorded one compiler sha256 and measured with another, because
a later buck build in its checkout replaced the live path it executed. The
pinned copy the tool reports must keep running (its $ORIGIN RPATH still finds
the checkout's LLVM toolchain) and keep hashing as recorded, and the pre-batch
check must refuse anything else."""
import contextlib
import hashlib
import io
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / 'core/tools'))
import measurement_artifacts as tool  # noqa: E402

CLANG = Path(os.environ.get('LLVM_SYS_231_PREFIX', '/usr')) / 'bin/clang'
LIBRARY = 'const char *toolchain(void) { return "toolchain found"; }\n'
# The real compiler dlopens libLLVM by name through RPATH
# $ORIGIN/../../../../../../tools/buck2/toolchains/llvm-files/lib.
PROGRAM = r'''
#include <dlfcn.h>
#include <stdio.h>
int main(void) {
    void *handle = dlopen("libtoolchain.so", RTLD_NOW);
    if (!handle) { fprintf(stderr, "%s\n", dlerror()); return 3; }
    const char *(*toolchain)(void) = (const char *(*)(void))dlsym(handle, "toolchain");
    puts(toolchain());
    puts(VERSION);
    return 0;
}
'''
COMPILER = 'buck-out/v2/art/root/release/__hirsel-core-driver-bin-hirsel-core__/hirsel_core'
RUNTIME = 'buck-out/v2/art/root/release/__hirsel-core-rt-archives__/hirsel-core-rt-archives'
FAKE_BUCK = f'''#!/bin/sh
set -e
cd "$(dirname "$0")/../.."
mkdir -p {Path(COMPILER).parent} {RUNTIME}/x86_64-unknown-linux-gnu
"{CLANG}" -DVERSION='"built first"' -x c program.c -ldl -Wl,--disable-new-dtags \
  -Wl,-rpath,'$ORIGIN/../../../../../../tools/buck2/toolchains/llvm-files/lib' -o {COMPILER}
printf 'shipping archive' > {RUNTIME}/x86_64-unknown-linux-gnu/libhirsel_core_rt.a
printf '{{"root//:hirsel-core-driver-bin-hirsel-core":"{COMPILER}","root//:hirsel-core-rt-archives":"{RUNTIME}"}}\\n'
'''


class PinnedArtifacts(unittest.TestCase):
    def setUp(self):
        scratch = tempfile.TemporaryDirectory(prefix='hirsel-measurement-pins-')
        self.addCleanup(scratch.cleanup)
        self.root = Path(scratch.name)
        lib = self.root / 'tools/buck2/toolchains/llvm-files/lib'
        lib.mkdir(parents=True)
        (self.root / 'lib.c').write_text(LIBRARY)
        subprocess.run([str(CLANG), '-shared', '-fPIC', str(self.root / 'lib.c'), '-o', str(lib / 'libtoolchain.so')], check=True)
        (self.root / 'program.c').write_text(PROGRAM)
        buck = self.root / '.buck2/bin/buck2'
        buck.parent.mkdir(parents=True)
        buck.write_text(FAKE_BUCK)
        buck.chmod(0o755)
        self.addCleanup(patch.stopall)
        patch.object(tool, 'ROOT', self.root).start()
        patch.object(tool, 'wait_load', lambda: None).start()

    def tool(self, *argv):
        out = io.StringIO()
        with patch.object(sys, 'argv', ['measurement_artifacts.py', *argv]), contextlib.redirect_stdout(out):
            try:
                code = tool.main()
            except SystemExit as stop:
                code = stop.code
        return code, out.getvalue()

    def test_pinned_compiler_outlives_a_rebuilt_live_path(self):
        code, out = self.tool()
        self.assertEqual(code, 0)
        recorded = json.loads(out)
        live, pinned = self.root / COMPILER, Path(recorded['compiler'])
        self.assertEqual(recorded['compiler_source'], str(live))
        first = hashlib.sha256(live.read_bytes()).hexdigest()
        self.assertEqual(recorded['compiler_sha256'], first)
        self.assertIn(first, pinned.name)
        # Another configuration's build replaces the live compiler in place.
        live.chmod(0o755)
        subprocess.run([str(CLANG), '-DVERSION="built later"', '-x', 'c', str(self.root / 'program.c'), '-ldl',
                        '-Wl,--disable-new-dtags', '-Wl,-rpath,$ORIGIN/../../../../../../tools/buck2/toolchains/llvm-files/lib',
                        '-o', str(live)], check=True)
        self.assertEqual(subprocess.run([str(live)], capture_output=True, text=True).stdout.splitlines()[1], 'built later')
        ran = subprocess.run([str(pinned)], capture_output=True, text=True, env={'PATH': os.environ['PATH']})
        self.assertEqual((ran.returncode, ran.stdout), (0, 'toolchain found\nbuilt first\n'), ran.stderr)
        identity = self.root / 'identity.json'
        identity.write_text(out)
        self.assertEqual(self.tool('--check', str(pinned), '--identity', str(identity)), (0, f'ok {first} {pinned}\n'))
        with contextlib.redirect_stderr(io.StringIO()):
            self.assertEqual(self.tool('--check', str(live), '--identity', str(identity))[0], 2)
        # The pinned runtime is a shipping identity a harness accepts.
        again = tool.identity(pinned, Path(recorded['runtime']), 'release')
        self.assertEqual(again['runtime_archive_profile'], 'shipping-release-fat-lto')
        self.assertEqual(again['compiler_sha256'], first)

    def test_check_refuses_a_pinned_path_whose_bytes_changed(self):
        code, out = self.tool()
        recorded = json.loads(out)
        identity = self.root / 'identity.json'
        identity.write_text(out)
        runtime = Path(recorded['runtime'])
        runtime.chmod(0o644)
        runtime.write_bytes(b'replaced archive')
        with contextlib.redirect_stderr(io.StringIO()) as err:
            self.assertEqual(self.tool('--check', str(runtime), '--identity', str(identity))[0], 2)
        self.assertIn(recorded['runtime_archive_sha256'], err.getvalue())


if __name__ == '__main__':
    unittest.main()
