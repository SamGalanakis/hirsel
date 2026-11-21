#!/usr/bin/env python3
"""Cold and warm checks and an unchanged build issue no sync calls.

The driver's build cache (store.rs) never syncs. A torn or missing cache
record is a miss. Every sync in a fresh build belongs to a new code object:
one successful file fsync per object, followed by fsyncs of its kind directory
and the code-store root. Host admission owes staged objects the same barriers.
See "Durable publication" in docs/spec/launch.md.

The preload shim traces fsync, fdatasync, syncfs, sync and sync_file_range in
the compiler and its subprocesses.
"""

import argparse
import os
import subprocess
import tempfile
import unittest
from pathlib import Path
from typing import NamedTuple

SHIM = r"""
#define _GNU_SOURCE
#include <dlfcn.h>
#include <errno.h>
#include <fcntl.h>
#include <inttypes.h>
#include <limits.h>
#include <stdio.h>
#include <stdlib.h>
#include <sys/stat.h>
#include <unistd.h>

static void log_call(const char *name, int result, int fd) {
    int saved_errno = errno;
    const char *log_path = getenv("SYNC_LOG");
    if (!log_path) return;

    struct stat info = {0};
    char type = '?';
    if (fd >= 0 && fstat(fd, &info) == 0) {
        type = S_ISREG(info.st_mode) ? 'f' : S_ISDIR(info.st_mode) ? 'd' : 'o';
    }
    char record[128 + PATH_MAX];
    int prefix = snprintf(record, sizeof(record), "%s %d %c %ju %ju ",
                          name, result, type, (uintmax_t)info.st_dev, (uintmax_t)info.st_ino);
    if (prefix < 0 || prefix >= 128) _exit(125);
    ssize_t length = 0;
    if (fd >= 0) {
        char descriptor[32];
        snprintf(descriptor, sizeof(descriptor), "/proc/self/fd/%d", fd);
        length = readlink(descriptor, record + prefix, PATH_MAX);
        if (length < 0 || length == PATH_MAX) length = 0;
    }
    // Terminate each record with NUL so whitespace in paths stays intact.
    record[prefix + length] = '\0';
    size_t size = prefix + length + 1;
    int log = open(log_path, O_WRONLY | O_APPEND | O_CREAT | O_CLOEXEC, 0600);
    if (log < 0 || write(log, record, size) != (ssize_t)size || close(log) != 0) _exit(125);
    errno = saved_errno;
}

#define RESOLVE(ret, name, params) \
    int incoming_errno = errno; \
    ret (*real) params = dlsym(RTLD_NEXT, #name); \
    errno = incoming_errno
#define WRAP(name, params, args) \
    int name params { \
        RESOLVE(int, name, params); \
        int result = real args; \
        log_call(#name, result, fd); \
        return result; \
    }
WRAP(fsync, (int fd), (fd))
WRAP(fdatasync, (int fd), (fd))
WRAP(syncfs, (int fd), (fd))
WRAP(sync_file_range, (int fd, off_t offset, off_t count, unsigned int flags), (fd, offset, count, flags))
void sync(void) {
    RESOLVE(void, sync, (void));
    real();
    log_call("sync", 0, -1);
}
"""


class SyncCall(NamedTuple):
    operation: str
    result: int
    kind: str
    identity: tuple[int, int]
    path: Path | None


def syncs(args, shim, log):
    """Run args with the shim and return the subprocess result and sync calls."""
    log.unlink(missing_ok=True)
    env = dict(os.environ, LD_PRELOAD=str(shim), SYNC_LOG=str(log))
    run = subprocess.run(args, capture_output=True, text=True, env=env, check=False)
    calls = []
    if log.exists():
        records = log.read_bytes().split(b"\0")
        if records.pop():
            raise ValueError(f"incomplete sync record in {log}")
        for record in records:
            operation, result, kind, device, inode, path = record.split(b" ", 5)
            calls.append(
                SyncCall(
                    operation.decode(),
                    int(result),
                    kind.decode(),
                    (int(device), int(inode)),
                    Path(os.fsdecode(path)) if path else None,
                )
            )
    return run, calls


