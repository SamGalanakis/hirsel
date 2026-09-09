# Native driver verification probes

These probes **assert the observed defects exist**, so a successful run confirms the September 9 baseline. After fixes they should fail and be replaced by normal regression assertions. See [the verification report](../verification-drivers.md) for results, interpretation, and provider-dependent limits.

The executed sources were in `/tmp/hirsel-driver-verification-20260909`: `examples/verify.rs`, `examples/verify_bus.rs`, and fake peers `bin/codex` / `bin/claude`. The retained native probe and peers are unchanged copies. The bus template retains only probe code; `prepare.py` extracts the actual production definitions at reproduction time. No production source or build outputs are vendored here. Packaging and the preparation script were added after the successful probes; the packaged workflow has not been rerun.

`tested-source-sha256.json` records SHA-256 hashes of the actual driver files compiled, the exact extracted bus definitions (without trailing newlines), and T3's captured wire fixture. The main checkout was dirty at HEAD `2df5888b8838ec2ea3c3cdadf4707b03bc4c4897`; HEAD alone is not its source baseline. The isolated crate used compatible dependencies resolved offline, rather than the full host workspace lockfile.

## Reproduce

Requires Linux, Rust with edition 2024 support, Python 3 at `/usr/bin/python3`, and the pinned T3 clone. For `child` mode the peer intentionally reads `/tmp/ref-t3code/apps/server/src/provider/testFixtures/codexMultiAgentWire.json`; ensure that checkout is at `e16b8b059c9f5ff6dfed1addecffb831c6aee043`. Obtain the clone from `https://github.com/pingdotgg/t3code.git` if needed. No provider authentication is used.

From this directory, create a fresh isolated directory (the preparation script refuses to overwrite one):

```bash
python3 prepare.py /workspace/code/hirsel /tmp/hirsel-driver-probe-repro
cd /tmp/hirsel-driver-probe-repro
cargo build --offline --examples
```

If dependencies are not already cached, omit `--offline`. Compare the copied `src/*.rs` against `tested-source-sha256.json` when reproducing the historical result; later production edits can change the outcome.

Run the fake peers with PATH overridden only inside each probe subprocess:

```bash
python3 - <<'PY'
import os
import pathlib
import subprocess

root = pathlib.Path.cwd()
for mode in ("startup", "reject", "queue", "exit0", "child"):
    env = os.environ.copy()
    env["PATH"] = str(root / "bin") + ":" + env["PATH"]
    env["VERIFY_MODE"] = mode
    env["VERIFY_PID"] = str(root / (mode + ".pid"))
    result = subprocess.run(
        [str(root / "target/debug/examples/verify")],
        cwd=root, env=env, capture_output=True, text=True, timeout=8,
    )
    print(mode, result.returncode, result.stdout, result.stderr)
    assert result.returncode == 0
subprocess.run(
    [str(root / "target/debug/examples/verify_bus")],
    cwd=root, check=True, timeout=5,
)
PY
```

The startup case explicitly kills its recorded fixture group; other successful native cases call the driver's retirement method. Only fake peers are launched. The fixed timeouts bound observation; progress notifications serve as ordering barriers for request rejection and queued turns. The bus probe checks extracted receiver behavior, not an injected failure in a live database.
