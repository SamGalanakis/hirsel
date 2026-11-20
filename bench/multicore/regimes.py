"""Inputs and checksum oracles for the scheduler cases in run.py."""
from dataclasses import dataclass
from functools import cache

MASK = (1 << 64) - 1
GRAINS = (0, 100, 1000, 10000, 100000)
CALIBRATION_SIZE = 50_000
CHECKBOX_CLIENTS = 64
CHECKBOX_TOGGLES = 200


@dataclass(frozen=True)
class Workload:
    size: int
    units: int = 1
    recorded: bool = False
    go: bool = True
    stages: int = 1
    idle: int = 0
    connections: int = 0


WORKLOADS = {
    "independent16": Workload(512, 16),
    "independent256": Workload(32, 256),
    "pairs": Workload(1024, 16, recorded=True),
    "single_pair": Workload(8192),
    "fanin": Workload(2048, 16, recorded=True),
    "fanout": Workload(2048, 16),
    "pipeline": Workload(2048, recorded=True, stages=8),
    "grain_spawn_join": Workload(8192, recorded=True),
    "grain_spawn_chain": Workload(512, 16, recorded=True),
    "spawn_tree": Workload(128, 64, recorded=True),
    "channel_hub": Workload(1024, 16),
    "arc_hub": Workload(2048, 16, go=False),
    "skew": Workload(8192, idle=1000),
    "timers": Workload(10_000),
    "sockets64": Workload(128, 64, go=False, connections=64),
    "sockets1024": Workload(8, 1024, go=False, connections=1024),
    "checkbox": Workload(CHECKBOX_TOGGLES, CHECKBOX_CLIENTS, go=False),
    "calibrate": Workload(CALIBRATION_SIZE),
}


def apply(columns, value):
    result = 0
    while value:
        bit = value & -value
        result ^= columns[bit.bit_length() - 1]
        value ^= bit
    return result


@cache
def xorshift_power(rounds):
    # The recurrence is a linear map over GF(2). Each column gives the
    # image of a single set bit. Compose powers by squaring the map.
    def column(value):
        value ^= (value << 13) & MASK
        value ^= value >> 7
        return value ^ ((value << 17) & MASK)

    identity = tuple(1 << bit for bit in range(64))
    power = tuple(map(column, identity))
    result = identity
    while rounds:
        if rounds & 1:
            result = tuple(apply(power, value) for value in result)
        power = tuple(apply(power, value) for value in power)
        rounds >>= 1
    return result


@cache
def kernel_sum(first, last, rounds):
    if rounds == 0:
        return triangle(last) - triangle(first - 1)
    columns = xorshift_power(rounds)
    shifts = range(0, last.bit_length(), 8)
    tables = [tuple(apply(columns, byte << offset) for byte in range(256)) for offset in shifts]
    total = 0
    for seed in range(first, last + 1):
        value = 0
        for offset, table in zip(shifts, tables):
            value ^= table[(seed >> offset) & 255]
        total += value
    return total


def triangle(n):
    return n * (n + 1) // 2


def checkbox_checksum(clients, toggles):
    # Sum the bytes of the final XOR board formed by the prescribed inputs.
    odd = set()
    boundary = (0, 7, 8, 511, 512, 999_999)
    for lane in range(clients):
        state = lane + 0x1062

        def advance():
            nonlocal state
            state ^= (state << 13) & MASK
            state ^= state >> 7
            state ^= (state << 17) & MASK
            return state

        advance()  # Select the reconnect round. This never adds a toggle.
        for i in range(toggles):
            bit = boundary[i] if i < len(boundary) else advance() % 1_000_000
            odd.symmetric_difference_update((bit,))
    return sum(1 << (bit % 8) for bit in odd)


def receipt(case, size, rounds):
    work = WORKLOADS[case]
    count = size * work.units + work.idle
    if case == "checkbox":
        return count, checkbox_checksum(work.units, size)
    checksum = triangle(work.idle) + kernel_sum(work.idle + 1, count, rounds * work.stages)
    if case == "arc_hub":
        checksum += count
    return count, checksum & MASK


def command(bins, case, language, width, size, grain, *args):
    if language == "Hirsel":
        return [bins / "hirsel/regimes", case, size, grain, *args]
    binary = bins / ("rust/regimes" if language == "Tokio" else "go/runtime")
    return [binary, case, width, size, grain, *args]


