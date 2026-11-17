#!/usr/bin/env python3
"""Build the multicore benchmark programs outside the repository."""
import argparse
from html import unescape
import json
import os
from pathlib import Path
import re
import shutil
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


def build_rust(out, game):
    path = out / "rust"
    path.mkdir(exist_ok=True)
    crate = BENCH / "rust"
    cargo = subprocess.check_output(
        [str(ROOT / "tools/capped"), "cargo", "build", "--release", "--locked",
         "--manifest-path", str(crate / "Cargo.toml"), "--target-dir", str(out / "cargo"),
         "--bin", "runtime", "--message-format=json"],
        cwd=crate, env=os.environ | {"HIRSEL_MEMORY_LIMIT": "2G", "HIRSEL_JOBS": "2"},
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
    rustc = subprocess.check_output(
        ["rustup", "which", "--toolchain", "1.99.0", "rustc"], text=True
    ).strip()
    for case, (version, _) in GAME_ROWS.items():
        src = out / f"{case}.rs"
        src.write_text(rust_source(game, case, version))
        deps = [arg for name in ("rayon", "bumpalo", "hashbrown")
                for arg in ("--extern", f"{name}={externs[name]}")]
        run([rustc, src, "-C", "opt-level=3", "-C", "target-cpu=ivybridge", "-C", "codegen-units=1",
             "-L", f"dependency={out / 'cargo/release/deps'}", *deps, "-o", path / case])


def build_go(out, game, go):
    path = out / "go"
    path.mkdir(exist_ok=True)
    env = os.environ | {"GOAMD64": "v2", "GOMAXPROCS": "2", "GOTOOLCHAIN": "local"}
    for case, (_, version) in GAME_ROWS.items():
        src = out / f"{case}.go"
        src.write_text(go_source(game, case, version))
        run([go, "build", "-o", path / case, src], env=env)
    src = out / "fasta.go"
    src.write_text(source(game, "fasta", "go", 1))
    run([go, "build", "-o", path / "fasta", src], env=env)
    run([go, "build", "-o", path / "runtime", BENCH / "go/runtime.go"], env=env)
    with (out / "input.fasta").open("wb") as file:
        run([path / "fasta", "120000"], stdout=file, env=env)


def hirsel_inputs(src):
    core = {"name": "core", "version": "1"}
    std = {"name": "std", "version": "1"}
    app = {"name": "multicore", "version": "1"}

    def unit(file, signatures):
        return {"file": str(file), "modules": [file.stem], "signatures": signatures}

    def library(name):
        return [unit(file, True) for file in sorted((ROOT / "core" / name).glob("*.hirsel"))]

    return {"core": core, "packages": [
        {"package": core, "dependencies": [], "sources": library("core")},
        {"package": std, "dependencies": [core], "sources": library("std")},
        {"package": app, "dependencies": [core, std], "sources": [unit(src, False)]},
    ]}


def build_hirsel(out, compiler, runtime, engine):
    path = out / "hirsel"
    path.mkdir(exist_ok=True)
    llvm = ROOT / "tools/buck2/toolchains/llvm-files"
    env = os.environ | {"HIRSEL_MEMORY_LIMIT": "2G", "HIRSEL_JOBS": "1"}
    env["LD_LIBRARY_PATH"] = os.pathsep.join(filter(None, [
        str(llvm / "lib"), os.environ.get("LD_LIBRARY_PATH")
    ]))
    succeeded = True
    for src in sorted((BENCH / "hirsel").glob("*.hirsel")):
        case = src.stem
        inputs = out / f"{case}.json"
        inputs.write_text(json.dumps(hirsel_inputs(src)))
        # Build independently so a refusal in one port still leaves the other binaries usable.
        log = out / f"hirsel-{case}.log"
        command = [
            ROOT / "tools/capped", compiler, "build", "--inputs", inputs,
            "--entry", f"multicore@1/{case}/main", "--backend", engine,
            "--runtime", runtime, "--cc", llvm / "bin/clang", "--store", out / "store",
            "--dir", out, "--grant", "Stdout", "--grant", "Tasks", "--limit", "268435456",
            "--json", "-o", path / case,
        ]
        if engine == "llvm":
            command.append("--release")
        with log.open("wb") as file:
            result = subprocess.run(list(map(str, command)), env=env, stdout=file, stderr=subprocess.STDOUT)
        if result.returncode:
            succeeded = False
            (path / case).unlink(missing_ok=True)
            print(f"Hirsel {case}: exit {result.returncode}; see {log}", flush=True)
    return succeeded


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
    parser.add_argument("--runtime", type=Path, help="shipping native runtime archive")
    parser.add_argument("--engine", choices=["llvm", "cranelift"], default="llvm")
    parser.add_argument("--languages", nargs="+", choices=["rust", "go", "hirsel", "goose"],
                        default=["rust", "go", "hirsel", "goose"])
    options = parser.parse_args()
    out = options.out.resolve()
    if out.is_relative_to(ROOT):
        parser.error("--out must be outside the repository")
    for needed, for_languages in [
        ("game", {"rust", "go"}), ("goose", {"goose"}),
        ("hirsel", {"hirsel"}), ("runtime", {"hirsel"}),
    ]:
        if for_languages.intersection(options.languages) and getattr(options, needed) is None:
            parser.error(f"--{needed} is required for {', '.join(sorted(for_languages))}")
    out.mkdir(parents=True, exist_ok=True)
    succeeded = True
    for language in dict.fromkeys(options.languages):
        if language == "rust":
            build_rust(out, options.game.resolve())
        elif language == "go":
            build_go(out, options.game.resolve(), options.go)
        elif language == "hirsel":
            succeeded = build_hirsel(out, options.hirsel.resolve(), options.runtime.resolve(), options.engine)
        elif language == "goose":
            build_goose(out, options.goose.resolve())
    return 0 if succeeded else 1


if __name__ == "__main__":
    raise SystemExit(main())
