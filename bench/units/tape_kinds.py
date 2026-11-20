#!/usr/bin/env python3
"""Split recorded tape payload bytes by (op, object): a DONE counts for the
operation its task last issued. The rest of each file is framing (headers,
issues, grants, digests, block headers).

Usage: tape_kinds.py HIRSEL TAPE...   (one JSON object per tape and a total)
"""
import collections, json, os, re, subprocess, sys

REC = re.compile(r"^\s*\d+ (OP|DONE|ISSUE|GRANT)\b")
FIELD = re.compile(r"\b(task|op|object)=(\d+)")


def split(hirsel, tape):
    p = subprocess.Popen([hirsel, "tape", "show", tape, "--include-credentials"], stdout=subprocess.PIPE, text=True)
    c = collections.Counter()
    issued = {}
    for line in p.stdout:
        m = REC.match(line)
        if not m:
            continue
        head = line.split(" payload=")[0]
        f = dict(FIELD.findall(head))
        if m[1] == "ISSUE":
            issued[f.get("task")] = (f.get("op"), f.get("object"))
            continue
        if m[1] == "OP":
            key = (f.get("op"), f.get("object"))
        elif m[1] == "DONE":
            key = issued.pop(f.get("task"), ("?", "?"))
        else:
            continue
        i = line.find("payload=[")
        if i < 0:
            continue
        body = line[i + 9:line.rfind("]")]
        n = body.count(",") + 1 if body.strip() else 0
        if n:
            c[f"op{key[0]}/obj{key[1]}"] += n
    p.wait()
    c["file_bytes"] = os.path.getsize(tape)
    c["framing"] = c["file_bytes"] - sum(v for k, v in c.items() if k.startswith("op"))
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
