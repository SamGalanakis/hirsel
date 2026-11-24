//! LLVM inlining policy for CoreIR definitions and direct callback calls.

use std::collections::BTreeMap;

use super::{empty, sink::Inline};
use hirsel_core_ir::*;

struct CallbackRole {
    caller: bool,
    output_loop: bool,
}

/// Inline eligible output loops at calls from small closure-specialized bodies.
pub(super) struct CallbackSites(Vec<CallbackRole>);

impl CallbackSites {
    pub(super) fn new(module: &Module, step_variants: &BTreeMap<FuncId, FuncId>) -> Self {
        Self(
            module
                .functions
                .iter()
                .enumerate()
                .map(|(i, f)| {
                    let sig = &module.signatures[f.signature.index()];
                    let caller = f.body.as_ref().is_some_and(|body| body.insts.len() <= 192)
                        && specialized(module, step_variants, FuncId(i as u32));
                    let output_loop = sig.dest.is_some()
                        && sig.params.contains(&Param::Value(Ty::Ptr))
                        && sig.params.iter().any(|p| {
                            matches!(
                                p,
                                Param::Address {
                                    place: PlaceTy::Value(Ty::Scalar(_)),
                                    loan: Loan::Mut,
                                }
                            )
                        })
                        && f.body.as_ref().is_some_and(|body| {
                            (257..=1024).contains(&body.insts.len()) && !empty::loop_free(body)
                        });
                    CallbackRole {
                        caller,
                        output_loop,
                    }
                })
                .collect(),
        )
    }

    pub(super) fn should_inline(&self, caller: FuncId, callee: FuncId) -> bool {
        self.0[caller.index()].caller && self.0[callee.index()].output_loop
    }
}

pub(super) fn specialized(
    module: &Module,
    step_variants: &BTreeMap<FuncId, FuncId>,
    function: FuncId,
) -> bool {
    module.code_variants.contains_key(&function) && !step_variants.contains_key(&function)
}

pub(super) fn definition(
    definition: &str,
    module: &Module,
    sig: &Signature,
    body: &Body,
    batch_steps: bool,
    called_in_loop: bool,
    specialized: bool,
) -> Inline {
    // Inline the reserved ASCII append and share the UTF-8/growth body.
    // A loop-free callable variant exists only to bind its call site's
    // closure, so it is inlined there unless that site is cold. A variant
    // with a loop amortizes its call and stays out of line. Single-call adapters
    // expose their result staging and cleanup to the caller's optimizer.
    match definition {
        "core@1/string/push_char" => Inline::Always,
        "core@1/string/push_scalar" => Inline::Never,
        "core@1/map/placement" | "core@1/map/probe" => Inline::Always,
        // pivot.rs leaves the recursive sample to the inliner.
        "core@1/list/median3_rec" => Inline::Default,
        "std@1/float/shortest64" | "std@1/float/push_fixed64" => Inline::Always,
        "std@1/float/shortest_decimal64" | "std@1/float/push_fixed_decimal64" => Inline::Never,
        // Share the argument reducers across the three trigonometric entries.
        "std@1/float/trig_reduce" | "std@1/float/trig_reduce_large" => Inline::Never,
        // Share regex construction across call sites. The parser and
        // translator each have a fixed workspace and independent error paths.
        "std@1/regex/read"
        | "std@1/regex/write"
        | "std@1/regex/insert"
        | "std@1/regex/ast"
        | "std@1/regex/check_depth"
        | "std@1/regex/translate"
        | "std@1/regex/translate_atom"
        | "std@1/regex/compile_hir"
        | "std@1/regex/sized_alts"
        | "std@1/regex/emit_alts"
        | "std@1/regex/choose_prefilter"
        | "std@1/regex/names_size"
        | "std@1/regex/write_names"
        | "std@1/regex/compile_new" => Inline::Never,
        // Keep lazy DFA misses off the cached transition path.
        "std@1/regex/dfa_transition" => Inline::Never,
        // Small mutable loops, and larger ones a caller runs in a loop, carry their scopes in.
        _ if batch_steps
            && (body.insts.len() <= 256 || (body.insts.len() <= 4096 && called_in_loop))
            && sig.params.iter().any(|p| {
                matches!(p, Param::Address { place, loan: Loan::Mut }
                    if matches!(place, PlaceTy::Value(Ty::Scalar(_)))
                        || matches!(place, PlaceTy::Declared(t) if hirsel_core_ir::roles::carrier(module, *t)))
            })
            && !empty::loop_free(body) =>
        {
            Inline::Always
        }
        _ if batch_steps
            && called_in_loop
            && body.insts.len() <= 96
            && matches!(sig.results.as_slice(), [Ty::Scalar(Scalar::Bool | Scalar::U8 | Scalar::I8)])
            && sig.params.iter().any(|p| matches!(p, Param::Address { loan: Loan::View, .. }))
            && empty::loop_free(body) =>
        {
            Inline::Always
        }
        _ if (specialized || body.calls.len() <= 1)
            && empty::loop_free(body) =>
        {
            Inline::Always
        }
        _ => Inline::Default,
    }
}