def ordered_operations(case, size):
    """Count the explicit spawn, join and channel operations of the workload."""
    lanes = 16
    if case in {"independent16", "independent256", "grain_spawn_join", "spawn_tree"}:
        nodes = 126 if case == "spawn_tree" else WORKLOADS[case].units
        return size * nodes * 2
    if case == "grain_spawn_chain":
        return lanes * 2 * (size + 1)
    if case in {"pairs", "single_pair", "skew"}:
        pairs = lanes if case == "pairs" else 1
        idle = WORKLOADS[case].idle
        return pairs * (4 * size + 2) + (2 * pairs if pairs > 1 else 3 * idle + 2 if idle else 0)
    if case == "fanin":
        return lanes * (2 * size + 2)
    if case == "fanout":
        return lanes * (4 * size + 2) + 2
    if case == "pipeline":
        return 18 * size + 18
    if case == "channel_hub":
        return lanes * (6 * size + 2) + 2
    if case == "arc_hub":
        return lanes * (2 * size + 2)
    if case == "timers":
        return 3 * size
    return None


def rate(row):
    return row["iterations"] / row.get("service_seconds", row["seconds"])


def render(rows, crossover_width):
    """Show measured cells, matched peer ratios, and the observed crossover."""
    from collections import defaultdict
    from statistics import median

    cells = defaultdict(list)
    for row in rows:
        key = (row["case"], row["mode"], row["grain_ns"], row["language"], row["width"])
        cells[key].append(row)
    widths = sorted({row["width"] for row in rows})
    grains = sorted({row["grain_ns"] for row in rows})
    peers = ("Hirsel", "Tokio", "Go")
    good = {key: {r["repeat"]: rate(r) for r in records if r["status"] == "ok"}
            for key, records in cells.items()}
    crossover = {}

    for mode in ("untaped", "recorded"):
        for case in WORKLOADS:
            points = [key for key in cells if key[:2] == (case, mode)]
            if not points or case == "calibrate":
                continue
            print(f"\n### {case}, {mode}\n")
            print("| Grain target (ns) | Port | " + " | ".join(map(str, widths)) + " |")
            print("| ---: | --- |" + " ---: |" * len(widths))
            for grain in grains:
                for lang in peers:
                    prefix = (case, mode, grain, lang)
                    baseline = good.get((*prefix, 1), {})
                    if not any(key[:4] == prefix for key in points):
                        continue
                    values = []
                    for width in widths:
                        key = (*prefix, width)
                        samples = good.get(key, {})
                        statuses = sorted({r["status"] for r in cells.get(key, ()) if r["status"] != "ok"})
                        if not samples:
                            values.append(", ".join(statuses) or "—")
                            continue
                        rates = list(samples.values())
                        speedup = median(rates) / median(baseline.values()) if baseline else None
                        if width == crossover_width and speedup and speedup > 1:
                            crossover.setdefault((case, mode, lang), grain)
                        tokio = good.get((case, mode, grain, "Tokio", width), {})
                        paired = [r / tokio[i] for i, r in samples.items() if i in tokio]
                        value = f"{median(rates):,.0f}"
                        if len(rates) > 1:
                            value += f" [{min(rates):,.0f}–{max(rates):,.0f}]"
                        value += f" / {speedup:.2f}×" if speedup is not None else " / —"
                        value += f" / {median(paired):.2f}×" if paired else " / —"
                        if len(paired) > 1:
                            value += f" [{min(paired):.2f}–{max(paired):.2f}]"
                        value += f" (n={len(rates)})"
                        if statuses:
                            value += f" ({', '.join(statuses)})"
                        values.append(value)
                    print(f"| {'service' if case == 'checkbox' else grain} | {lang} | " +
                          " | ".join(values) + " |")
    print(f"\n### Observed crossover at width {crossover_width}\n")
    print("| Case | Mode | Hirsel | Tokio | Go |")
    print("| --- | --- | ---: | ---: | ---: |")
    for case in WORKLOADS:
        if case in {"checkbox", "calibrate"}:
            continue
        for mode in ("untaped", "recorded"):
            if not any(key[:2] == (case, mode) for key in cells):
                continue
            values = []
            for lang in peers:
                available = any(all(good.get((case, mode, grain, lang, width)) for width in (1, crossover_width))
                                for grain in grains)
                values.append(str(crossover.get((case, mode, lang), "None observed")) if available else "—")
            print(f"| {case} | {mode} | " + " | ".join(values) + " |")
