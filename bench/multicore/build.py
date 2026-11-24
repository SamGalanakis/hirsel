#!/usr/bin/env python3
"""Build the multicore benchmark programs outside the repository."""
import argparse
from html import unescape
import importlib.util
import json
import os
from pathlib import Path
import re
import shutil
import socket
import subprocess

BENCH = Path(__file__).resolve().parent
ROOT = BENCH.parents[1]
GAME_REV = "40296663ed350d5fe4a6ab5e367bab61cb77c219"
GAME_ROWS = {
    "mandelbrot": (4, 4),
    "spectralnorm": (6, 4),
    "fannkuchredux": (6, 3),
    "knucleotide": (3, 7),
    "binarytrees": (5, 2),
}

STANDARD_RUST_SIZES = {
    "mandelbrot": 16000,
    "spectralnorm": 5500,
    "fannkuchredux": 12,
    "knucleotide": 25000000,
    "binarytrees": 21,
}

RUST_KERNEL = {
    "mandelbrot": ("    rows.par_chunks_mut", '    println!("P4'),
    "spectralnorm": ("    let answer = simd::spectralnorm", '    println!("{:.9}'),
    "fannkuchredux": ("    let (checksum, max_rev) = fannkuchredux", '    println!("{}'),
    "knucleotide": ("    let (results, (mono, di))", "    Freq(1).print(&mono)"),
    "binarytrees": ("    let messages =", "    for message in messages"),
}


def run(command, **kwargs):
    subprocess.run(list(map(str, command)), check=True, **kwargs)


def source(game, case, language, version):
    file = f"public/program/{case}-{language}-{version}.html"
    html = subprocess.check_output(
        ["git", "-C", game, "show", f"{GAME_REV}:{file}"], text=True
    )
    return unescape(re.sub(r"<[^>]*>", "", html.split("<pre>", 1)[1].split("</pre>", 1)[0])).lstrip("\n")


def replacement(text, old, new):
    if old not in text:
        raise ValueError(f"upstream source has changed near {old!r}")
    return text.replace(old, new)


def go_source(game, case, version):
    text = source(game, case, "go", version)
    # Keep upstream job decomposition while making the runtime's worker limit explicit.
    if case == "mandelbrot":
        text = replacement(text, "runtime.NumCPU() * 2\n    runtime.GOMAXPROCS(pool)",
                           "runtime.GOMAXPROCS(0) * 2")
        text = replacement(text, '    "sync/atomic"', '    "sync/atomic"\n    "time"')
        text = replacement(text, "    // start pool workers", "    start := time.Now()\n    // start pool workers")
        text = replacement(text, "    wg.Wait()", """    wg.Wait()
    elapsed := time.Since(start).Nanoseconds()
    fmt.Fprintf(os.Stderr, "multicore-kernel-ns=%d\\n", elapsed)""")
    elif case == "spectralnorm":
        text = replacement(text, "var nCPU = 4", "var nCPU = runtime.GOMAXPROCS(0)")
        text = replacement(text, "   runtime.GOMAXPROCS(nCPU)\n", "")
    elif case == "fannkuchredux":
        text = replacement(text, '    "runtime"\n', "")
        text = replacement(text, "    runtime.GOMAXPROCS(4)\n", "")
    elif case == "knucleotide":
        text = replacement(text, "runtime.NumCPU()", "runtime.GOMAXPROCS(0)")
    return text


def rust_source(game, case, version):
    text = source(game, case, "rust", version)
    if case == "knucleotide":
        text = replacement(text, "use std::thread;", "extern crate rayon;\nuse rayon::prelude::*;")
        start = text.index("    // In reverse to spawn big tasks first")
        text = text[:start] + """    let (results, (mono, di)) = rayon::join(
        || occs.into_par_iter().map(|item| {
            let counts = gen_freq(&input, item.0.len());
            (item, counts)
        }).collect::<Vec<_>>(),
        || rayon::join(|| gen_freq(&input, 1), || gen_freq(&input, 2)),
    );
    Freq(1).print(&mono);
    Freq(2).print(&di);
    for (item, freq) in results.into_iter().rev() {
        item.print(&freq);
    }
}
"""
    return text


