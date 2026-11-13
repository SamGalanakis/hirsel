//! LANG-1: the source-text rules in docs/spec/syntax.md.
use crate::*;

fn scan(text: &str) -> LexOutput {
    lex(text, &mut Symbols::default())
}
fn parsed(text: &str) -> ParseOutput {
    parse(text, ParseMode::Source, &mut Symbols::default())
}
fn refusal(text: &str, span: Span) {
    let l = scan(text);
    assert_eq!(l.diagnostics.len(), 1, "{text:?}: {:?}", l.diagnostics);
    assert_eq!(l.diagnostics[0].rule, "LANG-1");
    assert_eq!(l.diagnostics[0].primary, span, "{text:?}");
    assert!(l.tokens.iter().any(|t| t.kind == TokenKind::Invalid));
}

// Checked mutation A: feed malformed UTF-8 to an empty successful parse instead of refusing it.
#[test]
fn lang_1_utf8_is_validated_before_syntax_positions() {
    let text = "// é𝄞\nfn f() { \"中文\" }";
    let good = parse_bytes(text.as_bytes(), ParseMode::Source, &mut Symbols::default());
    assert!(good.diagnostics.is_empty());
    assert_eq!(good.syntax.text(), text);
    for (bad, len) in [
        (&b"\xff"[..], 1),
        (&b"\x80"[..], 1),
        (&b"\xc0\xaf"[..], 1),
        (&b"\xed\xa0\x80"[..], 1),
        (&b"\xf0\x9f"[..], 2),
    ] {
        // The valid prefix is 10 UTF-8 bytes; no lexical or syntax positions exist yet.
        let mut bytes = "// é𝄞\n".as_bytes().to_vec();
        bytes.extend_from_slice(bad);
        let p = parse_bytes(&bytes, ParseMode::Source, &mut Symbols::default());
        assert_eq!(p.diagnostics.len(), 1, "{bytes:?}");
        assert_eq!(p.diagnostics[0].rule, "LANG-1");
        assert_eq!(
            p.diagnostics[0].primary,
            Span {
                start: 10,
                end: 10 + len
            }
        );
        assert!(p.syntax.items.is_empty() && p.syntax.tree.tokens.is_empty());
        assert_eq!(p.resource_refusal, None);
    }
}

// Checked mutation A: suppress the scanner's LANG-1 source-character diagnostic.
#[test]
fn lang_1_source_has_no_byte_order_mark() {
    assert!(parsed("fn f() {}").diagnostics.is_empty());
    refusal("\u{feff}fn f() {}", Span { start: 0, end: 3 });
}

// Checked mutation A: suppress bare-CR refusals; B: stop recording LF/CRLF as newlines.
#[test]
fn lang_1_lf_crlf_and_bare_cr() {
    for nl in ["\n", "\r\n"] {
        let text = format!("// line{nl}fn f() {{{nl}let x = 1{nl}x{nl}}}");
        assert!(parsed(&text).diagnostics.is_empty(), "{text:?}");
        let text = format!("a{nl}b");
        let l = scan(&text);
        assert!(l.diagnostics.is_empty());
        assert!(l.tokens[1].line_before);
        assert_eq!(l.tokens[1].span.start as usize, 1 + nl.len());
    }
    for (text, start) in [("a\rb", 1), ("//a\rb", 3), ("/*a\rb*/", 3)] {
        refusal(
            text,
            Span {
                start,
                end: start + 1,
            },
        );
    }
}

