#!/usr/bin/env python3
"""mc-units-untied: the crossing price under the untied pool.

Usage: part2u.py <xbench_u> <old xbench> <out.jsonl> <groups>
Each group runs every configuration once, in random order, inside one timing
block (regimes + board lock). The old lane's thread-per-unit S3 cells and the
one-worker S1 channel run in the same blocks as references.
"""
import datetime, json, os, random, re, subprocess, sys, time
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from blocks import Block, cpuset

XU, XB, OUT = sys.argv[1], sys.argv[2], sys.argv[3]
GROUPS = int(sys.argv[4])
LINE = re.compile(r"^(\S+) n=(\d+) ns=(\d+) check=(\d+)$", re.M)
STATS = re.compile(r"^untied-stats (.*)$", re.M)
SRV = "0-3,8-11"


def exp_reqrep(n):
    return sum(((i + 1) & 0xff) for i in range(n))


def exp_fanin0(n, producers):
    return producers * sum((i & 0xff) for i in range(n))


def C(name, shape, side, argv, cpus, env=None, expect=None):
    return dict(name=name, shape=shape, side=side, argv=argv, cpus=cpus, env=env or {}, expect=expect)


def configs():
    out = []
    for size in (64, 1024):
        n_pp, n_pipe = 20000, 200000
        pp, pi = f"pingpong-{size}", f"pipe-{size}"
        for w, cs in ((1, "0"), (2, "0,1"), (8, SRV)):
            u = {"HIRSEL_UNITS": "2", "HIRSEL_UNTIED": str(w), "HIRSEL_XSTATS": "1"}
            out += [
                C(f"untied w{w}", pp, f"untied W={w}", [XU, "x_pingpong", n_pp, size], cs, u, exp_reqrep(n_pp)),
                C(f"untied w{w} pipe", pi, f"untied W={w}", [XU, "x_pipe", n_pipe, size], cs, u, exp_reqrep(n_pipe)),
            ]
        u0 = {"HIRSEL_UNITS": "2", "HIRSEL_UNTIED": "8", "HIRSEL_XSTATS": "1", "HIRSEL_UNTIED_SPIN_US": "0"}
        out += [
            C("untied w8 nospin", pp, "untied W=8 no spin", [XU, "x_pingpong", n_pp, size], SRV, u0, exp_reqrep(n_pp)),
            C("S3 units asleep", pp, "S3 units (old)", [XB, "x_pingpong", n_pp, size], "0,1",
              {"HIRSEL_UNITS": "2", "HIRSEL_UNIT_CPUS": "0,1"}, exp_reqrep(n_pp)),
            C("S3 units pipelined", pi, "S3 units (old)", [XB, "x_pipe", n_pipe, size], "0,1",
              {"HIRSEL_UNITS": "2", "HIRSEL_UNIT_CPUS": "0,1"}, exp_reqrep(n_pipe)),
            C("S1 w1 channel", pp, "S1 w1", [XB, "c_pingpong", 100000, size], "0", {"HIRSEL_S8_WORKERS": "1"}, exp_reqrep(100000)),
            C("S1 w1 channel pipe", pi, "S1 w1", [XB + "2", "c_pipe", n_pipe, size, 64], "0", {"HIRSEL_S8_WORKERS": "1"}, exp_reqrep(n_pipe)),
        ]
    n = 100000
    for w, cs in ((1, "0"), (4, "0-3"), (8, SRV)):
        out.append(C(f"untied w{w} fan-in", "fanin3-g0", f"untied W={w}", [XU, "x_fanin", n, 64, 0], cs,
                     {"HIRSEL_UNITS": "4", "HIRSEL_UNTIED": str(w), "HIRSEL_XSTATS": "1"}, exp_fanin0(n, 3)))
    out.append(C("S3 units fan-in", "fanin3-g0", "S3 units (old)", [XB, "x_fanin", n, 64, 0], "0-3",
                 {"HIRSEL_UNITS": "4", "HIRSEL_UNIT_CPUS": "0,1,2,3"}, exp_fanin0(n, 3)))
    return out


def run_one(c):
    env = os.environ.copy()
    for k in list(env):
        if k.startswith("HIRSEL_U") or k in ("HIRSEL_XSPIN", "HIRSEL_S8_WORKERS", "HIRSEL_XSTATS", "HIRSEL_XGATE"):
            env.pop(k, None)
    env.update(c["env"])
    cmd = ["taskset", "-c", c["cpus"]] + [str(a) for a in c["argv"]]
    t0 = time.time()
    try:
        r = subprocess.run(cmd, env=env, capture_output=True, text=True, timeout=120)
        rc, text = r.returncode, r.stderr + r.stdout
    except subprocess.TimeoutExpired as e:
        rc, text = "timeout", str(e.stderr or "")
    wall = time.time() - t0
    m = LINE.search(text)
    rec = dict(c, argv=[str(a) for a in c["argv"]], rc=rc, wall=wall, t=time.time())
    s = STATS.search(text)
    if s:
        rec["stats"] = dict(kv.split("=", 1) for kv in s[1].split())
    if m:
        rec.update(label=m[1], n=int(m[2]), ns=int(m[3]), check=int(m[4]))
        rec["ok"] = rc == 0 and int(m[4]) == c["expect"]
    else:
        rec.update(ok=False, text=text[-500:])
    return rec


def window_ok():
    now = datetime.datetime.now().time()
    return not (datetime.time(2, 45) <= now < datetime.time(4, 0))


def main():
    random.seed(int(time.time()))
    cs = configs()
    only = os.environ.get("PART2_ONLY")
    if only:
        cs = [c for c in cs if re.search(only, c["name"] + "/" + c["shape"])]
    with open(OUT, "a") as out:
        for g in range(GROUPS):
            order = cs[:]
            random.shuffle(order)
            cpus = set().union(*(cpuset(c["cpus"]) for c in order))
            while True:
                while not window_ok():
                    time.sleep(60)
                recs = []
                with Block(cpus) as b:
                    for c in order:
                        recs.append(run_one(c))
                if b.clean:
                    break
                print("block discarded; rerunning", flush=True)
            for r in recs:
                r["group"] = g
                out.write(json.dumps(r) + "\n")
            out.flush()
            bad = [r["name"] + "/" + r["shape"] for r in recs if not r["ok"]]
            print(f"group {g}: {len(recs)} runs, {len(bad)} bad {bad[:4]}", flush=True)


main()
