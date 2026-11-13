# Free-mode protocol models

Model checks of the free-mode protocol in `crates/hirsel-free` (design §5 Op sections and the
cut, Claim versus withdraw). Every main run checks them: `loom/` is a workspace crate whose
tests run with the workspace tests at preemption bound 2 (`sync::model`), and
`//:gate-free-genmc` runs the GenMC models on the host that built GenMC (its own fast-tier
stage). `check.sh` runs them all, with the bound-1 sweep and the mutations, and fails unless
every run has its expected result.



- `loom/tests/executors.rs` runs the production executor registry
  (`executors.rs`) against the section gate: two executors preferring one
  word, the second waiting for the first's retirement after its acknowledged
  close while a flush cuts, and the stop releasing a waiting registration
  unregistered. Lost wakes show as loom deadlocks; overlapping owners of one
  word as `UnsafeCell` races on its dirty list.
- `loom/tests/wakes.rs` runs the replay wake registry (`replay/wakes.rs`):
  register-and-recheck under the cell's lock against an advance that takes
  only the waits it enabled (Tokio `sync/tests/loom_notify.rs`). A spurious
  wake re-registers the same wait, as the engine's rerun poll does.
- `loom/tests/frames.rs` runs `frame.rs` through three dispatcher transitions:
  a compact frame's leave racing a role-free wake (one activation, from the
  saved image, on a lent stack); the leave's `Placement` words, so every
  dispatch routes from the executor the frame left; and a stale wake whose
  refused dispatch returns the frame through `Running::return_unstarted`,
  idle, with the `QuietCheck` its save deferred.
- `loom/tests/channel.rs` also runs the role-free replayed receive (an unready
  receive registers its park and rechecks under the cell lock, against a send
  reading `parked` there) and a last sender's role-free close taking every
  parked receiver's wake token under the lock (the fixture token wakes it).

GenMC is built once by `check.sh`, pinned by commit, into
`${XDG_CACHE_HOME:-~/.cache}/genmc-<commit>`; it needs cmake, clang and LLVM 19-22 with their
development files, and libffi. `GENMC=<path>` uses another build.

## Section waits: scope and refinement

The raw spin/yield section models exhausted both 1000 and 10000 branches at
preemption bound 2. One counterexample to the *branch budget* pauses the cutter
just before `stopped := true`: two workers repeatedly read the closed gate and
`stopped == false`, yielding to each other while the cutter never resumes. This
is neither a protocol assertion failure nor a completed safety exploration.
The earlier bound-1 result is retained as historical evidence, not bound-2 proof.

`SectionWaits` factors the wait policy out of the production source. In the
runtime it is zero-sized: `wait` performs the original spin/yield, and `wake` is
a no-op. Protocol atomics, their orderings, initial Dekker accesses, log writes,
publication, and scans are unchanged. The Loom facade instead parks a failed
poll on a persistent per-thread `loom::sync::Notify`. This follows Loom 0.7.2
`src/sync/notify.rs:7-53` and `src/rt/notify.rs:52-159`: a retained notification,
one waiter per token, and at most one spurious wake per token. The host mutex
protecting the token map is deliberately not a modeled synchronization edge.

The reduction concerns finite safety traces, with these obligations:

1. Parking occurs only after the original loop has observed its wait condition.
   Only those polling loops are abstracted. Every entry handshake, critical
   access, phase transition, and scan still runs through the production source
   and Loom's race detector. No iteration that publishes or mutates protocol
   state is skipped.
2. A failed-poll run changes no protocol word or protected data. Interior
   repeated reads can be removed; any constraints they imposed only restricted
   the original execution. Other threads can run before parking, during the
   wait, and after waking. A failed poll that overlaps a release is covered by
   the original loop's reads, the facade's acquire/SC recheck, and a retained
   notification. Tokens exist even before registration, preventing lost wakes.
3. Each notification is immediately after the existing release that ends the
   wait: `active := 0`, `cut := 0`, or `stopped := true`. There is no intervening
   protected access in that thread. Its acquire edge substitutes for the
   acquire of that release by a successful original poll. The original loop
   rechecks after waking; notification alone never admits a section or scan.
   If a newer gate/slot epoch has started, the recheck either keeps waiting or
   observes an equal-or-later release; same-thread order (or the cutter-role
   mutex across cutters) carries the earlier edge. A terminal wake substitutes
   for observing `stopped`, and cannot authorize a further section.
4. Neither the initial entry handshake nor a cutter's first slot check receives
   notification synchronization. In particular, both sides missing each other
   remains detectable: dropping the SC fences still produces the original
   concurrent-write `UnsafeCell` violation, not a deadlock or budget failure.

The bound-2 claim is exhaustive exploration of these six **wait-abstracted**
models at the stated bounds, together with this failed-poll refinement argument.
It is not completed enumeration of arbitrary raw spinning, a proof of fairness
or starvation freedom, or an unbounded proof. Blocking lets other runnable
threads proceed without spending preemptions on failed polls; scheduling,
cancellation, atomic, mutex, and channel model bounds remain separate. The
GenMC RC11/IMM models still check the actual SC handshake independently, without
this wait facade. No branch cap is raised and no exploration is suppressed with
`skip_branch` or `stop_exploring`.
