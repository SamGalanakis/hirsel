#!/usr/bin/env python3
"""Build the AWFY binaries with the LangArena release profiles."""
import argparse
import ast
import hashlib
import json
import os
from pathlib import Path
import subprocess
import shutil
import sys

HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[1]
MANIFEST_HASHES = ("manifest_sha256", "workload_sources_sha256", "std_sha256")
sys.path.append(str(ROOT / "core/tools"))
from langarena import inputs


def llvm_path():
    return ROOT / "tools/buck2/toolchains/llvm-files"


def capped_env(out, limit="2G", jobs="1"):
    return {**os.environ, "HIRSEL_MEMORY_LIMIT": limit, "HIRSEL_JOBS": jobs,
            "XDG_DATA_HOME": str(out / "data"), "XDG_CACHE_HOME": str(out / "cache")}


def sha256(path):
    with path.open("rb") as source:
        return hashlib.file_digest(source, "sha256").hexdigest()


def manifest_digests(manifest):
    inputs = json.loads(manifest.read_text())
    std, workload = [], []
    for package in inputs["packages"]:
        for source in package["sources"]:
            source["file"] = sha256(Path(source["file"]))
        if package["package"]["name"] in ("core", "std"):
            std.append(package)
        else:
            workload.append(package)

    def digest(value):
        return hashlib.sha256(json.dumps(value, sort_keys=True, separators=(",", ":")).encode()).hexdigest()

    return {
        "manifest_sha256": digest(inputs),
        "workload_sources_sha256": digest(inputs | {"packages": workload}),
        "std_sha256": digest(std),
    }


def native_provenance(command, manifest, *, cwd=None):
    # Keep every option, with private directory paths represented by their
    # roles. The files that affect the build have separate content identities.
    if cwd is None:
        cwd = Path.cwd()
    flags = list(map(str, command[2:]))
    path_roles = {"--inputs": "manifest", "--store": "store", "--runtime": "runtime",
                  "--dir": "workload directory", "-o": "image"}
    for index, flag in enumerate(flags[:-1]):
        if flag in path_roles:
            flags[index + 1] = path_roles[flag]
        elif flag == "--cc":
            flags[index + 1] = sha256(cwd / flags[index + 1])
    return {
        "flags": flags,
        **manifest_digests(manifest),
        "compiler_sha256": sha256(cwd / command[0]),
        "runtime_archive_sha256": sha256(cwd / command[command.index("--runtime") + 1]),
    }


def write_native_report(binary, stdout, provenance, *, path=None):
    report = json.loads(stdout)
    report["measurement"] = provenance | {"image_sha256": sha256(binary)}
    target = path if path is not None else binary.with_name(f"{binary.name}-build.json")
    target.write_text(json.dumps(report) + "\n")
    return report


def fixture(upstream):
    source = (upstream / "benchmarks/JavaScript/json.js").read_text()
    prefix = "const RAP_BENCHMARK_MINIFIED = "
    literal, = (line[len(prefix):-1] for line in source.splitlines() if line.startswith(prefix))
    text = ast.literal_eval(literal)
    return json.dumps(text, ensure_ascii=False)


def hirsel(args, matched=False):
    artifacts = json.loads(args.artifacts.read_text())
    compiler, runtime = (artifacts[key] for key in ("compiler", "runtime"))
    spec = inputs(ROOT, "harness")
    rap = args.out / "rap.hirsel"
    rap.write_text(f"pub fn document() -> String {{ {fixture(args.upstream)} }}\n")
    sources = list((HERE / "hirsel").glob("*.hirsel"))
    if matched:
        variants = HERE / "pool-ports/hirsel"
        harness = args.out / "pool_index.hirsel"
        port(HERE / "hirsel/harness.hirsel", harness, {
            "havlak.benchmark(inner)": "havlak_pool.benchmark(inner)",
            "cd.benchmark(inner)": "cd_pool.benchmark(inner)",
            "deltablue.benchmark(inner)": "deltablue_pool.benchmark(inner)",
            "lists.list_benchmark()": "linked_small.list_benchmark()",
            "trees.towers_benchmark()": "linked_small.towers_benchmark()",
        })
        sources += [harness, *variants.glob("*.hirsel")]
    spec["packages"][-1].update(package={"name": "awfy", "version": "1"}, sources=[
        {"file": str(source), "modules": [source.stem], "signatures": False}
        for source in [*sorted(sources), rap]
    ])
    out = args.out
    manifest = out / "hirsel-inputs.json"
    manifest.write_text(json.dumps(spec))
    for module in args.hirsel_entries:
        binary = out / f"hirsel-{module}"
        command = [
            compiler, "build", "--inputs", str(manifest),
            "--entry", f"awfy@1/{module}/main", "--store", str(out / "store"),
            "--backend", "llvm", "--release", "--debug-lines", "on", "--runtime", runtime,
            "--cc", str(llvm_path() / "bin/clang"),
            "--grant", "Stdout", "--json", "-o", str(binary),
        ]
        provenance = native_provenance(command, manifest)
        result = subprocess.run([ROOT / "tools/capped", *command],
                                env=getattr(args, "env", capped_env(out)), stdout=subprocess.PIPE)
        if result.returncode:
            sys.stderr.buffer.write(result.stdout)
            result.check_returncode()
        write_native_report(binary, result.stdout, provenance)


