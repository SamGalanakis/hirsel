"""Receipt fields and coverage shared by fair and the comparison board."""
from datetime import timedelta, timezone

from bench.multicore import regimes

VERSION = 2
CPU_DEFAULTS = {
    "server": (0, 1, 2, 3, 16, 17, 18, 19),
    "load": (24, 25),
    "helper": (26, 27),
    "controller": (26, 27),
}
MEASURE_CPUS = set().union(*CPU_DEFAULTS.values())
BUILD_CPUS = (8, 9, 10, 11)
BUILD_JOBS = 4
BUILD_MEMORY_BYTES = 4 << 30
MEASUREMENT_MEMORY_BYTES = 2 << 30
SYNC_POLICY = {"wait_us": 400, "max_writes": 4096, "durable_syncs_per_group": 1}
HOLD_POLICY = "foreign-cpu-share"
MEASUREMENT_SECONDS = 240
IDLE_LIMIT = .05
MIN_ARM_NS = 1_000_000_000
FOREIGN_SHARE_LIMIT = .015
CONNECTIONS = (256, 2048, 8192)
CASES = tuple(case for case in regimes.WORKLOADS if case != "calibrate")
ARMS = ("hirsel", "tokio")
A1_SIDES = ("untied", "tokiow8")
A1_BINARIES = ("a1/bin/session_u", "a1/bin/twin", "a1/bin/loadgen",
               "a1/bin/toolsrv", "a1/software/twin")
REGIME_BINARIES = ("regimes/hirsel/regimes", "regimes/rust/regimes")
CHECKBOX_BINARIES = ("regimes/checkbox/checkbox", "regimes/checkbox/wants.bin",
                     "regimes/wire/release/wire-reference", "regimes/wire/release/wire-load")


def valid_topology(cpu_sets):
    if len(cpu_sets["server"]) != 8:
        return False
    if any(not cpus or any(type(cpu) is not int or cpu not in MEASURE_CPUS for cpu in cpus)
           or len(cpus) != len(set(cpus)) for cpus in cpu_sets.values()):
        return False
    server, load = (set(cpu_sets[role]) for role in ("server", "load"))
    auxiliary = set(cpu_sets["helper"]) | set(cpu_sets["controller"])
    return not (server & load or (server | load) & auxiliary)


def a1_cells(full):
    populations = CONNECTIONS if full else (256, 8192)
    cells = [("ext4", size, "hardware") for size in populations]
    if full:
        cells.extend(("tmpfs", size, "hardware") for size in populations)
        cells.append(("ext4", 8192, "software"))
    return cells


def regime_grains(case, full):
    if case == "checkbox":
        return (0,)
    return regimes.GRAINS if full else (100_000,)


def regime_widths(full):
    return (1, 4, 8) if full else (1, 8)


def expected_rows(suites, cases, full):
    rows = {}
    if "a1" in suites:
        for storage, size, sha in a1_cells(full):
            label = f"{storage}/{size}/{sha}"
            rows["A1", label] = storage, size, sha, not full
            if full:
                rows["A1", label + "/matched"] = storage, size, sha, True
    if "regimes" in suites:
        for case in cases:
            for grain in regime_grains(case, full):
                for width in regime_widths(full):
                    rows["Regimes", f"{case}/{grain}ns/w{width}"] = case, grain, width
    return rows


def admits_arm(arm):
    return (arm["trace_complete"] is True and arm["duration_ns"] >= MIN_ARM_NS
            and all(0 <= arm["foreign_share"][role] < FOREIGN_SHARE_LIMIT
                    for role in ("server", "load")))


def timing_window_open(begin, end):
    begin, end = begin.astimezone(timezone.utc), end.astimezone(timezone.utc)
    blocked = begin.replace(hour=2, minute=45, second=0, microsecond=0)
    if begin >= blocked.replace(hour=4, minute=0):
        blocked += timedelta(days=1)
    return begin <= end < blocked


def admits_hold(hold):
    return (hold["board_window_open"] is True and hold["cc1plus_start"] == hold["cc1plus_end"] == []
            and 0 < hold["hold_s"] < MEASUREMENT_SECONDS and hold["load_start"] <= 8 and hold["load_end"] <= 8
            and not ({"load_exceeded", "port_conflict"} & hold.keys())
            and bool(hold["arm_cpu"])
            and all(admits_arm(arm) for arm in hold["arm_cpu"].values()))
