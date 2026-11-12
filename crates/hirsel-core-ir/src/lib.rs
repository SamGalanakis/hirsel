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
//! `propagate`); logical admission and refund with bound scopes (`limit_enter`,
//! `limit_exit`); task custody through runtime calls (`rt.spawn`, `rt.join`,
//! `rt.cancel`, `rt.check_cancel`, `rt.channel_new`, `rt.channel_sender`,
//! `rt.send`, `rt.try_send`, `rt.receive`). Rule texts: failure.md, budgets.md and
//! tasks.md at spec revision 2 and the failure-model report (FAIL-2..14,
//! MEM-14..16, MEM-19..29, CONC-3..9).
//!
//! **FORM v1b (draft, not frozen; round 3 of the spec is simplifying these):**
//! regions (`region_open`, `region_close`, `alloc .. in`); stable, scoped and
//! compact types with their span, text, link and builder recipes, and the
//! destination writer; callable environments (`ownfn`, `closure_env`); provider
//! operations and host-resource admission. Producers emit none of these yet.
//! Static brands, branded resources, `SlotOwner` and `uses` lists are not in
//! FORM v1 at all: a pool handle is plain data checked at run time.
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
//! whose used part is handed over at the end. A counted node is a u64 count at
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
//! Every function with a body takes the task context pointer as a hidden first
//! argument, before the destination; a call that can fail returns the failure
//! status beside its results, and a nonzero status enters the call's failure
//! successor (or propagates). The context carries the current limit stack and the
//! cause chain. Runtime calls are calls to engine entry points with the rows
//! above. Layouts are the recipes': resources and boxes are pointers, counted
//! nodes are count then payload.
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
//! status are FORM v1a additions to this convention (Native ABI, above).
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
