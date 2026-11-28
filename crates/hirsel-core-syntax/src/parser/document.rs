use super::*;

impl<const BORROWED: bool> Parser<'_, BORROWED> {
    pub(super) fn data_document(&mut self) -> Parsed<NodeId> {
        let start = self.pos;
        let module = self.name()?;
        self.expect(sym::DOT)?;
        self.name()?;
        let head = self.node(start, NodeKind::Path, Some(module), &[])?;
        self.expect(sym::LPAREN)?;
        let mut children = smallvec![head];
        self.list_into(sym::RPAREN, &mut children, Self::call_argument)?;
        if !self.eof() {
            return self.fail("LANG-5", "expected end of data document");
        }
        self.node(start, NodeKind::Call, None, &children)
    }
}

pub(super) fn items(p: &mut Parser, text: &str, lexical_diagnostics: usize) -> SyntaxFile {
    let mut syntax = SyntaxFile::default();
    let mut owned = 0;
    let mut byte_owned = 0;
    if p.mode == ParseMode::Data && p.eof() {
        let _ = p.fail::<()>("LANG-5", "expected a qualified record constructor");
    }
    while !p.eof() && p.resource.is_none() {
        if p.mode != ParseMode::Data && p.eat(sym::SEMICOLON) {
            continue;
        }
        let start = p.pos;
        p.delimiter_pos = start;
        p.clear_delimiters();
        let diagnostic_start = p.diagnostics.len();
        let file_offset = p.tokens[p.current_index].span.start;
        p.base = file_offset;
        let nodes_start = p.tree.nodes.len() as u32;
        p.resolution_size = ResolutionSize::default();
        let root = if p.mode == ParseMode::Data {
            p.data_document().unwrap_or_else(|()| {
                p.node(start, NodeKind::Error, None, &[])
                    .expect("compiler arena")
            })
        } else {
            match p.item() {
                Ok(root) => {
                    if p.end(false).is_err() {
                        p.recover(start, true);
                    }
                    root
                }
                Err(()) => {
                    p.recover(start, true);
                    p.node(start, NodeKind::Error, None, &[])
                        .expect("compiler arena")
                }
            }
        };
        let (end, byte_end) = item_end(p, text, owned);
        let has_errors = p.resource.is_some()
            || p.diagnostics.len() != diagnostic_start
            || p.diagnostics[..lexical_diagnostics].iter().any(|d| {
                d.primary.start >= byte_owned
                    && (d.primary.start < byte_end
                        || byte_end as usize == text.len() && d.primary.start == byte_end)
            });
        syntax.items.push(ItemRange {
            resolution_size: p.resolution_size,
            has_errors,
            owned: byte_owned..byte_end,
            file_offset,
            end: byte_end,
            tokens: owned as u32..end as u32,
            nodes: nodes_start..p.tree.nodes.len() as u32,
            root,
        });
        owned = end;
        byte_owned = byte_end;
        if p.mode == ParseMode::Data {
            break;
        }
    }
    syntax.trailing_start = byte_owned;
    syntax
}

pub(super) fn item_end<const BORROWED: bool>(
    p: &Parser<BORROWED>,
    text: &str,
    owned: usize,
) -> (usize, u32) {
    let mut end = p.index();
    let consumed = p
        .significant
        .get(p.pos.saturating_sub(1))
        .copied()
        .map_or(owned, |i| i + 1);
    let mut byte_end = p.tokens[end].span.start;
    // Split the first line break after the item, retaining following documentation.
    // Whitespace occupies source gaps instead of rows in the token table.
    for i in consumed..=end {
        let gap_start = if i == 0 { 0 } else { p.tokens[i - 1].span.end } as usize;
        let next_start = p.tokens[i].span.start as usize;
        if let Some(newline) = text[gap_start..next_start].find('\n') {
            end = i;
            byte_end = (gap_start + newline + 1) as u32;
            break;
        }
    }
    (end, byte_end)
}
