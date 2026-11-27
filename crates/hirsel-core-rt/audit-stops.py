#!/usr/bin/env python3
"""Which runtime rows can reach a run stop (PACK-13).

    audit-stops.py <libhirsel_core_rt.a> [class ...]

Reads the built runtime archive with objdump. A node is one section of one
member: Rust gives each function and each datum its own. An edge is a
relocation in one section against a symbol another defines, so calls, tail
calls, taken addresses and tables of functions are all edges, and the graph
over-approximates the call graph. An address computed at run time is the one
thing it cannot see.

For each row of the named classes (default pure and metered) it prints the
functions that call `run_stop::raise`, `run_stop::record` or
`process::uncontained_stop` directly and that the row reaches, with the
shortest path to each. It gates nothing: the disposition of each such
function is a judgement recorded in docs/spec/packing.md.
"""
import collections
import pathlib
import re
import subprocess
import sys

ROWS = pathlib.Path(__file__).resolve().parent.parent / "hirsel-mod/src/rows.rs"
SINK = re.compile(r"14hirsel_core_rt(8run_stop(5raise|6record)|7process16uncontained_stop)$")
SKIP = (".debug", ".eh_frame", ".gcc_except")


def load(archive):
    out = subprocess.run(
        ["objdump", "-t", "-r", archive], capture_output=True, text=True, check=True
    ).stdout
    shared, local = {}, {}
    names = collections.defaultdict(set)
    refs = collections.defaultdict(set)
    member = mode = section = None
    for line in out.splitlines():
        if m := re.match(r"^(\S+):\s+file format", line):
            member, mode = m.group(1), None
        elif line.startswith("SYMBOL TABLE"):
            mode = "symbols"
        elif m := re.match(r"^RELOCATION RECORDS FOR \[(.*)\]:", line):
            mode, section = "relocations", m.group(1)
        elif mode == "symbols":
            m = re.match(r"^[0-9a-f]{16} (.{7}) (\S+)\s+[0-9a-f]{16}\s*(.*)$", line)
            if not m or m.group(2) in ("*UND*", "*ABS*", "*COM*"):
                continue
            flags, name = m.group(1), re.sub(r"^\.hidden ", "", m.group(3).strip())
            node = (member, m.group(2))
            local[(member, name)] = node
            if flags[0] != "l":
                shared.setdefault(name, node)
            if "F" in flags:
                names[node].add(name)
        elif mode == "relocations" and not section.startswith(SKIP):
            if m := re.match(r"^[0-9a-f]{16}\s+\S+\s+(.*)$", line):
                symbol = re.sub(r"[+-]0x[0-9a-f]+$", "", m.group(1).strip())
                refs[(member, section)].add((member, symbol))
    graph = collections.defaultdict(set)
    for node, targets in refs.items():
        for key in targets:
            target = local.get(key) or shared.get(key[1])
            if target and target != node:
                graph[node].add(target)
    return graph, names, shared


def main():
    archive, classes = sys.argv[1], {c.lower() for c in sys.argv[2:]} or {"pure", "metered"}
    graph, names, shared = load(archive)
    sinks = {n for n, symbols in names.items() if any(SINK.search(s) for s in symbols)}
    assert len(sinks) == 3, f"expected the three stop entries, found {len(sinks)}"
    raisers = {n for n, targets in graph.items() if targets & sinks} - sinks

    def name(node):
        return min(names.get(node) or [node[1]])

    lines, reached, total = [], 0, 0
    for symbol, cls in re.findall(r'\("(\w+)", Class::(\w+)\)', ROWS.read_text()):
        if cls.lower() not in classes:
            continue
        total += 1
        root = shared.get(symbol)
        if root is None:
            lines.append(f"{cls}\t{symbol}\t(not in this archive)")
            continue
        before, queue = {root: None}, collections.deque([root])
        while queue:
            node = queue.popleft()
            for target in graph.get(node, ()):
                if target not in before:
                    before[target] = node
                    queue.append(target)
        hits = sorted(raisers & before.keys(), key=name)
        reached += bool(hits)
        for hit in hits:
            path, node = [], hit
            while node is not None:
                path.append(name(node))
                node = before[node]
            lines.append(f"{cls}\t{symbol}\t{' > '.join(reversed(path))}")
    text = "\n".join(lines)
    try:
        text = subprocess.run(["rustfilt"], input=text, capture_output=True, text=True).stdout
    except FileNotFoundError:
        pass
    print(text)
    print(f"{reached} of {total} rows reach a stop", file=sys.stderr)


main()
