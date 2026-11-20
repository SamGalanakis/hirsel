#!/usr/bin/env python3
"""mc-units-proto Part 3: maximum passing session-turn rate per side.

A run passes when every issued turn completed with a correct checksum, no
turn timed out, and p99 server-added latency <= LIMIT_NS (declared before
measuring). For each (side, cell) a search finds the highest passing rate:
start from a guess, step up by STEP until a run fails (or down until one
passes), then bisect twice. Each search runs inside its own timing block
(regimes lock, board lock for CPUs 10-11, cc1plus check); within a group the
sides of a cell run in random order. Only pidfiles of processes this script
started are ever signalled, after checking their command name.

Usage: part3.py PLAN OUT.jsonl GROUPS   (PLAN: sweep | cells | pilot)
"""
import json, os, random, signal, socket, subprocess, sys, time
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from blocks import Block, cpuset

LIMIT_NS = 5_000_000
BIN = "/tmp/mcu/bin3"
STORE = "/tmp/mcu/store"
TAPES = "/tmp/mcu/tapes"
WORK = "/tmp/mcu/p3"
SERVER = "0-3,8-11"
SERVER_LIST = [0, 1, 2, 3, 8, 9, 10, 11]
LOAD = "16-19"
TOOL = "24-27"
PORT, TOOL_PORT = 47700, 47690
SESSIONS, PARTITIONS, SYNCS = 256, 8, 8
TOOL_DELAY_US = 1000
RUN = dict(dur=8, warm=1, burst_at=4, burst_len=1, burst_x=1.5)
STEP = 1.25


def log(*a):
    print(time.strftime("%H:%M:%S"), *a, flush=True)


def side_cmd(side, par, rec, k=None):
    """(argv, env, cpus, nports) for a server side."""
    env = {"HIRSEL_SYNC_HELPERS": str(SYNCS)}
    exe = f"{BIN}/session_rec" if rec else f"{BIN}/session"
    base = [str(PORT), str(SESSIONS), str(PARTITIONS), str(par), str(TOOL_PORT)]
    if side.startswith("S1"):
        w = int(side.split("w")[1])
        env["HIRSEL_S8_WORKERS"] = str(w)
        cpus = SERVER if w <= 8 else "0-3,8-11,16-19,24-27"
        return [exe] + base + ["0"], env, cpus, 1
    if side.startswith("S3"):
        k = int(side.split("k")[1])
        units = k + 1
        pins = SERVER_LIST[:k] + ([SERVER_LIST[k]] if k < len(SERVER_LIST) else [])
        env |= {"HIRSEL_UNITS": str(units), "HIRSEL_UNIT_CPUS": ",".join(map(str, pins)),
                "HIRSEL_XRING": "65536"}
        return [exe] + base + ["1"], env, SERVER, k
    if side.startswith("tokio"):
        w = int(side.split("w")[1])
        return [f"{BIN}/twin"] + base + [str(w), str(SYNCS), STORE], {}, SERVER, 1
    raise ValueError(side)


def kill_own(pidfile, comms):
    try:
        pid = int(open(pidfile).read())
    except (OSError, ValueError):
        return
    r = subprocess.run(["ps", "-o", "comm=", "-p", str(pid)], capture_output=True, text=True)
    if r.stdout.strip() in comms:
        os.kill(pid, signal.SIGKILL)
        time.sleep(0.3)


def cpu_ticks(pid):
    try:
        f = open(f"/proc/{pid}/stat").read().rsplit(")", 1)[1].split()
        return int(f[11]) + int(f[12])
    except OSError:
        return None


def masks(pid):
    out = {}
    try:
        for t in os.listdir(f"/proc/{pid}/task"):
            st = open(f"/proc/{pid}/task/{t}/status").read()
            name = open(f"/proc/{pid}/task/{t}/comm").read().strip()
            m = [l.split(":", 1)[1].strip() for l in st.splitlines() if l.startswith("Cpus_allowed_list")]
            out.setdefault(name + ":" + m[0], 0)
            out[name + ":" + m[0]] += 1
    except OSError:
        pass
    return out


