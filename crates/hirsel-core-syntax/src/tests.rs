use super::*;
fn parse(text: &str, mode: ParseMode) -> ParseOutput {
    crate::parse(text, mode, &mut Symbols::default())
}
fn parse_bytes(bytes: &[u8], mode: ParseMode) -> ParseOutput {
    crate::parse_bytes(bytes, mode, &mut Symbols::default())
}
fn lex(text: &str) -> LexOutput {
    crate::lex(text, &mut Symbols::default())
}

fn formatted(text: &str, mode: ParseMode) -> String {
    let p = parse(text, mode);
    assert!(p.diagnostics.is_empty(), "{text}: {:?}", p.diagnostics);
    let output = format(&p).unwrap();
    let q = parse(&output, mode);
    assert!(q.diagnostics.is_empty(), "{output}: {:?}", q.diagnostics);
    assert_eq!(format(&q).as_deref(), Some(output.as_str()));
    let tokens = |text: &str| {
        lex(text)
            .tokens
            .into_iter()
            .filter_map(|t| {
                (!matches!(t.kind, TokenKind::Eof)).then(|| {
                    (
                        t.kind,
                        text[t.span.start as usize..t.span.end as usize].to_owned(),
                    )
                })
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(tokens(text), tokens(&output));
    let shape = |p: &ParseOutput| {
        p.syntax
            .items()
            .map(|i| {
                (
                    i.root.0 - i.range.nodes.start,
                    i.tree.nodes[i.range.nodes.start as usize..i.range.nodes.end as usize]
                        .iter()
                        .flat_map(|n| {
                            i.tree.edges[n.children.start as usize..n.children.end as usize]
                                .iter()
                                .map(|x| x.0 - i.range.nodes.start)
                        })
                        .collect::<Vec<_>>(),
                    i.tree.nodes[i.range.nodes.start as usize..i.range.nodes.end as usize]
                        .iter()
                        .map(|n| {
                            (
                                n.kind,
                                n.token.map(|t| i.tree.token_text(&i.text, t).to_owned()),
                                (n.children.end - n.children.start),
                            )
                        })
                        .collect::<Vec<_>>(),
                )
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(shape(&p), shape(&q));
    output
}

#[test]
fn formatter_canonical_blocks_lists_and_attributes() {
    let text = "@twin( B ) type A{x:u64,y: (u64,u64)}\nfn f [T](x:T)->T{let mut y:T=x; if true {y}else{ x }}";
    assert_eq!(
        formatted(text, ParseMode::Source),
        "@twin(B)\ntype A {\n    x: u64,\n    y: (u64, u64)\n}\n\nfn f[T](x: T) -> T {\n    let mut y: T = x;\n    if true {\n        y\n    } else {\n        x\n    }\n}\n"
    );
    assert_eq!(
        formatted(
            "pub fn f [T](take x:T,bytes n:u64)->Result[T,Capacity] waits\npub type X",
            ParseMode::Signatures
        ),
        "pub fn f[T](take x: T, bytes n: u64) -> Result[T, Capacity] waits\n\npub type X\n"
    );
}

#[test]
fn formatter_keeps_statement_values_and_exit_boundaries() {
    assert_eq!(
        formatted(
            "fn f(){return\n(x)\nloop{break\ny}\nlet z=1;\nz}",
            ParseMode::Source
        ),
        "fn f() {\n    return\n    (x)\n    loop {\n        break\n        y\n    }\n    let z = 1;\n    z\n}\n"
    );
    formatted("fn f(){match x{A=>{1},B=>2\nC=>3}}", ParseMode::Source);
    formatted(
        "fn f(){ 'again: loop { if true { break 'again 3 }; continue 'again } }",
        ParseMode::Source,
    );
    formatted(
        "resource type R { x: u64 } drop(r) { f(r.x); }",
        ParseMode::Source,
    );
}

#[test]
fn formatter_preserves_comments_and_literal_bytes() {
    let text = "// header\n/// doc\nfn f(){/* before */let x=r##\"a\n  b\"##; // trailing\n/* alone\n exact */\nx /* inline */ + 1}\n// end";
    assert_eq!(
        formatted(text, ParseMode::Source),
        "// header\n/// doc\nfn f() {\n    /* before */\n    let x = r##\"a\n  b\"##; // trailing\n    /* alone\n exact */\n    x /* inline */ + 1\n}\n// end\n"
    );
    formatted("fn f() { /* only */ }", ParseMode::Source);
    formatted("// only comment  ", ParseMode::Source);
    formatted(
        "fn f() {}\n// attached\nfn g() {}\n/* tail */",
        ParseMode::Source,
    );
}

#[test]
fn formatter_uses_syntax_roles_for_spacing() {
    assert_eq!(
        formatted(
            "fn f(x:u64){- -x + ! !x;\n1 .0;\nf (x)[0].1?\nlet g=scoped fn(x:u64)->u64{x}}",
            ParseMode::Source
        ),
        "fn f(x: u64) {\n    --x + !!x;\n    1 .0;\n    f(x)[0].1?\n    let g = scoped fn(x: u64) -> u64 {\n        x\n    }\n}\n"
    );
    formatted(
        "fn f(g:own[send] fn(take u64)->u64 waits){ g(take x) }",
        ParseMode::Source,
    );
    assert_eq!(
        formatted("fn f(){ - (x) }", ParseMode::Source),
        "fn f() {\n    -(x)\n}\n"
    );
    formatted("fn f(){ scoped fn(x:u64)->u64{x} }", ParseMode::Source);
}

#[test]
fn formatter_refuses_invalid_or_incomplete_source() {
    assert!(format(&parse("fn f() { let = 1 }", ParseMode::Source)).is_none());
    let mut parsed = parse("fn f() {}", ParseMode::Source);
    parsed.resource_refusal = Some(ResourceRefusal::SourceTooLarge);
    assert!(format(&parsed).is_none());
}
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
    let item = &file.item(0);
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
    let moved = shifted.item(1);
    assert_eq!(moved.source_text(), item.source_text());
    let spans = |i: Item<'_>| {
        i.tree.nodes[i.range.nodes.start as usize..i.range.nodes.end as usize]
            .iter()
            .map(|n| (n.kind, n.origin.span))
            .collect::<Vec<_>>()
    };
    assert_eq!(spans(moved), spans(*item));
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
        let last = p.syntax.items().last().unwrap();
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
        .items()
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
    let mut cursor = 0;
    let mut recovered = String::new();
    for token in &l.tokens {
        let start = token.span.start as usize;
        let end = token.span.end as usize;
        assert!(
            text[cursor..start]
                .bytes()
                .all(|b| matches!(b, b' ' | b'\t' | b'\r' | b'\n'))
        );
        recovered.push_str(&text[cursor..end]);
        cursor = end;
    }
    assert_eq!(recovered, text);
    for (s, r) in [
        ("1u128", "LANG-3"),
        ("'ab'", "LANG-3"),
        ("b'é'", "LANG-3"),
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
        "@twin(Stable) type Own {}\nstable type Stable {}\nresource type R { x: u64 } drop(r) {}\nfn f(g: own[send] fn(take u64) -> u64 waits, p: ((u64, u64), u64)) -> u64 { p.0.1 }\n",
    );
    assert!(
        f.item(3)
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
            TokenKind::Dot,
            TokenKind::Integer,
            TokenKind::Dot,
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

#[test]
fn invalid_utf8_has_a_raw_primary_byte_span() {
    let p = parse_bytes(b"fn f() {}\n\xff", ParseMode::Source);
    assert_eq!(p.diagnostics.len(), 1);
    assert_eq!(p.diagnostics[0].rule, "LANG-1");
    assert_eq!(p.diagnostics[0].primary, Span { start: 10, end: 11 });
}

#[test]
fn restricted_subjects_and_exit_labels_follow_revision_two() {
    for source in [
        "fn f() { let x = {} else { return } }",
        "fn f() { if 1 + {} {} }",
        "fn f() { return 'target 1 }",
    ] {
        let p = parse(source, ParseMode::Source);
        assert!(p.diagnostics.iter().any(|d| d.rule == "LANG-5"), "{source}");
        assert_eq!(p.syntax.text(), source);
    }
    good("fn f() { let x = ({}) else { return } }");
    assert_eq!(
        parse("const BAD: u64 = 1.", ParseMode::Source).diagnostics[0].rule,
        "LANG-3"
    );
}

#[test]
fn revision_three_deletions_and_sized_parameters() {
    let s = "pub fn of(bytes n: u64, uses: u64) -> u64 { n + uses }";
    let f = good(s);
    assert!(
        f.item(0)
            .tree
            .nodes
            .iter()
            .any(|n| n.kind == NodeKind::BytesParameter)
    );
    for s in [
        "type T of p",
        "type T uses Cap",
        "fn f(g: fn() uses Cap) {}",
        "resource type R {} drop(r, mut owner) {}",
        "fn f() { if true {} 1 }",
        "fn f(g: fn[T]()) {}",
    ] {
        assert!(!parse(s, ParseMode::Source).diagnostics.is_empty(), "{s}");
    }
    assert_eq!(
        lex("of uses")
            .tokens
            .iter()
            .filter(|t| t.kind == TokenKind::Ident)
            .count(),
        2
    );
}

#[test]
fn malformed_numeric_point_primary_site_excludes_trailing_comment() {
    let source = "const BAD: u64 = 1. // comment\n";
    let start = source.find("1.").unwrap() as u32;
    let p = parse(source, ParseMode::Source);
    assert_eq!(
        p.diagnostics[0].primary,
        Span {
            start,
            end: start + 2
        }
    );
    assert_eq!(p.diagnostics[0].rule, "LANG-3");
}

#[test]
fn revision_four_inline_arrays_and_bytes_keyword() {
    let f = good(
        "const POW10: Inline[u64, 4] = [1, 10, 100, 1000]\nfn f() { let a = [2; 4]; let b = []; a[1] }",
    );
    let kinds: Vec<_> = f
        .items()
        .flat_map(|i| i.tree.nodes.iter().map(|n| n.kind))
        .collect();
    assert!(kinds.contains(&NodeKind::Array));
    assert!(kinds.contains(&NodeKind::RepeatArray));
    assert_eq!(lex("bytes").tokens[0].kind, TokenKind::KwBytes);
    assert!(
        !parse("fn bytes() {}", ParseMode::Source)
            .diagnostics
            .is_empty()
    );
    assert!(
        !parse("fn f() { [1; n] }", ParseMode::Source)
            .diagnostics
            .is_empty()
    );
    assert_eq!(
        formatted("fn f(){ [2; 4] }", ParseMode::Source),
        "fn f() {\n    [2; 4]\n}\n"
    );
}
#[test]
fn symbols_are_shared_across_files_and_preserve_written_spellings() {
    let mut symbols = Symbols::default();
    let a = crate::parse(
        "fn first(x: u64) { x; None }",
        ParseMode::Source,
        &mut symbols,
    );
    let x = symbols.find("x").unwrap();
    let none = symbols.find("None").unwrap();
    assert_ne!(none, sym::NONE);
    assert_eq!(symbols.text(none), "None");
    let b = crate::parse("fn second(x: u64) { x }", ParseMode::Source, &mut symbols);
    assert_eq!(symbols.find("x"), Some(x));
    for file in [&a.syntax, &b.syntax] {
        let uses = file.tree.tokens.iter().filter(|t| t.symbol == x).count();
        assert_eq!(uses, 2);
        for t in &file.tree.tokens {
            if t.kind == TokenKind::Ident {
                assert_eq!(
                    symbols.text(t.symbol),
                    &file.source[t.span.start as usize..t.span.end as usize]
                );
            }
        }
    }
    assert_eq!(a.syntax.items().count(), 1);
    assert_eq!(b.syntax.items().count(), 1);
}

#[test]
fn streaming_bounds_forms_and_region_result_anchors() {
    let text = "fn fill[T: packed + streamable, in r](take w: Writer[SeqSpan[T], in r], t: T) -> Result[Child[SeqSpan[T]], BuildError] { writer.append(mut w, scoped fn(take x: Writer[T, in r]) -> Result[Child[T], BuildError] { emit(take x, t) })?; writer.close(take w) }\nfn peek[T: packed, in r](w: Writer[IndexedSpan[T], in r]) -> Option[View[T]] in r { writer.last_completed(w) }";
    good(text);
    formatted(text, ParseMode::Source);
    let p = parse("fn f() { let x }", ParseMode::Source);
    assert_eq!(p.diagnostics[0].rule, "LANG-5");
    assert_eq!(
        &p.syntax.text()
            [p.diagnostics[0].primary.start as usize..p.diagnostics[0].primary.end as usize],
        "let"
    );
    assert!(
        parse("fn f() { let (x, y): (u64, u64) }", ParseMode::Source)
            .diagnostics
            .iter()
            .any(|d| d.rule == "LANG-5")
    );
    good(
        "fn f() { let mut x: u64; x = 1; emit(scoped fn(take y: Writer[u64, in r]) -> Result[Child[u64], BuildError] { emit(take y, 1) }) }",
    );
}

#[test]
fn positional_arguments_are_direct_expression_rows() {
    let p = good("fn f(x: u64) { f(x, 2, field: x, bytes 3) }");
    let item = p.item(0);
    let call = item
        .tree
        .nodes
        .iter()
        .position(|n| n.kind == NodeKind::Call)
        .unwrap();
    let args = item.tree.children(NodeId(call as u32));
    assert_eq!(
        args.iter()
            .map(|&n| item.tree.node(n).kind)
            .collect::<Vec<_>>(),
        [
            NodeKind::Path,
            NodeKind::Path,
            NodeKind::Literal,
            NodeKind::LabelledArgument,
            NodeKind::BytesArgument
        ]
    );
}