class StoreNoSync(unittest.TestCase):
    def setUp(self):
        self.compiler = str(Path(os.environ["HIRSEL_SYNC_COMPILER"]).resolve())

    def assert_publication_syncs(self, calls, root, *, staging=None):
        objects = {}
        pending_directories = {}
        for kind in ("bodies", "annexes", "groups", "recipes"):
            directory = root / kind
            files = list(directory.iterdir())
            self.assertTrue(files, f"no objects published in {directory}")
            pending_directories[directory] = set()
            for path in files:
                self.assertRegex(path.name, r"\A[0-9a-f]{64}\Z")
                self.assertTrue(path.is_file() and not path.is_symlink(), path)
                info = path.stat()
                identity = (info.st_dev, info.st_ino)
                self.assertNotIn(identity, objects, f"aliased code object {path}")
                objects[identity] = path

        published = set(objects.values())
        pending_root = set()
        for call in calls:
            self.assertEqual((call.operation, call.result), ("fsync", 0), call)
            self.assertTrue(call.path and call.path.is_absolute(), call)
            if call.kind == "f":
                # The writer syncs the inode before installing its hash name.
                self.assertIn(call.identity, objects, f"unexpected file sync {call}")
                path = objects.pop(call.identity)
                in_staging = staging is not None and call.path.is_relative_to(staging)
                self.assertTrue(call.path.parent == path.parent or in_staging, call)
                pending_directories[path.parent].add(path)
            elif call.kind == "d" and call.path == root:
                self.assertTrue(
                    pending_root,
                    f"root sync without a preceding kind directory sync: {call}",
                )
                pending_root.clear()
            else:
                self.assertEqual(call.kind, "d", call)
                self.assertIn(
                    call.path, pending_directories, f"unexpected directory sync {call}"
                )
                pending = pending_directories[call.path]
                self.assertTrue(
                    pending,
                    f"kind directory sync without a preceding object sync: {call}",
                )
                pending_root.update(pending)
                pending.clear()

        self.assertFalse(
            objects, f"missing object file syncs: {list(objects.values())}"
        )
        pending = set().union(*pending_directories.values())
        self.assertFalse(pending, f"missing kind directory syncs: {sorted(pending)}")
        self.assertFalse(
            pending_root, f"missing code-store root syncs: {sorted(pending_root)}"
        )
        return published

    def test_check_and_build(self):
        cc = Path(os.environ["LLVM_SYS_231_PREFIX"]) / "bin/clang"
        archive = (
            Path(os.environ["HIRSEL_CORE_RT_DEFAULT_ARCHIVES"]).resolve()
            / "x86_64-unknown-linux-gnu/libhirsel_core_rt.a"
        )
        with tempfile.TemporaryDirectory(prefix="hirsel-no-sync-") as scratch:
            scratch = Path(scratch).resolve()
            shim = scratch / "shim.so"
            shim_source = scratch / "shim.c"
            shim_source.write_text(SHIM)
            subprocess.run(
                [cc, "-shared", "-fPIC", "-O1", "-o", shim, shim_source, "-ldl"],
                check=True,
            )

            probe = scratch / "probe file"
            probe.touch()
            run, calls = syncs(
                [
                    "python3",
                    "-c",
                    "import os, sys; os.fsync(os.open(sys.argv[1], os.O_RDONLY))",
                    probe,
                ],
                shim,
                scratch / "probe.log",
            )
            self.assertEqual(run.returncode, 0, run.stdout + run.stderr)
            info = probe.stat()
            self.assertEqual(
                calls, [SyncCall("fsync", 0, "f", (info.st_dev, info.st_ino), probe)]
            )

            source = scratch / "source"
            source.mkdir()
            (source / "main.hirsel").write_text(
                "pub fn main() -> Result[(), Exit] { Error(Exit(status:7)) }\n"
            )
            check = [self.compiler, "check", source, "--store", scratch / "check-store"]
            for request in ("cold", "warm"):
                run, calls = syncs(check, shim, scratch / "check.log")
                self.assertEqual(run.returncode, 0, run.stdout + run.stderr)
                self.assertEqual(calls, [], f"{request} check")

            store = scratch / "build-store"
            code = store / "code-store"
            binary = scratch / "program"
            build = [
                self.compiler,
                "build",
                source,
                "--entry",
                "app@1/main/main",
                "--backend",
                "cranelift",
                "--runtime",
                archive,
                "--cc",
                cc,
                "--store",
                store,
                "-o",
                binary,
            ]
            run, calls = syncs(build, shim, scratch / "build.log")
            self.assertEqual(run.returncode, 0, run.stdout + run.stderr)
            published = self.assert_publication_syncs(calls, code)

            run, calls = syncs(build, shim, scratch / "build.log")
            self.assertEqual(run.returncode, 0, run.stdout + run.stderr)
            self.assertEqual(calls, [], "unchanged build")
            self.assertEqual(subprocess.run([binary], check=False).returncode, 7)

            # The uninstalled compiler admits and publishes the initial release,
            # then stops when host assembly requires an installed toolchain.
            (recipe,) = [
                path.name for path in published if path.parent == code / "recipes"
            ]
            state = scratch / "host"
            host_code = state / "code"
            host = [
                self.compiler,
                "host",
                recipe,
                "--state",
                state,
                "--code-store",
                code,
                "--control",
                scratch / "control",
                "--listen",
                "127.0.0.1:0",
            ]
            run, calls = syncs(host, shim, scratch / "host.log")
            self.assertEqual(run.returncode, 2, run.stdout + run.stderr)
            self.assertIn("host assembly requires an installed toolchain", run.stderr)
            # Startup also syncs the records and child PID directories.
            setup_dirs = {state, state / "children"}
            publication = [
                call
                for call in calls
                if not (
                    (call.operation, call.result, call.kind) == ("fsync", 0, "d")
                    and call.path in setup_dirs
                )
            ]
            received = self.assert_publication_syncs(
                publication, host_code, staging=host_code / "staging"
            )
            self.assertEqual(
                {path.relative_to(host_code) for path in received},
                {path.relative_to(code) for path in published},
            )


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("--test-threads", type=int, choices=[1], default=1)
    parser.parse_args()
    unittest.main(argv=[__file__], verbosity=2)
