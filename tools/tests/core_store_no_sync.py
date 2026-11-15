#!/usr/bin/env python3
"""A check and a build issue no sync system call.

Law: the store is never synced (`crates/hirsel-core-driver/src/store.rs`). A
record reads back only when its checksum checks, so after a power loss a torn,
short, zero-filled or missing record is a miss and is rebuilt. A sync buys
nothing and costs I/O wait. Each case counts every fsync, fdatasync, syncfs,
sync and sync_file_range the compiler and its children call, through a
preloaded library that logs each one, on a fresh store and then again on that
store with no change. Bug caught: a writer that syncs its file or its
directory.
"""
import argparse
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

# Each wrapper logs its name, then calls the real function.
SHIM = r"""
#define _GNU_SOURCE
#include <dlfcn.h>
#include <fcntl.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
static void logged(const char *name) {
    const char *path = getenv("SYNC_LOG");
    if (!path) return;
    int fd = open(path, O_WRONLY | O_APPEND | O_CREAT | O_CLOEXEC, 0600);
    if (fd < 0) return;
    (void)!write(fd, name, strlen(name));
    (void)!write(fd, "\n", 1);
    close(fd);
}
#define WRAP(ret, name, params, args) \
    ret name params { logged(#name); ret (*real) params = dlsym(RTLD_NEXT, #name); return real args; }
WRAP(int, fsync, (int fd), (fd))
WRAP(int, fdatasync, (int fd), (fd))
WRAP(int, syncfs, (int fd), (fd))
WRAP(int, sync_file_range, (int fd, off_t offset, off_t count, unsigned int flags), (fd, offset, count, flags))
void sync(void) { logged("sync"); void (*real)(void) = dlsym(RTLD_NEXT, "sync"); real(); }
"""


def syncs(args, shim, log):
    """Run `args` with `shim` preloaded; return its status and each sync call it logged."""
    log.unlink(missing_ok=True)
    env = dict(os.environ, LD_PRELOAD=str(shim), SYNC_LOG=str(log))
    run = subprocess.run(args, capture_output=True, text=True, env=env)
    return run, log.read_text().split() if log.exists() else []


class StoreNoSync(unittest.TestCase):
    def setUp(self):
        self.compiler = str(Path(os.environ['HIRSEL_SYNC_COMPILER']).resolve())

    def assert_no_sync(self, args, scratch, name):
        for request in ['fresh', 'no change']:
            run, calls = syncs([self.compiler, *args], scratch / 'shim.so', scratch / f'{name}.log')
            self.assertEqual(run.returncode, 0, run.stdout + run.stderr)
            self.assertEqual(calls, [], f'{name}, {request}')

    def test_check_and_build(self):
        cc = Path(os.environ['LLVM_SYS_231_PREFIX']) / 'bin/clang'
        archive = (Path(os.environ['HIRSEL_CORE_RT_DEFAULT_ARCHIVES']).resolve()
                   / 'x86_64-unknown-linux-gnu/libhirsel_core_rt.a')
        with tempfile.TemporaryDirectory(prefix='hirsel-no-sync-') as scratch:
            scratch = Path(scratch)
            (scratch / 'shim.c').write_text(SHIM)
            subprocess.run([str(cc), '-shared', '-fPIC', '-O1', '-o', str(scratch / 'shim.so'),
                            str(scratch / 'shim.c'), '-ldl'], check=True)
            # The shim sees a sync: a run that cannot be observed proves nothing.
            subprocess.run(['python3', '-c', 'import os; os.fsync(os.open(".", os.O_RDONLY))'],
                           env=dict(os.environ, LD_PRELOAD=str(scratch / 'shim.so'),
                                    SYNC_LOG=str(scratch / 'probe.log')), check=True)
            self.assertEqual((scratch / 'probe.log').read_text().split(), ['fsync'])
            source = scratch / 'source'
            source.mkdir()
            (source / 'main.hirsel').write_text('pub fn main() -> Result[(), Exit] { Error(Exit(status:7)) }\n')
            store = str(scratch / 'store')
            self.assert_no_sync(['check', str(source), '--store', store], scratch, 'check')
            binary = scratch / 'program'
            self.assert_no_sync(['build', str(source), '--entry', 'app@1/main/main', '--backend', 'cranelift',
                                 '--runtime', str(archive), '--cc', str(cc), '--store', store, '-o', str(binary)],
                                scratch, 'build')
            self.assertEqual(subprocess.run([binary]).returncode, 7)


if __name__ == '__main__':
    parser = argparse.ArgumentParser()
    parser.add_argument('--test-threads', type=int, choices=[1], default=1)
    parser.parse_args()
    unittest.main(argv=[__file__], verbosity=2)
