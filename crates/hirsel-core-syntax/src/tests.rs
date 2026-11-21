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
    assert_eq!(shape(&p), shape(&q));
    output
}

type NodeShape = (NodeKind, Option<String>, u32);
type ItemShape = (u32, Vec<u32>, Vec<NodeShape>);
fn shape(p: &ParseOutput) -> Vec<ItemShape> {
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
}

#[test]
fn item_source_and_anchor_start_at_the_first_significant_token() {
    let parsed = parse(
        "// before\n\nfn first() -> u64 { 1 }\n\n// attached\nfn second() -> u64 { 2 }\n",
        ParseMode::Source,
    );
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    let first = parsed.syntax.item(0);
    let second = parsed.syntax.item(1);
    assert_eq!(first.source_text(), "fn first() -> u64 { 1 }\n");
    assert_eq!(second.source_text(), "fn second() -> u64 { 2 }\n");
    assert_eq!(
        first.file_offset as usize,
        parsed.syntax.source.find("fn first").unwrap()
    );
    assert_eq!(
        second.file_offset as usize,
        parsed.syntax.source.find("fn second").unwrap()
    );
    assert_eq!(first.range.owned.start, 0);
    assert_eq!(second.range.owned.start, first.range.owned.end);
    assert!(
        (second.range.owned.start as usize) < parsed.syntax.source.find("// attached").unwrap()
    );
    assert!(second.range.owned.start < second.file_offset);
}

