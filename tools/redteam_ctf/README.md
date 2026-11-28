# Red-team containment CTF

A capture-the-flag run against Hirsel's containment. One pack host process holds two
Places in one pool:

- the **Canary Place**: trusted Hirsel code (`trusted/canary.hirsel`) on its own
  execution and thread, holding a fresh random 256-bit secret in its heap;
- the **Attacker Place**: a second execution in the same pool, where a trusted warden
  (`trusted/warden.hirsel`) loads one challenge module with no capability and runs its
  entry under a byte bound, a step bound and `isolate`.

Each challenge in `challenges/` is one attack. The attacker wins by reading the secret,
by taking the host or the canary down, or by making the host use memory or time nobody
charged it for. `oracle.py` judges every run from outside the host process.

```
tools/redteam_ctf/ctf.py --build                        # whole corpus, both engines
tools/redteam_ctf/ctf.py --build --self-check           # every broken host must be caught
tools/redteam_ctf/ctf.py --build --sabotage leak        # one broken host: BREACH, exit 1
tools/redteam_ctf/ctf.py --build --only ports_bound_churn --engine cranelift --verbose
tools/redteam_ctf/ctf.py --build --receipt out.json --write-scoreboard
```

`--build` builds `//:hirsel-pack-example-ctf_host` and `//:hirsel-core-driver-bin-hirsel`
with Buck2; `--host` and `--hirsel` name ones already built. Exit status: 0 when every
case is `DEFENDED`, 1 on any `BREACH` or `UNEXPECTED`, 2 on a harness error or no case.
A full run is 68 cases and takes a few minutes. It needs Linux (`/proc`).

## Verdicts

| verdict | meaning | passes |
|---|---|---|
| `DEFENDED` | no breach signal, the canary answered a fresh nonce after the attack and retired cleanly, the host exited 0, and the attack ended by a mechanism its challenge declared | yes |
| `BREACH` | at least one breach signal below | no |
| `UNEXPECTED` | no breach, but the attack ended some other way than the challenge declared (a clean return where a refusal was due, a host line the oracle does not know, a run stop nobody declared) | no |
| `ERROR` | the harness never got to attack: the host did not start, seal or answer, or refused a harness command | no |

Breach signals, each found without trusting anything the host says about itself:

| signal | how it is seen |
|---|---|
| `leak:canary`, `leak:decoy` | 9 or more consecutive bytes of the secret (or of the decoy token the attacker was handed) in the host's standard output or standard error, or inside any hex field of a host line, spelled raw, as hex of either case, or as base64 |
| `crash:<SIGNAL>` | the host process was ended by a signal the harness did not send |
| `oom` | the host exited with `hirsel_guard`'s memory-limit status (71) |
| `rss` | the host process's own peak resident memory passed the ceiling (`--rss-ceiling-mib`, default 256; an idle host is about 9 MiB) and the harness ended it |
| `hang` | the host did not answer within the compile cap plus the challenge's wall bound plus slack, and the harness ended it |
| `exit:<n>` | any other non-zero exit status |
| `canary:probe` | after the attack the canary did not answer `sha256(secret ‖ nonce)` for a nonce it had not seen |
| `canary:retire` | the canary did not retire cleanly at the end |

`--self-check` runs the real host with each `--sabotage` mode (it echoes the secret in
five ways, aborts, faults, hangs, hoards memory, swaps or kills the canary) and fails
unless every one is reported as its breach and the unbroken host is `DEFENDED`.

## How an attack may end

A challenge declares the mechanisms that count as contained. The oracle derives the
mechanism from the host's reply line and maps anything it does not recognise in full to
`unknown`, which no challenge can declare.

