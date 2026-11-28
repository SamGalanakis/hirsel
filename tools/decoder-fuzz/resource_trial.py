#!/usr/bin/env python3
"""Measure the least evaluator step budget for the HTTP resource laws.

--source-root can point at a parent checkout for the comparison arm. Both arms
use this checkout's law bodies, partition schedules and logical byte budgets.
"""
import argparse
import json
from pathlib import Path
import sys
import tempfile

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "core/tools"))
from contract_exec import Evaluator, source_manifest


def trials():
    for fields in (1, 2, 4, 8, 16):
        size = 64 * fields
        for width in (1, 13, size):
            yield ("http1", size, width, "0",
                   f"let raw = envelope_request({fields})\nlet ends = envelope_partition({size}, {width})",
                   f"scheduled_head(raw, ends, Complete, {size})", "Some(_)")
    for width in (13, 62):
        yield ("http_client_reader", 62, width,
               "2 * option.unwrap(charge.seq(bytes[u8], http1.HEAD_MAX))",
               "let raw = response_example()\nif list.len(raw) != 62 { return 0 }",
               f"metered_response(raw, {width})", "true")
    # The final four pipelined bytes are available but unconsumed.
    size = 32 * len(b'1;ext="v"\r\na\r\n') + len(b"0\r\nX: end\r\n\r\n")
    for width in (1, 13, size + 4):
        yield ("http", size, width, "option.unwrap(charge.seq(bytes[u8], list.len(payload)))",
               f"let (wire, payload) = envelope_chunks()\nif list.len(wire) != {size + 4} {{ return 0 }}",
               f"chunk_stream(wire, payload, {width})", "true")


def entry(name, setup, call, success, room):
    # Require the upper endpoint to succeed before using it in bisection.
    return f'''
pub fn {name}() -> u64 {{
    {setup}
    let trial = bound bytes {room} {{
        let mut lo: u64 = 0
        let mut hi: u64 = 65536
        let mut mid = hi
        loop {{
            let r = bound steps mid {{ {call} }}
            match r {{
                Ok({success}) => {{ hi = mid }},
                Error(_) => if mid == hi {{ return 0 }} else {{ lo = mid + 1 }},
                _ => return 0
            }}
            if lo == hi {{ break }}
            mid = lo + (hi - lo) / 2
        }}
        hi
    }}
    result.unwrap(trial)
}}
'''


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--evaluator", required=True, type=Path)
    parser.add_argument("--source-root", default=ROOT, type=Path)
    args = parser.parse_args()
    by_module = {}
    for module, size, width, room, setup, call, success in trials():
        name = f"measure_{size}_{width}"
        by_module.setdefault(module, []).append((name, size, width, entry(name, setup, call, success, room)))

    with tempfile.TemporaryDirectory() as tmp, Evaluator(args.evaluator.resolve()) as evaluator:
        for module, cases in by_module.items():
            source = Path(tmp) / f"{module}.hirsel"
            source.write_text((ROOT / f"core/std/tests/{module}.hirsel").read_text()
                              + "".join(body for _, _, _, body in cases))
            manifest = Path(tmp) / f"{module}.json"
            manifest.write_text(json.dumps(source_manifest(args.source_root.resolve(), "std", ["tests", module], source)))
            for name, size, width, _ in cases:
                result = evaluator.evaluate(manifest, f"std@1/tests/{module}/{name}", instructions=1_000_000_000)
                result.check_returncode()
                row = json.loads(result.stdout)
                outcome = row.get("outcome", {})
                if outcome.get("kind") != "returned" or row.get("used") != 0 or not outcome["values"][0]:
                    raise ValueError(f"{module}/{name}: {row}")
                print(json.dumps(dict(module=module, consumed=size, width=width, steps=outcome["values"][0])))


if __name__ == "__main__":
    main()
