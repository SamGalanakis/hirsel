//! MEM-29's element condition: LANG-14's capture mask, read transitively through
//! instantiated OWN-2 summaries with each rigid parameter read from its bounds. One walk
//! serves type formation (declarations and bodies) and every call that instantiates an
//! element parameter of a core operation.

use crate::decls::{H_CAPABILITY, H_OPAQUE, H_REGION, H_RESOURCE, H_SCOPED, TypeDecl, param_holds};
use crate::types::*;
use hirsel_core_ids::DefPath;
use hirsel_core_resolve::{ModuleId, NameIndex};

/// LANG-14's capture mask: no resource, capability, view, scoped value or region
/// reference at any depth.
pub(crate) const H_CAPTURE: u8 = H_RESOURCE | H_CAPABILITY | H_SCOPED | H_REGION;
pub(crate) const WANTED: &str = "no resource, capability, view, scoped value or region reference";
pub(crate) const MESSAGE: &str =
    "a Cell's element holds no resource, capability, view, scoped value or region reference";

/// The core module whose operations are declared over an open element; the condition
/// is checked where they are called, not inside them.
const MODULES: [&str; 1] = ["cell"];

/// Whether `ty` breaks the element condition under the rigid bounds `params`.
pub(crate) fn refused(
    decls: &crate::Rows<TypeDecl>,
    t: &impl TyRead,
    ty: TyId,
    params: &[u16],
) -> bool {
    holds(decls, t, ty, params) & H_CAPTURE != 0
}

/// What `ty` holds (OWN-2) with each rigid parameter read from its bounds.
fn holds(decls: &crate::Rows<TypeDecl>, t: &impl TyRead, ty: TyId, params: &[u16]) -> u8 {
    match t.kind(ty) {
        TyKind::Param(i) => param_holds(params.get(*i as usize).copied().unwrap_or(0)),
        TyKind::Tuple(xs) => xs.iter().fold(0, |h, &x| h | holds(decls, t, x, params)),
        TyKind::Array(x, _) => holds(decls, t, *x, params),
        TyKind::Anchored(x, _) => holds(decls, t, *x, params) | H_REGION,
        TyKind::View(_) | TyKind::Region(_) => H_SCOPED,
        TyKind::Fn(_, _, _, Callable::Scoped | Callable::ScopedView) => H_SCOPED,
        TyKind::Fn(_, _, _, Callable::Own | Callable::OwnSend) => H_OPAQUE,
        TyKind::Adt(d, xs) => {
            let summary = &decls[d.index()].summary;
            xs.iter()
                .enumerate()
                .filter(|(i, _)| *i >= 64 || summary.holds_params & (1 << i) != 0)
                .fold(summary.holds, |h, (_, &x)| h | holds(decls, t, x, params))
        }
        _ => 0,
    }
}

/// Whether type parameter `i` of the core operation at `path` is an element (a Cell's
/// `T`, `cell.with`'s `A`). `cell.with`'s `R` is not: it is the callback's
/// result, and a plain callable's result is never scoped (LANG-13 refuses an anchored
/// one), so no loan of the lent payload escapes.
pub(crate) fn parameter(path: &DefPath, i: usize) -> bool {
    let core = path.package_is("core", "1") && path.0.modules.len() == 1;
    core && MODULES.contains(&path.0.modules[0].as_str()) && !(path.0.name == "with" && i == 2)
}

/// Whether `module` is core's element module.
pub(crate) fn exempt(index: &NameIndex, module: ModuleId) -> bool {
    MODULES
        .iter()
        .any(|name| index.symbols.find(name).and_then(|s| index.core_module(s)) == Some(module))
}
