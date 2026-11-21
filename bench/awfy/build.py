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
    return text


def hirsel(args, matched=False):
    artifacts = json.loads(args.artifacts.read_text())
    compiler, runtime = (artifacts[key] for key in ("compiler", "runtime"))
    spec = inputs(ROOT, "harness")
    rap_data = args.out / "rap.json"
    rap_data.write_bytes(fixture(args.upstream).encode("utf-8"))
    rap_digest = sha256(rap_data)
    sources = list((HERE / "hirsel").glob("*.hirsel"))
    if matched:
        variants = HERE / "pool-ports/hirsel"
        harness = args.out / "pool_index.hirsel"
        port(HERE / "hirsel/harness.hirsel", harness, {
            "havlak.benchmark(inner)": "havlak_pool.benchmark(inner)",
            "cd.benchmark(inner)": "cd_pool.benchmark(inner)",
            "deltablue.benchmark(inner)": "deltablue_pool.benchmark(inner)",
            "lists.list_benchmark(first)": "linked_small.list_benchmark(first)",
            "trees.towers_benchmark(disks)": "linked_small.towers_benchmark(disks)",
        })
        sources += [harness, *variants.glob("*.hirsel")]
    spec["packages"][-1].update(package={"name": "awfy", "version": "1"}, sources=[
        {"file": str(source), "modules": [source.stem], "signatures": False}
        for source in sorted(sources)
    ])
    out = args.out
    manifest = out / "hirsel-inputs.json"
    manifest.write_text(json.dumps(spec))
    env = getattr(args, "env", capped_env(out))
    for module in args.hirsel_entries:
        binary = out / f"hirsel-{module}"
        command = [
            compiler, "build", "--inputs", str(manifest),
            "--entry", f"awfy@1/{module}/main", "--store", str(out / "store"),
            "--backend", "llvm", "--release", "--debug-lines", "on", "--runtime", runtime,
            "--cc", str(llvm_path() / "bin/clang"),
            "--grant", "Stdout", "--json", "-o", str(binary),
        ]
        provenance = native_provenance(command, manifest) | {"rap_sha256": rap_digest}
        result = subprocess.run([ROOT / "tools/capped", *command],
                                env=env, stdout=subprocess.PIPE)
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
    source = args.out / "rust-src"
    shutil.copytree(args.rust / "src", source)
    main = source / "main.rs"
    port(main, main, {
        "fn run(name: &str, inner: i32)": "fn run(name: &str, inner: i32, input: &str)",
        **{f"small::{name}_inner(inner)": f"small::{name}_inner(inner, input)"
           for name in ("bounce", "list", "permute", "sieve", "storage", "towers")},
        "small::queens_inner(inner)": "small::queens_inner(inner, input.as_bytes())",
        "json::inner(inner)": "json::inner(inner, input)",
        "richards::inner(inner)": "richards::inner(inner, input)",
        "if !run(name, inner)": "if !run(name, inner, args.get(4).map_or(name, String::as_str))",
    })
    small = source / "small.rs"
    port(small, small, {
        "use crate::som::Random;": """use crate::som::Random;

pub fn verify_numbers(inner: i32, input: &str, limit: i32, mut check: impl FnMut(i32) -> bool) -> bool {
    (0..inner).zip(input.split(',').cycle()).all(|(_, value)| {
        value.parse::<i32>().is_ok_and(|n| n > 0 && n <= limit && check(n))
    })
}""",
        "fn bounce()": "fn bounce(steps: i32)",
        "for _ in 0..50 {": "for _ in 0..steps {",
        "pub fn bounce_inner(inner: i32) -> bool { (0..inner).all(|_| bounce() == 1331) }":
            "pub fn bounce_inner(inner: i32, input: &str) -> bool { verify_numbers(inner, input, 50, |steps| bounce(steps) == 1331) }",
        "fn list()": "fn list(first: i32)",
        "make_list(15)": "make_list(first)",
        "let xn = x.unwrap().next.as_deref();": "let xn = x?.next.as_deref();",
        "let yn = y.unwrap().next.as_deref();": "let yn = y?.next.as_deref();",
        "let zn = z.unwrap().next.as_deref();": "let zn = z?.next.as_deref();",
        "let r = tail(a.as_deref(), b.as_deref(), c.as_deref()).unwrap();":
            "let Some(r) = tail(a.as_deref(), b.as_deref(), c.as_deref()) else { return 0; };",
        "pub fn list_inner(inner: i32) -> bool { (0..inner).all(|_| list() == 10) }":
            "pub fn list_inner(inner: i32, input: &str) -> bool { verify_numbers(inner, input, 15, |first| list(first) == 10) }",
        "pub fn permute_inner(inner: i32) -> bool {\n    (0..inner).all(|_| {":
            "pub fn permute_inner(inner: i32, input: &str) -> bool {\n    verify_numbers(inner, input, 6, |size| {",
        "p.permute(6);": "p.permute(size);",
        "pub fn sieve_inner(inner: i32) -> bool {\n    (0..inner).all(|_| {":
            "pub fn sieve_inner(inner: i32, input: &str) -> bool {\n    verify_numbers(inner, input, 5000, |size| {",
        "sieve(&mut flags, 5000)": "sieve(&mut flags, size)",
        "pub fn storage_inner(inner: i32) -> bool {\n    (0..inner).all(|_| {":
            "pub fn storage_inner(inner: i32, input: &str) -> bool {\n    verify_numbers(inner, input, 7, |depth| {",
        "build_tree_depth(7,": "build_tree_depth(depth,",
        "pub fn towers_inner(inner: i32) -> bool {\n    (0..inner).all(|_| {":
            "pub fn towers_inner(inner: i32, input: &str) -> bool {\n    verify_numbers(inner, input, 13, |disks| {",
        "t.build_tower_at(0, 13);": "t.build_tower_at(0, disks);",
        "t.move_disks(13, 0, 1);": "t.move_disks(disks, 0, 1);",
        "fn queens(&mut self)": "fn queens(&mut self, size: i32)",
        "self.place_queen(0)": "self.place_queen(0, size)",
        "fn place_queen(&mut self, c: i32)": "fn place_queen(&mut self, c: i32, size: i32)",
        "for r in 0..8 {": "for r in 0..size {",
        "if c == 7": "if c + 1 == size",
        "self.place_queen(c + 1)": "self.place_queen(c + 1, size)",
        """fn queens() -> bool {
    let mut q = Queens { free_maxs: vec![], free_rows: vec![], free_mins: vec![], queen_rows: vec![] };
    let mut result = true;
    for _ in 0..10 { result = result && q.queens(); }
    result
}

pub fn queens_inner(inner: i32) -> bool { (0..inner).all(|_| queens()) }""":
        """pub fn queens_inner(inner: i32, sizes: &[u8]) -> bool {
    if sizes.is_empty() { return false; }
    let mut sizes = sizes.iter().copied().cycle();
    (0..inner).all(|_| {
        let mut q = Queens { free_maxs: vec![], free_rows: vec![], free_mins: vec![], queen_rows: vec![] };
        let mut solved = 0;
        for digit in sizes.by_ref().take(10) {
            if !(b'4'..=b'8').contains(&digit) { return false; }
            if q.queens(i32::from(digit - b'0')) { solved += 1; }
        }
        solved == 10
    })
}""",
    })
    port(source / "json.rs", source / "json.rs", {
        "pub fn inner(inner: i32)": "pub fn inner(inner: i32, input: &str)",
        "Parser::new(RAP)": "Parser::new(input)",
        '\nconst RAP: &str = include_str!("rap.json");\n': "",
    })
    port(source / "richards.rs", source / "richards.rs", {
        "fn start(&mut self)": "fn start(&mut self, count: i32)",
        "control: 1, count: 10000": "control: 1, count",
        "pub fn inner(inner: i32) -> bool {\n    (0..inner).all(|_| {":
            "pub fn inner(inner: i32, input: &str) -> bool {\n    crate::small::verify_numbers(inner, input, 10000, |count| {",
        "s.start()": "s.start(count)",
    })
    sources = [("harness", main)]
    if getattr(args, "hello", True):
        sources.append(("hello", HERE / "hello.rs"))
    if matched:
        standard = source
        source = args.out / "rust-pool-src"
        shutil.copytree(standard, source)
        variants = sorted((HERE / "pool-ports/rust").glob("*.rs"))
        for variant in variants:
            shutil.copyfile(variant, source / variant.name)
        main = source / "main.rs"
        port(main, main, {
            "mod som;": "mod som;\n" + "\n".join(f"mod {variant.stem};" for variant in variants),
            "havlak::inner(inner)": "havlak_pool::benchmark(inner)",
            "cd::inner(inner)": "cd_pool::inner(inner)",
            "deltablue::inner(inner)": "deltablue_pool::inner(inner)",
            "small::list_inner(inner, input)": "small::verify_numbers(inner, input, 15, |n| linked_small::list(n.into()) == 10)",
            "small::towers_inner(inner, input)": "small::verify_numbers(inner, input, 13, |n| linked_small::towers(n as usize) == 8191)",
            "    match name {\n": '    match name {\n'
            '        "JsonPool" => (0..inner).all(|_| json_pool::benchmark(input.as_bytes()) == 156),\n'
            '        "StoragePool" => small::verify_numbers(inner, input, 7, |n| trees_pool::benchmark(n as u32) == 5461),\n',
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
    parser.add_argument("--hirsel-entries", nargs="+", default=["hello", "harness", "pools"],
                        help="Hirsel entry modules, including pool_index")
    parser.add_argument("--goose-entries", choices=["hello", "harness", "pools", "pool_index"], nargs="+",
                        default=["hello", "harness", "pools"])
    parser.add_argument("--goose", type=Path, help="Goose release compiler")
    parser.add_argument("--rust", type=Path, help="Resid's bench/v2/t1-awfy/rust directory")
    args = parser.parse_args()
    args.out = args.out.resolve()
    args.out.mkdir(parents=True, exist_ok=True)
    if args.artifacts:
        hirsel(args, matched="pool_index" in args.hirsel_entries)
    if args.goose:
        goose(args, matched="pool_index" in args.goose_entries)
    if args.rust:
        rust(args, matched=True)


if __name__ == "__main__":
    main()