def port(source, destination, replacements):
    body = source.read_text()
    for old, new in replacements.items():
        if old not in body:
            raise ValueError(f"{source}: missing pool dispatch anchor {old!r}")
        body = body.replace(old, new)
    destination.write_text(body)


def goose_executable(compiler, source, output, env, cflags=()):
    generated = output.with_suffix(".c")
    command = [ROOT / "tools/capped"]
    subprocess.run([*command, compiler, "-O2", "--standalone", "-o", generated, source],
                   env=env, check=True)
    subprocess.run([*command, "clang", "-O2", "-w", *cflags, generated, "-o", output,
                    "-lm", "-pthread", "-ldl"], env=env, check=True)


def goose(args, matched=False):
    source = args.out / "goose-src"
    shutil.copytree(HERE / "goose", source, dirs_exist_ok=True)
    (source / "rap.goose").write_text(
        f"namespace rap;\nfn document() -> const u8[:] {{ return {fixture(args.upstream)}; }}\n")
    if matched:
        for variant in (HERE / "pool-ports/goose").glob("*.goose"):
            shutil.copyfile(variant, source / variant.name)
        port(source / "harness.goose", source / "pool_index.goose", {
            "import havlak;": "import havlak_pool;", "havlak::": "havlak_pool::",
            "import lists;": "import list_index;", "lists::": "list_index::",
        })
    env = getattr(args, "env", capped_env(args.out))
    for module in args.goose_entries:
        goose_executable(args.goose, source / f"{module}.goose", args.out / f"goose-{module}",
                         env, ("-ffp-contract=off", "-gline-tables-only"))


def rust(args, matched=False):
    sources = [("harness", args.rust / "src/main.rs")]
    if getattr(args, "hello", True):
        sources.append(("hello", HERE / "hello.rs"))
    if matched:
        source = args.out / "rust-pool-src"
        shutil.copytree(args.rust / "src", source)
        variants = sorted((HERE / "pool-ports/rust").glob("*.rs"))
        for variant in variants:
            shutil.copyfile(variant, source / variant.name)
        main = source / "main.rs"
        port(main, main, {
            "mod som;": "mod som;\n" + "\n".join(f"mod {variant.stem};" for variant in variants),
            "havlak::inner(inner)": "havlak_pool::benchmark(inner)",
            "cd::inner(inner)": "cd_pool::inner(inner)",
            "deltablue::inner(inner)": "deltablue_pool::inner(inner)",
            "small::list_inner(inner)": "(0..inner).all(|_| linked_small::list() == 10)",
            "small::towers_inner(inner)": "(0..inner).all(|_| linked_small::towers() == 8191)",
            "    match name {\n": '    match name {\n'
            '        "JsonPool" => (0..inner).all(|_| json_pool::benchmark() == 156),\n'
            '        "StoragePool" => (0..inner).all(|_| trees_pool::benchmark() == 5461),\n',
        })
        sources.append(("pool_index", main))
    for module, source in sources:
        subprocess.run([ROOT / "tools/capped", "rustc", "--edition=2021", "-C", "opt-level=3", "-C", "debuginfo=1",
                        source, "-o", args.out / f"rust-{module}"],
                       env=getattr(args, "env", capped_env(args.out)), check=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("out", type=Path)
    parser.add_argument("--upstream", type=Path, required=True, help="pinned AWFY checkout")
    parser.add_argument("--artifacts", type=Path, help="JSON from core/tools/measurement_artifacts.py")
    parser.add_argument("--hirsel-entries", nargs="+", default=["hello", "harness", "pools"])
    parser.add_argument("--goose-entries", choices=["hello", "harness", "pools"], nargs="+",
                        default=["hello", "harness", "pools"])
    parser.add_argument("--goose", type=Path, help="Goose release compiler")
    parser.add_argument("--rust", type=Path, help="Resid's bench/v2/t1-awfy/rust directory")
    args = parser.parse_args()
    args.out = args.out.resolve()
    args.out.mkdir(parents=True, exist_ok=True)
    if args.artifacts:
        hirsel(args)
    if args.goose:
        goose(args)
    if args.rust:
        rust(args, matched=True)


if __name__ == "__main__":
    main()
