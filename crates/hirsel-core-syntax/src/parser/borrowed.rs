//! The data reader needs the expression tree and byte spans. Its input may be
//! guest memory, so token spellings stay in that slice for the entire parse.
use super::*;

pub enum DataParseError {
    Syntax(Span),
    Resource(ResourceRefusal),
    HostMemory,
}

pub struct DataSyntax<'a> {
    text: &'a str,
    tree: Tree,
    range: ItemRange,
}

impl DataSyntax<'_> {
    pub fn item(&self) -> Item<'_> {
        Item {
            text: self.text,
            tree: &self.tree,
            range: &self.range,
            file_offset: self.range.file_offset,
            root: self.range.root,
        }
    }
}

pub fn parse_data(bytes: &[u8]) -> Result<DataSyntax<'_>, DataParseError> {
    parse_document(bytes, None)
}

/// Parse a source-preserving host document with the same first-error and
/// nesting rules as the guest. The owned tree also retains canonical tokens.
pub fn parse_data_file(bytes: &[u8]) -> Result<SyntaxFile, DataParseError> {
    let parsed = parse_document(bytes, Some(&mut Symbols::default()))?;
    Ok(SyntaxFile {
        source: parsed.text.to_owned(),
        trailing_start: parsed.range.owned.end,
        items: vec![parsed.range],
        tree: parsed.tree,
        generated_origins: Vec::new(),
    })
}

fn parse_document<'a>(
    bytes: &'a [u8],
    symbols: Option<&mut Symbols>,
) -> Result<DataSyntax<'a>, DataParseError> {
    if bytes.len() > u32::MAX as usize {
        return Err(DataParseError::Resource(ResourceRefusal::SourceTooLarge {
            bytes: bytes.len() as u64,
        }));
    }
    let text = std::str::from_utf8(bytes).map_err(|err| {
        let start = err.valid_up_to();
        let end = err.error_len().map_or(bytes.len(), |size| start + size);
        DataParseError::Syntax(Span {
            start: start as u32,
            end: end as u32,
        })
    })?;
    let output = if let Some(symbols) = symbols {
        let output = crate::lexer::lex(text, symbols);
        if let Some(d) = output.diagnostics.first() {
            return Err(DataParseError::Syntax(d.primary));
        }
        output
    } else {
        crate::lexer::lex_data(text)
            .map_err(|_| DataParseError::HostMemory)?
            .map_err(DataParseError::Syntax)?
    };
    let first = output.significant[0];
    let current = output.tokens[first];
    let mut parser = Parser::<true> {
        text,
        symbols: None,
        host_memory: false,
        data_error: None,
        tokens: output.tokens,
        significant: output.significant,
        tree: Tree {
            canonical: output.canonical,
            canonical_offsets: output.canonical_offsets,
            ..Tree::default()
        },
        current,
        current_index: first,
        pos: 0,
        resolution_size: ResolutionSize::default(),
        base: current.span.start,
        whitespace: 0,
        depth: 0,
        delimiter_pos: 0,
        delimiters: smallvec![],
        delimiter_openings: Default::default(),
        diagnostics: Vec::new(),
        resource: None,
        mode: ParseMode::Data,
    };
    let root = parser.data_document().map_err(|()| {
        if parser.host_memory {
            DataParseError::HostMemory
        } else if let Some(resource) = parser.resource.take() {
            DataParseError::Resource(resource)
        } else {
            DataParseError::Syntax(parser.data_error.unwrap_or(parser.current.span))
        }
    })?;
    let (end, byte_end) = document::item_end(&parser, text, 0);
    let range = ItemRange {
        resolution_size: parser.resolution_size,
        has_errors: false,
        owned: 0..byte_end,
        file_offset: parser.base,
        end: byte_end,
        tokens: 0..end as u32,
        nodes: 0..parser.tree.nodes.len() as u32,
        root,
    };
    parser.tree.tokens = parser.tokens;
    Ok(DataSyntax {
        text,
        tree: parser.tree,
        range,
    })
}