def time_rust_kernel(text, case):
    # Pin the unchanged upstream kernel between preparation and output.
    before, after = RUST_KERNEL[case]
    text = replacement(text, before, """    rayon::ThreadPoolBuilder::new().build_global().unwrap();
    let kernel_start = std::time::Instant::now();
""" + before)
    return replacement(text, after, '''    let kernel_ns = kernel_start.elapsed().as_nanos();
    eprintln!("multicore-kernel-ns={}", kernel_ns);
''' + after)


def build_rust(out, game, standard, s0=False, regimes=False, *,
               prefix=(ROOT / "tools/capped",), jobs=2):
    path = out / "rust"
    path.mkdir(exist_ok=True)
    crate = BENCH / "rust"
    if regimes:
        binaries = ["--bin", "regimes"]
    elif s0:
        binaries = ["--bin", "runtime", "--bin", "s0"]
    else:
        binaries = ["--bin", "runtime"]
    cargo = subprocess.check_output(
        [*prefix, "cargo", "build", "--release", "--locked", f"-j{jobs}",
         "--manifest-path", str(crate / "Cargo.toml"), "--target-dir", str(out / "cargo"),
         *binaries, "--message-format=json"],
        cwd=crate, env=os.environ | {"HIRSEL_MEMORY_LIMIT": "2G", "HIRSEL_JOBS": str(jobs)},
        text=True,
    )
    externs = {}
    for line in cargo.splitlines():
        artifact = json.loads(line)
        if artifact["reason"] == "compiler-artifact":
            target = artifact["target"]["name"]
            for file in artifact["filenames"]:
                if file.endswith(".rlib"):
                    externs[target] = file
            if target == "runtime" and artifact["executable"]:
                shutil.copy2(artifact["executable"], out / "tokio")
            if target in {"s0", "regimes"} and artifact["executable"]:
                shutil.copy2(artifact["executable"], path / target)
    if s0 or regimes:
        return
    rustc = subprocess.check_output(
        ["rustup", "which", "--toolchain", "1.99.0", "rustc"], text=True
    ).strip()
    for case, (version, _) in GAME_ROWS.items():
        src = out / f"{case}.rs"
        text = rust_source(game, case, version)
        if standard or case == "mandelbrot":
            text = time_rust_kernel(text, case)
        src.write_text(text)
        deps = [arg for name in ("rayon", "bumpalo", "hashbrown")
                for arg in ("--extern", f"{name}={externs[name]}")]
        run([rustc, src, "-C", "opt-level=3", "-C", "target-cpu=ivybridge", "-C", "codegen-units=1",
             "-L", f"dependency={out / 'cargo/release/deps'}", *deps, "-o", path / case])

    if standard:
        src = out / "fasta.rs"
        src.write_text(source(game, "fasta", "rust", 1))
        fasta = path / "fasta"
        run([rustc, src, "-C", "opt-level=3", "-o", fasta])
        with (out / "input.fasta").open("wb") as file:
            run([fasta, STANDARD_RUST_SIZES["knucleotide"]], stdout=file)


def build_go(out, game, go):
    path = out / "go"
    path.mkdir(exist_ok=True)
    env = os.environ | {"GOAMD64": "v2", "GOMAXPROCS": "2", "GOTOOLCHAIN": "local"}
    run([go, "build", "-p", "2", "-o", path / "runtime",
         BENCH / "go/runtime.go", BENCH / "go/regimes.go"], env=env)
    if game is None:
        return
    for case, (_, version) in GAME_ROWS.items():
        src = out / f"{case}.go"
        src.write_text(go_source(game, case, version))
        run([go, "build", "-o", path / case, src], env=env)
    src = out / "fasta.go"
    src.write_text(source(game, "fasta", "go", 1))
    run([go, "build", "-o", path / "fasta", src], env=env)
    with (out / "input.fasta").open("wb") as file:
        run([path / "fasta", "120000"], stdout=file, env=env)


