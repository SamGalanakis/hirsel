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
//! - Providers (CAP-5): a provider operation is a call of an engine function by
//!   its signature row, a cancellation point when it `waits` (so it can fail),
//!   admitting nothing. `HostFile` is a handle (`hostfile`) of a mechanical
//!   resource: no cause cell, cleaned by closing it and ignoring the outcome.
//!
//! Brands, `SlotOwner` and `uses` lists are not in FORM v1 at all: a pool handle
//! is plain data checked at run time.
//!
//! # Compact data (FORM v1b, spec 6ef88514)
//!
//! Compact values are region bytes. FORM adds four primitives for them and
//! nothing else: a `byte vN` step (an unaligned byte offset), `pointer aN` (an
//! address as a `ptr`), `trim vB charge vN` (lowers a region block's charge and
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
//! - **Build**: a record `{blob, cursor, end = 16 + n, w, id = rt.dest_id}`; a
//!   writer frame `{record, start, count, last}`; a `Child` `{id, at}`. Every
//!   write checks, in order: the identity of each link it stores, in field order
//!   (`Bounds`); then fit and the W cap (`Overflow`, an earlier fit failure
//!   first); then writes, each header, scalar or field checked before its bytes.
//!   An appended element's returned `Child` is checked at receipt (id and at are
//!   the frame's, else `Bounds`) before the count advances. Closing an
//!   `IndexedSpan` checks the whole trailer fits before storing any entry.
//!   `build_stream_in` refuses with `Overflow` when n exceeds the W cap, then
//!   admits round8(n). Finish stores used = cursor - 16 and W; `finish_trim`
//!   then `trim`s the blob to round8(used) (MEM-23). Streaming roots are stable
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
//! the grant takes precedence. A resource value is a pointer to its block, charged
//! storage + cause cell + grant; its finalizer runs under a limit of its grant,
//! whose used part is handed over at the end. A whole destructure moves the fields
//! out with `copy` and `free`s the block, refunding that complete charge (MEM-23). A counted node is a u64 count at
//! offset 0 and the payload at offset 8; `deref` of an rc or arc field names the
//! payload. A counted payload holds no resource, so a release runs no user code.
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
//! calls through a code pointer (`fnref`) and `extern` rows (providers):
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
//! `rt.dest_id` is a runtime row with no parameters and one `u64`, no status.
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
