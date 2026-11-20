"""Diagnostic block (not reported): sides with optional extra env, one rate.
Usage: diag4.py RATE SIDE[+K=V...] ..."""
import sys, json
sys.path.insert(0, "/home/sam/.t3/worktrees/hirsellang/t3-72e2eab5-da842f76728ddb8a/bench/units")
import part3
from blocks import Block, cpuset
rate = float(sys.argv[1]); specs = sys.argv[2:]
orig = part3.side_cmd
cell = dict(f=0.0, variant="base", rec=False)
with Block(cpuset(part3.SERVER) | cpuset(part3.LOAD) | cpuset(part3.TOOL), log=part3.log) as b:
    with part3.Tool():
        for spec in specs:
            side, *kv = spec.split("+")
            extra = dict(x.split("=", 1) for x in kv)
            part3.side_cmd = lambda s, p, r, k=None, extra=extra: (lambda a: (a[0], a[1] | extra, a[2], a[3]))(orig(s, p, r))
            r = part3.one_run(side, cell, rate, 12345)
            L = r["load"] or {}
            print(json.dumps(dict(spec=spec, rate=rate, **{k: round(L.get(k, 0) / 1e6, 2) for k in ("p50_ns", "p99_ns", "p999_ns", "max_ns")}, done=L.get("completed"), issued=L.get("issued"), cpu=r.get("server_cpu_s"))), flush=True)
print("clean", b.clean)