def hirsel_inputs(src):
    core = {"name": "core", "version": "1"}
    std = {"name": "std", "version": "1"}
    app = {"name": "multicore", "version": "1"}

    def unit(file, modules, signatures):
        return {"file": str(file), "modules": modules, "signatures": signatures}

    def library(name):
        directory = ROOT / "core" / name
        return [
            unit(file, list(relative.with_suffix("").parts), True)
            for file in sorted(directory.rglob("*.hirsel"))
            if (relative := file.relative_to(directory)).parts[0] != "tests"
        ]

    sources = [unit(src, [src.stem], False)]
    if src.stem == "matmul_t16":
        sources += [unit(ROOT / "core/langarena" / f"{name}.hirsel", [name], False)
                    for name in ("matmul", "helper", "harness")]

    return {"core": core, "packages": [
        {"package": core, "dependencies": [], "sources": library("core")},
        {"package": std, "dependencies": [core], "sources": library("std")},
        {"package": app, "dependencies": [core, std], "sources": sources},
    ]}


def build_hirsel(out, compiler, runtime, engine, s0=False, regimes=False, *,
                 prefix=(ROOT / "tools/capped",), jobs=1):
    path = out / "hirsel"
    path.mkdir(exist_ok=True)
    llvm = ROOT / "tools/buck2/toolchains/llvm-files"
    env = os.environ | {"HIRSEL_MEMORY_LIMIT": "2G", "HIRSEL_JOBS": str(jobs)}
    env["LD_LIBRARY_PATH"] = os.pathsep.join(filter(None, [
        str(llvm / "lib"), os.environ.get("LD_LIBRARY_PATH")
    ]))
    succeeded = True
    for src in sorted((BENCH / "hirsel").glob("*.hirsel")):
        case = src.stem
        if (case == "regimes") != regimes:
            continue
        if s0 and case not in {"runtime", "matmul_t16"}:
            continue
        if not s0 and case == "matmul_t16":
            continue
        inputs = out / f"{case}.json"
        inputs.write_text(json.dumps(hirsel_inputs(src)))
        # Build independently so a refusal in one port still leaves the other binaries usable.
        log = out / f"hirsel-{case}.log"
        common = [
            "--inputs", inputs, "--entry", f"multicore@1/{case}/main",
            "--backend", engine, "--cc", llvm / "bin/clang", "--store", out / "store",
        ]
        if engine == "llvm":
            common.append("--release")
        if regimes:
            common += ["--debug-lines", "on"]
        grants = ["--grant", "Stdout", "--grant", "Tasks", "--limit", "268435456"]
        if case in {"knucleotide", "regimes"}:
            grants += ["--dir", out]
        if case == "mandelbrot" or regimes:
            grants += ["--grant", "Clock", "--grant", "Stderr"]
        if regimes:
            grants += ["--grant", "Net=Listen 127.0.0.1/32 0-65535"]
        command = [
            *prefix, compiler, "build", *common, *grants,
            "--runtime", runtime, "--json", "-o", path / case,
        ]
        with log.open("wb") as file:
            result = subprocess.run(list(map(str, command)), env=env, stdout=file, stderr=subprocess.STDOUT)
        if result.returncode:
            succeeded = False
            (path / case).unlink(missing_ok=True)
            print(f"Hirsel {case}: exit {result.returncode}; see {log}", flush=True)
    return succeeded


def build_wire(out, *, prefix=(ROOT / "tools/capped",), jobs=2):
    env = os.environ | {"HIRSEL_MEMORY_LIMIT": "2G", "HIRSEL_JOBS": str(jobs)}
    run([*prefix, "cargo", "build", f"-j{jobs}", "--release", "--locked", "--bins",
         "--manifest-path", ROOT / "bench/wire/Cargo.toml", "--target-dir", out / "wire"], env=env)


