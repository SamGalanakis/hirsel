#!/usr/bin/env python3
"""Split recorded tape bytes by kind: payload bytes of network reads (op 1,
READ: client and tool sockets), of crossing receives (op 27, RECV_BOUNDED),
of other operations, and the rest of the file (frames, issues, grants).

Usage: tape_kinds.py HIRSEL TAPE...   (prints one JSON object per tape and a total)
"""
import collections, json, os, re, subprocess, sys

KIND = {1: "network_in", 27: "crossing_in"}
REC = re.compile(r"^[<>] \d+ (OP|DONE|ISSUE|GRANT)\b.*?\bop=(\d+)")


def split(hirsel, tape):
    p = subprocess.Popen([hirsel, "tape", "show", tape, "--include-credentials"], stdout=subprocess.PIPE, text=True)
    c = collections.Counter()
    for line in p.stdout:
        m = REC.match(line)
        if not m:
            continue
        c["records"] += 1
        i = line.find("payload=[")
        if i < 0:
            continue
        body = line[i + 9:line.rfind("]")]
        n = body.count(",") + 1 if body.strip() else 0
        c[KIND.get(int(m[2]), "other") + "_payload"] += n
    p.wait()
    c["file_bytes"] = os.path.getsize(tape)
    c["framing"] = c["file_bytes"] - sum(v for k, v in c.items() if k.endswith("_payload"))
    return c


def main():
    hirsel, tapes = sys.argv[1], sys.argv[2:]
    tot = collections.Counter()
    for t in tapes:
        c = split(hirsel, t)
        print(json.dumps({"tape": os.path.basename(t), **c}))
        tot.update(c)
    print(json.dumps({"tape": "TOTAL", **tot}))


main()
