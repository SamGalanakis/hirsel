use crate::support;
use hirsel_syntax::*;

#[test]
fn recovered_lexical_tokens_and_missing_tokens_mark_the_shared_status() {
    let (output, _) = support::parse("fn f() {}\n$");
    assert!(output.syntax.has_syntax_errors);
    let (output, _) = support::parse("fn f(x u32) {}");
    assert!(output.syntax.has_syntax_errors);
}

#[test]
fn comparison_range_assignment_and_return_binding_follow_the_table() {
    for source in [
        "fn f() { a < b < c }",
        "fn f() { a..b..c }",
        "fn f() { a = b = c }",
        "fn f() { ..b }",
        "fn f() { a.. }",
        "fn f() { (a = b) }",
    ] {
        assert!(!support::parse(source).0.diagnostics.is_empty(), "{source}");
    }
    for expression in ["(a < b) == c", "a..(b..c)", "(a..b)..c", "a < b && c < d"] {
        assert!(
            support::parse(&format!("fn f() {{ {expression} }}"))
                .0
                .diagnostics
                .is_empty(),
            "{expression}"
        );
    }
    let (output, _) = support::parse("fn f() { return a..b || c && d } ");
    assert!(output.diagnostics.is_empty());
    let returned = output
        .syntax
        .items
        .iter()
        .flat_map(|i| &i.tree.expressions)
        .find_map(|e| match e.kind {
            ExprKind::Return(Some(e)) => Some(e),
            _ => None,
        })
        .unwrap();
    assert!(matches!(
        output.syntax.items[0].tree.expressions[returned.0 as usize].kind,
        ExprKind::Binary {
            op: BinaryOp::Range,
            ..
        }
    ));
}

#[test]
fn function_type_modifiers_cover_what_follows_and_relations_bind_the_nearest_type() {
    let (output, _) = support::parse(
        "type A = fn() -> once T\ntype B = once fn() -> T\ntype C = fn() -> T in x\ntype D = fn() -> T uses {} in x",
    );
    assert!(output.diagnostics.is_empty());
    // Each alias's type, in its own item's arenas.
    let alias = |i: usize| {
        let syntax = &output.syntax.items[i];
        match syntax.item.kind {
            ItemKind::Type {
                body: TypeBody::Alias(ty),
                ..
            } => (&syntax.tree, ty),
            _ => panic!(),
        }
    };
    let (tree, ty) = alias(0);
    let TypeExprKind::Function(sig) = &tree.types[ty.0 as usize].kind else {
        panic!()
    };
    assert!(matches!(
        tree.types[sig.result.unwrap().0 as usize].kind,
        TypeExprKind::Qualified { .. }
    ));
    let (tree, ty) = alias(1);
    let TypeExprKind::Qualified { base, .. } = tree.types[ty.0 as usize].kind else {
        panic!()
    };
    let TypeExprKind::Function(sig) = &tree.types[base.0 as usize].kind else {
        panic!()
    };
    assert!(matches!(
        tree.types[sig.result.unwrap().0 as usize].kind,
        TypeExprKind::Named { .. }
    ));
    // A relation after a result binds the result; after `uses`, the function.
    let (tree, ty) = alias(2);
    let TypeExprKind::Function(sig) = &tree.types[ty.0 as usize].kind else {
        panic!()
    };
    assert!(matches!(
        tree.types[sig.result.unwrap().0 as usize].kind,
        TypeExprKind::Qualified { .. }
    ));
    let (tree, ty) = alias(3);
    let TypeExprKind::Qualified { base, .. } = tree.types[ty.0 as usize].kind else {
        panic!()
    };
    assert!(matches!(
        tree.types[base.0 as usize].kind,
        TypeExprKind::Function(_)
    ));
}

#[test]
fn numeric_size_arguments_are_unsuffixed_u64_literals_without_arithmetic() {
    for ty in [
        "Inline[T,0]",
        "Inline[T,18_446_744_073_709_551_615]",
        "Inline[T,0xff]",
        "Inline[T,n]",
    ] {
        assert!(
            support::parse(&format!("type A = {ty}"))
                .0
                .diagnostics
                .is_empty(),
            "{ty}"
        );
    }
    for ty in [
        "Inline[T,-1]",
        "Inline[T,1u64]",
        "Inline[T,18446744073709551616]",
        "Inline[T,n+1]",
        "Inline[]",
    ] {
        assert!(
            !support::parse(&format!("type A = {ty}"))
                .0
                .diagnostics
                .is_empty(),
            "{ty}"
        );
    }
}

