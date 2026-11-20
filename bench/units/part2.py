#!/usr/bin/env python3
"""mc-units-proto Part 2: the crossing price alone.

Usage: part2.py <xbench> <xprim> <out.jsonl> <groups> [grain]
Each group runs every configuration once in random order inside one timing
block; board-CPU configurations run in their own blocks.
"""
import json, os, random, re, subprocess, sys, time
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from blocks import Block, cpuset

XB, XP, OUT = sys.argv[1], sys.argv[2], sys.argv[3]
GROUPS = int(sys.argv[4])
GRAIN = int(sys.argv[5]) if len(sys.argv) > 5 else 7000
LINE = re.compile(r"^(\S+) n=(\d+) ns=(\d+) check=(\d+)$", re.M)
M64 = (1 << 64) - 1


def kernel(x, rounds):
    for _ in range(rounds):
        x ^= (x << 13) & M64
        x ^= x >> 7
        x ^= (x << 17) & M64
    return x


def exp_reqrep(n):
    return sum(((i + 1) & 0xff) for i in range(n))


_kc = {}
def exp_fanin(n, producers, grain):
    key = (n, grain)
    if key not in _kc:
        per = [kernel(b, grain) & 0xff for b in range(256)]
        _kc[key] = sum(per[i & 0xff] for i in range(n))
    return producers * _kc[key]


def C(name, shape, side, argv, cpus, env=None, expect=None):
    return dict(name=name, shape=shape, side=side, argv=argv, cpus=cpus, env=env or {}, expect=expect)


def configs():
    out = []
    for size in (64, 1024):
        n_sleep, n_fast, n_pipe = 20000, 100000, 200000
        u = {"HIRSEL_UNITS": "2", "HIRSEL_UNIT_CPUS": "0,1"}
        ux = {"HIRSEL_UNITS": "2", "HIRSEL_UNIT_CPUS": "0,8"}
        pp, pf, pi = f"pingpong-{size}", f"pingpong-poll-{size}", f"pipe-{size}"
        out += [
            C("units asleep", pp, "S3 units", [XB, "x_pingpong", n_sleep, size], "0,1", u, exp_reqrep(n_sleep)),
            C("units asleep x-L3", pp, "S3 units x-L3", [XB, "x_pingpong", n_sleep, size], "0,8", ux, exp_reqrep(n_sleep)),
            C("units busy-poll", pf, "S3 units", [XB, "x_pingpong", n_fast, size], "0,1", u | {"HIRSEL_XSPIN": "1"}, exp_reqrep(n_fast)),
            C("units pipelined", pi, "S3 units", [XB, "x_pipe", n_pipe, size], "0,1", u, exp_reqrep(n_pipe)),
            C("prim asleep", pp, "raw primitive", [XP, "prim_pingpong_sleep", n_sleep, size], "0,1", {"XCPUS": "0,1"}, exp_reqrep(n_sleep)),
            C("prim busy-poll", pf, "raw primitive", [XP, "prim_pingpong_poll", n_fast, size], "0,1", {"XCPUS": "0,1"}, exp_reqrep(n_fast)),
            C("prim pipelined", pi, "raw primitive", [XP, "prim_pipe_sleep", n_pipe, size, 64], "0,1", {"XCPUS": "0,1"}, exp_reqrep(n_pipe)),
            C("tokio 2 runtimes", pp, "tokio 2 rt", [XP, "tokio_tworuntimes", n_sleep, size], "0,1", {"XCPUS": "0,1"}, exp_reqrep(n_sleep)),
        ]
        for w, cs in ((1, "0"), (2, "0,1"), (4, "0-3")):
            s1 = {"HIRSEL_S8_WORKERS": str(w)}
            out += [
                C(f"S1 w{w} channel", pp, f"S1 w{w}", [XB, "c_pingpong", n_fast, size], cs, s1, exp_reqrep(n_fast)),
                C(f"S1 w{w} channel pipe", pi, f"S1 w{w}", [XB, "c_pipe", n_pipe, size, 64], cs, s1, exp_reqrep(n_pipe)),
                C(f"tokio w{w}", pp, f"tokio w{w}", [XP, "tokio_pingpong", n_sleep, size, w], cs, {}, exp_reqrep(n_sleep)),
                C(f"tokio w{w} pipe", pi, f"tokio w{w}", [XP, "tokio_pipe", n_pipe, size, w, 64], cs, {}, exp_reqrep(n_pipe)),
            ]
    for grain, n in ((0, 100000), (GRAIN, 4000)):
        sh = f"fanin3-g{grain}"
        out += [
            C("units fan-in", sh, "S3 units", [XB, "x_fanin", n, 64, grain], "0-3",
              {"HIRSEL_UNITS": "4", "HIRSEL_UNIT_CPUS": "0,1,2,3"}, exp_fanin(n, 3, grain)),
            C("prim fan-in", sh, "raw primitive", [XP, "prim_fanin_sleep", n, 64, 3, grain], "0-3", {"XCPUS": "0,1,2,3"}, exp_fanin(n, 3, grain)),
        ]
        for w, cs in ((1, "0"), (2, "0,1"), (4, "0-3")):
            out += [
                C(f"S1 w{w} fan-in", sh, f"S1 w{w}", [XB, "c_fanin", n, 64, 3, grain], cs, {"HIRSEL_S8_WORKERS": str(w)}, exp_fanin(n, 3, grain)),
                C(f"tokio w{w} fan-in", sh, f"tokio w{w}", [XP, "tokio_fanin", n, 64, 3, grain, w], cs, {}, exp_fanin(n, 3, grain)),
            ]
    return out


