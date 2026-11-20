import sys, json
sys.path.insert(0, "/home/sam/.t3/worktrees/hirsellang/t3-72e2eab5-da842f76728ddb8a/bench/units")
import part3
from blocks import Block, cpuset
side = sys.argv[1]; rates = [float(x) for x in sys.argv[2].split(",")]; bursts = [float(x) for x in sys.argv[3].split(",")]
cell = dict(f=0.0, variant="base", rec=False)
with Block(cpuset(part3.SERVER) | cpuset(part3.LOAD) | cpuset(part3.TOOL), log=part3.log) as b:
    with part3.Tool():
        for bx in bursts:
            part3.RUN["burst_x"] = bx
            for rate in rates:
                r = part3.one_run(side, cell, rate, 12345)
                L = r["load"] or {}
                print(json.dumps(dict(side=side, rate=rate, burst=bx, p50=L.get("p50_ns"), p99=L.get("p99_ns"), p999=L.get("p999_ns"), e2e99=L.get("e2e_p99_ns"), late=L.get("late"), done=L.get("completed"), issued=L.get("issued"), cpu=r.get("server_cpu_s"), sys=r.get("server_sys_s"), tool=r.get("tool_cpu_s"))), flush=True)
print("clean", b.clean)
