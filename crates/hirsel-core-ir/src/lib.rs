//! CoreIR: the lowered form the one-pass front end emits and both native emitters
//! read. A [`Module`] is the whole emitter input apart from layout recipes
//! (`hirsel-core-layout`, FORM v1), the target and the optimisation policy.
//!
//! # Versions
//!
//! **FORM v0 (frozen):** neutral identities (`hirsel-core-ids`); the CFG (blocks with
//! parameters, edges with arguments, `Jump`/`Branch`/`Switch`/`Return`); values,
//! slots and addresses; the scalar, compare, convert, storage and call
//! instructions; signature, instance and function rows; constants and statics;
//! item-relative source anchors; the text form ([`text`]); the calling convention
//! and instance rules below.
//!
//! **FORM v1a (frozen; header `core v1`):** declared types with layout and drop
//! recipes (`Module::types`, `hirsel-core-layout`, charge policy `charge-v1`);
//! own allocation, growth (`alloc .. like`), `free` and `copy` (moves, replace
//! with install-before-drop, growth); `drop` by recipe, with initialization bits
//! kept by `UpdateMask`; resources as blocks with a cause cell and a finalizer
//! grant; Rc/Arc nodes (`alloc counted`, `share`, `release`, `try_unwrap`);
//! failure successors (`fail bN` on the instructions that can fail, `raise`,
//! `propagate`); `never` signatures, whose calls end their block before
//! `unreachable`; logical admission and refund with bound scopes (`limit_enter`,
//! `limit_exit`); task custody through runtime calls (`rt.spawn`, `rt.join`,
//! `rt.cancel`, `rt.check_cancel`, `rt.channel_new`, `rt.channel_sender`,
//! `rt.send`, `rt.try_send`, `rt.receive`). Rule texts: failure.md, budgets.md and
//! tasks.md (FAIL-2..14, MEM-14..16, MEM-19..28, CONC-3..9).
//!
//! **FORM v1b (frozen at spec revision 4):**
//! - Variants: `case` reads a variant place's case number (`u32`), `set_case`
//!   writes it, and `case c f` address steps reach a case's payload. Own, resource,
//!   stable and scoped variants; `Option` and `Result` are ordinary variant rows of
//!   the type table, one per instantiation. The tag's place and width stay private
//!   (MEM-13): a producer never names the tag bytes.
//! - Inline arrays `Inline[T, n]` (SEM-9): field type `array n T`, `n` elements at
//!   `T`'s native stride, no block and no charge of their own; the `element v`
//!   step fails with `Bounds` at its `address` when `v` is not below `n`; cleanup
//!   goes from index 0 up.
//! - Regions (`region_open`, `region_close`, `alloc .. in`). A region handle is a
//!   value and never a field (MEM-1).
//! - Stable and scoped records, compact records and active-size compact variants
//!   (MEM-9..11), links (MEM-10), `Text` (`W` count, then bytes) and `FixedSpan`
//!   (`W` count, then elements at a fixed stride) (MEM-12), with the charges
//!   `charge::standalone` and `charge::span_builder` (MEM-24). Span builders are
//!   ordinary code over these: `alloc .. in`, `copy`.
//! - Callable environments: an `own fn` is a code pointer and an environment
//!   pointer (logically 16). The environment block is one word naming its drop
//!   glue, then the captures in the order the body first names them; it is
//!   charged `charge::closure_env` of the captures' sizes (the glue word is never
//!   charged, MEM-24) and its glue cleans the captures in reverse, frees the block
//!   and refunds it. A captureless `own fn` has a null environment. Captures hold
//!   no resource, so this cleanup never fails and runs no finalizer.
//! - Providers (CAP-5): a provider operation is a closed runtime row (below,
//!   "Providers"), a cancellation point when it `waits` (so it can fail),
//!   admitting nothing. `HostFile` is a mechanical resource that is not a
//!   block: a `u64` provider handle (field type `hostfile`, 8 bytes, logically 8),
//!   with no cause cell, no grant and no charge. Its drop step calls the runtime
//!   entry `hirsel_core_hostfile_drop(context, handle)`, which issues the
//!   provider's close through the replay point, ends any lock, ignores the
//!   outcome, admits nothing and cannot fail (core.md, MEM-24, FAIL-13); it is
//!   not a cancellation point and adds no status. It is cleaned where its field
//!   falls in the recipe's order. It never reaches counted cleanup: it is not
//!   `shareable`, and a counted payload holds no resource (MEM-16).
//! - Cleanup of `List`, `String` and `Map` storage, the `Map` entry slot, and the
//!   step order a counted release resumes by, dynamic sequences and maps
//!   included, are stated in `hirsel-core-layout` (`DropRecipe`, `map_entry`).
//!
//! Brands, `SlotOwner` and `uses` lists are not in FORM v1 at all: a pool handle
//! is plain data checked at run time.
//!
//! # Compact data (FORM v1b, spec 6ef88514)
//!
//! Compact values are region bytes. FORM adds four primitives for them and
//! nothing else: a `byte vN` step (an unaligned byte offset), `pointer aN` (an
//! address as a `ptr`), `trim vB in vR entry vE from vP charge vN` (lowers a region block's charge and
//! refunds the difference to its record) and `rt.dest_id` (a run-unique `u64`,
//! cannot fail). `alloc`'s size is a value, so a blob of `n` bytes is one alloc.
//! Writers, readers, `clone_in`, `seq.index_in`, `text.as_string`, `writer.push`
//! and `emit` are ordinary producer code over these, `copy`, `read` and `store`.
//! - **Blob**: a region block of a 16-byte engine header (used bytes at 0,
//!   written at finish; W in bytes at 8) then the data, charged round8(n) and
//!   never the header (MEM-24). Data offsets below are from the block pointer.
//! - **Locator**: a view of, or stable reference to, compact data is
//!   `{blob ptr, at}` (`FieldTy::Locator`, logically 16); `Stable` of a compact
//!   type is refused. An `IndexedSpan` reads and iterates as a `SeqSpan` with the
//!   same locator and W; its trailer (count W-wide offsets from the first element
//!   byte) follows the elements, outside its own length but inside the holder's
//!   extent. It is never a writer, a `Child` or a field encoding.
//! - A region alloc can return an optional third u64 result, the opaque payer-entry
//!   token captured at admission. Only the destination writer passes it back to
//!   trim; its numeric value is engine bookkeeping and is not program data.
//!   `alloc plain` has Own's semantics but promises no outgoing mechanical drop;
//!   general `alloc own` and counted nodes prepay a resumable native frame.
//! - **Build**: a record `{blob, region, entry, charge, cursor, end = 16 + n, w, id = rt.dest_id}`; a
//!   writer frame `{record, start, count, last}`; a `Child` `{id, at}`. Every
//!   write checks, in order: the identity of each link it stores, in field order
//!   (`Bounds`); then fit and the W cap (`Overflow`, an earlier fit failure
//!   first); then writes, each header, scalar or field checked before its bytes.
//!   An appended element's returned `Child` is checked at receipt (id and at are
//!   the frame's, else `Bounds`) before the count advances. Closing an
//!   `IndexedSpan` checks the whole trailer fits before storing any entry.
//!   `build_stream_in` refuses with `Overflow` when n exceeds the W cap, then
//!   admits round8(n). Finish stores used = cursor - 16 and W; `finish_trim`
//!   then `trim`s the blob with its region, captured entry and previous charge to
//!   round8(used) (MEM-23). Streaming roots are stable
//!   streamable types only.
//! - **Packed** element types (layout's predicate): numbers, bool, char, `Text`,
//!   compact types, non-empty plain records and tuples of packed fields (encoded
//!   field after field), `FixedSpan` and sequences of packed types. `()`, Never,
//!   callables, capabilities, links and `Inline` of anything else are refused.
//! - **`clone_in`** of a streamable value copies exactly its bytes at its own W
//!   into a new blob, as a new root, charged round8(length); any other compact
//!   value copies its whole blob. `copy_closed_in` does not exist.
//!   `seq.index_in` allocates round8(24 + 8 count) (`charge::seq_index`).
//! - `text.as_string(t)` is a `String` view `{pointer of the bytes, length}`;
//!   `emit` of a `String` view or a `Text` into a `Text` is one fit check and one
//!   `copy`. An unmarked `T` parameter also takes a `View[T]` (OWN-7): the
//!   producer passes the locator.
//! - **A view of a plain type** is that type's own representation (a scalar, or a
//!   place of the type), copied when the view is made: plain values have no identity
//!   and viewed storage is frozen, so the copy cannot be told apart. A view of a
//!   plain packed element decodes it field after field into a native place (a compact
//!   field becomes its locator); a view of a compact element is its locator.
//!   Iterating a span advances by the element's packed size, constant for a plain
//!   element and `compact_extent` at the holder's W otherwise. A `FixedSpan` element
//!   MEM-9 does not fix uses the same packed encoding.
//!
//! # Strings and literals (FORM v1b)
//!
//! A `List`, `String` or `Map` carrier `{data, length, capacity}` owns storage
//! exactly when its capacity is not 0. Cleanup, the counted release's last step
//! and growth test the capacity, never the data pointer: a capacity-0 carrier frees
//! nothing, and its first growth is an `alloc own` against the current limit
//! (MEM-26), never `alloc .. like`.
//! A dynamic ordinary/counted or region allocation may carry `overflow vN`, a
//! bool saying its exact logical charge exceeds u64. It refuses admission even
//! with u64::MAX available, reports that limit's actual room, and follows the
//! same finalizer-grant failure path as any refusal. Omission means false.
//! - **A string literal** (core.md "Literals") is `{static dN, byte length, 0}`:
//!   one `static` read and two constants, stored through `byte` steps over the
//!   carrier. It admits nothing, charges nothing and cannot fail. `""` may use a
//!   null pointer. Passed to a view parameter it is that carrier in a frame slot,
//!   with nothing to clean: a capacity-0 `String` has no storage and plain
//!   elements, so a producer may omit its `drop`.
//! - A literal's length exceeds its capacity, so every room test is
//!   `length + n > capacity` (after the overflow check), never `capacity - length`.
//!   Growth copies `length` bytes out of the static data and frees nothing. Static
//!   data is read-only: no `store` or `copy` targets it (the evaluator stops on
//!   one). `string.into_bytes` and `clone` of a capacity-0 `String` copy into
//!   newly admitted storage of capacity equal to the length (core.md, MEM-24);
//!   `string.as_bytes` reports capacity 0.
//! - **Order** of `String`, `Text` (through its `text.as_string` view), `Bytes`
//!   and `List[u8]` is `rt.compare_bytes(a, a_len, b, b_len) -> i8` (-1, 0, 1;
//!   unsigned bytes, a proper prefix first, SEM-7): `==` is `eq r, 0`, `<` is
//!   `lt r, 0`. Other sequences, tuples, records and variants compare by producer
//!   code over their components, calling it for each text component. Comparison
//!   admits nothing.
//!
//! # Providers (FORM v1b)
//!
//! A provider or run-time C entry is reached only through a closed runtime row
//! ([`RtOp`]); FORM has no call of a C symbol by name. The provider rows are
//! `rt.args_len`, `rt.args_get`, `rt.host_*` and `rt.stdout_write` /
//! `rt.stderr_write` ([`HostOp`], one row per core operation; signatures at
//! [`RtOp::signature`]). Their operands are scalars and pointers only: the
//! capability, `Args` or `HostFile` handle, and each `String` or `Bytes` as a
//! pointer to its carrier `{data, length, capacity}` (a view is already that
//! pointer). Every `host`, `stdout` and `stderr` row returns the outcome
//! `value: u64, error: u8, code: u32, written: u64`, from which the producer
//! builds the `Result`; the rows that `wait` add the failure status (Native ABI)
//! and fail with `Cancelled` when cancellation is pending, `host_mkdtemp` also with
//! `Bounds` when the name has fewer than 32 spare bytes. `host_wall_ms` and
//! `host_mono_ns` return the `u64` alone and cannot fail; neither can the `args`
//! rows. The run time reads and writes the carrier: `host_read` fills spare
//! capacity and `host_mkdtemp` appends the directory's name, each storing the new
//! length; neither changes the data pointer or the capacity (no provider
//! operation admits). `rt.args_get` returns a run-owned `String` carrier of
//! capacity 0, or null past the end: it is the `View[String]` of `Some`, valid
//! until the run ends.
//!
//! Engines map the rows to the run time's entries, the context first:
//! - `rt.args_len` → `uint64_t hirsel_core_args_len(const HirselCoreContext *,
//!   uint64_t args)`.
//! - `rt.args_get` → `const HirselCoreBuffer *hirsel_core_args_get(const
//!   HirselCoreContext *, uint64_t args, uint64_t index)`.
//! - every `HostOp` → `HirselCoreHostOutcome hirsel_core_host_op(HirselCoreContext
//!   *, uint8_t operation, uint64_t handle, HirselCoreBuffer *operand, uint8_t
//!   mode, uint64_t site)`, with [`HostOp::code`], the row's handle, its carrier
//!   pointer (null when it has none) and `mode` (0 when it has none).
//!   `HirselCoreHostOutcome` is the row's results then the status, laid out as
//!   the Native ABI's result struct; a row that cannot fail ignores the status
//!   and keeps `value`. The entry resolves the provider: `open` and the
//!   directory and stream operations by the capability handle, the file
//!   operations by the `HostFile`'s retained provider.
//!
//! # Maps (FORM v1b)
//!
//! `map.new` is the empty carrier `{0, 0, 0}` and `map.len` reads the live count.
//! `get`, `insert`, `set` and `remove` are ordinary code in core's `map` module over
//! private bodiless operations, each a sequence over the `map_entry` recipe: an
//! entry's hash, state, key and value by slot; the lookup index (a power of two of
//! u64 words after the entries, each a slot + 1 or 0, probed linearly); appending
//! an entry; swapping a value; removing an entry (state 0, the key cleaned, the
//! value moved out); and growth (admit, copy the live entries in order, free the
//! old storage when its capacity is not 0). Cleanup is the recipe's: live entries in
//! insertion order, each key before its value.
//!
//! A key's hash is glue built from `K`'s recipe: from `h = 0` it folds
//! [`hash_mix`] `(h, w) = rotl((h ^ w) × 0x9E3779B97F4A7C15, 26)` over each scalar
//! component zero-extended to `u64`, in field order (a variant's tag first), and
//! over `rt.hash_bytes(data, len)` for each `String`, `Bytes` or `Text` component,
//! then returns `h ^ (h >> 32)`. `rt.hash_bytes` ([`hash_bytes`]) starts at
//! `h = len` and folds the little-endian 8-byte words, the last zero-padded. The
//! function is fixed, not seeded: the producer hashes static map literals with
//! [`hash_bytes`] and the run time must agree. Iteration is insertion order
//! (SEM-5), so no hash is observable. Engines map `rt.hash_bytes` to `uint64_t
//! hirsel_core_hash_bytes(const uint8_t *, uint64_t)`, with no context or status.
//!
//! # Identities
//!
//! `rt.dest_id` is the run's one counter of fresh `u64` identities: a
//! destination's (MEM-4) and every `Unique` (`unique.new()` is `call rt.dest_id ()`).
//!
//! # Causes (FORM v1b)
//!
//! A `Causes` value's one handle is its head cause cell (null when empty); the cells
//! are the run time's. `for c in cs` walks them, primary cause first:
//! `rt.cause_value(cell) -> ptr` is the cell's `Cause` and `rt.cause_next(cell) -> ptr`
//! the next cell, null after the last. Neither takes the context, admits, waits or
//! fails; engines map them to `hirsel_core_cause_value` and `hirsel_core_cause_next`.
//! The `Cause` is in core's layout (40 bytes): the `CauseKind` tag (u8, declaration
//! order: Overflow 0, Bounds 1, Division 2, Unwrap 3, Fail 4, Cancelled 5, Capacity 6)
//! at 0, `Fail`'s `String` carrier at 8, `Site` (u64) at 32. `View[Cause]` is that
//! pointer, valid while the `Causes` lives. A `Fail` cause owns its message; cleaning
//! the `Causes` frees it when its capacity is not 0.
//!
//! # Capabilities and the entry (FORM v1b)
//!
//! A capability value (`Dir`, `Net`, `Clock`, `Random`, `Stdout`, `Stderr`,
//! `Stdin`, `Env`, `Process`, `Tasks`) and `Args` is a `u64` provider handle
//! (field type `cap`, logically 8). It is plain (CAP-2): copying it copies the
//! handle, and nothing in the program cleans it. The provider makes every handle
//! (entry grants, attenuation) and keeps what it designates in host memory,
//! outside every limit, until the run ends; provider operations take the handle.
//! `Args` designates the run's argument table, read by its runtime operations.
//!
//! `main`'s row is the context, its result destination, then one `u64` per
//! parameter in declaration order. The process entry passes the configuration's
//! `grants[i]` for parameter `i` (`Args` included); the driver maps each parameter
//! type to the configuration's grant and refuses a run whose configuration does
//! not grant one before calling `hirsel_core_run` (CAP-4). After `main` returns and
//! its cleanup is done, the run ends its grants: the provider closes what they
//! designate. That is not program cleanup: it admits nothing, runs no finalizer
//! and cannot fail, and no copy of a handle is live by then.
//!
//! # Failure (FORM v1a)
//!
//! An instruction that can fail ([`Inst::can_fail`]) may name a failure successor:
//! a block without parameters, entered with the cause already appended to the
//! task's chain. Without one, the failure leaves the function at once, so a
//! producer names a successor exactly where owners are live. A failure path ends
//! in `propagate` (or `raise`, which appends a new cause first) and never reaches a
//! `return`: nothing catches a failure (FAIL-14). Propagation reserves nothing;
//! the chain lives in one cell per task and one per resource (FAIL-10). Cleanup on
//! a failure path drops the live owners in the order the normal exit would. A
//! failing finalizer appends its cause, its resource's fields are still cleaned and
//! its block freed (FAIL-12); a pending result is discarded. Mechanical cleanup
//! (fields, boxes, counted releases, handles) never fails (FAIL-13). `Capacity` is
//! a failure only inside a finalizer, against its grant (MEM-27); everywhere else a
//! refused admission is a value: `alloc` yields null and the room left in the
//! limit it tried (MEM-22 `available`).
//!
//! # Storage and admission (FORM v1a)
//!
//! Every charge is a `charge-v1` logical size computed by the producer and passed
//! to `alloc`; `free` refunds the same amount to the limit the block's record now
//! leads to (MEM-20, MEM-23). `limit_enter b` admits `b` and makes a limit of `b`
//! current; `limit_exit` returns its unused part and forwards its record to the
//! parent, so blocks made inside stay charged (MEM-25). `alloc .. like v` admits
//! against the limit `v` was charged to (MEM-26), except inside a finalizer, where
//! it admits from the current limit (MEM-27): the grant, or a bound scope entered
//! within the finalizer, which is then current (MEM-19). A refusal is the failure
//! `Capacity` only when the refusing limit is the grant itself; a bound scope's
//! refusal, inside a finalizer too, is a value (MEM-25), and `limit_enter`'s always
//! is. A resource value is a pointer to its block, charged
//! storage + cause cell + grant; its finalizer runs under a limit of its grant,
//! whose used part is handed over at the end. A whole destructure moves the fields
//! out with `copy` and `free`s the block, refunding that complete charge (MEM-23). A counted node is a u64 count at
//! offset 0 and the payload at offset 8; `deref` of an rc or arc field names the
//! payload. A counted payload holds no resource, so a release runs no user code.
//! Every counted row names its kind: `alloc counted rc|arc` and `share`,
//! `release` and `try_unwrap` with `rc|arc`, so each row alone says whether its
//! count operations are atomic. An `arc` node is made and counted atomically, and
//! each of these rows is an event in the run's order (MEM-15); `rc` never crosses
//! a task (Rc is not `send`). The producer takes the kind from the core type the
//! source operation is on, `rc.*` or `Rc[T]` gives `rc`, `arc.*` or `Arc[T]`
//! gives `arc`, never from bounds of `T`; a row whose kind differs from its node's
//! allocation is malformed (the evaluator stops on it).
//!
//! # Runtime calls (FORM v1a)
//!
//! `Callee::Runtime` names an engine operation with a fixed signature row
//! ([`RtOp::signature`]); the call site's signature must be that row. Task and
//! channel charges are admitted whole at `rt.spawn` and `rt.channel_new` (queue,
//! result slot, record, primary cell and the child's limit), so send, receive and
//! join admit nothing. A refused spawn returns null with `x` still the caller's.
//! `rt.join` writes the result, or returns a `Causes` that owns the child's used
//! cells until it is dropped (`JoinError = Failed(Causes) | Cancelled(Causes)`).
//!
//! # Native ABI (FORM v1a)
//!
//! **Every function row** uses one convention, the same for direct calls, indirect
//! calls through a code pointer (`fnref`) and `extern` rows (a FORM function
//! defined in another module, linked by its instance digest; never a C symbol):
//! - The task context pointer comes first, then the destination pointer when
//!   `dest` is present, then the parameters as in FORM v0. The context carries the
//!   current limit stack and the cause chain.
//! - The function returns, in the target's C convention, a struct of its result
//!   scalars **followed by a `u8` failure status**: 0 when it returned, 1 when it
//!   failed, its cause already appended to the task's chain in the context. No
//!   other value occurs. With no results the return is a plain C `u8`; with one
//!   result it is `{ r0, u8 }` (two registers on x86-64 SysV and AArch64).
//!   A `never` function returns only the status, which is always 1.
//! - After the call the caller tests the status once: zero continues with the
//!   results, nonzero enters the call's failure successor, or propagates (returns
//!   status 1 itself) when there is none. Results are undefined when it is 1.
//!
//! Every function row has the status, whether or not its body can fail: a caller
//! reads only the callee's signature (LANG-8) and an instance is compiled and
//! cached alone, so whether a body can fail is not known at the call. A function
//! whose body cannot fail returns status 0 by a constant; an emitter that has the
//! callee's body in hand (inlining, or a callee in the same unit with no failing
//! exit) may drop the test. `main` is entered by the engine the same way.
//!
//! **Runtime rows** (`Callee::Runtime`) are the runtime's own declared C entry
//! points, with the row's parameters and results. Only the rows that can fail
//! ([`RtOp::fails`]: `join`, `check_cancel`, `channel_new`, `send`, `receive`)
//! append the `u8` status, and only those may name a failure successor. The
//! runtime entries behind instructions (`alloc`, `free`, `limit_enter`, region
//! operations, `trim`) are not calls in FORM and report refusals as values.
//! `rt.dest_id` is a runtime row with no parameters and one `u64`, no status;
//! `rt.compare_bytes` likewise takes no context and has no status.
//!
//! Layouts are the recipes': resources and boxes are pointers, counted nodes are
//! count then payload.
//!
//! # Values, slots and addresses
//!
//! A value is defined once, by a block parameter or an instruction result, and is
//! a scalar ([`Scalar`]) or a pointer. A slot is a typed piece of frame memory with
//! no initialization discipline beyond what its producer guarantees: `StorageLive`
//! begins it, `StorageDead` ends it, reads and stores in between name addresses.
//! An address is a base (a slot, a pointer value or another address) and steps;
//! `Inst::Address` evaluates it and runs its bounds checks, and later reads and
//! stores use it. Uses need not be dominated by definitions in any sense stronger
//! than the program's own control flow: emitters do not re-check.
//!
//! # Calling convention (FORM v0)
//!
//! A function with signature `(p0, ..., pn) -> (r0, ..., rm) dest t` is called
//! with, in order: a pointer to the destination when `dest` is present; then each
//! parameter, a scalar or pointer by value, or a by-address parameter (`&t`) as a
//! pointer to a place of that type the caller owns for the duration of the call.
//! Results come back in registers in the target's C convention for a struct of the
//! result scalars (`m <= 1`: the plain C return). `bool` travels as a byte 0/1,
//! `char` as a u32 scalar value. Link names are `h` followed by the hex digest of the
//! instance key ([`InstanceKey::digest`]). The context parameter and the failure
//! status are the FORM v1a additions to this convention (Native ABI, above).
//!
//! # Instances
//!
//! A function row names an instance: a definition plus the canonical text of each
//! closed type argument and of each implementation chosen for a constraint. Sizes
//! are not part of the key: they are a function of the types and the target, and
//! sharing code between same-layout instances is an emitter's identical-code
//! folding, not identity. The driver collects the instances a program reaches and
//! caches them by digest; emitters materialise the bodies they are given. A
//! program whose instantiation keeps growing (`f[T]` calling `f[List[T]]`) is
//! refused by the driver as a compiler resource refusal (type-argument nesting or
//! instance count past the compiler's resource limit), never as a language error
//! and never by a depth rule in the language.

#[cfg(test)]
hirsel_guard::install!();

/// A test evaluator for the FORM v0 subset, for this crate's samples and its producers'
/// behaviour cases.
#[cfg(any(test, feature = "eval"))]
pub mod eval;
mod ir;
pub mod text;
#[cfg(any(test, feature = "wf"))]
pub mod wf;

pub use ir::*;

#[cfg(test)]
mod tests;
