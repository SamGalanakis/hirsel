"""Rendered Buck targets for release bundles and distribution tests."""

import importlib.util
import json
import tomllib

from compiler_source import toolchain_packages


def release_product(reads):
    return tomllib.loads(reads.text(reads.root / "tools/toolchain-dist/product.toml"))


def archive_slots(reads):
    return {name: slot for name, slot in release_product(reads)["runtime_slots"].items()
            if "buck_suffix" in slot}


def runtime_product_bzl(reads, starlark, header):
    product = release_product(reads)
    platforms = {triple: row["platform"] for triple, row in product["runtime_targets"].items()
                 if "slots" not in row}
    embed_platforms = {triple: row["platform"] for triple, row in product["runtime_targets"].items()
                       if "embed-runtime" in row.get("slots", [])}
    exec_helper_platforms = {triple: row["platform"] for triple, row in product["runtime_targets"].items()
                             if row.get("exec_helper")}
    return (header + f"RUNTIME_TARGETS = {starlark(platforms, 0)}\n"
            + f"EMBED_RUNTIME_TARGETS = {starlark(embed_platforms, 0)}\n"
            + f"EXEC_HELPER_TARGETS = {starlark(exec_helper_platforms, 0)}\n"
            + f"HOSTS = {starlark(product['hosts'], 0)}\n")


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


def distribution_gates(out, call, Raw, reads, slots):
    for family in ("darwin", "ios"):
        for library in ("libSystem", "CoreFoundation", "Security"):
            out.append("\n" + call("export_file", name=f"{family}-{library.lower()}-symbols",
                                   src=f"tools/native-toolchain/{family}/{library}.symbols", visibility=["PUBLIC"]))
    out.append("\n" + call("hirsel_core_runtime_archives", name="hirsel-core-rt-ios-embed-archives",
                            library="//:hirsel-core-rt-embed", archive_name="libhirsel_core_rt.a",
                            embed=True))
    out.append("\n" + call("hirsel_toolchain_packages", name="toolchain-packages", hirsel="//:hirsel-core-driver-bin-hirsel",
                            packages=toolchain_packages(reads, reads.root)))
    out.append("\n" + call("hirsel_script_test", name="test-toolchain-packages", script="tools/tests/toolchain_packages.py",
                            data=[],
                            files_env={"HIRSEL_PACK_COMPILER": "//:hirsel-core-driver-bin-hirsel",
                                       "HIRSEL_PACK_PROGRAMS": "//tools/toolchain-dist:toolchain_programs.py"},
                            extra_env={"HIRSEL_JOBS": "1"}))
    out.append("\n" + call("hirsel_toolchain_bundle", name="hirsel-toolchain", hirsel="//:hirsel-core-driver-bin-hirsel",
                            runtimes={flag: "//:hirsel-core-rt" + slot["buck_suffix"] + "-archives"
                                      for flag, slot in slots.items()},
                            embed_runtime="//:hirsel-core-rt-ios-embed-archives",
                            exec_helpers="//:hirsel-exec-helpers",
                            toolchain_packages="//:toolchain-packages",
                            platform_sources=Raw('glob(["core/programs/platform/*.hirsel"]) + ["core/programs/platform/hirsel.toml"]'),
                            visibility=["PUBLIC"]))
    out.append("\n" + call("hirsel_script_test", name="store-restore-directory", script="tools/tests/store_restore.py",
                            data=Raw('glob(["core/programs/store/corpus/format-14/directory/*"])'),
                            files_env={"HIRSEL_TOOLCHAIN_BUNDLE": "//:hirsel-toolchain"}))
    out.append("\n" + call("hirsel_script_test", name="check-installer", script="//tools/toolchain-dist:check-installer",
                            data=["//tools/toolchain-dist:install.sh.in", "//tools/toolchain-dist:product.py",
                                  "//tools/toolchain-dist:product.toml", "tools/native-toolchain/apple-targets.json"]))
    out.append("\n" + call("hirsel_script_test", name="gate-distribution-notices", script="tools/tests/distribution_notices.py",
                            args=["--present", *adapted_citations_present(reads)],
                            data=["tools/tests/distribution_notices.py", "//tools/toolchain-dist:build",
                                  "//tools/toolchain-dist:darwin_host.py", "//tools/buck2/native-toolchain:link-kit-source",
                                  "//tools/buck2:apple_records.py", "//tools/buck2:generated.py",
                                  "tools/native-toolchain/apple-targets.json",
                                  "//tools/toolchain-dist:install.sh.in", "//tools/toolchain-dist:product.py",
                                  "//tools/toolchain-dist:product.toml",
                                  "NOTICES.md", "Cargo.lock",
                                  *("tools/tests/fixtures/glibc-gate/" + name for name in ("strong-glibc239", "weak-dynsym", "weak-symtab")),
                                  "//third-party:NOTICES.md", "tools/native-toolchain/llvm-23.lock", "//tools/toolchain-dist:notice-sources.json",
                                  *("//tools/toolchain-dist:" + path.removeprefix("tools/toolchain-dist/") for path in distribution_license_paths(reads))],
                            files_env={"HIRSEL_DISTRIBUTION_NOTICES": "//tools/toolchain-dist:notices.py",
                                       "HIRSEL_DISTRIBUTION_LLVM": "//tools/buck2/native-toolchain:llvm",
                                       "HIRSEL_TOOLCHAIN_BUNDLE": "//:hirsel-toolchain"}))
