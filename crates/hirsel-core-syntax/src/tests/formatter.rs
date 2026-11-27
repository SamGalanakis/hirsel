use super::*;

#[test]
fn formatter_trailing_commas_break_lists() {
    for (input, expected, mode) in [
        (
            "fn f(a:u64,b:u64,){g(a,b,);g(\na,b\n)}",
            "fn f(\n    a: u64,\n    b: u64,\n) {\n    g(\n        a,\n        b,\n    );\n    g(a, b)\n}\n",
            ParseMode::Source,
        ),
        (
            "pub fn f[T,](a:fn(u64,u64,),b:Box[(T,),],)->(u64,u64,)",
            "pub fn f[\n    T,\n](\n    a: fn(\n        u64,\n        u64,\n    ),\n    b: Box[\n        (\n            T,\n        ),\n    ],\n) -> (\n    u64,\n    u64,\n)\n",
            ParseMode::Signatures,
        ),
        (
            "fn f(){g(// first\n(1,2,), // pair\n// next\n[3,4,], // last\n)}",
            "fn f() {\n    g(\n        // first\n        (\n            1,\n            2,\n        ), // pair\n        // next\n        [\n            3,\n            4,\n        ], // last\n    )\n}\n",
            ParseMode::Source,
        ),
        (
            "type Shape = Point(u64,u64,)\nfn f(s:Shape){match s{Point((x,y,),z,)=>g([x;2],z)}}",
            "type Shape = Point(\n    u64,\n    u64,\n)\n\nfn f(s: Shape) {\n    match s {\n        Point(\n            (\n                x,\n                y,\n            ),\n            z,\n        ) => g([x; 2], z)\n    }\n}\n",
            ParseMode::Source,
        ),
    ] {
        assert_eq!(formatted(input, mode), expected, "{input}");
    }
}

#[test]
fn formatter_equivalence_refuses_candidate_that_changes_syntax_or_trivia() {
    let mode = ParseMode::Source;
    for (original, candidate, reason) in [
        ("fn f() {}", "fn f() {", "did not parse cleanly"),
        ("fn f() { 1 }", "fn f() { 2 }", "changed tokens or comments"),
        (
            "// written\nfn f() {}",
            "// rewritten\nfn f() {}",
            "changed tokens or comments",
        ),
        (
            "fn f() { return\n(x) }",
            "fn f() { return (x) }",
            "changed the syntax tree",
        ),
        (
            "fn f() {} /// doc\nfn g() {}",
            "fn f() {}\n/// doc\nfn g() {}",
            "changed item comment attachment",
        ),
    ] {
        let original = parse(original, mode);
        assert!(original.diagnostics.is_empty());
        let error =
            crate::format::verify_format(&original, candidate, mode, &mut Symbols::default())
                .unwrap_err();
        assert_eq!(
            error.to_string(),
            format!("internal formatter error: formatted source {reason}")
        );
    }
}
