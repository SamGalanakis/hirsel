"""Host facts recorded with each goal board: what its seconds and counters ran on."""
import os
from pathlib import Path

NOTE = ("Each seconds value is one sample per side with no repeat, so it carries no spread; the "
        "instruction counts are the gated measurement. The board's compiler is built without BOLT "
        "(DIST_BOLT=off), while releases ship a BOLT-optimized compiler trained on the release std "
        "and cold LangArena builds.")


def sysfs(path):
    try:
        return Path(path).read_text().strip()
    except OSError:
        return None


def facts(measure_cpus):
    """What the board ran on: CPU model, kernel, frequency policy, boost and SMT per measured CPU."""
    cpus = "/sys/devices/system/cpu"
    models = {line.split(":", 1)[1].strip() for line in Path("/proc/cpuinfo").read_text().splitlines()
              if line.startswith("model name")}
    turbo = sysfs(f"{cpus}/intel_pstate/no_turbo")
    measured = {}
    for cpu in sorted(measure_cpus):
        policy = f"{cpus}/cpu{cpu}/cpufreq"
        boost = sysfs(f"{policy}/boost") or sysfs(f"{cpus}/cpufreq/boost")
        if boost is None and turbo is not None:
            boost = "0" if turbo == "1" else "1"
        measured[str(cpu)] = {
            "governor": sysfs(f"{policy}/scaling_governor"), "driver": sysfs(f"{policy}/scaling_driver"),
            "energy_preference": sysfs(f"{policy}/energy_performance_preference"),
            "boost": {"1": True, "0": False}.get(boost),
            "smt_siblings": sysfs(f"{cpus}/cpu{cpu}/topology/thread_siblings_list"),
        }
    return {"cpu_model": ", ".join(sorted(models)), "kernel": os.uname().release,
            "smt": {"active": sysfs(f"{cpus}/smt/active") == "1", "control": sysfs(f"{cpus}/smt/control")},
            "measure_cpus": sorted(measure_cpus), "cpus": measured,
            "seconds_samples_per_side": 1}


def markdown(host):
    """The board.md lines for a host block, or none for a report without one."""
    if not host:
        return []
    lines = ["## Host", "", f"{host['cpu_model']}, kernel {host['kernel']}, SMT {host['smt']['control']}, "
             f"seconds samples per side {host['seconds_samples_per_side']}", ""]
    return lines + [f"- CPU {cpu}: governor {cpu_facts['governor']} ({cpu_facts['driver']}, "
                    f"{cpu_facts['energy_preference']}), boost {cpu_facts['boost']}, "
                    f"SMT siblings {cpu_facts['smt_siblings']}" for cpu, cpu_facts in host["cpus"].items()] + [""]
