#!/usr/bin/env python3
"""mc-units-untied: Part 3 load, search and pass rule, untied side added.

Copied from part3.py with the load, LIMIT_NS, the pass rule and the search
unchanged. Sides: untied (W=8 workers, one unit per session, children in par
cells), S3k7 (old thread-per-unit, floating) and tokiow8. A block holds one
cell and one rate step per side (see run_chunk); the first side rotates.
Usage: part3u.py PLAN OUT.jsonl GROUPS   (PLAN: base | par | rec | all | pilot)

Original header:

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
UBIN, UBIN_REC = "/tmp/mcut/progs/session_u", "/tmp/mcut/progs_rec/session_u"
STORE = "/tmp/mcu/store"
STORES = ["/tmp/mcu/store", "/tmp/mcut/store"]
TAPES = "/tmp/mcu/tapes"
TAPE_DIRS = {s: "/tmp/mcut/tapes" for s in ("untied", "untied-oo", "untied-rh", "untied-oo-rh")}
WORK = "/tmp/mcut/p3"
SERVER = "0-3,8-11"
SERVER_LIST = [0, 1, 2, 3, 8, 9, 10, 11]
LOAD = "16-19"
TOOL = "24-27"
# Below the ephemeral range (32768-60999), so no outgoing connection can hold them.
PORT, TOOL_PORT = 29700, 29690
SESSIONS, PARTITIONS, SYNCS = 256, 8, 8
TOOL_DELAY_US = 1000
RUN = dict(dur=8, warm=1, burst_at=4, burst_len=1, burst_x=1.5)
STEP = 1.25
BLOCK_SIDES = 4


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
        # S3kK: K session units plus the service unit, each its own thread,
        # free to move within the server CPUs. S3kKpin pins unit i to one CPU.
        pinned = side.endswith("pin")
        k = int(side.split("k")[1].removesuffix("pin"))
        units = k + 1
        env |= {"HIRSEL_UNITS": str(units), "HIRSEL_XRING": "65536"}
        if pinned:
            pins = SERVER_LIST[:k] + ([SERVER_LIST[k]] if k < len(SERVER_LIST) else [])
            env["HIRSEL_UNIT_CPUS"] = ",".join(map(str, pins))
        return [exe] + base + ["1"], env, SERVER, k
    if side.endswith("-rh"):
        # Attribution variant (marked): process retention headroom raised from
        # 48 MiB to 1 GiB, so per-unit tape buffers no longer crowd out the
        # reuse of task stacks.
        argv, env, cpus, nports = side_cmd(side.removesuffix("-rh"), par, rec)
        return argv, env | {"HIRSEL_RETENTION_MB": "1024"}, cpus, nports
    if side == "untied-oo":
        # Order-only recording (not replayable): payloads over 256 bytes are
        # recorded as their length only. Recorded cells only.
        argv, env, cpus, nports = side_cmd("untied", par, rec)
        return argv, env | {"HIRSEL_TAPE_ORDER_ONLY": "1"}, cpus, nports
    if side == "untied":
        env |= {"HIRSEL_UNTIED": os.environ.get("P3_UW", "8"), "HIRSEL_UNITS": str(PARTITIONS + 1),
                "HIRSEL_FRONT": str(PORT), "HIRSEL_XSTATS": "1"}
        env |= json.loads(os.environ.get("P3_UENV", "{}"))
        return [UBIN_REC if rec else UBIN] + base + ["2"], env, SERVER, 1
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
    tapes_dir = TAPE_DIRS.get(side, TAPES)
    for d in STORES + [tapes_dir]:
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
    perf = os.environ.get("P3_PERF")
    if perf:
        # Attribution runs only: perf on CPUs 6-7 against the server for 3 s,
        # starting 3 s into the load. P3_PERF is "record" (CPU, call graphs),
        # "sched" (off-CPU: sched_switch with call graphs) or "stat".
        time.sleep(3.5)
        tag = f"{WORK}/perf-{side}-{cell['variant']}-{cell['f']}-{int(rec)}-{rate:.0f}-{perf}"
        base_cmd = ["taskset", "-c", "6-7", "perf"]
        if perf == "record":
            pcmd = base_cmd + ["record", "-N", "-F", "1999", "-g", "-o", tag + ".data", "-p", str(srv.pid), "--", "sleep", "3"]
        elif perf == "sched":
            pcmd = base_cmd + ["record", "-N", "-e", "sched:sched_switch", "-g", "-o", tag + ".data", "-p", str(srv.pid), "--", "sleep", "3"]
        else:
            pcmd = base_cmd + ["stat", "-e", "task-clock,context-switches,cpu-migrations,syscalls:sys_enter_write,syscalls:sys_enter_epoll_wait,syscalls:sys_enter_futex",
                               "-o", tag + ".txt", "-p", str(srv.pid), "--", "sleep", "3"]
        subprocess.run(pcmd, capture_output=True)
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
        kill_own(f"{WORK}/server.pid", {"session", "session_rec", "twin", "session_u"})
        ru = wait_rusage(srv, None)
    err.close()
    server_err = open(f"{WORK}/server.err").read()[-2000:]
    rec_ = dict(side=side, cell=cell, rate=rate, seed=seed, wall=wall, rc=srv.returncode,
                server=server_err.strip().splitlines()[-12:], masks=seen_masks,
                server_cpu_s=round(ru.ru_utime + ru.ru_stime, 3), server_sys_s=round(ru.ru_stime, 3),
                tool_cpu_s=(tool1 - tool0) / 100 if tool0 is not None and tool1 is not None else None)
    tapes = sum(os.path.getsize(os.path.join(tapes_dir, n)) for n in os.listdir(tapes_dir)) if rec else 0
    rec_["tape_bytes"] = tapes
    rec_["tape_files"] = len(os.listdir(tapes_dir)) if rec else 0
    full_err = open(f"{WORK}/server.err").read()
    for line in full_err.splitlines():
        if line.startswith("untied-stats "):
            rec_["ustats"] = dict(kv.split("=", 1) for kv in line.split()[1:])
    # Law 1: the entry check aborts the process (SIGABRT) on a second worker.
    rec_["aborted"] = srv.returncode in (-6, 134) or "UnitEntered" in full_err
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


def run_rate(side, cell, rate, seed, group):
    """One trial at one rate (plus infra reruns); returns (pass, records)."""
    recs = []
    r = one_run(side, cell, rate, seed)
    for _ in range(2):
        L0 = r["load"]
        # A server that failed although every turn completed correctly and
        # in time failed for a reason outside the load: on this host, an
        # HTTP probe from another process connecting to the listening port
        # (seen as a "GET / HT" hello). Not a verdict on this rate: rerun.
        stray = bool(L0 and r["rc"] != 0 and L0["failures"] == 0 and L0["timeouts"] == 0
                     and L0["completed"] == L0["issued"] and L0["late"] == 0 and L0["p99_ns"] <= LIMIT_NS)
        if r["load"] is not None and not stray:
            break
        # The load generator produced no result (the server never came
        # up): an infrastructure fault, not a verdict on this rate. Keep
        # the record, then rerun the rate.
        r["group"] = group
        r["infra_retry"] = "server-failed-load-clean" if r["load"] is not None else "no-load-result"
        recs.append(r)
        log(f"  {side} rate={rate:.0f} no load result; rerunning: {r.get('load_err', '')[-160:]!r}")
        r = one_run(side, cell, rate, seed)
    r["group"] = group
    recs.append(r)
    L = r["load"] or {}
    log(f"  {side} {cell} rate={rate:.0f} pass={r['pass']} p99={L.get('p99_ns', 0) / 1e6:.2f}ms e2e99={L.get('e2e_p99_ns', 0) / 1e6:.2f}ms late={L.get('late')} cpu={r.get('server_cpu_s')} "
        f"done={L.get('completed')}/{L.get('issued')} fail={L.get('failures')} to={L.get('timeouts')} tool={r['tool_cpu_s']}")
    return r["pass"], recs


def search_steps(guess):
    """The unchanged search as a generator: yields a rate, is sent its pass,
    returns the highest passing rate (to within STEP**0.25)."""
    lo, hi = None, None
    rate = guess
    if (yield rate):
        lo = rate
        while hi is None:
            rate *= STEP
            if rate > 400000:
                break
            if (yield rate):
                lo = rate
            else:
                hi = rate
    else:
        hi = rate
        while lo is None:
            rate /= STEP
            if rate < 20:
                break
            if (yield rate):
                lo = rate
            else:
                hi = rate
    if lo is not None and hi is not None:
        for _ in range(2):
            mid = (lo * hi) ** 0.5
            if (yield mid):
                lo = mid
            else:
                hi = mid
    return lo


CELLS = [dict(f=0.0, variant="base", rec=False), dict(f=1.0, variant="base", rec=False),
         dict(f=0.5, variant="par", rec=False), dict(f=0.1, variant="par", rec=False),
         dict(f=0.0, variant="base", rec=True), dict(f=0.5, variant="par", rec=True)]
SIDES = ["untied", "S3k7", "tokiow8"]


def plan(name):
    """(cells, sides) for a plan."""
    pick = {"base": [0, 1], "par": [2, 3], "rec": [4, 5], "all": [0, 1, 2, 3, 4, 5],
            "pilot": [0, 2]}[name]
    if os.environ.get("P3_CELLS"):
        pick = [int(x) for x in os.environ["P3_CELLS"].split(",")]
    sides = os.environ.get("P3_SIDES", ",".join(SIDES)).split(",")
    return [CELLS[i] for i in pick], sides


def main():
    pname, outp, groups = sys.argv[1], sys.argv[2], int(sys.argv[3])
    cells, sides = plan(pname)
    guesses = json.loads(os.environ.get("P3_GUESS", "{}"))
    random.seed(int(time.time()))
    best = {}
    rot = int(os.environ.get("P3_ROT", "0"))
    with open(outp, "a") as out:
        for g in range(int(os.environ.get("P3_G0", "0")), int(os.environ.get("P3_G0", "0")) + groups):
            for cell in cells:
                cell_sides = [s for s in sides if not (cell["rec"] and s.startswith("tokio"))]
                k = rot % len(cell_sides)
                cell_sides = cell_sides[k:] + cell_sides[:k]
                rot += 1
                cpus = cpuset(SERVER) | cpuset(LOAD) | cpuset(TOOL)
                run_chunk(cell_sides, cell, cpus, best, guesses, out, g)


HOLD_GAP_S = 15


def run_chunk(cell_sides, cell, cpus, best, guesses, out, g):
    """All sides' searches of one cell advance in lock step. One lock hold
    (regimes, then board; cc1plus checked at both ends) covers one
    interleaved set: one trial per side still searching, at that side's next
    rate step, first side rotating from set to set. Both locks are released
    after the set and the runner waits HOLD_GAP_S before queuing again with a
    blocking flock, so other lanes get their turn (lock sharing, 2026-11-20)."""
    gens, pending, done, seeds = {}, {}, {}, {}
    for side in cell_sides:
        key = f"{side}|{json.dumps(cell, sort_keys=True)}"
        guess = (best.get(key) or guesses.get(f"{side}|{cell['variant']}|{cell['f']}|{int(cell['rec'])}")
                 or guesses.get(f"{side}|{cell['variant']}") or guesses.get(side, 2000))
        gens[side] = search_steps(guess * 0.9)
        pending[side] = next(gens[side])
        seeds[side] = random.randrange(1, 1 << 30)
    k = 0
    while pending:
        live = [s for s in cell_sides if s in pending]
        live = live[k % len(live):] + live[:k % len(live)]
        k += 1
        while True:
            results = {}
            t0 = time.time()
            with Block(cpus, log=log) as b:
                with Tool():
                    for side in live:
                        results[side] = run_rate(side, cell, pending[side], seeds[side], g)
            held = time.time() - t0
            for side, (ok, recs) in results.items():
                for r in recs:
                    r["discarded"] = not b.clean
                    r["set_order"] = live
                    out.write(json.dumps(r) + "\n")
            out.flush()
            log(f"  set {live} held {held:.0f}s clean={b.clean}")
            time.sleep(HOLD_GAP_S)
            if b.clean:
                break
            log("set discarded (cc1plus); rerunning")
        for side, (ok, recs) in results.items():
            try:
                pending[side] = gens[side].send(ok)
            except StopIteration as e:
                done[side] = e.value
                del pending[side]
    for side in cell_sides:
        lo = done[side]
        key = f"{side}|{json.dumps(cell, sort_keys=True)}"
        best[key] = lo or best.get(key)
        out.write(json.dumps(dict(summary=True, side=side, cell=cell, group=g, max_rate=lo, order=cell_sides)) + "\n")
        log(f"group {g} {side} {cell}: max passing rate {lo}")
    out.flush()


if __name__ == "__main__":
    main()
