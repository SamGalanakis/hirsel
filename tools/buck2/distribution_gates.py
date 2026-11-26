"""Rendered Buck targets for release bundles and distribution tests."""

import importlib.util
import json

from compiler_source import toolchain_packages


def adapted_citations_present(reads):
    """The cited files that exist in the tree. The gate reports missing files."""
    path = reads.root / "tools/toolchain-dist/notices.py"
    reads.bytes(path)
    spec = importlib.util.spec_from_file_location("notices", path)
    notices = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(notices)
    cited = {cited for _, paths in notices.citations(reads.text(reads.root / notices.ADAPTED)) for cited in paths}
    return sorted(path for path in cited if reads.is_file(reads.root / path))


def distribution_license_paths(reads):
    sources = json.loads(reads.text(reads.root / "tools/toolchain-dist/notice-sources.json"))
    return sorted({path for runtime in sources["host_runtimes"].values() for path in runtime["texts"]})


def distribution_gates(out, call, Raw, reads):
    out.append("\n" + call("hirsel_toolchain_packages", name="toolchain-packages", hirsel="//:hirsel-core-driver-bin-hirsel",
                            packages=toolchain_packages(reads, reads.root)))
    out.append("\n" + call("hirsel_toolchain_bundle", name="hirsel-toolchain", hirsel="//:hirsel-core-driver-bin-hirsel",
                            runtimes="//:hirsel-core-rt-archives",
                            recording_runtimes="//:hirsel-core-rt-record-archives",
                            prof_runtimes="//:hirsel-core-rt-prof-archives",
                            prof_recording_runtimes="//:hirsel-core-rt-prof-record-archives",
                            observed_runtimes="//:hirsel-core-rt-observed-archives",
                            observed_recording_runtimes="//:hirsel-core-rt-observed-record-archives",
                            toolchain_packages="//:toolchain-packages",
                            platform_sources=Raw('glob(["core/programs/platform/*.hirsel"]) + ["core/programs/platform/hirsel.toml"]'),
                            visibility=["PUBLIC"]))
    out.append("\n" + call("hirsel_script_test", name="check-installer", script="//tools/toolchain-dist:check-installer",
                            data=["//tools/toolchain-dist:install.sh", ".github/workflows/release.yml"]))
    out.append("\n" + call("hirsel_script_test", name="gate-distribution-notices", script="tools/tests/distribution_notices.py",
                            args=["--present", *adapted_citations_present(reads)],
                            data=["tools/tests/distribution_notices.py", "//tools/toolchain-dist:build",
                                  "//tools/toolchain-dist:darwin_host.py", "//tools/buck2/native-toolchain:link-kit-source",
                                  "//tools/toolchain-dist:install.sh", "NOTICES.md", "Cargo.lock",
                                  *("tools/tests/fixtures/glibc-gate/" + name for name in ("strong-glibc239", "weak-dynsym", "weak-symtab")),
                                  "//third-party:NOTICES.md", "tools/native-toolchain/llvm-23.lock", "//tools/toolchain-dist:notice-sources.json",
                                  *("//tools/toolchain-dist:" + path.removeprefix("tools/toolchain-dist/") for path in distribution_license_paths(reads))],
                            files_env={"HIRSEL_DISTRIBUTION_NOTICES": "//tools/toolchain-dist:notices.py",
                                       "HIRSEL_TOOLCHAIN_BUNDLE": "//:hirsel-toolchain"}))