const BIDI: [u32; 12] = [
    0x061c, 0x200e, 0x200f, 0x202a, 0x202b, 0x202c, 0x202d, 0x202e, 0x2066, 0x2067, 0x2068, 0x2069,
];
// Checked mutation A: suppress LANG-1 diagnostics while scanning forbidden source scalars.
#[test]
fn lang_1_bidi_controls_are_refused_in_every_token_context() {
    for code in BIDI {
        let c = char::from_u32(code).unwrap();
        for (before, after, whole_token) in [
            ("", "", false),
            ("//", "", false),
            ("///", "", false),
            ("/*", "*/", false),
            ("/**", "*/", false),
            ("\"", "\"", false),
            ("r#\"", "\"#", false),
            ("b\"", "\"", false),
            ("br#\"", "\"#", false),
            // Invalid character literals are one quoted token, including its delimiters.
            ("'", "'", true),
            ("b'", "'", true),
        ] {
            let text = format!("{before}{c}{after}");
            let span = if whole_token {
                Span {
                    start: 0,
                    end: text.len() as u32,
                }
            } else {
                Span {
                    start: before.len() as u32,
                    end: (before.len() + c.len_utf8()) as u32,
                }
            };
            refusal(&text, span);
        }
    }
    for code in [
        0x061b, 0x061d, 0x200d, 0x2010, 0x2029, 0x202f, 0x2065, 0x206a,
    ] {
        let c = char::from_u32(code).unwrap();
        for text in [format!("//{c}"), format!("/*{c}*/"), format!("\"{c}\"")] {
            assert!(scan(&text).diagnostics.is_empty(), "U+{code:04X}: {text:?}");
        }
    }
}

// Checked mutation A: reject otherwise valid Unicode escapes in quoted strings.
#[test]
fn lang_1_string_escapes_may_denote_bidi_controls() {
    for code in BIDI {
        let text = format!("fn f() {{ \"\\u{{{code:x}}}\" }}");
        let p = parsed(&text);
        assert!(p.diagnostics.is_empty(), "{text}: {:?}", p.diagnostics);
        assert_eq!(p.syntax.text(), text);
    }
}

// Checked mutation A: omit the LANG-1 diagnostic on a non-ASCII identifier character.
#[test]
fn lang_1_identifiers_are_ascii() {
    let l = scan("Alpha_9 _private z0");
    assert!(l.diagnostics.is_empty());
    assert!(l.tokens[..3].iter().all(|t| t.kind == TokenKind::Ident));
    assert_eq!(
        l.tokens[..3].iter().map(|t| t.span).collect::<Vec<_>>(),
        [
            Span { start: 0, end: 7 },
            Span { start: 8, end: 16 },
            Span { start: 17, end: 19 },
        ]
    );
    refusal("aé", Span { start: 1, end: 3 });
    refusal("a𝄞", Span { start: 1, end: 5 });
}

// Checked mutation C: store scalar counts in token spans instead of byte offsets.
#[test]
fn lang_1_positions_are_byte_offsets() {
    let text = "/*é𝄞*/$";
    refusal(text, Span { start: 10, end: 11 });
    let l = scan(text);
    assert_eq!(l.tokens[0].span, Span { start: 0, end: 10 });
    assert_eq!(l.tokens[1].span, Span { start: 10, end: 11 });
    assert_eq!(l.tokens[2].span, Span { start: 11, end: 11 });
}

// Checked mutation B: consume CR as part of a line comment instead of its newline.
#[test]
fn lang_1_comment_scalars_and_delimiters() {
    for nl in ["\n", "\r\n"] {
        let text = format!("//é𝄞{nl}x");
        let l = scan(&text);
        assert!(l.diagnostics.is_empty());
        assert_eq!(l.tokens[0].kind, TokenKind::LineComment);
        assert_eq!(l.tokens[0].span, Span { start: 0, end: 8 });
        assert_eq!(
            &text[l.tokens[1].span.start as usize..l.tokens[1].span.end as usize],
            "x"
        );
    }
    // Slash and star are content until they begin a complete delimiter.
    let l = scan("/* /x *y é */x");
    assert!(l.diagnostics.is_empty());
    assert_eq!(l.tokens[0].kind, TokenKind::BlockComment);
    assert_eq!(l.tokens[0].span, Span { start: 0, end: 14 });
}

