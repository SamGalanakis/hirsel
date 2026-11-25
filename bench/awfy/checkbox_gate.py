"""Checkbox memory-gate arithmetic."""
from decimal import Decimal
from pathlib import Path

CHECKBOX_CLIENTS = 64


def process_rss_bytes(pid):
    try:
        line = next(line for line in Path(f"/proc/{pid}/smaps_rollup").read_text().splitlines()
                    if line.startswith("Rss:"))
    except (OSError, StopIteration) as error:
        raise ValueError(f"missing resident RSS for server process {pid}") from error
    return int(line.split()[1]) * 1024


def session_rss_sample(sample, idle_rss):
    loaded_rss = sample["server_rss_bytes"]
    return sample | {
        "server_idle_rss_bytes": idle_rss,
        "server_loaded_rss_bytes": loaded_rss,
        "server_rss_bytes": loaded_rss - idle_rss,
    }


def checkbox_value(sample, metric):
    value = Decimal(str(sample[metric]))
    if sample["status"] != "ok" or not value.is_finite() or value <= 0:
        raise ValueError(f"invalid {metric} sample")
    if metric == "server_rss_bytes":
        if sample["connections"] != CHECKBOX_CLIENTS:
            raise ValueError(f"RSS must cover {CHECKBOX_CLIENTS} live sessions")
        value /= CHECKBOX_CLIENTS
    return value


def checkbox_changes(reference, candidate, metric):
    for row in (reference, candidate):
        if row["status"] != "ok" or tuple(row["metrics"]) != (metric,):
            raise ValueError("checkbox row has a different metric or an incomplete sample")
    for side in ("hirsel", "rust"):
        before, after = (checkbox_value(row["samples"][side], metric) for row in (reference, candidate))
        percent = Decimal(100) * (after - before) / before
        yield side, before, after, percent