| mechanism | meaning |
|---|---|
| `refused:<RULE-N>`, `refused:<FAMILY>` | the compiler refused the module and its first diagnostic names that rule (`refused:OWN` admits any `OWN-n`) |
| `refused:resource` | the implementation's resource refusal, which names no rule (PROG-6) |
| `compile-limit:<Cpu\|Memory\|Wall>` | the compile worker reached a PACK-3 cap |
| `compile-unsupported`, `compile-tool-refusal`, `compile-unavailable`, `compile-busy` | the other PACK-3 refusals |
| `admit-refused`, `load-refused:<Refusal>` | the loader refused the module (PACK-10, PACK-14) |
| `raw-refused:<Refusal>`, `raw-loaded` | what the loader did with altered module bytes |
| `returned:<text>` | the entry returned those bytes; a challenge that must survive says what it returns |
| `bounded:OutOfTime`, `bounded:Capacity`, `bounded:Stack` | the step bound, a byte limit or the stack ceiling ended the run as a typed cause under `isolate` |
| `bounded:Wall` | the host stopped the run at the challenge's wall bound |
| `failed:<Overflow\|Bounds\|Division\|Unwrap\|Fail\|Cancelled>` | the entry failed with that typed cause |
| `stopped:<Name>` | a run stop (FAIL-15) ended the Attacker Place |
| `escaped:<n>` | the entry ended the warden's execution outside `isolate` |

A clean return is never folded into "bounded" and a run stop is never folded into a
typed failure: each passes only where a challenge declares it.

## Challenges

A challenge is a Hirsel module exporting `attack(seed: u64) -> Bytes` (or the entry its
attack needs), with its terms in leading `// ctf:` lines:

```
// ctf: class heap-exhaustion           the attack class (required)
// ctf: about <free text>               what it tries and which rule should stop it
// ctf: expect bounded:Capacity, ...    the mechanisms that count as contained
// ctf: arena plain|held                held: the warden hands it a decoy token and a Held value
// ctf: pool pure|provider              provider: the pool also admits Class::Provider imports
// ctf: steps N / bytes N / root N / wall N    step bound, byte bound, root limit, wall ms
// ctf: tamper <plan> expect <mechanisms>      alter the admitted module's bytes and load them raw
```

Expectations come from the specification, never from a first run. Tamper plans are
`keep:N`, `drop:N`, `flip:POS`, `grow:N`, `length:SECTION`, `noise:N` and `bloat:MiB`,
each a fixed function of the module's bytes. There is no stack knob: the stack ceiling
is the runtime's own (FAIL-17).

Classes in the corpus: type expansion, port exhaustion, metering, ownership, stack
exhaustion, heap exhaustion, `Held` confusion, literal custody, capability forgery,
malformed modules, and one benign control.

## Receipt and scoreboard

`--receipt` writes `hirsel.redteam-ctf.receipt.v1` JSON: the base commit, the ceiling,
the leak window, counts per verdict, and one record per case sorted by challenge and
engine (verdict, breaches, mechanisms, the host's words, what was expected, elapsed
milliseconds, the host's peak resident KiB, the peak of the host with its compile
workers, exit status, the challenge's SHA-256). The secret is never in it. Verdicts and
mechanisms are deterministic; milliseconds and KiB are measurements.

`--write-scoreboard` rewrites `SCOREBOARD.md` from the same data. It is a snapshot of
one run on one commit, not a gate: nothing compares against it.

## What `DEFENDED` does not mean

- One owner per pool. The Canary and the Attacker share a pool and a process, which is
  the strongest co-residency the pack host offers; two tenants who do not trust each
  other still belong in separate processes.
- No speculative-execution, cache or timing channels. The oracle looks for bytes of the
  secret in what the host writes, not for bits recovered through a side channel.
- Only these attacks. A green board says this corpus was contained on that commit.
- The memory ceiling is on the host process. Compile workers are separate processes
  capped by PACK-3 and reported as `tree_peak_rss_kb`.

## Gate

`//:test-redteam-ctf` runs `test_oracle.py`: the oracle's laws on hand-built transcripts
and the runner against `stub_host.py`, a stand-in that speaks the host's protocol and
breaks in every way the oracle must catch. It needs no compiler and no pack host, and
its inputs are files of this directory only.