def wait_rusage(p, timeout):
    """Reap the server ourselves to get its rusage (taskset execs, so the pid is the server)."""
    end = None if timeout is None else time.time() + timeout
    while True:
        pid, status, ru = os.wait4(p.pid, os.WNOHANG)
        if pid:
            p.returncode = os.waitstatus_to_exitcode(status)
            return ru
        if end is not None and time.time() > end:
            return None
        time.sleep(0.05)


class Tool:
    def __enter__(self):
        os.makedirs(WORK, exist_ok=True)
        self.err = open(f"{WORK}/tool.err", "w")
        self.p = subprocess.Popen(["taskset", "-c", TOOL, f"{BIN}/toolsrv", str(TOOL_PORT), str(TOOL_DELAY_US), "4"],
                                  stderr=self.err)
        open(f"{WORK}/tool.pid", "w").write(str(self.p.pid))
        time.sleep(0.5)
        # The block's own tool server must be the one listening: a stray
        # listener on the port would otherwise serve the calls unnoticed.
        if self.p.poll() is not None or "toolsrv ready" not in open(f"{WORK}/tool.err").read():
            raise SystemExit(f"tool server did not start: {open(f'{WORK}/tool.err').read()[-400:]}")
        return self

    def __exit__(self, *a):
        kill_own(f"{WORK}/tool.pid", {"toolsrv"})
        self.p.wait()
        self.err.close()


