use super::*;
use read::Index;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PathPart {
    Field(String),
    Index(usize),
    Key(String),
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Edit {
    pub span: Span,
    pub replacement: String,
}
impl Edit {
    fn apply(&self, source: &str) -> String {
        let mut out = source.to_owned();
        out.replace_range(
            self.span.start as usize..self.span.end as usize,
            &self.replacement,
        );
        out
    }
}
fn lookup<'a>(
    mut index: &'a Index,
    mut value: &'a Value,
    path: &[PathPart],
) -> Option<(&'a Index, &'a Value)> {
    for part in path {
        let pos = index.children.iter().position(|(p, _)| p == part)?;
        if index.tagged {
            value = value.get("fields")?
        }
        value = match part {
            PathPart::Field(key) => value.get(key)?,
            PathPart::Index(i) => value.get(*i)?,
            PathPart::Key(_) => value.get(pos)?.get(1)?,
        };
        index = &index.children[pos].1;
    }
    Some((index, value))
}

impl Document<'_> {
    /// Parse the splice and check that only the selected value changed. Return
    /// no edit when the written value already has the requested meaning.
    pub fn replace(
        &self,
        path: &[PathPart],
        literal: &str,
        limits: Limits,
    ) -> Result<Option<Edit>, DataError> {
        let (index, old) = lookup(&self.index, &self.value, path)
            .ok_or_else(|| error(ErrorKind::Invalid("unknown edit path"), Span::default()))?;
        let span = index.span.unwrap();
        let edit = Edit {
            span,
            replacement: literal.into(),
        };
        let candidate = decode(
            edit.apply(&self.syntax.source).as_bytes(),
            &[self.schema],
            limits,
        )?;
        let (changed, new) = lookup(&candidate.index, &candidate.value, path)
            .ok_or_else(|| error(ErrorKind::Invalid("edit changed path"), span))?;
        let supplied = Span {
            start: span.start,
            end: span.start
                + u32::try_from(literal.len())
                    .map_err(|_| error(ErrorKind::Limit("input bytes"), span))?,
        };
        if changed.span != Some(supplied) {
            return Err(error(
                ErrorKind::Invalid("expected one complete replacement literal"),
                span,
            ));
        }
        if old == new {
            return Ok(None);
        }
        if self
            .syntax
            .tree
            .tokens
            .iter()
            .any(|t| t.kind.is_trivia() && t.span.start >= span.start && t.span.end <= span.end)
        {
            return Err(error(
                ErrorKind::Invalid("replacement would remove comments"),
                span,
            ));
        }
        Ok(Some(edit))
    }

    /// Add an absent optional/default field or a map entry. Sorted maps stay
    /// sorted. In other collections append after the final written sibling.
    pub fn insert(
        &self,
        path: &[PathPart],
        part: PathPart,
        literal: &str,
        limits: Limits,
    ) -> Result<Option<Edit>, DataError> {
        let (index, _) = lookup(&self.index, &self.value, path)
            .ok_or_else(|| error(ErrorKind::Invalid("unknown edit path"), Span::default()))?;
        let span = index.span.unwrap();
        if index.children.iter().any(|(p, _)| p == &part) {
            let mut target = path.to_vec();
            target.push(part);
            return self.replace(&target, literal, limits);
        }
        let item = self.syntax.item(0);
        let node = item
            .range
            .nodes
            .clone()
            .map(|i| Site(item, NodeId(i)))
            .find(|s| s.span() == span && matches!(s.kind(), NodeKind::Array | NodeKind::Call))
            .ok_or_else(|| error(ErrorKind::Invalid("expected editable collection"), span))?;
        let mut entries = node.kids();
        if node.kind() == NodeKind::Call {
            entries.next();
        }
        let siblings: Vec<_> = entries.collect();
        let (entry, position) = match &part {
            PathPart::Key(key) if index.map => {
                let keys: Vec<_> = index
                    .children
                    .iter()
                    .filter_map(|(p, _)| {
                        if let PathPart::Key(k) = p {
                            Some(k)
                        } else {
                            None
                        }
                    })
                    .collect();
                let pos = if keys.is_sorted() {
                    keys.partition_point(|k| *k < key)
                } else {
                    keys.len()
                };
                let quoted = quote(key);
                (format!("({quoted}, {literal})"), pos)
            }
            PathPart::Field(name)
                if index.record && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_') =>
            {
                (format!("{name}: {literal}"), siblings.len())
            }
            _ => return Err(node.error("invalid insertion path")),
        };
        let source = &self.syntax.source;
        let close = span.end - 1;
        let (at, replacement) = if let Some(next) = siblings.get(position) {
            let at = next.span().start;
            let prev = position
                .checked_sub(1)
                .map_or(span.start, |i| siblings[i].span().end);
            if self.has_comment(prev, at) {
                return Err(node.error("ambiguous comment attachment"));
            }
            let indent = source[..at as usize]
                .rsplit_once('\n')
                .map(|(_, s)| s)
                .filter(|s| s.trim().is_empty());
            (
                at,
                match indent {
                    Some(indent) => format!("{entry},\n{indent}"),
                    None => format!("{entry}, "),
                },
            )
        } else {
            let last = siblings.last().map(|n| n.span().end);
            let prev = last.unwrap_or(span.start);
            if self.has_comment(prev, close) {
                return Err(node.error("ambiguous comment attachment"));
            }
            let at = last.unwrap_or(close);
            let multiline = source[prev as usize..close as usize].contains('\n');
            let indent = siblings
                .last()
                .and_then(|n| source[..n.span().start as usize].rsplit_once('\n'))
                .map(|(_, s)| s)
                .filter(|s| s.trim().is_empty())
                .unwrap_or("");
            let separator = if last.is_none() {
                String::new()
            } else if multiline {
                format!(",\n{indent}")
            } else {
                ", ".into()
            };
            // The existing trailing comma follows the inserted entry, unchanged.
            (at, format!("{separator}{entry}"))
        };
        let edit = Edit {
            span: Span { start: at, end: at },
            replacement,
        };
        let candidate = decode(edit.apply(source).as_bytes(), &[self.schema], limits)?;
        let (parent, _) = lookup(&candidate.index, &candidate.value, path)
            .ok_or_else(|| node.error("invalid insertion"))?;
        let mut expected: Vec<_> = index.children.iter().map(|(p, _)| p).collect();
        expected.insert(position, &part);
        if !parent.children.iter().map(|(p, _)| p).eq(expected) {
            return Err(node.error("insertion changed siblings"));
        }
        let inserted = &parent.children[position].1;
        let inner = inserted.span.unwrap();
        if &candidate.syntax.source[inner.start as usize..inner.end as usize] != literal {
            return Err(node.error("expected one complete inserted literal"));
        }
        if let PathPart::Field(name) = part {
            let (_, before) = lookup(&self.index, &self.value, path).unwrap();
            let (_, after) = lookup(&candidate.index, &candidate.value, path).unwrap();
            if before == after && before.get(&name).is_some() {
                return Ok(None);
            }
        }
        Ok(Some(edit))
    }
    fn has_comment(&self, start: u32, end: u32) -> bool {
        self.syntax
            .tree
            .tokens
            .iter()
            .any(|t| t.kind.is_trivia() && t.span.start >= start && t.span.end <= end)
    }
}
fn quote(s: &str) -> String {
    use std::fmt::Write;
    let mut out = String::from("\"");
    for ch in s.chars() {
        match ch {
            '"' | '\\' => {
                out.push('\\');
                out.push(ch)
            }
            c if c.is_control()
                || matches!(c,'\u{61c}'|'\u{200e}'|'\u{200f}'|'\u{202a}'..='\u{202e}'|'\u{2066}'..='\u{2069}') =>
            {
                write!(out, "\\u{{{:x}}}", c as u32).unwrap()
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}