def build_checkbox(out, compiler, runtime, *, prefix=(ROOT / "tools/capped",), jobs=2, wire=True):
    if wire:
        build_wire(out, prefix=prefix, jobs=jobs)
    env = os.environ | {"HIRSEL_MEMORY_LIMIT": "2G", "HIRSEL_JOBS": str(jobs)}
    spec = importlib.util.spec_from_file_location(
        "checkbox_run", ROOT / "core/programs/checkbox/run.py")
    checkbox = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(checkbox)
    work = out / "checkbox"
    web = work / "web"
    web.mkdir(parents=True, exist_ok=True)
    with socket.socket() as listener:
        listener.bind(("127.0.0.1", 0))
        port = listener.getsockname()[1]
    listen = f"127.0.0.1:{port}"
    clang = ROOT / "tools/buck2/toolchains/llvm-files/bin/clang"
    with (work / "build.log").open("wb") as log:
        binary = checkbox.build(
            compiler, work, web, listen, prefix=prefix,
            flags=("--backend", "llvm", "--release", "--runtime", runtime, "--cc", clang, "--json"),
            stdout=log, env=env, entry="shipping_benchmark",
        )
    run([*prefix, compiler, "sdk", "ts", binary, "--output", web,
         "--n-place", checkbox.N_PLACE], env=env)
    run(["node", "--input-type=module", "-e", """
import { writeFile } from 'node:fs/promises';
import { pathToFileURL } from 'node:url';
const base = pathToFileURL(process.argv[1] + '/');
const { Frame } = await import(new URL('frame.js', base));
const { want } = await import(new URL('entries.js', base));
await writeFile(process.argv[2], Frame.encode({tag: 'Hello', values: [{tag: 'None'}, want]}));
""", web, work / "wants.bin"])
    (work / "port").write_text(str(port) + "\n")


def build_goose(out, compiler):
    path = out / "goose"
    path.mkdir(exist_ok=True)
    for case in (*GAME_ROWS, "runtime"):
        src = BENCH / "goose" / f"{case}.goose"
        c_file = out / f"goose-{src.stem}.c"
        run([compiler, "-O2", "--standalone", "-o", c_file, src])
        run(["clang", "-O2", c_file, "-o", path / src.stem, "-lm", "-pthread"])


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--out", type=Path, required=True)
    parser.add_argument("--game", type=Path, help=f"Benchmarks Game clone containing {GAME_REV}")
    parser.add_argument("--go", default="go")
    parser.add_argument("--goose", type=Path, help="Goose compiler built from d5ae5bbd")
    parser.add_argument("--hirsel", type=Path, help="release Hirsel driver")
    parser.add_argument("--runtime", type=Path, help="native runtime archive (shipping archive for --regimes)")
    parser.add_argument("--engine", choices=["llvm", "cranelift"], default="llvm")
    selection = parser.add_mutually_exclusive_group()
    selection.add_argument("--languages", nargs="+", choices=["rust", "go", "hirsel", "goose"])
    selection.add_argument("--regimes", action="store_true", help="build the scheduler grain fixtures")
    selection.add_argument("--s0", action="store_true", help="build only the S0 Hirsel, Tokio and Rust fixtures")
    selection.add_argument("--standard-rust", action="store_true", help="build published Rust data inputs and phase timings")
    options = parser.parse_args()
    if options.regimes:
        languages = ["rust", "go", "hirsel"]
    elif options.s0:
        languages = ["rust", "hirsel"]
    else:
        languages = ["rust"] if options.standard_rust else options.languages or ["rust", "go", "hirsel", "goose"]
    out = options.out.resolve()
    if out.is_relative_to(ROOT):
        parser.error("--out must be outside the repository")
    for needed, for_languages in [
        ("game", set() if options.s0 or options.regimes else {"rust", "go"}), ("goose", {"goose"}),
        ("hirsel", {"hirsel"}), ("runtime", {"hirsel"}),
    ]:
        if for_languages.intersection(languages) and getattr(options, needed) is None:
            parser.error(f"--{needed} is required for {', '.join(sorted(for_languages))}")
    out.mkdir(parents=True, exist_ok=True)
    succeeded = True
    for language in dict.fromkeys(languages):
        if language == "rust":
            build_rust(out, options.game, options.standard_rust, options.s0, options.regimes)
        elif language == "go":
            build_go(out, options.game.resolve() if options.game else None, options.go)
        elif language == "hirsel":
            succeeded = build_hirsel(
                out, options.hirsel.resolve(), options.runtime.resolve(), options.engine,
                options.s0, options.regimes,
            )
        elif language == "goose":
            build_goose(out, options.goose.resolve())
    if options.regimes:
        build_checkbox(out, options.hirsel.resolve(), options.runtime.resolve())
    return 0 if succeeded else 1


if __name__ == "__main__":
    raise SystemExit(main())
