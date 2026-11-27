# Fleet Formal Specifications and Proofs

Verification artifacts for the Hirsel Fleet per-Place move-authorization protocol
(`docs/spec/fleet.md`, `core/programs/fleet`). The nightly laws stage replays the
fresh three-host SM1 capture. Run the bounded exploration and other proofs on demand
via `python3 proofs/fleet/check_proofs.py`.

## Artifacts

- **`FleetMove.tla` + `FleetMove.cfg`**: Bounded TLA+ model of `ClaimAuth`, `ORIGIN` (O1–O4),
  Rule F1 (single predecessor per open set), concurrent `Permit` and `Continue` opens, and
  read-only settlement, checked exhaustively with TLC.
- **`TraceFleetMove.tla` + `TraceFleetMove.cfg`**: TLC NDJSON trace-validation wrapper checking
  SM1 traces (e.g., `sample_trace.ndjson` or traces exported from `std.sim.trace`) against
  `FleetMove`.
- **`fleet_move.qnt`**: Quint model of the same protocol for randomized invariant verification
  and ITF model-based test generation (`quint run --mbt --out-itf=...`).
- **`FleetMoveVeil.lean`**: Veil (Lean 4) all-`N`, unbounded-epoch inductive fencing proof over
  uninterpreted `type node`, discharging all 7 safety properties and 8 inductive invariants via
  `#check_invariants` and cross-checked against bounded TLC.

## Running

External tools live outside the repository (`~/.local/opt/fleet-formal` for TLC/CommunityModules/Quint
and `/tmp/ref-veil` or `$VEIL_DIR` for Veil):

```sh
python3 proofs/fleet/check_proofs.py --only all
python3 proofs/fleet/check_proofs.py --only tlc
python3 proofs/fleet/check_proofs.py --only trace
python3 proofs/fleet/check_proofs.py --only trace --trace-from-tests tests.json
python3 proofs/fleet/check_proofs.py --only quint
python3 proofs/fleet/check_proofs.py --only veil
```
