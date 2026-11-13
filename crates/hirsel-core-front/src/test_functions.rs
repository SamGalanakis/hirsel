//! PROG-4 is a declaration contract. References use resolved identities, so local
//! shadowing stays ordinary and an installed library keeps the same restriction.
use crate::{Def, ERROR, ItemRef, Mode, Program, TyId, TyKind, UNIT, refusal};
use hirsel_core_resolve::Target;
use hirsel_core_syntax::NodeKind;
use std::collections::HashSet;

pub(crate) fn check(p: &mut Program<'_>) {
    let tests: HashSet<ItemRef> = (p.fns.base_len()..p.fns.len())
        .filter(|&f| p.fns[f].is_test)
        .map(|f| p.fns[f].at)
        .collect();
    p.has_test_functions |= !tests.is_empty();
    if !p.has_test_functions {
        return;
    }
    let core = |ty: TyId, names: &[&str]| match p.types.kind(ty) {
        TyKind::Adt(d, args) => {
            let path = &p.decls[d.index()].path;
            (path.package_is("core", "1")
                && path.0.modules == ["core"]
                && names.contains(&path.0.name.as_str()))
            .then_some(args.as_slice())
        }
        _ => None,
    };
    for f in p.fns.base_len()..p.fns.len() {
        let fd = &p.fns[f];
        if !fd.is_test {
            continue;
        }
        if !fd.generics.is_empty() {
            let item = p.item(fd.at);
            let generics = item
                .tree
                .children(item.root)
                .iter()
                .copied()
                .find(|&n| item.tree.node(n).kind == NodeKind::Generics)
                .unwrap();
            p.refusals.push(refusal(
                "PROG-4",
                p.site(fd.at, generics),
                "a test takes no type parameters",
            ));
        }
        for param in &fd.params {
            if param.ty != ERROR
                && (param.mode != Mode::View
                    || core(
                        param.ty,
                        &[
                            "Dir", "Net", "Clock", "Random", "Stdout", "Stderr", "Stdin", "Env",
                            "Process", "Tasks",
                        ],
                    )
                    .is_none())
            {
                p.refusals.push(refusal(
                    "PROG-4",
                    param.site,
                    "a test parameter is an unmarked capability",
                ));
            }
        }
        if fd.result != ERROR && core(fd.result, &["Result"]).is_none_or(|a| a[0] != UNIT) {
            p.refusals.push(refusal(
                "PROG-4",
                fd.result_site,
                "a test returns `Result[(), E]`",
            ));
        }
    }
    for (module, items) in p.items.iter().enumerate() {
        for (item, info) in items.iter().enumerate() {
            let at = ItemRef {
                module: module as u32,
                item: item as u32,
            };
            if tests.contains(&at) {
                continue;
            }
            for named in &info.resolution.names {
                if let Target::Definition(id) = named.target
                    && let Some(Def::Fn(f)) = p.by_definition[id.0 as usize]
                    && p.fns[f as usize].is_test
                {
                    let mut r = refusal(
                        "PROG-4",
                        p.site(at, named.node),
                        "only another test function may name a test function",
                    );
                    r.causal.push(p.fns[f as usize].site);
                    p.refusals.push(r);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn prog4_signature_and_reference_cases() {
        let root =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../core/front/cases/body");
        for name in [
            "test-functions",
            "test-signatures-refused",
            "test-references-refused",
        ] {
            let path = root.join(format!("{name}.hirsel"));
            assert_eq!(
                crate::tests::run_case(&path),
                Vec::<String>::new(),
                "{name}"
            );
        }
    }
}
