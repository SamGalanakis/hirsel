#!/usr/bin/env python3
"""Make missing, failed and unjustified skipped Mac CI jobs red."""

import json
import os

JOBS = {
    "plan": "planner",
    "cross": "Linux image build",
    "mac": "Mac checks",
    "mac_driver_inputs": "Mac driver fixtures",
    "mac_driver": "Native Mac driver unittests",
    "nightly_cross": "nightly Linux corpus",
    "nightly_native": "nightly cross/native comparison",
}
MAC_STEPS = {
    "platform": "Host platform assertion",
    "download": "Linux artifact download",
    "digests": "Linux image digests",
    "keychain": "Keychain bootstrap",
    "images": "Linux-built Mach-O laws",
    "build": "Native backend test build",
    "laws": "Native backend laws",
    "hygiene": "Mac hygiene",
}


def failed_mac_steps(outputs):
    failures = {}
    for step, label in MAC_STEPS.items():
        if outputs.get(step) != "failure":
            continue
        cases = json.loads(outputs.get(f"{step}_failures") or "[]")
        if cases:
            for case in cases:
                failures[f"mac-{case}"] = f"{label} failed in `{case}`."
        else:
            failures[f"mac-{step}"] = f"{label} failed."
    return failures


def aggregate(jobs):
    reasons = jobs.get("plan", {}).get("outputs", {})
    failures = {}
    rows = []
    for job, label in JOBS.items():
        state = jobs.get(job, {})
        result = state.get("result", "missing")
        reason = reasons.get(f"{job}_reason", "").strip()
        if job == "mac_driver" and result == "success" and state.get("outputs", {}).get("tests") != "success":
            detail = "missing successful driver test step"
            failures["mac-mac_driver"] = f"{label}: {detail}."
        elif result == "success":
            detail = "passed"
        elif result == "skipped" and reason:
            detail = f"skipped · {reason}"
        else:
            detail = "unexpected skip" if result == "skipped" else result
            checks = failed_mac_steps(state.get("outputs", {})) if job == "mac" else {}
            failures.update(checks or {f"mac-{job}": f"{label}: {detail}."})
        rows.append(f"- {label}: {detail}")
    rows.append("- S8 store crash cuts: Linux only. The provider fixture requires ELF linker wrapping.")
    return failures, rows


def main():
    failures, rows = aggregate(json.loads(os.environ["NEEDS"]))
    lines = ["## Mac CI", *rows, "", *[f"- {key}: {detail}" for key, detail in failures.items()]]
    print("\n".join(lines))
    with open(os.environ["GITHUB_STEP_SUMMARY"], "a") as summary:
        print("\n".join(lines), file=summary)
    return bool(failures)


if __name__ == "__main__":
    raise SystemExit(main())