#[test]
fn package_paths_are_adjacent_and_unversioned_slash_is_division() {
    let (output, _) = support::parse("fn f() { a/b }\nfn g() { json@1/value.parse(x) }");
    assert!(output.diagnostics.is_empty());
    assert!(
        output
            .syntax
            .items
            .iter()
            .flat_map(|i| &i.tree.expressions)
            .any(|e| matches!(
                e.kind,
                ExprKind::Binary {
                    op: BinaryOp::Div,
                    ..
                }
            ))
    );
    assert!(
        output
            .syntax
            .items
            .iter()
            .flat_map(|i| &i.tree.expressions)
            .any(|e| matches!(&e.kind, ExprKind::Path(path) if path.package.is_some()))
    );
    for expression in [
        "json @1/value.parse(x)",
        "json@ 1/value.parse(x)",
        "json@1 /value.parse(x)",
        "json@1u8/value.parse(x)",
    ] {
        assert!(
            !support::parse(&format!("fn f() {{ {expression} }}"))
                .0
                .diagnostics
                .is_empty(),
            "{expression}"
        );
    }
}

#[test]
fn incomplete_bindings_continue_types_but_complete_type_aliases_do_not_reopen() {
    for source in [
        "fn f() { let x: T\n in x\n = y }",
        "const C: T\n in heap\n = y",
    ] {
        assert!(support::parse(source).0.diagnostics.is_empty(), "{source}");
    }
    for source in [
        "type A = T\n in env",
        "type A = fn()\n -> T",
        "type A = fn() -> T\n uses {}",
        "type A = A\n | B",
        "type A = A | B\n | C",
    ] {
        assert!(!support::parse(source).0.diagnostics.is_empty(), "{source}");
    }
}

#[test]
fn drop_is_a_finalizer_only_after_a_resource_body_and_a_name_elsewhere() {
    // (source, finalizer binding, field or variant names) per resource item.
    let finalized: [(&str, Option<&str>, &[&str]); 4] = [
        (
            "resource type R { drop: u32 }\ndrop(r) { drop(r.drop) }",
            Some("r"),
            &["drop"],
        ),
        (
            "resource type R = A | drop(u8)\ndrop(d) {}",
            Some("d"),
            &["A", "drop"],
        ),
        ("resource type R = A | drop(u8)", None, &["A", "drop"]),
        (
            "resource type R = A\n@noalloc drop(drop) {}",
            Some("drop"),
            &[],
        ),
    ];
    for (source, binding, names) in finalized {
        let (output, _) = support::parse(source);
        assert!(output.diagnostics.is_empty(), "{source}");
        assert_eq!(output.syntax.items.len(), 1, "{source}");
        let ItemKind::Type { body, drop, .. } = &output.syntax.items[0].item.kind else {
            panic!("{source}");
        };
        assert_eq!(
            drop.as_ref().map(|d| d.binding.as_str()),
            binding,
            "{source}"
        );
        let declared: Vec<&str> = match body {
            TypeBody::Record(fields) => fields.iter().map(|f| f.name.as_str()).collect(),
            TypeBody::Variants(variants) => variants.iter().map(|v| v.name.as_str()).collect(),
            _ => vec![],
        };
        assert_eq!(declared, names, "{source}");
    }
    // Outside a resource body `drop` is an ordinary function, parameter,
    // local and member name.
    let (output, _) = support::parse(
        "fn drop(drop: u32) -> u32 { let dropped = drop\n x.drop(drop).drop }\nfn g() { drop(1) }",
    );
    assert!(output.diagnostics.is_empty());
    assert_eq!(output.syntax.items[0].item.name.as_str(), "drop");
    // A copyable type still cannot declare destruction.
    for source in [
        "type C { x: u32 }\ndrop(c) {}",
        "type C = A\n@noalloc drop(c) {}",
    ] {
        let (output, _) = support::parse(source);
        assert!(
            output
                .diagnostics
                .iter()
                .any(|d| d.rule.as_str() == "DROP-1"),
            "{source}"
        );
    }
}
