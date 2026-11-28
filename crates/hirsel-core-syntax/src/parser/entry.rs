use super::*;

/// Decode external source bytes. Invalid UTF-8 has a raw byte span (PROG-7).
pub fn parse_bytes(bytes: &[u8], mode: ParseMode, symbols: &mut Symbols) -> ParseOutput {
    match std::str::from_utf8(bytes) {
        Ok(text) => parse(text, mode, symbols),
        Err(error) => {
            if bytes.len() > u32::MAX as usize {
                return ParseOutput {
                    syntax: SyntaxFile::default(),
                    diagnostics: vec![],
                    resource_refusal: Some(ResourceRefusal::SourceTooLarge {
                        bytes: bytes.len() as u64,
                    }),
                };
            }
            let start = error.valid_up_to();
            let end = start + error.error_len().unwrap_or(bytes.len() - start);
            ParseOutput {
                syntax: SyntaxFile::default(),
                diagnostics: vec![Diagnostic::error(
                    "LANG-1",
                    Span {
                        start: start as u32,
                        end: end as u32,
                    },
                    "source is not UTF-8",
                )],
                resource_refusal: None,
            }
        }
    }
}

pub fn parse(text: &str, mode: ParseMode, symbols: &mut Symbols) -> ParseOutput {
    parse_derived(text, mode, symbols, usize::MAX)
}
/// Parse `text` whose bytes from `derived` on are generated (see `lexer::lex_derived`).
pub(crate) fn parse_derived(
    text: &str,
    mode: ParseMode,
    symbols: &mut Symbols,
    derived: usize,
) -> ParseOutput {
    if text.len() > u32::MAX as usize {
        return ParseOutput {
            syntax: SyntaxFile::default(),
            diagnostics: vec![],
            resource_refusal: Some(ResourceRefusal::SourceTooLarge {
                bytes: text.len() as u64,
            }),
        };
    }
    let output = crate::lexer::lex_derived(text, symbols, derived);
    let lexical_diagnostics = output.diagnostics.len();
    let significant = output.significant;
    let canonical = output.canonical;
    let canonical_offsets = output.canonical_offsets;
    let node_capacity = significant.len();
    let mut p = Parser::<false> {
        text,
        symbols: Some(symbols),
        host_memory: false,
        data_error: None,
        current: output.tokens[significant[0]],
        current_index: significant[0],
        resolution_size: ResolutionSize::default(),
        tokens: output.tokens,
        significant,
        pos: 0,
        tree: Tree {
            tokens: vec![],
            nodes: Vec::with_capacity(node_capacity),
            edges: Vec::with_capacity(node_capacity),
            canonical,
            canonical_offsets,
        },
        base: 0,
        whitespace: 0,
        depth: 0,
        delimiter_pos: 0,
        delimiters: smallvec![],
        delimiter_openings: [Vec::new(), Vec::new(), Vec::new()],
        diagnostics: output.diagnostics,
        resource: None,
        mode,
    };
    let mut syntax = document::items(&mut p, text, lexical_diagnostics);
    syntax.source = text.to_owned();
    p.tree.tokens = p.tokens;
    syntax.tree = p.tree;
    ParseOutput {
        syntax,
        diagnostics: p.diagnostics,
        resource_refusal: p.resource,
    }
}
