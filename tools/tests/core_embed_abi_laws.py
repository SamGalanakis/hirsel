#!/usr/bin/env python3
"""Native C11 header and discriminant, refusing-allocator (M3), and shared
3145 leased-stream kernel transition vector laws (TS == Rust)."""
import os
from pathlib import Path
import re
import shutil
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]

EXPECTED_EXPORTS_V1 = [
    "hirsel_embed_v1_abi",
    "hirsel_embed_v1_manifest",
    "hirsel_embed_v1_start",
    "hirsel_embed_v1_open",
    "hirsel_embed_v1_call",
    "hirsel_embed_v1_abandon",
    "hirsel_embed_v1_subscribe",
    "hirsel_embed_v1_unsubscribe",
    "hirsel_embed_v1_room_join",
    "hirsel_embed_v1_room_set",
    "hirsel_embed_v1_room_send",
    "hirsel_embed_v1_room_leave",
    "hirsel_embed_v1_close",
    "hirsel_embed_v1_stop",
]

KERNEL_VECTOR_SCRIPT = """\
SCENARIO handshake_lanes_and_stale_drop 2 0
Start
SocketOpen 0
SocketOpen 1
FrameWelcome 1 2
WelcomeBound 1 10 3
FramePlaced 1 0
FramePlaced 1 1
PrepareCall 1 0
SendCall 1 0 ws
PrepareCall 1 1
SendCall 1 1 ws
PrepareCall 1 2
SendCall 1 2 ws
PrepareCall 1 3
CallFinished 1 0
PrepareCall 1 3
SendCall 1 3 ws
RetryCall 1 1
RetryElapsed 1 1
AdvanceVersion 1 5
FrameBatch 1 5
SocketLost 1 1006
FrameBatch 1 6
LossRecorded 2
BackoffElapsed 3
SocketOpen 4
FrameWelcome 4 2
WelcomeBound 4 100 3
FramePlaced 4 0
FramePlaced 4 1
StableElapsed 4 20000
StableElapsed 4 30100
Stop Closed LocallyUnknown

SCENARIO regressed_batch_version_incompatible 0 0
Start
SocketOpen 1
FrameWelcome 1 1
WelcomeBound 1 10 2
AdvanceVersion 1 5
FrameBatch 1 4

SCENARIO credential_refresh_and_unbound 0 1
Start
CredentialResult 1 tokA
SocketOpen 2
SocketLost 2 4401
LossRecorded 3
CredentialResult 4 tokA
Reconnect
Begin 6
CredentialResult 7 tokB
SocketOpen 8
FrameWelcome 8 1
WelcomeBound 8 50 2
SocketLost 8 4401
LossRecorded 9
CredentialResult 10 tokC
SocketOpen 11
SocketLost 11 1008
LossRecorded 12

SCENARIO incompatible_1008_1009_and_gone 1 0
Start
SocketOpen 1
FrameWelcome 1 0
WelcomeBound 1 0 1
FramePlaced 1 gone
Reconnect
Begin 3
SocketOpen 4
FrameWelcome 4 0
WelcomeBound 4 0 1
FramePlaced 4 0
SocketLost 4 1009
LossRecorded 5
Reconnect
Begin 7
SocketOpen 8
FrameWelcome 8 0
WelcomeBound 8 0 1
FramePlaced 8 0
SocketLost 8 1008
LossRecorded 9
Stop Fenced RequestMismatch
"""

