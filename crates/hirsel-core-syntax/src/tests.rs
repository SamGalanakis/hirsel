use super::*;
fn good(text: &str) -> SyntaxFile {
    let p = parse(text, ParseMode::Source);
    assert_eq!(p.diagnostics, vec![], "{text}");
    assert_eq!(p.resource_refusal, None);
    assert_eq!(p.syntax.text(), text);
    p.syntax
}
#[test]
fn precedence_and_relative_arenas() {
    let text = "// docs\nfn sum(a: u64, b: u64) -> u64 { a + b * 2 }\n";
    let file = good(text);
    let item = &file.items[0];
    let t = &item.tree;
    let binaries: Vec<_> = t
        .nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Binary)
        .collect();
    assert_eq!(binaries.len(), 2);
    assert_eq!(t.token_text(&item.text, binaries[0].token.unwrap()), "*");
    assert_eq!(t.token_text(&item.text, binaries[1].token.unwrap()), "+");
    let shifted = good(&format!("type Before {{}}\n{text}"));
    assert_eq!(shifted.items[1].tree, item.tree);
    assert_eq!(shifted.items[1].text, item.text);
}
#[test]
fn statement_boundaries_and_recovery() {
    for (text, rule) in [
        ("fn f() { let x = 1\n+ 2 }\nfn next() {}", "LANG-4"),
        ("fn f() { 1 < 2 < 3 }\nfn next() {}", "LANG-5"),
        ("fn f() { match 1 { A | B => 0 } }\nfn next() {}", "LANG-5"),
        ("fn view() {}\nfn next() {}", "LANG-2"),
        ("fn f() { let x =\nfn next() {}", "LANG-5"),
    ] {
        let p = parse(text, ParseMode::Source);
        assert!(p.diagnostics.iter().any(|d| d.rule == rule), "{text}");
        assert_eq!(p.syntax.text(), text);
        let last = p.syntax.items.last().unwrap();
        assert_eq!(
            last.tree.token_text(
                &last.text,
                last.tree
                    .node(last.root)
                    .token
                    .unwrap_or_else(|| panic!("next item hidden: {text} {:?}", p.diagnostics))
            ),
            "next"
        );
    }
    good("fn f() { let x =\n 1 +\n 2\n if true {}\n else {}\n x }\n");
    good("fn f() { let x = (1\n + 2); x }\n");
}
#[test]
fn new_forms_have_written_nodes() {
    let f = good(
        "stable type P[in r] { x: u64 }\nfn f[in r](x: P in r) -> u64 where r: r { let mut y: u64; region a { bound bytes 8 { loop { break (x, 1).0 } } } }\n",
    );
    let kinds: Vec<_> = f
        .items
        .iter()
        .flat_map(|i| i.tree.nodes.iter().map(|n| n.kind))
        .collect();
    for k in [
        NodeKind::StableType,
        NodeKind::RegionGeneric,
        NodeKind::Anchor,
        NodeKind::Outlives,
        NodeKind::LetMut,
        NodeKind::Region,
        NodeKind::Bound,
        NodeKind::Loop,
        NodeKind::Break,
        NodeKind::Projection,
    ] {
        assert!(kinds.contains(&k), "{k:?}");
    }
}
#[test]
fn literals_are_lossless_and_decode_the_new_rules() {
    assert_eq!(byte_value("b'\\xff'"), Some(255));
    assert_eq!(character_value("'\\u{1f40f}'"), Some('🐏'));
    assert_eq!(character_value("'\\x80'"), None);
    assert_eq!(byte_value("b'\\u{41}'"), None);
    let text = "/* a\n /* nest */ */ 1..2 b\"a\\xff\" r##\"x\"#\n\"## 'a' 'scan: 0xff_ff 12f32";
    let l = lex(text);
    assert_eq!(l.diagnostics, vec![]);
    assert_eq!(
        l.tokens
            .iter()
            .map(|t| &text[t.span.start as usize..t.span.end as usize])
            .collect::<String>(),
        text
    );
    for (s, r) in [
        ("1u128", "LANG-3"),
        ("0b2", "LANG-3"),
        ("\"\\q\"", "LANG-3"),
        ("b\"é\"", "LANG-3"),
        ("/* \u{202e} */", "LANG-1"),
        ("\r", "LANG-1"),
        ("\u{feff}", "LANG-1"),
    ] {
        let l = lex(s);
        assert_eq!(l.diagnostics[0].rule, r, "{s}");
        assert!(l.tokens.iter().any(|t| t.kind == TokenKind::Invalid));
    }
}
#[test]
fn signature_mode_is_explicit_and_diagnostics_are_machine_readable() {
    let s = "pub fn push[T](mut xs: List[T], take x: T) -> Result[(), Capacity] waits\n";
    assert!(parse(s, ParseMode::Signatures).diagnostics.is_empty());
    assert!(!parse(s, ParseMode::Source).diagnostics.is_empty());
    let d = Diagnostic::error("LANG-5", Span { start: 4, end: 5 }, "expected type");
    let json = serde_json::to_value(d).unwrap();
    assert_eq!(json["rule"], "LANG-5");
    assert_eq!(json["primary"]["start"], 4);
}

#[test]
fn revised_callable_summaries_and_projection_tokenization() {
    let f = good(
        "@twin(Stable) type Own {}\nstable type Stable {}\nresource type R uses Stdout { x: u64 } drop(r, mut context) {}\nfn f(g: own[send] fn(take u64) -> u64 uses Stdout waits, p: ((u64, u64), u64)) -> u64 { p.0.1 }\n",
    );
    assert!(
        f.items[3]
            .tree
            .nodes
            .iter()
            .any(|n| n.kind == NodeKind::OwnCallableType)
    );
    let l = lex("p.0.1");
    assert_eq!(
        l.tokens.iter().map(|t| t.kind).collect::<Vec<_>>(),
        [
            TokenKind::Ident,
            TokenKind::Punct(Punct::Dot),
            TokenKind::Integer,
            TokenKind::Punct(Punct::Dot),
            TokenKind::Integer,
            TokenKind::Eof
        ]
    );
}
#[test]
fn nesting_exhaustion_is_an_incomplete_tool_result() {
    let text = format!("fn f() {{ {}1{} }}", "(".repeat(500), ")".repeat(500));
    let p = parse(&text, ParseMode::Source);
    assert!(matches!(
        p.resource_refusal,
        Some(ResourceRefusal::ParserStack { .. })
    ));
    assert_eq!(p.syntax.text(), text);
}