// Checked mutation B: match > before >>= rather than choosing the longest token.
#[test]
fn lang_1_tokens_use_longest_match() {
    let text = "letter9 >>= 12..=13 == 14 -> x";
    let l = scan(text);
    assert!(l.diagnostics.is_empty());
    let words = l
        .tokens
        .iter()
        .filter(|t| t.kind != TokenKind::Eof)
        .map(|t| &text[t.span.start as usize..t.span.end as usize])
        .collect::<Vec<_>>();
    assert_eq!(
        words,
        ["letter9", ">>=", "12", "..=", "13", "==", "14", "->", "x"]
    );
}

// Checked mutation B: allow the number scanner to absorb a fraction after a projection dot.
#[test]
fn lang_1_no_float_starts_after_a_projection_dot() {
    let l = scan("p.0.1 0.1");
    assert!(l.diagnostics.is_empty());
    assert_eq!(
        l.tokens.iter().map(|t| t.kind).collect::<Vec<_>>(),
        [
            TokenKind::Ident,
            TokenKind::Dot,
            TokenKind::Integer,
            TokenKind::Dot,
            TokenKind::Integer,
            TokenKind::Float,
            TokenKind::Eof,
        ]
    );
}

// Checked mutation B: fail to increment depth on an inner /* delimiter.
#[test]
fn lang_1_comments_nest() {
    let text = "/* outer /* inner */ tail */x";
    let l = scan(text);
    assert!(l.diagnostics.is_empty());
    assert_eq!(l.tokens.len(), 3);
    assert_eq!(l.tokens[0].kind, TokenKind::BlockComment);
    assert_eq!(l.tokens[0].span, Span { start: 0, end: 28 });
    assert_eq!(l.tokens[1].kind, TokenKind::Ident);
}

// Checked mutation B: classify /// and /** as ordinary rather than documentation tokens.
#[test]
fn lang_1_documentation_comments_are_otherwise_comments() {
    let text = "/// line\n/** block */fn f() {}";
    let l = scan(text);
    assert!(l.diagnostics.is_empty());
    assert_eq!(
        (l.tokens[0].kind, l.tokens[1].kind),
        (TokenKind::DocLine, TokenKind::DocBlock)
    );
    assert_eq!(
        l.significant
            .iter()
            .map(|&i| l.tokens[i].kind)
            .collect::<Vec<_>>(),
        [
            TokenKind::KwFn,
            TokenKind::Ident,
            TokenKind::LParen,
            TokenKind::RParen,
            TokenKind::LBrace,
            TokenKind::RBrace,
            TokenKind::Eof,
        ]
    );
    assert!(parsed(text).diagnostics.is_empty());
}

// Checked mutation B: omit the block-comment newline flag.
#[test]
fn lang_1_block_comment_newlines_count_for_lang_4() {
    assert!(
        parsed("fn f() { 1 /* no line */ + 2 }")
            .diagnostics
            .is_empty()
    );
    assert!(parsed("fn f() { (1 /*\n*/ + 2) }").diagnostics.is_empty());
    let text = "fn f() { 1 /*\n*/ + 2 }";
    let p = parsed(text);
    assert_eq!(p.diagnostics.len(), 1);
    assert_eq!(p.diagnostics[0].rule, "LANG-4");
    assert_eq!(p.diagnostics[0].primary, Span { start: 17, end: 18 });
}

// Checked mutation C: accept EOF with an unmatched block-comment delimiter.
#[test]
fn lang_1_unterminated_comment_is_one_invalid_token() {
    assert!(scan("// EOF is allowed").diagnostics.is_empty());
    for text in ["/*", "/**", "/* outer /* inner */"] {
        refusal(
            text,
            Span {
                start: 0,
                end: text.len() as u32,
            },
        );
        let l = scan(text);
        assert_eq!(
            l.tokens.iter().map(|t| t.kind).collect::<Vec<_>>(),
            [TokenKind::Invalid, TokenKind::Eof]
        );
    }
}