NODE_RUNNER_JS = r"""
import { initialPhase, step } from './state.ts';
import fs from 'node:fs';

const script = fs.readFileSync(0, 'utf8');
const noop = () => {};

function phaseBackoff(p) {
  return 'backoff' in p ? p.backoff : -1;
}
function owedCount(p) {
  if (p.tag !== 'Ready') return 0;
  let n = 0;
  for (const l of Object.values(p.lanes)) {
    const t = l.attempt.tag;
    if (t === 'Preparing' || t === 'WebSocket' || t === 'Http') n++;
  }
  return n;
}
function actionSig(a) {
  switch (a.tag) {
    case 'State': return `State:${a.state}`;
    case 'ArmBackoff': return `ArmBackoff:${a.maxMs}`;
    case 'ArmRetry': return `ArmRetry:${a.lane}:${a.maxMs}`;
    case 'BecomeReady': return `BecomeReady:${a.welcomedAt}`;
    case 'Finish': return `Finish:${a.outcome}`;
    case 'RejectReady': return `RejectReady:${a.code}`;
    case 'RejectCalls': return `RejectCalls:${a.code}`;
    default: return a.tag;
  }
}

let ctx = { wants: 0, hasCredentialSource: false };
let phase = initialPhase;
let out = '';

for (const raw of script.split('\n')) {
  const line = raw.trim();
  if (!line || line.startsWith('#')) continue;
  const parts = line.split(/\s+/);
  if (parts[0] === 'SCENARIO') {
    ctx = { wants: Number(parts[2]), hasCredentialSource: parts[3] === '1' };
    phase = initialPhase;
    out += `SCENARIO ${parts[1]}\n`;
    continue;
  }
  let ev;
  switch (parts[0]) {
    case 'Start': ev = { tag: 'Start' }; break;
    case 'Reconnect': ev = { tag: 'Reconnect' }; break;
    case 'Begin': ev = { tag: 'Begin', epoch: Number(parts[1]) }; break;
    case 'SocketOpen': ev = { tag: 'SocketOpen', epoch: Number(parts[1]) }; break;
    case 'CredentialResult':
      ev = {
        tag: 'CredentialResult',
        epoch: Number(parts[1]),
        bytes: parts[2] === '-' ? undefined : new TextEncoder().encode(parts[2]),
      };
      break;
    case 'SocketLost':
      ev = {
        tag: 'SocketLost',
        epoch: Number(parts[1]),
        code: parts[2] === '-' ? undefined : Number(parts[2]),
      };
      break;
    case 'LossRecorded': ev = { tag: 'LossRecorded', epoch: Number(parts[1]) }; break;
    case 'BackoffElapsed': ev = { tag: 'BackoffElapsed', epoch: Number(parts[1]) }; break;
    case 'StableElapsed': ev = { tag: 'StableElapsed', epoch: Number(parts[1]), at: Number(parts[2]) }; break;
    case 'FrameWelcome':
      ev = {
        tag: 'Frame',
        epoch: Number(parts[1]),
        frame: {
          tag: 'Welcome',
          values: [{ issuer: 1, subject: new Uint8Array([1]) }, Number(parts[2]), 0n, new Uint8Array(), 't', 16777216n, 262144n, 1000n],
        },
      };
      break;
    case 'WelcomeBound':
      ev = { tag: 'WelcomeBound', epoch: Number(parts[1]), at: Number(parts[2]), width: Number(parts[3]) };
      break;
    case 'FramePlaced':
      ev = {
        tag: 'Frame',
        epoch: Number(parts[1]),
        frame: {
          tag: 'Placed',
          value: parts[2] === 'gone' ? { tag: 'Gone' } : { tag: 'Ready', value: Number(parts[2]) },
        },
      };
      break;
    case 'FrameBatch':
      ev = {
        tag: 'Frame',
        epoch: Number(parts[1]),
        frame: { tag: 'Batch', values: [BigInt(parts[2]), [], [], []] },
      };
      break;
    case 'AdvanceVersion':
      ev = { tag: 'AdvanceVersion', epoch: Number(parts[1]), version: BigInt(parts[2]), apply: noop };
      break;
    case 'PrepareCall':
      ev = { tag: 'PrepareCall', epoch: Number(parts[1]), lane: Number(parts[2]), prepare: noop };
      break;
    case 'SendCall':
      ev = {
        tag: 'SendCall',
        epoch: Number(parts[1]),
        lane: Number(parts[2]),
        delivery: parts[3] === 'ws' ? { tag: 'WebSocket' } : { tag: 'Http', p: BigInt(parts[3]) },
        send: noop,
      };
      break;
    case 'CallFinished':
      ev = { tag: 'CallFinished', epoch: Number(parts[1]), lane: Number(parts[2]) };
      break;
    case 'RetryCall':
      ev = { tag: 'RetryCall', epoch: Number(parts[1]), lane: Number(parts[2]) };
      break;
    case 'RetryElapsed':
      ev = { tag: 'RetryElapsed', epoch: Number(parts[1]), lane: Number(parts[2]) };
      break;
    case 'Stop':
      ev = {
        tag: 'Stop',
        state: parts[1],
        ...(parts[2] ? { outcome: parts[2] } : {}),
      };
      break;
    default:
      throw new Error(`unknown event: ${parts[0]}`);
  }
  const tr = step(phase, ev, ctx);
  phase = tr.phase;
  const acts = tr.actions.map(actionSig).join(',');
  out += `${phase.tag}|${phase.epoch}|${phaseBackoff(phase)}|${owedCount(phase)}|${acts}\n`;
}
process.stdout.write(out);
"""


