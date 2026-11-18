#!/usr/bin/env python3
"""Run one checkbox law against a fresh loopback endpoint."""
import argparse
from pathlib import Path
import socket
import subprocess
import sys
from tempfile import TemporaryDirectory

from run import PROGRAM, ROOT, build, command, prepare_web, wait_for_listener

sys.path.insert(0, str(ROOT / "tools/tests"))
from wire_process import stop_process

BROWSER = {"canvas_click", "browser_ticks"}
WIRE = {
    "bit_pixels", "toggle_smoke", "toggle_no_rerun", "session_limit_503",
    "principal_limit_1013", "call_rate_overloaded",
}
SDK_MUTANTS = {"resend_new_p", "skip_resubscribe_after_reset", "clear_on_first_batch"}
SERVER_EDITS = {
    "bit_order_msb": ("main", "1u8 << int.narrow(i.index & 7u32)", "128u8 >> int.narrow(i.index & 7u32)"),
    "unchecked_u32_arg": ("main", "if i < 1000000 {", "if true {"),
    "range_in_board": ("main", "    store.runner.cover(store.store.shape(h.chunks), Unbounded, Unbounded)",
                      "    let _ = result.unwrap(store.store.range(tx, store.store.shape(h.chunks), Unbounded, Unbounded))\n"
                      "    store.runner.cover(store.store.shape(h.chunks), Unbounded, Unbounded)"),
    "no_publish_to_others": ("runner", "        let has_content = next.reset",
                             "        if list.len(rows) == 0u64 && list.len(next.complete) == 0u64 { next.rows = list.new() }\n"
                             "        let has_content = next.reset"),

}


def replace(text, before, after):
    assert text.count(before) == 1, before
    return text.replace(before, after)


def scratch_sources(work, case, mutant):
    paths = {
        "main": PROGRAM / "main.hirsel",
        "runner": PROGRAM.parent / "store/runner.hirsel",
        "ingress": PROGRAM.parent / "store/ingress.hirsel",
    }
    changed = {}
    if case == "toggle_no_rerun":
        # Wire-only witness in the scratch server. An empty key cannot be a
        # data-tree key. Count one marker after each query body returns a Cover.
        original = "        Some(Query(_, _, go)) => go(s, h, arg, prior, old, sub, rows),"
        marker = """        Some(Query(_, _, go)) => {
            let answer = go(s, h, arg, prior, old, sub, rows)
            if let Covered(_) = answer {
                result.unwrap(log.append(rows, Row(sub: sub, key: list.new(), value: None)))
            }
            take answer
        },"""
        changed["runner"] = replace(paths["runner"].read_text(), original, marker)
    if mutant in SERVER_EDITS:
        module, before, after = SERVER_EDITS[mutant]
        changed[module] = replace(changed.get(module, paths[module].read_text()), before, after)
    for module, text in changed.items():
        paths[module] = work / (module + ".hirsel")
        paths[module].write_text(text)
    return paths.pop("main"), paths


def qualify(compiler, work, case, mutant, clients, toggles, pace, seed):
    with socket.socket() as reservation:
        reservation.bind(("127.0.0.1", 0))
        port = reservation.getsockname()[1]
    listen = f"127.0.0.1:{port}"
    origin = f"http://{listen}/"
    web = work / "web"
    web.mkdir()
    source, store = scratch_sources(work, case, mutant)
    executable = build(compiler, work, web, listen, source, store)
    prepare_web(compiler, executable, web)
    overrides = []
    if case in ("toggle_no_rerun", "converge"):
        overrides = ["--per-ip-sessions", "20000", "--per-ip-new-principals", "20000"]
        print("per_ip_sessions=20000 per_ip_new_principals=20000/min", flush=True)
    if case in BROWSER:
        law = ["browser-laws.mjs", case, origin]
    elif case in WIRE:
        law = ["wire-laws.mjs", case, origin, web]
    else:
        law = ["converge.mjs", origin, web, clients, toggles, pace, seed]
        if mutant in SDK_MUTANTS:
            law.append(mutant)
    with subprocess.Popen(command(executable, listen, overrides), cwd=ROOT) as server:
        try:
            wait_for_listener(server, listen)
            return subprocess.run(["node", PROGRAM / law[0], *map(str, law[1:])], cwd=ROOT).returncode
        finally:
            stop_process(server)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--hirsel", type=Path, required=True)
    parser.add_argument("--scratch", type=Path)
    parser.add_argument("--mutant", choices=sorted(SDK_MUTANTS | SERVER_EDITS.keys()))
    parser.add_argument("--clients", default=64, type=int)
    parser.add_argument("--toggles", default=1000, type=int)
    parser.add_argument("--pace", default=10, type=float)
    parser.add_argument("--seed", default=14059, type=int)
    parser.add_argument("case", choices=sorted(BROWSER | WIRE | {"converge"}))
    args = parser.parse_args()
    if args.mutant in SDK_MUTANTS and args.case != "converge":
        parser.error(f"{args.mutant} requires converge")
    with TemporaryDirectory(prefix="checkbox-", dir=args.scratch) as directory:
        return qualify(args.hirsel.resolve(), Path(directory), args.case, args.mutant,
                       args.clients, args.toggles, args.pace, args.seed)


if __name__ == "__main__":
    raise SystemExit(main())
