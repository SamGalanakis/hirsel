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
//! **FORM v1 (not frozen until the settled S2 rules):** layout and drop recipes;
//! own allocation and growth, destinations, regions, Rc/Arc, compact projections
//! and builders; what a failure block receives (cause storage), cleanup and
//! propagation of failure across calls (the `fail` successor of a call), logical
//! admission and refund, the context parameter, task custody and providers. v0 names
//! a failure successor's *existence* on the instructions that can fail, nothing more.
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
//! instance key ([`InstanceKey::digest`]). The context parameter, failure status and
//! task entry are FORM v1 additions to this convention.
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

mod ir;
pub mod text;
#[cfg(any(test, feature = "wf"))]
pub mod wf;

pub use ir::*;

#[cfg(test)]
mod tests;