def extract_header_v1() -> str:
    src = (ROOT / "crates/hirsel-core-ids/src/embed.rs").read_text()
    m = re.search(r'pub const HEADER_V1: &str = r#"([\s\S]*?)"#;', src)
    if not m:
        raise AssertionError("HEADER_V1 not found in crates/hirsel-core-ids/src/embed.rs")
    return m.group(1)


def c_discriminant_names(header: str) -> list[str]:
    header = re.sub(r"/\*.*?\*/|//[^\n]*", "", header, flags=re.S)
    enums = {
        name: body
        for body, name in re.findall(r"typedef\s+enum\s*\{([^}]*)\}\s*(\w+)\s*;", header)
    }
    names = []
    for enum in ("hirsel_status", "hirsel_end_code"):
        names.extend(re.findall(r"(?:^|,)\s*([A-Za-z_]\w*)\s*(?==|,|$)", enums[enum]))
    macros = re.findall(r"^\s*#\s*define[ \t]+(HIRSEL_\w+)", header, flags=re.M)
    names.extend(name for name in macros if name not in {"HIRSEL_EMBED_ABI", "HIRSEL_EMBED_H"})
    return names


class EmbedAbiLaws(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.archive = Path(os.environ["HIRSEL_EMBED_ABI_FIXTURE"]).resolve()
        cls.cc = Path(os.environ["LLVM_SYS_231_PREFIX"]) / "bin/clang"
        cls.node = Path(os.environ["HIRSEL_NODE_EXE"]).resolve()
        cls.scratch = tempfile.TemporaryDirectory(prefix="hirsel-embed-abi-laws-")
        cls.dir = Path(cls.scratch.name)
        cls.header_path = cls.dir / "hirsel_embed.h"
        cls.header_path.write_text(extract_header_v1())

    @classmethod
    def tearDownClass(cls):
        cls.scratch.cleanup()

    def _link_flags(self):
        return ["-lgcc_s", "-lpthread", "-ldl", "-lm", "-lrt", "-lutil"]

    def _compile_strict_c11(self, source: Path) -> Path:
        binary = source.with_suffix("")
        subprocess.run(
            [
                str(self.cc),
                "-std=c11",
                "-Wall",
                "-Wextra",
                "-Werror",
                "-pedantic",
                f"-I{self.dir}",
                str(source),
                str(self.archive),
                *self._link_flags(),
                "-o",
                str(binary),
            ],
            check=True,
        )
        return binary

    def test_status_and_end_discriminants_match_compiled_rust(self):
        self.maxDiff = None
        c_names = c_discriminant_names(self.header_path.read_text())
        self.assertEqual(len(c_names), len(set(c_names)), "duplicate C code names")
        # Check declarations independently of the compiled fixture's list.
        abi_rs = (ROOT / "crates/hirsel-core-rt/src/embed/abi.rs").read_text()
        rust_names = re.findall(r"^\s*pub\s+const\s+(HIRSEL_\w+)\s*:", abi_rs, flags=re.M)

        probe = self.dir / "discriminants.c"
        print_c_codes = "".join(
            f'    printf("C {name} %" PRId64 "\\n", (int64_t){name});\n' for name in c_names
        )
        probe.write_text(
            '#include "hirsel_embed.h"\n'
            '#include <inttypes.h>\n'
            '#include <stdio.h>\n'
            'extern void embed_abi_fixture_discriminants(void (*visit)(hirsel_bytes, int64_t));\n'
            'static void print_rust_code(hirsel_bytes name, int64_t value) {\n'
            '    printf("Rust %.*s %" PRId64 "\\n", (int)name.len, (const char *)name.ptr, value);\n'
            '}\n'
            'int main(void) {\n'
            + print_c_codes
            + '    embed_abi_fixture_discriminants(print_rust_code);\n'
            '    return ferror(stdout) != 0;\n'
            '}\n'
        )
        binary = self._compile_strict_c11(probe)
        output = subprocess.check_output([str(binary)], text=True)
        rows = {"C": [], "Rust": []}
        for line in output.splitlines():
            side, name, value = line.split()
            rows[side].append((name, int(value)))
        self.assertCountEqual([name for name, _ in rows["Rust"]], rust_names, "Rust fixture inventory")
        self.assertCountEqual(c_names, rust_names, "C and Rust code inventories")
        self.assertDictEqual(dict(rows["C"]), dict(rows["Rust"]), "C and Rust discriminants")

    def test_header_v1_strict_c11_and_exact_14_exports(self):
        header_text = self.header_path.read_text()
        found = re.findall(r"\b(hirsel_embed_v1_[a-z_]+)\s*\(", header_text)
        self.assertEqual(found, EXPECTED_EXPORTS_V1)

        # Strict C11 compile check exercising every struct and all 14 symbols.
        check_c = self.dir / "strict_c11.c"
        check_c.write_text(
            '#include "hirsel_embed.h"\n'
            "int main(void) {\n"
            "  volatile uint32_t v = HIRSEL_EMBED_ABI;\n"
            "  (void)v;\n"
            "  (void)&hirsel_embed_v1_abi;\n"
            "  (void)&hirsel_embed_v1_manifest;\n"
            "  (void)&hirsel_embed_v1_start;\n"
            "  (void)&hirsel_embed_v1_open;\n"
            "  (void)&hirsel_embed_v1_call;\n"
            "  (void)&hirsel_embed_v1_abandon;\n"
            "  (void)&hirsel_embed_v1_subscribe;\n"
            "  (void)&hirsel_embed_v1_unsubscribe;\n"
            "  (void)&hirsel_embed_v1_room_join;\n"
            "  (void)&hirsel_embed_v1_room_set;\n"
            "  (void)&hirsel_embed_v1_room_send;\n"
            "  (void)&hirsel_embed_v1_room_leave;\n"
            "  (void)&hirsel_embed_v1_close;\n"
            "  (void)&hirsel_embed_v1_stop;\n"
            "  return 0;\n"
            "}\n"
        )
        self._compile_strict_c11(check_c)

    def test_refusing_allocator_m3_host_memory(self):
        source = ROOT / "crates/hirsel-core-rt/testdata/embed_abi_oom.c"
        binary = self.dir / "embed_abi_oom"
        store_dir = self.dir / "store_oom"
        store_dir.mkdir(exist_ok=True)
        subprocess.run(
            [
                str(self.cc),
                "-O2",
                "-std=c11",
                "-Wall",
                "-Wextra",
                "-Werror",
                f"-I{self.dir}",
                str(source),
                str(self.archive),
                "-Wl,--wrap=malloc,--wrap=exit,--wrap=_exit",
                *self._link_flags(),
                "-o",
                str(binary),
            ],
            check=True,
        )
        run = subprocess.run([str(binary), str(store_dir)], capture_output=True, text=True)
        self.assertEqual(
            run.returncode,
            0,
            f"embed_abi_oom exited {run.returncode}\nstdout:\n{run.stdout}\nstderr:\n{run.stderr}",
        )
        self.assertEqual(run.stderr, "")
        self.assertEqual(run.stdout, "EMBED_ABI_OOM_OK\n")

    def test_shared_3145_kernel_vectors_match_ts_and_rust(self):
        self.maxDiff = None
        # 1. Build and run Rust `embed::kernel::step` vector runner.
        source = ROOT / "crates/hirsel-core-rt/testdata/embed_abi_vectors.c"
        binary = self.dir / "embed_abi_vectors"
        subprocess.run(
            [
                str(self.cc),
                "-O2",
                "-std=c11",
                f"-I{self.dir}",
                str(source),
                str(self.archive),
                *self._link_flags(),
                "-o",
                str(binary),
            ],
            check=True,
        )
        rust_run = subprocess.run(
            [str(binary)],
            input=KERNEL_VECTOR_SCRIPT,
            capture_output=True,
            text=True,
            check=True,
        )
        # Also verify `--header` emitted from compiled Rust `HEADER_V1` matches source header.
        hdr_run = subprocess.run(
            [str(binary), "--header"], capture_output=True, text=True, check=True
        )
        self.assertEqual(hdr_run.stdout, self.header_path.read_text())

        # 2. Run exact same vector script against `sdk/client/state.ts` via Node.
        node_dir = self.dir / "node_vectors"
        node_dir.mkdir(exist_ok=True)
        (node_dir / "package.json").write_text('{"type":"module"}\n')
        (node_dir / "state.ts").write_text((ROOT / "sdk/client/state.ts").read_text())
        runner_js = node_dir / "runner.mjs"
        runner_js.write_text(NODE_RUNNER_JS)
        ts_run = subprocess.run(
            [str(self.node), "--experimental-strip-types", str(runner_js)],
            input=KERNEL_VECTOR_SCRIPT,
            capture_output=True,
            text=True,
        )
        self.assertEqual(
            ts_run.returncode,
            0,
            f"node state.ts runner failed:\nstdout:\n{ts_run.stdout}\nstderr:\n{ts_run.stderr}",
        )
        self.assertEqual(
            rust_run.stdout,
            ts_run.stdout,
            "3145 kernel transition mismatch between Rust embed::kernel and sdk/client/state.ts",
        )


if __name__ == "__main__":
    unittest.main()
