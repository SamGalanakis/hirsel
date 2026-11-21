#!/usr/bin/env python3
"""The crossing price on the language path, against the entry-point reference.

Usage: part2u.py <xbench_u> <units_conn> <xprim> <out.jsonl> <holds>
Each hold takes the regimes and board locks and runs every configuration once,
in random order; holds are at least 15 s apart. A hold taken while cc1plus ran
is discarded and retried.
"""
import datetime, json, os, random, re, subprocess, sys, time
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from blocks import Block, cpuset

XU, REF, XP, OUT = sys.argv[1:5]
HOLDS = int(sys.argv[5])
LINE = re.compile(r"^(\S+) n=(\d+) ns=(\d+) check=(\d+)$", re.M)
STATS = re.compile(r"^untied-stats (.*)$", re.M)
PP = "0-3"
WARM, N = 2000, 50000


def exp_reqrep(n, start=0):
    return sum(((i + 1) & 0xff) for i in range(start, n))


def C(name, shape, argv, cpus, env, expect):
    return dict(name=name, shape=shape, argv=[str(a) for a in argv], cpus=cpus, env=env, expect=expect)


def configs():
    pool = lambda w: {"HIRSEL_UNTIED": str(w)}
    out = [
        # The reference: mc-conn-lines' units_conn, two workers.
        C("ref parked w2", "pingpong-64", [REF, "parked", WARM, N, 64], PP, pool(2), N),
        C("ref ready w2", "pingpong-64", [REF, "ready", WARM, N, 64], PP, pool(2), N),
        C("tokio w2", "pingpong-64", [XP, "tokio_pingpong", N, 64, 2], PP, {}, exp_reqrep(N)),
    ]
    for w in (1, 2, 4):
        out.append(C(f"lang w{w}", "pingpong-64", [XU, "x_pingpong", N, 64, WARM], PP, pool(w), exp_reqrep(WARM + N)))
    out += [
        C("lang w4 pipe", "pipe-64", [XU, "x_pipe", 200000, 64], PP, pool(4), exp_reqrep(200000)),
        C("tokio w4 pipe", "pipe-64", [XP, "tokio_pipe", 200000, 64, 4, 64], PP, {}, exp_reqrep(200000)),
    ]
    return out


def run_one(c):
    env = {k: v for k, v in os.environ.items() if not k.startswith("HIRSEL_")} | c["env"]
    cmd = ["taskset", "-c", c["cpus"]] + c["argv"]
    t0 = time.time()
    try:
        r = subprocess.run(cmd, env=env, capture_output=True, text=True, timeout=120)
        rc, text = r.returncode, r.stderr + r.stdout
    except subprocess.TimeoutExpired as e:
        rc, text = "timeout", str(e.stderr or "")
    rec = dict(c, rc=rc, wall=time.time() - t0, t=time.time())
    if s := STATS.search(text):
        rec["stats"] = dict(kv.split("=", 1) for kv in s[1].split())
    if m := LINE.search(text):
        rec.update(label=m[1], n=int(m[2]), ns=int(m[3]), check=int(m[4]))
        rec["ns_per"] = int(m[3]) / int(m[2])
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
    with open(OUT, "a") as out:
        for g in range(HOLDS):
            if g:
                time.sleep(16)
            order = cs[:]
            random.shuffle(order)
            cpus = set().union(*(cpuset(c["cpus"]) for c in order))
            while True:
                while not window_ok():
                    time.sleep(60)
                recs = []
                with Block(cpus, board=True) as b:
                    held = time.time()
                    for c in order:
                        recs.append(run_one(c))
                    held = time.time() - held
                if b.clean:
                    break
                print("hold discarded (cc1plus); rerunning", flush=True)
                time.sleep(16)
            for r in recs:
                r["hold"], r["held_s"] = g, held
                out.write(json.dumps(r) + "\n")
            out.flush()
            bad = [r["name"] for r in recs if not r["ok"]]
            print(f"hold {g}: {len(recs)} runs in {held:.1f} s, bad {bad}", flush=True)


main()
