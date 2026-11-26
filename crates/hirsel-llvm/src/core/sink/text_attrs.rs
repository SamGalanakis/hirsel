//! LLVM text attributes for function signatures and declarations.
use super::*;

pub(super) fn parameter(out: &mut String, p: &Parameter) {
    write!(out, "{}{}", p.ty, p.extension.text()).unwrap();
    if p.noalias {
        out.push_str(" noalias nofree");
    }
    if p.readonly {
        out.push_str(" readonly");
    }
    if p.dereferenceable != 0 {
        write!(out, " dereferenceable({})", p.dereferenceable).unwrap();
    }
    if let Some(ty) = &p.sret {
        write!(out, " sret({ty})").unwrap();
    }
    if let Some(ty) = &p.byval {
        write!(out, " byval({ty})").unwrap();
    }
    if p.align != 0 {
        write!(out, " align {}", p.align).unwrap();
    }
}
pub(super) fn function_attrs(out: &mut String, attrs: &FnAttrs) {
    match attrs.inline {
        Inline::Default => {}
        Inline::Always => out.push_str(" alwaysinline"),
        Inline::Never => out.push_str(" noinline"),
    }
    if attrs.cold {
        out.push_str(" cold");
    }
    if attrs.nounwind {
        out.push_str(" nounwind");
    }
    if attrs.argmem_read {
        out.push_str(" memory(argmem: read)");
    }
    if attrs.map_probe {
        out.push_str(" \"hirsel-map-probe\"");
    }
    if !attrs.carriers.is_empty() {
        write!(out, " \"hirsel-carriers\"=\"{}\"", attrs.carriers).unwrap();
    }
    if let Some(level) = attrs.level {
        write!(out, " \"target-features\"=\"{}\"", level.llvm_features()).unwrap();
    }
    if let Some(scope) = attrs.subprogram {
        write!(out, " !dbg !{scope}").unwrap();
    }
}
