#!/usr/bin/env python3
"""Slice 2 ASan C harness covering B1, B2, and M13 against `hirsel_embed.h`.

Runs with `host=True` in Buck2 because AddressSanitizer's 16 TiB virtual shadow
reservation requires unbounded `RLIMIT_AS` (`ulimit -v`), which RE workers cap.
"""
import os
from pathlib import Path
import re
import resource
import shutil
import subprocess
import tempfile
import unittest


def _lift_asan_rlimit():
    # test_launcher.sh sets a soft RLIMIT_DATA and documents that sanitizer children lift it
    # so AddressSanitizer can map its virtual shadow address space.
    _, hard = resource.getrlimit(resource.RLIMIT_DATA)
    resource.setrlimit(resource.RLIMIT_DATA, (hard, hard))

ROOT = Path(__file__).resolve().parents[2]


def extract_header_v1() -> str:
    src = (ROOT / "crates/hirsel-core-ids/src/embed.rs").read_text()
    m = re.search(r'pub const HEADER_V1: &str = r#"([\s\S]*?)"#;', src)
    if not m:
        raise AssertionError("HEADER_V1 not found in crates/hirsel-core-ids/src/embed.rs")
    return m.group(1)


class EmbedAbiAsanLaws(unittest.TestCase):
    def test_asan_b1_b2_m13(self):
        archive = Path(os.environ["HIRSEL_EMBED_ABI_FIXTURE"]).resolve()
        source = ROOT / "crates/hirsel-core-rt/testdata/embed_abi_asan.c"
        asan_cc = shutil.which("gcc") or shutil.which("clang")
        self.assertIsNotNone(asan_cc, "gcc or clang with libasan required for ASan law")
        with tempfile.TemporaryDirectory(prefix="hirsel-embed-abi-asan-") as scratch:
            d = Path(scratch)
            (d / "hirsel_embed.h").write_text(extract_header_v1())
            store_dir = d / "store_asan"
            store_dir.mkdir()
            binary = d / "embed_abi_asan"
            subprocess.run(
                [
                    asan_cc,
                    "-O1",
                    "-g",
                    "-std=c11",
                    "-Wall",
                    "-Wextra",
                    "-Werror",
                    "-fsanitize=address",
                    "-fno-omit-frame-pointer",
                    f"-I{d}",
                    str(source),
                    str(archive),
                    "-lgcc_s",
                    "-lpthread",
                    "-ldl",
                    "-lm",
                    "-lrt",
                    "-lutil",
                    "-o",
                    str(binary),
                ],
                check=True,
            )
            env = dict(os.environ)
            env["ASAN_OPTIONS"] = "detect_leaks=0:abort_on_error=1:halt_on_error=1"
            run = subprocess.run(
                [str(binary), str(store_dir)],
                capture_output=True,
                text=True,
                env=env,
                preexec_fn=_lift_asan_rlimit,
            )
            self.assertEqual(
                run.returncode,
                0,
                f"embed_abi_asan exited {run.returncode}\nstdout:\n{run.stdout}\nstderr:\n{run.stderr}",
            )
            self.assertEqual(run.stderr, "")
            self.assertEqual(run.stdout, "EMBED_ABI_ASAN_OK\n")


if __name__ == "__main__":
    unittest.main()
