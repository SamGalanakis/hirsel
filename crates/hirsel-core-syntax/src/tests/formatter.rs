use super::*;

#[test]
fn formatter_wraps_at_syntax_boundaries() {
    let first = "a".repeat(42);
    let fits = "b".repeat(46);
    let over = "b".repeat(47);
    for (second, call) in [
        (&fits, format!("    call({first}, {fits})")),
        (&over, format!("    call({first},\n        {over})")),
    ] {
        let input = format!("fn f(){{call({first},{second})}}");
        assert_eq!(
            formatted(&input, ParseMode::Source),
            format!("fn f() {{\n{call}\n}}\n")
        );
    }

    let input = format!("fn f(){{return {first} - {over} // difference\n}}");
    assert_eq!(
        formatted(&input, ParseMode::Source),
        format!("fn f() {{\n    return {first} -\n        {over} // difference\n}}\n")
    );

    let literal = format!("r#\"{}\"#", "a b ".repeat(30));
    let input = format!("fn f(){{record({literal},2)}}");
    assert_eq!(
        formatted(&input, ParseMode::Source),
        format!("fn f() {{\n    record(\n        {literal},\n        2)\n}}\n")
    );

    let input = format!("pub fn f({first}: u64, {over}: u64) -> u64");
    assert_eq!(
        formatted(&input, ParseMode::Foreign),
        format!("pub fn f({first}: u64,\n    {over}: u64) -> u64\n")
    );

    let field = "go: own[send] fn(Tasks, take JobGrant[G], take Caller, take Bytes, take Option[JobReport])";
    let input = format!(
        "type GrantedLaunch[G: send]: send {{ {field} -> Result[Task[Outcome], Capacity] }}"
    );
    assert_eq!(
        formatted(&input, ParseMode::Source),
        format!(
            "type GrantedLaunch[G: send]: send {{\n    {field} ->\n        Result[Task[Outcome], Capacity]\n}}\n"
        )
    );
}

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

#[test]
fn data_document_requires_one_qualified_constructor_and_formats_labelled_lists() {
    let data = ParseMode::Data;
    let input = "// head\npkg . Package (sources:[1,2], // sources\n)";
    assert_eq!(
        formatted(input, data),
        "// head\npkg.Package(\n    sources: [1, 2], // sources\n)\n"
    );
    assert_eq!(
        formatted("fn f(a:List[u8]){g(sources:[a[0]])}", ParseMode::Source),
        "fn f(a: List[u8]) {\n    g(sources: [a[0]])\n}\n"
    );
    for invalid in [
        "",
        "// empty",
        ";pkg.Package()",
        "pkg.Package();",
        "Package()",
        "a.b.C()",
        "a.B",
        "a.B()\na.B()",
        "const x: A = A()",
    ] {
        assert!(!parse(invalid, data).diagnostics.is_empty(), "{invalid}");
    }
    for mode in [ParseMode::Source, ParseMode::Signatures, ParseMode::Foreign] {
        assert!(!parse(input, mode).diagnostics.is_empty());
    }
}