#[test]
fn formatter_canonical_blocks_lists_and_attributes() {
    let text = "@compact(u32) type A{x:u64,y: (u64,u64)}\nfn f [T](x:T)->T{let mut y:T=x; if true {y}else{ x }}";
    assert_eq!(
        formatted(text, ParseMode::Source),
        "@compact(u32)\ntype A {\n    x: u64,\n    y: (u64, u64)\n}\n\nfn f[T](x: T) -> T {\n    let mut y: T = x;\n    if true {\n        y\n    } else {\n        x\n    }\n}\n"
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
    assert_eq!(
        formatted("fn f() { a // continues\n + b }", ParseMode::Source),
        "fn f() {\n    a // continues\n        + b\n}\n"
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
    assert_eq!(
        formatted(
            "fn f(){a?=>|(c,_)|Full(c);b ?=>json.wrap}",
            ParseMode::Source
        ),
        "fn f() {\n    a ?=> |(c, _)| Full(c);\n    b ?=> json.wrap\n}\n"
    );
}

#[test]
fn formatter_refuses_invalid_or_incomplete_source() {
    assert!(format(&parse("fn f() { let = 1 }", ParseMode::Source)).is_none());
    let mut parsed = parse("fn f() {}", ParseMode::Source);
    parsed.resource_refusal = Some(ResourceRefusal::SourceTooLarge { bytes: 1 << 32 });
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
fn lang_5_precedence_and_prog_7_relative_arenas() {
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
fn lang_2_lang_4_lang_5_statement_boundaries_and_recovery() {
    for (text, rule) in [
        ("fn f() { return\n + a }\nfn next() {}", "LANG-4"),
        (
            include_str!("../../../core/front/cases/LANG-5-nonassociative.hirsel"),
            "LANG-5",
        ),
        ("fn f() { match 1 { A | B => 0 } }\nfn next() {}", "LANG-5"),
        (
            include_str!("../../../core/front/cases/LANG-2-keyword-name.hirsel"),
            "LANG-2",
        ),
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
    let mut equivalents = Vec::new();
    for op in [
        "&&", "||", "==", "!=", "<", "<=", ">", ">=", "+", "*", "/", "%", "&", "|", "^", "<<",
        ">>", "..", "..=",
    ] {
        equivalents.push((
            format!("fn f() {{ a {op} b }}"),
            format!("fn f() {{ a // next token continues\n\n /* comment */ {op} b }}"),
        ));
    }
    for op in [
        "=", "+=", "-=", "*=", "/=", "%=", "<<=", ">>=", "&=", "|=", "^=",
    ] {
        equivalents.push((
            format!("fn f() {{ x {op} a + b }}"),
            format!("fn f() {{ x\n {op} a\n + b }}"),
        ));
    }
    for (single, split) in [
        ("fn f() { a.b.c }", "fn f() { a\n .b\n .c }"),
        ("fn f() { a? ?=> m }", "fn f() { a\n ?\n ?=> m }"),
        (
            "fn f() { a().b.0?.c? }",
            "fn f() { a()\n .b\n .0\n ?\n .c\n ? }",
        ),
        (
            "fn f() { let y: u64 = a + b }",
            "fn f() { let y: u64\n = a\n + b }",
        ),
        (
            "fn f() { match x { A => a + b, -1 => c } }",
            "fn f() { match x { A => a\n + b\n -1 => c } }",
        ),
        ("type T = A | B(u64) | C", "type T = A\n | B(u64)\n | C"),
        ("fn f() { return a + b }", "fn f() { return a\n + b }"),
        (
            "fn f() { loop { break 'l a + b } }",
            "fn f() { loop { break 'l a\n + b } }",
        ),
        ("fn f() { if a && b {} }", "fn f() { if a\n && b {} }"),
        ("fn f() { while a && b {} }", "fn f() { while a\n && b {} }"),
        (
            "fn f() { match x.kind { A => a } }",
            "fn f() { match x\n .kind { A => a } }",
        ),
        ("fn f() { { a } + b * c }", "fn f() { { a }\n + b\n * c }"),
        ("fn f() { a - b * c }", "fn f() { a -\n b\n * c }"),
        (
            "fn f() { a + b * c == d || e && f }",
            "fn f() { a\n + b\n * c\n == d\n || e\n && f }",
        ),
        (
            "fn f() {}\nfn g() { a + b }",
            "fn f() {}\nfn g() { a\n + b }",
        ),
        ("fn f() { a + (b\n - c) }", "fn f() { a\n + (b\n - c) }"),
        ("fn f() { a + b }", "fn f() { a /*\n*/ + b }"),
        ("fn f() { a - b }", "fn f() { a - // required operand\n b }"),
        ("fn f() { a + b }", "fn f() { a + // required operand\n b }"),
        ("fn f() { let y = a + b }", "fn f() { let y\n = a\n + b }"),
        ("type T { a: u64, b: u64 }", "type T { a: u64\n b: u64 }"),
    ] {
        equivalents.push((single.to_owned(), split.to_owned()));
    }
    let mut failures = Vec::new();
    let equivalent_count = equivalents.len();
    for (single, split) in equivalents {
        let expected = parse(&single, ParseMode::Source);
        assert!(
            expected.diagnostics.is_empty(),
            "{single}: {:?}",
            expected.diagnostics
        );
        let actual = parse(&split, ParseMode::Source);
        if !actual.diagnostics.is_empty() || shape(&actual) != shape(&expected) {
            failures.push(format!("{split:?}: {:?}", actual.diagnostics));
            continue;
        }
        let output = formatted(&split, ParseMode::Source);
        assert_eq!(
            shape(&parse(&output, ParseMode::Source)),
            shape(&expected),
            "{split}"
        );
    }
    eprintln!(
        "{equivalent_count} AST equivalence and formatter round-trip cases; {} failed",
        failures.len()
    );
    assert!(
        failures.is_empty(),
        "{} continuation cases failed:\n{}",
        failures.len(),
        failures.join("\n")
    );
    for next in ["-b", "!b", "~b", "(b)", "[b]", "{ b }"] {
        let text = format!("fn f() {{ a\n {next} }}");
        let p = parse(&text, ParseMode::Source);
        assert!(p.diagnostics.is_empty(), "{text}: {:?}", p.diagnostics);
        let item = p.syntax.item(0);
        let block = *item.tree.children(item.root).last().unwrap();
        assert_eq!(item.tree.children(block).len(), 2, "{text}");
        formatted(&text, ParseMode::Source);
    }
    for exit in ["return", "break", "break 'l", "continue", "continue 'l"] {
        for token in ["+", ".", "?", "=", "-=", "&&"] {
            let text = format!("fn f() {{ {exit}\n {token} a }}");
            let p = parse(&text, ParseMode::Source);
            let start = text.find(token).unwrap() as u32;
            assert!(
                p.diagnostics.iter().any(|d| d.rule == "LANG-4"
                    && d.primary.start == start
                    && d.primary.end == start + token.len() as u32),
                "{text}: {:?}",
                p.diagnostics
            );
        }
        let text = format!("fn f() {{ {exit}\n -a }}");
        let p = parse(&text, ParseMode::Source);
        assert!(p.diagnostics.is_empty(), "{text}: {:?}", p.diagnostics);
        formatted(&text, ParseMode::Source);
    }
    for (source, site, rule) in [
        ("fn f() { return\n + a }", "+", "LANG-4"),
        ("fn f() { break\n .a }", ".", "LANG-4"),
        ("fn f() { continue\n ? }", "?", "LANG-4"),
        ("type T =\n | A", "|", "LANG-4"),
        ("type T { a: u64\n + b: u64 }", "+", "LANG-4"),
        ("type T { a: u64 = 1\n + 2 }", "+", "LANG-4"),
        ("type T = A(u64)\n + B", "+", "LANG-4"),
        ("type T = A(u64)\n .B", ".", "LANG-4"),
        ("type T = A(u64) | B\n (u32)", "(u32)", "LANG-5"),
        ("type T = A\n (u64)", "(u64)", "LANG-5"),
        ("type T: plain\n + shareable {}", "+", "LANG-4"),
        ("type T = X\n .Y", ".", "LANG-4"),
        ("fn f() { match x { 1\n ..= 2 => a } }", "..=", "LANG-4"),
        ("fn f() { match x { A\n (b) => a } }", "(b)", "LANG-5"),
        ("fn f() {}\n + a", "+", "LANG-4"),
        ("fn f() { a;\n + b }", "+", "LANG-4"),
        ("fn f() { a < b\n < c }", "< c", "LANG-5"),
        ("fn f() { match a\n + {} {} }", "{} {}", "LANG-5"),
        ("fn f() { match x { a\n .b => c } }", ".", "LANG-4"),
        ("fn f() { a ?=> m(b) }", "(b)", "LANG-5"),
        ("fn f() { a ?=> m?}", "?}", "LANG-5"),
    ] {
        let p = parse(source, ParseMode::Source);
        let at = source.find(site).unwrap() as u32;
        assert!(
            p.diagnostics
                .iter()
                .any(|d| d.rule == rule && d.primary.start == at),
            "{source}: {:?}",
            p.diagnostics
        );
    }
    good("fn f() { let x =\n 1 +\n 2\n if true {}\n else {}\n x }\n");
    good("fn f() { let x = (1\n + 2); x }\n");
}
#[test]
fn new_forms_have_written_nodes() {
    let f = good(
        "@compact(u32) type P[in r] { x: u64 }\nfn f[in r](x: P in r) -> u64 where r: r { let mut y: u64; region a { bound bytes 8 { bound steps 10 { loop { break (x, 1).0 } } } } }\n",
    );
    let kinds: Vec<_> = f
        .items()
        .flat_map(|i| i.tree.nodes.iter().map(|n| n.kind))
        .collect();
    for k in [
        NodeKind::Attribute,
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
fn lang_1_lang_3_literals_are_lossless_and_decode_the_new_rules() {
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
    let source = parse(s, ParseMode::Source);
    assert_eq!(source.diagnostics.len(), 1);
    assert_eq!(source.diagnostics[0].rule, "LANG-5");
    let end = s.trim_end().len() as u32;
    assert_eq!(source.diagnostics[0].primary, Span { start: end, end });
    let source = parse(
        "fn width() -> u64  // no body\nfn next() { 1 }",
        ParseMode::Source,
    );
    assert_eq!(source.diagnostics.len(), 1);
    assert_eq!(source.diagnostics[0].rule, "LANG-5");
    assert_eq!(source.diagnostics[0].primary, Span { start: 17, end: 17 });
    assert_eq!(source.syntax.items.len(), 2);
    assert!(!source.syntax.item(1).range.has_errors);
    let d = Diagnostic::error("LANG-5", Span { start: 4, end: 5 }, "expected type");
    let json = serde_json::to_value(d).unwrap();
    assert_eq!(json["rule"], "LANG-5");
    assert_eq!(json["primary"]["start"], 4);
}

#[test]
fn revised_callable_summaries_and_projection_tokenization() {
    let f = good(
        "@compact(u32) type P { x: u64 }\nresource type R { x: u64 } drop(r) {}\nfn f(g: own[send] fn(take u64) -> u64 waits, p: ((u64, u64), u64)) -> u64 { p.0.1 }\n",
    );
    assert!(
        f.item(2)
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
fn lang_1_invalid_utf8_has_a_raw_primary_byte_span() {
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

#[test]
fn callable_parameter_modes_and_refused_mode_positions() {
    for (mark, kind) in [
        ("", NodeKind::Parameter),
        ("mut ", NodeKind::MutParameter),
        ("take ", NodeKind::TakeParameter),
        ("bytes ", NodeKind::BytesParameter),
    ] {
        for name in ["", "n: "] {
            for prefix in ["", "scoped ", "own "] {
                let text = format!("fn f(g: {prefix}fn({mark}{name}u64) -> u64) {{}}");
                let f = good(&text);
                let parameters = f.tree.nodes.iter().filter(|n| n.kind == kind).count();
                assert_eq!(parameters, if mark.is_empty() { 2 } else { 1 }, "{text}");
                let canonical = formatted(&text, ParseMode::Source);
                assert!(
                    canonical.contains(&format!("{mark}{name}u64")),
                    "{canonical}"
                );
            }
            let text = format!("fn f() {{ let g = fn({mark}{name}u64) -> u64 {{ 1 }}; }}");
            good(&text);
            formatted(&text, ParseMode::Source);
        }
    }
    for first in ["mut", "take", "bytes"] {
        for second in ["mut", "take", "bytes"] {
            for text in [
                format!("fn f(g: fn({first} {second} u64)) {{}}"),
                format!("fn f({first} {second} n: u64) {{}}"),
                format!("fn f() {{ let g = fn({first} {second} n: u64) {{}}; }}"),
            ] {
                let p = parse(&text, ParseMode::Source);
                assert_eq!(
                    p.diagnostics.first().map(|d| d.rule.as_str()),
                    Some("LANG-5"),
                    "{text}: {:?}",
                    p.diagnostics
                );
            }
        }
        let text = format!("fn f(g: fn() -> {first} u64) {{}}");
        let p = parse(&text, ParseMode::Source);
        assert_eq!(
            p.diagnostics.first().map(|d| d.rule.as_str()),
            Some("LANG-5"),
            "{text}: {:?}",
            p.diagnostics
        );
    }
    let p = parse(
        "fn f(g: fn(bytes u64) -> u64) { g(n: bytes 1) }",
        ParseMode::Source,
    );
    assert_eq!(
        p.diagnostics.first().map(|d| d.rule.as_str()),
        Some("LANG-5"),
        "{:?}",
        p.diagnostics
    );
}

struct NestingCase {
    name: &'static str,
    context: (&'static str, &'static str),
    syntax: (&'static str, &'static str, &'static str),
    depth: (usize, usize),
}
impl NestingCase {
    fn source(&self, layers: usize) -> String {
        format!(
            "{}{}{}{}{}",
            self.context.0,
            self.syntax.0.repeat(layers),
            self.syntax.1,
            self.syntax.2.repeat(layers),
            self.context.1
        )
    }
}
fn nesting_cases() -> impl Iterator<Item = NestingCase> {
    let e = ("fn f() { ", " }");
    let t = ("type T = ", "");
    let p = ("fn f() { let ", " = 1; }");
    [
        ("group expression", e, ("(", "1", ")"), (2, 2)),
        ("tuple expression", e, ("(1, ", "1", ")"), (2, 2)),
        ("array expression", e, ("[", "1", "]"), (2, 2)),
        ("unary expression", e, ("!", "true", ""), (2, 2)),
        ("binary right operand", e, ("1 + (", "1", ")"), (2, 3)),
        ("call argument", e, ("g(", "1", ")"), (2, 1)),
        (
            "labelled constructor argument",
            e,
            ("Node(child: ", "1", ")"),
            (2, 1),
        ),
        ("index expression", e, ("g[", "1", "]"), (2, 1)),
        ("block", e, ("{", "1", "}"), (2, 3)),
        ("closure body", e, ("fn() {", "1", "}"), (2, 3)),
        (
            "scoped closure body",
            e,
            ("scoped fn() {", "1", "}"),
            (2, 3),
        ),
        ("if body", e, ("if true {", "1", "}"), (2, 3)),
        ("else if", e, ("if true {} else ", "{}", ""), (3, 1)),
        ("match arm", e, ("match 1 { _ => ", "1", "}"), (2, 2)),
        ("loop", e, ("loop {", "1", "}"), (2, 3)),
        ("while", e, ("while true {", "1", "}"), (2, 3)),
        ("for", e, ("for x in 0 {", "1", "}"), (2, 3)),
        ("region", e, ("region r {", "1", "}"), (2, 3)),
        ("bound", e, ("bound bytes 1 {", "1", "}"), (2, 3)),
        ("labelled loop", e, ("'a: loop {", "1", "}"), (2, 4)),
        ("return operand", e, ("return ", "1", ""), (2, 2)),
        ("group type", t, ("(", "u64", ")"), (1, 1)),
        ("tuple type", t, ("(u64, ", "u64", ")"), (1, 1)),
        ("generic type argument", t, ("List[", "u64", "]"), (1, 1)),
        ("callable parameter type", t, ("fn(", "u64", ")"), (1, 1)),
        (
            "own callable parameter type",
            t,
            ("own[send] fn(", "u64", ")"),
            (1, 1),
        ),
        ("callable result type", t, ("fn() -> ", "u64", ""), (1, 1)),
        ("tuple pattern", p, ("(", "x", ")"), (2, 1)),
        ("constructor pattern", p, ("Some(", "x", ")"), (2, 1)),
        (
            "labelled constructor pattern",
            p,
            ("Node(child: ", "x", ")"),
            (2, 1),
        ),
        ("range pattern", p, ("1..=", "x", ""), (2, 1)),
    ]
    .into_iter()
    .map(|(name, context, syntax, depth)| NestingCase {
        name,
        context,
        syntax,
        depth,
    })
}

#[test]
fn recursive_productions_obey_one_nesting_budget() {
    for case in nesting_cases() {
        let last_complete = (PARSER_NESTING_LIMIT - case.depth.0) / case.depth.1;
        for layers in [last_complete, last_complete + 1, 100_000] {
            let text = case.source(layers);
            let p = parse(&text, ParseMode::Source);
            assert!(
                p.diagnostics.is_empty(),
                "{} at {layers}: {:?}",
                case.name,
                p.diagnostics
            );
            assert_eq!(p.syntax.text(), text);
            if layers == last_complete {
                assert_eq!(p.resource_refusal, None, "{} at {layers}", case.name);
            } else {
                assert!(
                    matches!(p.resource_refusal,
                    Some(ResourceRefusal::ParserStack { limit: PARSER_NESTING_LIMIT, at }) if at.start < at.end && at.end as usize <= text.len()),
                    "{} at {layers}: {:?}",
                    case.name,
                    p.resource_refusal
                );
                assert!(format(&p).is_none());
            }
        }
    }
}

#[test]
fn nesting_budget_has_an_exact_boundary_and_a_located_tool_message() {
    let text = format!(
        "// header\ntype T = {}u64{}",
        "List[".repeat(PARSER_NESTING_LIMIT - 1),
        "]".repeat(PARSER_NESTING_LIMIT - 1)
    );
    let p = parse(&text, ParseMode::Source);
    assert!(p.diagnostics.is_empty());
    assert_eq!(p.resource_refusal, None);
    let text = format!(
        "// header\ntype T = {}u64{}",
        "List[".repeat(PARSER_NESTING_LIMIT),
        "]".repeat(PARSER_NESTING_LIMIT)
    );
    let p = parse(&text, ParseMode::Source);
    assert!(p.diagnostics.is_empty());
    let at = text.find("u64").unwrap() as u32;
    let resource = p.resource_refusal.unwrap();
    assert_eq!(
        resource.site(),
        Span {
            start: at,
            end: at + 3
        }
    );
    assert_eq!(
        resource.to_string(),
        format!(
            "parser nesting limit of {PARSER_NESTING_LIMIT} active productions reached at bytes {at}..{}",
            at + 3
        )
    );
}

#[test]
fn iterative_attributes_paths_and_postfixes_do_not_spend_nesting_budget() {
    for text in [
        format!("{}type T {{}}", "@test ".repeat(100_000)),
        format!("type T = {}T", "m.".repeat(100_000)),
        format!("fn f() {{ x{} }}", ".field".repeat(100_000)),
        format!("fn f() {{ x{} }}", "()".repeat(100_000)),
    ] {
        let p = parse(&text, ParseMode::Source);
        assert!(p.diagnostics.is_empty(), "{:?}", p.diagnostics);
        assert_eq!(p.resource_refusal, None);
        assert_eq!(p.syntax.text(), text);
    }
    let text = format!(
        "@compact({}u32{}) type T {{}}",
        "(".repeat(100_000),
        ")".repeat(100_000)
    );
    let p = parse(&text, ParseMode::Source);
    assert!(p.diagnostics.iter().any(|d| d.rule == "LANG-5"));
    assert_eq!(p.resource_refusal, None);
    assert_eq!(p.syntax.text(), text);
}

#[test]
fn prog_7_expected_tokens_and_names_include_the_compared_facts() {
    for (source, declared, required) in [
        ("fn f(x u64) {}", "u64", ":"),
        ("fn if() {}", "if", "non-keyword identifier"),
        (
            "@compact(u8) type T {}",
            "u8",
            "u16, u32 or u64 compact width",
        ),
        ("@unknown fn f() {}", "unknown", "test or compact attribute"),
        (
            "type T = A(x: u64, u64) | B",
            "positional payload",
            "labelled payload",
        ),
    ] {
        let p = parse(source, ParseMode::Source);
        let d = &p.diagnostics[0];
        assert_eq!(d.declared.as_deref(), Some(declared));
        assert_eq!(d.required.as_deref(), Some(required));
        let json = serde_json::to_value(d).unwrap();
        assert_eq!(json["declared"], declared);
        assert_eq!(json["required"], required);
    }
    let d = &lex("$").diagnostics[0];
    assert!(
        d.declared.is_none() && d.required.is_none(),
        "source validity compares no declaration"
    );
}

#[test]
fn formatter_preserves_tokens_and_ast_across_the_live_source_corpus() {
    use std::{fs, path::Path};
    fn visit(dir: &Path, mode: ParseMode, count: &mut usize) {
        for entry in fs::read_dir(dir).unwrap() {
            let entry = entry.unwrap();
            let path = entry.path();
            if entry.file_type().unwrap().is_dir() {
                visit(&path, mode, count);
            } else if path.extension().is_some_and(|e| e == "hirsel") {
                let bytes = fs::read(&path).unwrap();
                let text = String::from_utf8_lossy(&bytes);
                let fixture = text.lines().any(|line| {
                    line.trim_start()
                        .strip_prefix("//")
                        .and_then(|s| s.trim_start().strip_prefix("expect:"))
                        .is_some_and(|s| {
                            matches!(s.split_whitespace().next(), Some("refuse" | "gap"))
                        })
                });
                if fixture {
                    continue;
                }
                eprintln!("format round trip: {}", path.display());
                formatted(std::str::from_utf8(&bytes).unwrap(), mode);
                *count += 1;
            }
        }
    }
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut count = 0;
    for (dir, mode) in [
        ("core/core", ParseMode::Signatures),
        ("core/std", ParseMode::Source),
        ("core/front/cases", ParseMode::Source),
        ("core/programs/m1", ParseMode::Source),
        ("core/langarena", ParseMode::Source),
    ] {
        visit(&root.join(dir), mode, &mut count);
    }
    assert!(count > 0);
    eprintln!("{count} source files preserve tokens, AST shape and canonical idempotence");
}

#[test]
fn view_keyword_is_a_callable_bound_only() {
    formatted(
        "fn f(g: scoped[view] fn(mut u64) -> u64) {}",
        ParseMode::Source,
    );
    // LANG-13, rather than the grammar, decides which callable bound fits.
    formatted("fn f(g: own[view] fn() -> u64) {}", ParseMode::Source);
    assert!(
        !parse("fn f[T: view](x: T) {}", ParseMode::Source)
            .diagnostics
            .is_empty()
    );
}

#[test]
fn codec_bound_and_literal_defaults_roundtrip() {
    assert!(!keyword_symbol(sym::CODEC));
    assert_eq!(lex("codec").tokens[0].kind, TokenKind::Ident);
    formatted(
        "pub type Wrapper[T]: codec { x: T, pinned: bool = false, n: i8 = -128, s: String = \"value\" }\nfn codec[T: codec](x: T) {}",
        ParseMode::Source,
    );
    formatted(
        "pub type Row: codec { pinned: bool = false }",
        ParseMode::Signatures,
    );
    for source in [
        "type X: codec { n: u64 = 1 + 2 }",
        "type X: codec { n: u64 = f() }",
        "type X: codec { n: f64 = -1.25 }",
    ] {
        assert!(
            !parse(source, ParseMode::Source).diagnostics.is_empty(),
            "{source}"
        );
    }
}

#[test]
fn codec_override_and_tombstone_roundtrip() {
    for source in [
        "pub opaque type Email: codec via String (to: text, from: parse) { text: String }",
        "type Row: codec { id: u64, removed note: String, pinned: bool = false }",
        "type Shape: codec = Circle(u64) | removed Square(u64) | Line(u64, u64)",
        "type First: codec = removed Old | New",
        "type Context { removed: u64, via: u64, from: bool, to: char }",
    ] {
        formatted(source, ParseMode::Source);
    }
    for word in [sym::VIA, sym::TO, sym::FROM, sym::REMOVED] {
        assert!(!keyword_symbol(word));
    }
}
