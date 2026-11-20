#!/usr/bin/env python3
"""mc-units-untied: attribution runs for the recorded cells (not searches).

Usage: attrib3u.py OUT.jsonl CONFIGS_JSON
CONFIGS_JSON: [[side, cell_index, rate], ...]. For each perf mode (stat,
sched, record) one lock hold runs every config once at its fixed rate with
that perf mode attached to the server (part3u's P3_PERF hook: perf on CPUs
6-7, 3 s, starting 3.5 s into the load). Same lock discipline as part3u's
sets: one hold per mode, both locks released after it, 15 s before queuing
again. After the last hold the tapes of the last config stay in
/tmp/mcut/tapes for a by-kind split (tape_kinds below).
"""
import json, os, sys, time
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import part3u as P
from blocks import Block, cpuset


def main():
    outp, configs = sys.argv[1], json.loads(sys.argv[2])
    modes = os.environ.get("ATTRIB_MODES", "stat,sched,record").split(",")
    cpus = cpuset(P.SERVER) | cpuset(P.LOAD) | cpuset(P.TOOL)
    with open(outp, "a") as out:
        for mode in modes:
            os.environ["P3_PERF"] = mode
            while True:
                recs = []
                t0 = time.time()
                with Block(cpus, log=P.log) as b:
                    with P.Tool():
                        for side, ci, rate in configs:
                            r = P.one_run(side, P.CELLS[ci], rate, 12345)
                            r["perf_mode"] = mode
                            recs.append(r)
                            L = r["load"] or {}
                            P.log(f"  attrib {mode} {side} cell{ci} rate={rate} pass={r['pass']} p99={L.get('p99_ns', 0) / 1e6:.2f}ms "
                                  f"cpu={r['server_cpu_s']} done={L.get('completed')}")
                P.log(f"  attrib hold {mode} {time.time() - t0:.0f}s clean={b.clean}")
                for r in recs:
                    r["discarded"] = not b.clean
                    out.write(json.dumps(r) + "\n")
                out.flush()
                time.sleep(P.HOLD_GAP_S)
                if b.clean:
                    break


main()
