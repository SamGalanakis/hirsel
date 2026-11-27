use super::diagnostics::uri;
use super::documents::{Encoding, Position};
use super::navigation::{Location, range};
use super::snapshot::EditorSnapshot;
use hirsel_core_ids::{DefPath, PackageId};
use hirsel_core_syntax::Span;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(super) enum Key {
    Definition(DefPath, Option<PathBuf>),
    Binder(PathBuf, u32, u32),
    Member(DefPath, PathBuf, String),
    Alias(PathBuf, PackageId, String),
    Module(PathBuf, PackageId, Vec<String>),
    Builtin(String),
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(super) struct Occurrence {
    pub(super) path: PathBuf,
    pub(super) start: u32,
    pub(super) end: u32,
    pub(super) declaration: bool,
}

impl Occurrence {
    pub(super) fn span(&self) -> Span {
        Span {
            start: self.start,
            end: self.end,
        }
    }
}

#[derive(Default)]
pub(super) struct References {
    by_key: BTreeMap<Key, BTreeSet<Occurrence>>,
    by_path: BTreeMap<PathBuf, BTreeMap<(u32, u32), BTreeSet<Key>>>,
}

impl References {
    pub(super) fn use_site(&mut self, key: Key, path: &Path, span: Span) {
        self.record(key, path, span, false);
    }

    pub(super) fn declaration(&mut self, key: Key, location: &Location) {
        self.record(key, &location.path, location.selection, true);
    }

    fn record(&mut self, key: Key, path: &Path, span: Span, declaration: bool) {
        if span.is_empty() {
            return;
        }
        self.by_path
            .entry(path.to_owned())
            .or_default()
            .entry((span.start, span.end))
            .or_default()
            .insert(key.clone());
        self.by_key.entry(key).or_default().insert(Occurrence {
            path: path.to_owned(),
            start: span.start,
            end: span.end,
            declaration,
        });
    }

    pub(super) fn at(&self, path: &Path, offset: u32) -> Option<(&Key, Span)> {
        let ((start, end), keys) = self
            .by_path
            .get(path)?
            .range(..=(offset, u32::MAX))
            .rev()
            .find(|((_, end), _)| offset < *end)?;
        if keys.len() != 1 {
            return None;
        }
        Some((
            keys.first()?,
            Span {
                start: *start,
                end: *end,
            },
        ))
    }

    pub(super) fn occurrences(&self, key: &Key) -> impl Iterator<Item = &Occurrence> {
        self.by_key.get(key).into_iter().flatten()
    }
}

impl EditorSnapshot {
    pub(super) fn references(
        &self,
        path: &Path,
        position: Position,
        include_declaration: bool,
        encoding: Encoding,
    ) -> Option<Value> {
        let offset = self.offset(path, position, encoding)?;
        let (key, _) = self.references.at(path, offset)?;
        let locations = self
            .references
            .occurrences(key)
            .filter(|site| include_declaration || !site.declaration)
            .filter_map(|site| {
                Some(json!({
                    "uri": uri(&site.path, &self.inputs.overlay),
                    "range": range(self.origins.get(&site.path)?, site.span(), encoding)?,
                }))
            })
            .collect::<Vec<_>>();
        Some(json!(locations))
    }
}