def board_configs():
    out = []
    cs = "0-3,8-11"
    for grain, n in ((0, 50000), (GRAIN, 2000)):
        sh = f"fanin7-g{grain}"
        out += [
            C("units fan-in 7", sh, "S3 units", [XB, "x_fanin", n, 64, grain], cs,
              {"HIRSEL_UNITS": "8", "HIRSEL_UNIT_CPUS": "0,1,2,3,8,9,10,11"}, exp_fanin(n, 7, grain)),
            C("S1 w1 fan-in 7", sh, "S1 w1", [XB, "c_fanin", n, 64, 7, grain], "0", {"HIRSEL_S8_WORKERS": "1"}, exp_fanin(n, 7, grain)),
            C("S1 w4 fan-in 7", sh, "S1 w4", [XB, "c_fanin", n, 64, 7, grain], "0-3", {"HIRSEL_S8_WORKERS": "4"}, exp_fanin(n, 7, grain)),
            C("tokio w8 fan-in 7", sh, "tokio w8", [XP, "tokio_fanin", n, 64, 7, grain, 8], cs, {}, exp_fanin(n, 7, grain)),
        ]
    return out


def run_one(c):
    env = os.environ.copy()
    for k in ("HIRSEL_UNITS", "HIRSEL_UNIT_CPUS", "HIRSEL_XSPIN", "HIRSEL_S8_WORKERS", "HIRSEL_XTAPE_DIR", "HIRSEL_XREPLAY"):
        env.pop(k, None)
    env.update(c["env"])
    cmd = ["taskset", "-c", c["cpus"]] + [str(a) for a in c["argv"]]
    t0 = time.time()
    try:
        r = subprocess.run(cmd, env=env, capture_output=True, text=True, timeout=300)
        rc, text = r.returncode, r.stderr + r.stdout
    except subprocess.TimeoutExpired as e:
        rc, text = "timeout", str(e.stderr or "")
    wall = time.time() - t0
    m = LINE.search(text)
    rec = dict(c, argv=[str(a) for a in c["argv"]], rc=rc, wall=wall, t=time.time())
    if m:
        rec.update(label=m[1], n=int(m[2]), ns=int(m[3]), check=int(m[4]))
        rec["ok"] = rc == 0 and int(m[4]) == c["expect"]
    else:
        rec.update(ok=False, text=text[-500:])
    return rec


def main():
    random.seed(int(time.time()))
    plain, board = configs(), board_configs()
    with open(OUT, "a") as out:
        for g in range(GROUPS):
            for kind, cs in (("plain", plain), ("board", board)):
                order = cs[:]
                random.shuffle(order)
                cpus = set().union(*(cpuset(c["cpus"]) for c in order))
                while True:
                    recs = []
                    with Block(cpus) as b:
                        for c in order:
                            recs.append(run_one(c))
                    for r in recs:
                        r["group"], r["discarded"] = g, not b.clean
                        out.write(json.dumps(r) + "\n")
                    out.flush()
                    if b.clean:
                        break
                bad = [r["name"] + "/" + r["shape"] for r in recs if not r["ok"]]
                print(f"group {g} {kind}: {len(recs)} runs, failures {bad}", flush=True)


main()
