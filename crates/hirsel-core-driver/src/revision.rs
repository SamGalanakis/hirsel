//! An entry's code revision: a hash of the checked CoreIR of the instances it
//! reaches by the IR's own edges (code-revision-1038 DESIGN v2).
//!
//! - `body(I)` hashes instance `I`'s module with its diagnostic-only fields
//!   cleared: source anchors, and `callable_parameters` (optimisation-only
//!   information, ir.rs). A callable code variant is an LLVM release copy,
//!   not semantic code, so `code_variants` is cleared too.
//! - `I`'s edges are the instances its module names outside itself (an
//!   instance record's `children`): calls, function values and dispatch
//!   candidates, including the selections the front end makes from types.
//! - `revision(f)` hashes `f`'s identity and the sorted rows
//!   `(identity(I), body(I))` of every `I` reachable from `f`. A call site
//!   names its callee's identity inside the caller's module, so a retargeted
//!   edge changes the caller's `body`.
//!
//! The compiler, the backend, the runtime, the target and the build
//! configuration are not inputs: a compiler change enters only through some
//! instance's `body`. Nothing reads the value back in this slice; `hirsel
//! build --json` reports it.
use hirsel_core_ids::ContentHash;
use hirsel_core_ir::{Identity, Module};
use std::collections::BTreeMap;
use std::io;

/// `body(I)` of the module an instance materialises to. `scratch` is reused
/// across modules by the caller.
pub(crate) fn body(module: &Module, scratch: &mut Vec<u8>) -> io::Result<ContentHash> {
    let mut module = module.clone();
    module.code_variants.clear();
    for body in module.functions.iter_mut().filter_map(|f| f.body.as_mut()) {
        body.callable_parameters.clear();
        body.inst_anchors.iter_mut().for_each(|a| *a = None);
        body.blocks.iter_mut().for_each(|b| b.anchor = None);
    }
    scratch.clear();
    bincode::serialize_into(&mut *scratch, &module).map_err(io::Error::other)?;
    Ok(ContentHash::of_parts(
        "hirsel.code-revision-body.v1",
        [&scratch[..]],
    ))
}

/// `revision(root)` over the instances `row` describes: each one's `body`
/// and the instances it names. None when an instance the walk reaches has no
/// row, which a complete build's set never leaves.
pub(crate) fn revision(
    root: &Identity,
    mut row: impl FnMut(&Identity) -> Option<(ContentHash, Vec<Identity>)>,
) -> Option<ContentHash> {
    let mut reached = BTreeMap::new();
    let mut pending = vec![root.clone()];
    while let Some(identity) = pending.pop() {
        if reached.contains_key(&identity) {
            continue;
        }
        let (body, children) = row(&identity)?;
        pending.extend(children.into_iter().filter(|c| !reached.contains_key(c)));
        reached.insert(identity, body);
    }
    let root = root.digest();
    let rows: Vec<[u8; 64]> = {
        let mut rows: Vec<[u8; 64]> = reached
            .iter()
            .map(|(identity, body)| {
                let mut row = [0; 64];
                row[..32].copy_from_slice(&identity.digest().0);
                row[32..].copy_from_slice(&body.0);
                row
            })
            .collect();
        rows.sort_unstable();
        rows
    };
    let count = (rows.len() as u64).to_le_bytes();
    Some(ContentHash::of_parts(
        "hirsel.code-revision.v1",
        [&root.0[..], &count[..]]
            .into_iter()
            .chain(rows.iter().map(|r| &r[..])),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use hirsel_core_ir::text;

    /// `body` reads what a body runs and not the callable-parameter list,
    /// which execution ignores (ir.rs `Body::callable_parameters`). No source
    /// edit moves the list alone, so the native corpus cannot see a hash that
    /// reads it. Oracle: that rule, and a changed constant for the converse.
    #[test]
    fn body_ignores_callable_parameters() {
        const MODULE: &str = "core v1\nsig g0 = (ptr, u64) -> (u64)\ninst n0 = app@1/main/apply [] []\nfn f0 = n0 g0 {\n callable v0\nb0(v0: ptr, v1: u64):\n v2: ptr = const 0x0\n v3: u64 = call v0 g0 (v2, v1)\n v4: u64 = const 41\n v5: u64 = add v3, v4\n return v5\n}\n";
        let of = |t: &str| body(&text::parse(t).unwrap(), &mut Vec::new()).unwrap();
        let with = of(MODULE);
        assert_eq!(of(&MODULE.replace(" callable v0\n", "")), with);
        assert_ne!(of(&MODULE.replace("const 41", "const 42")), with);
    }
}