def one_run(side, cell, rate, seed):
    f, variant, rec = cell["f"], cell["variant"], cell["rec"]
    par = 1 if variant == "par" else 0
    argv, env, cpus, nports = side_cmd(side, par, rec)
    for d in (STORE, TAPES):
        os.makedirs(d, exist_ok=True)
        for n in os.listdir(d):
            os.unlink(os.path.join(d, n))
    for p in range(PORT, PORT + 9):
        with socket.socket() as s:
            if s.connect_ex(("127.0.0.1", p)) == 0:
                raise SystemExit(f"port {p} already has a listener before the run")
    full_env = {k: v for k, v in os.environ.items() if not k.startswith("HIRSEL_")} | env
    err = open(f"{WORK}/server.err", "w")
    srv = subprocess.Popen(["taskset", "-c", cpus] + argv, env=full_env, stderr=err)
    open(f"{WORK}/server.pid", "w").write(str(srv.pid))
    tool_pid = int(open(f"{WORK}/tool.pid").read())
    lg = ["taskset", "-c", LOAD, f"{BIN}/loadgen", f"port={PORT}", f"nports={nports}",
          f"sessions={SESSIONS}", f"rate={rate:.0f}", f"f={int(f * 1000)}", f"par={par}",
          f"skew={1 if variant == 'skew' else 0}", f"seed={seed}", "threads=4", "grace=5"]
    lg += [f"{k}={v}" for k, v in RUN.items()]
    t0, tool0 = time.time(), cpu_ticks(tool_pid)
    lgp = subprocess.Popen(lg, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
    seen_masks = {}
    if side.startswith("S1"):
        time.sleep(RUN["dur"] / 2 + 1.5)
        seen_masks = masks(srv.pid)
    try:
        out, lerr = lgp.communicate(timeout=120)
    except subprocess.TimeoutExpired:
        lgp.kill()
        out, lerr = lgp.communicate()
    wall = time.time() - t0
    tool1 = cpu_ticks(tool_pid)
    ru = wait_rusage(srv, 30)
    if ru is None:
        kill_own(f"{WORK}/server.pid", {"session", "session_rec", "twin"})
        ru = wait_rusage(srv, None)
    err.close()
    server_err = open(f"{WORK}/server.err").read()[-2000:]
    rec_ = dict(side=side, cell=cell, rate=rate, seed=seed, wall=wall, rc=srv.returncode,
                server=server_err.strip().splitlines()[-12:], masks=seen_masks,
                server_cpu_s=round(ru.ru_utime + ru.ru_stime, 3), server_sys_s=round(ru.ru_stime, 3),
                tool_cpu_s=(tool1 - tool0) / 100 if tool0 is not None and tool1 is not None else None)
    tapes = sum(os.path.getsize(os.path.join(TAPES, n)) for n in os.listdir(TAPES)) if rec else 0
    rec_["tape_bytes"] = tapes
    try:
        rec_["load"] = json.loads(out.strip().splitlines()[-1])
    except (ValueError, IndexError):
        rec_["load"] = None
        rec_["load_err"] = lerr[-800:]
    L = rec_["load"]
    rec_["pass"] = bool(L and L["failures"] == 0 and L["timeouts"] == 0 and L["completed"] == L["issued"] and L["late"] == 0
                        and L["p99_ns"] <= LIMIT_NS and srv.returncode == 0)
    rec_["correct"] = bool(L and L["failures"] == 0)
    return rec_


def search(side, cell, guess, out, group):
    """Highest passing rate, to within STEP**0.25."""
    seed = random.randrange(1, 1 << 30)
    runs = []

    def go(rate):
        r = one_run(side, cell, rate, seed)
        r["group"] = group
        runs.append(r)
        L = r["load"] or {}
        log(f"  {side} {cell} rate={rate:.0f} pass={r['pass']} p99={L.get('p99_ns', 0) / 1e6:.2f}ms e2e99={L.get('e2e_p99_ns', 0) / 1e6:.2f}ms late={L.get('late')} cpu={r.get('server_cpu_s')} "
            f"done={L.get('completed')}/{L.get('issued')} fail={L.get('failures')} to={L.get('timeouts')} tool={r['tool_cpu_s']}")
        return r["pass"]

    lo, hi = None, None
    rate = guess
    if go(rate):
        lo = rate
        while hi is None:
            rate *= STEP
            if rate > 400000:
                break
            if go(rate):
                lo = rate
            else:
                hi = rate
    else:
        hi = rate
        while lo is None:
            rate /= STEP
            if rate < 20:
                break
            if go(rate):
                lo = rate
            else:
                hi = rate
    if lo is not None and hi is not None:
        for _ in range(2):
            mid = (lo * hi) ** 0.5
            if go(mid):
                lo = mid
            else:
                hi = mid
    return lo, runs


def plan(name):
    """(cells, sides) for a plan."""
    if name == "pilot":
        return [dict(f=0.0, variant="base", rec=False)], ["S1w8", "S3k7", "tokiow8"]
    if name == "sweep":
        cells = [dict(f=f, variant="base", rec=False) for f in (0.0, 1.0)]
        return cells, ["S1w1", "S1w2", "S1w4", "S1w8", "S3k4", "S3k7", "S3k8", "tokiow8"]
    if name == "cells":
        cells = []
        for variant in ("base", "skew", "par"):
            for f in (0.0, 0.1, 0.5, 1.0):
                for rec in (False, True):
                    cells.append(dict(f=f, variant=variant, rec=rec))
        return cells, os.environ.get("P3_SIDES", "S1w8,S3k7,tokiow8").split(",")
    if name == "smt16":
        return [dict(f=f, variant="base", rec=False) for f in (0.0, 1.0)], ["S1w16"]
    raise SystemExit(name)


def main():
    pname, outp, groups = sys.argv[1], sys.argv[2], int(sys.argv[3])
    cells, sides = plan(pname)
    guesses = json.loads(os.environ.get("P3_GUESS", "{}"))
    random.seed(int(time.time()))
    best = {}
    with open(outp, "a") as out:
        for g in range(groups):
            order = cells[:]
            random.shuffle(order)
            for cell in order:
                if cell["rec"]:
                    cell_sides = [s for s in sides if not s.startswith("tokio")]
                else:
                    cell_sides = sides[:]
                random.shuffle(cell_sides)
                for side in cell_sides:
                    if side == "S1w16":
                        cpus = cpuset("0-3,8-11,16-19,24-27")
                    else:
                        cpus = cpuset(SERVER) | cpuset(LOAD) | cpuset(TOOL)
                    key = f"{side}|{json.dumps(cell, sort_keys=True)}"
                    guess = best.get(key) or guesses.get(side, 2000)
                    while True:
                        with Block(cpus, log=log) as b:
                            with Tool():
                                lo, runs = search(side, cell, guess * 0.9, out, g)
                        for r in runs:
                            r["discarded"] = not b.clean
                            out.write(json.dumps(r) + "\n")
                        out.flush()
                        if b.clean:
                            break
                    best[key] = lo or guess
                    out.write(json.dumps(dict(summary=True, side=side, cell=cell, group=g, max_rate=lo)) + "\n")
                    out.flush()
                    log(f"group {g} {side} {cell}: max passing rate {lo}")


if __name__ == "__main__":
    main()
