"""Attribution block (not a reported cell): one side, the par variant, one
rate, three runs inside one timing block.

  run A: per-thread CPU (from /proc, mid-run window) + perf stat counters
  run B: perf record (flat, kernel+user) for the attribution by symbol
  run C: plain rerun at the same rate, for the latency figure without perf

Usage: diag6.py SIDE RATE F OUTDIR
"""
import json, os, subprocess, sys, threading, time
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import part3
from blocks import Block, cpuset

side, rate, f, outdir = sys.argv[1], float(sys.argv[2]), float(sys.argv[3]), sys.argv[4]
os.makedirs(outdir, exist_ok=True)
cell = dict(f=f, variant="par", rec=False)
WINDOW = 4.0  # seconds, inside the 8 s load (after warm-up, spanning the burst at 4 s)


def threads(pid):
    out = {}
    for t in os.listdir(f"/proc/{pid}/task"):
        try:
            name = open(f"/proc/{pid}/task/{t}/comm").read().strip()
            st = open(f"/proc/{pid}/task/{t}/stat").read().rsplit(")", 1)[1].split()
            sched = open(f"/proc/{pid}/task/{t}/status").read()
            vcs = int([l for l in sched.splitlines() if l.startswith("voluntary_ctxt")][0].split()[1])
            ncs = int([l for l in sched.splitlines() if l.startswith("nonvoluntary_ctxt")][0].split()[1])
            out[t] = (name, int(st[11]) + int(st[12]), vcs, ncs)
        except (OSError, IndexError):
            pass
    return out


def server_pid(before):
    end = time.time() + 5
    while time.time() < end:
        try:
            pid = int(open(f"{part3.WORK}/server.pid").read())
            if pid != before and os.path.exists(f"/proc/{pid}"):
                return pid
        except (OSError, ValueError):
            pass
        time.sleep(0.02)
    return None


def prev_pid():
    try:
        return int(open(f"{part3.WORK}/server.pid").read())
    except (OSError, ValueError):
        return None


def watcher(mode, result):
    pid = server_pid(prev_pid_holder[0])
    if pid is None:
        result["error"] = "no server pid"
        return
    time.sleep(0.2 + 1.5)  # loadgen start offset + warm-up
    if mode == "stat":
        t0 = threads(pid)
        p = subprocess.run(["perf", "stat", "-x", ",", "-e",
                            "task-clock,context-switches,cpu-migrations,cycles,instructions,"
                            "syscalls:sys_enter_sendto,syscalls:sys_enter_recvfrom,syscalls:sys_enter_epoll_wait,"
                            "syscalls:sys_enter_write,syscalls:sys_enter_read,syscalls:sys_enter_futex",
                            "-p", str(pid), "--", "sleep", str(WINDOW)], capture_output=True, text=True)
        t1 = threads(pid)
        per = {}
        for tid, (name, c1, v1, n1) in t1.items():
            c0, v0, n0 = (t0.get(tid) or (name, 0, 0, 0))[1:]
            key = name
            a = per.setdefault(key, [0, 0.0, 0, 0])
            a[0] += 1
            a[1] += (c1 - c0) / 100 / WINDOW  # cores
            a[2] += v1 - v0
            a[3] += n1 - n0
        # per-unit-thread detail, sorted by load
        detail = sorted(((name, round((c1 - (t0.get(tid) or (0, 0))[1]) / 100 / WINDOW, 3))
                         for tid, (name, c1, _, _) in t1.items()), key=lambda x: -x[1])[:24]
        result["threads"] = {k: dict(n=v[0], cores=round(v[1], 3), vol_cs=v[2], invol_cs=v[3]) for k, v in per.items()}
        result["busiest"] = detail
        result["perf_stat"] = p.stderr
    elif mode == "record":
        data = os.path.join(outdir, f"{side}.perf.data")
        p = subprocess.run(["perf", "record", "-F", "1999", "-o", data, "-p", str(pid), "--", "sleep", str(WINDOW)],
                           capture_output=True, text=True)
        result["perf_record"] = p.stderr[-300:]
        result["perf_data"] = data


prev_pid_holder = [None]
results = []
with Block(cpuset(part3.SERVER) | cpuset(part3.LOAD) | cpuset(part3.TOOL), log=part3.log) as b:
    with part3.Tool():
        for mode in ("stat", "record", "plain"):
            prev_pid_holder[0] = prev_pid()
            extra = {}
            th = None
            if mode != "plain":
                th = threading.Thread(target=watcher, args=(mode, extra))
                th.start()
            r = part3.one_run(side, cell, rate, 777)
            if th:
                th.join()
            L = r["load"] or {}
            rec = dict(side=side, mode=mode, rate=rate, f=f, passed=r["pass"],
                       **{k: L.get(k) for k in ("p50_ns", "p99_ns", "p999_ns", "completed", "issued", "late", "timeouts")},
                       server_cpu_s=r.get("server_cpu_s"), server_sys_s=r.get("server_sys_s"),
                       tool_cpu_s=r.get("tool_cpu_s"), **extra)
            results.append(rec)
            print(json.dumps({k: v for k, v in rec.items() if k != "perf_stat"}), flush=True)
            if "perf_stat" in rec:
                print(rec["perf_stat"], flush=True)
for rec in results:
    rec["block_clean"] = b.clean
with open(os.path.join(outdir, f"{side}.json"), "w") as fh:
    json.dump(results, fh, indent=1)
print("clean", b.clean)
