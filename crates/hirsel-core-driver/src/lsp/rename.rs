use super::completion::Callable;
use super::diagnostics::uri;
use super::documents::{Encoding, Position};
use super::navigation::range;
use super::references::Key;
use super::snapshot::EditorSnapshot;
use crate::FileProduct;
use hirsel_core_resolve::{NameIndex, Resolution, ResolutionScratch};
use hirsel_core_syntax::{ParseMode, Span, Symbols, SyntaxFile, TokenKind, lex, parse_bytes};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

pub(super) type Edits = BTreeMap<PathBuf, Vec<Edit>>;

pub(super) struct Edit {
    pub(super) span: Span,
    pub(super) text: String,
}

pub(super) struct Graph {
    pub(super) index: Arc<NameIndex>,
    pub(super) files: Vec<Arc<FileProduct>>,
    pub(super) paths: BTreeMap<hirsel_core_resolve::SourceId, PathBuf>,
    pub(super) resolutions: Vec<Vec<Arc<Resolution>>>,
    pub(super) complete: bool,
    pub(super) callables: BTreeMap<hirsel_core_ids::DefPath, Callable>,
    pub(super) field_types: super::cache::FieldTypes,
}

impl Graph {
    fn validates(&self, changed: &BTreeMap<PathBuf, String>) -> bool {
        if !self.paths.values().any(|path| changed.contains_key(path)) {
            return true;
        }
        if !self.complete || self.index.definitions.base_len() != 0 {
            return false;
        }
        let mut symbols = self.index.symbols.as_ref().clone();
        let mut parsed = BTreeMap::<_, SyntaxFile>::new();
        for file in &self.files {
            let Some(source) = self
                .paths
                .get(&file.file)
                .and_then(|path| changed.get(path))
            else {
                continue;
            };
            // A rename would need to regenerate the source-derived declarations.
            if !file.syntax.generated_origins.is_empty() {
                return false;
            }
            let mode = if self.index.module_is_foreign(file.module) {
                ParseMode::Foreign
            } else {
                ParseMode::Source
            };
            let output = parse_bytes(source.as_bytes(), mode, &mut symbols);
            if output.resource_refusal.is_some()
                || !output.diagnostics.is_empty()
                || output.syntax.items.len() != file.syntax.items.len()
            {
                return false;
            }
            parsed.insert(file.file, output.syntax);
        }
        let files = self.files.iter().map(|file| {
            let syntax = parsed.get(&file.file).unwrap_or(&file.syntax);
            (file.module, file.file, syntax)
        });
        let mut index = self.index.as_ref().clone();
        index.symbols = Arc::new(symbols);
        index.clear_editor_headers();
        for (module, source, syntax) in files.clone() {
            for (ordinal, item) in syntax.items().enumerate() {
                index.index_item(module, source, ordinal as u32, item);
            }
        }
        if let Some(core) = self.index.core {
            index.configure_core(core);
        }
        let identities = |def: &hirsel_core_resolve::Definition| {
            (def.site.module, def.site.source, def.site.item, def.kind)
        };
        if !index.diagnostics.is_empty()
            || !index
                .definitions()
                .map(|(_, def)| identities(def))
                .eq(self.index.definitions().map(|(_, def)| identities(def)))
        {
            return false;
        }
        let mut scratch = ResolutionScratch::default();
        files
            .zip(&self.resolutions)
            .all(|((module, source, syntax), before)| {
                syntax
                    .items()
                    .zip(before)
                    .enumerate()
                    .all(|(ordinal, (item, before))| {
                        let after = scratch.resolve(&index, module, source, ordinal as u32, item);
                        after.resource_refusal.is_none()
                            && after.diagnostics.is_empty()
                            && before
                                .binders
                                .iter()
                                .map(|b| (b.id, b.kind))
                                .eq(after.binders.iter().map(|b| (b.id, b.kind)))
                            && before
                                .names
                                .iter()
                                .map(|u| (u.node, u.target, u.projections))
                                .eq(after
                                    .names
                                    .iter()
                                    .map(|u| (u.node, u.target, u.projections)))
                    })
            })
    }
}

impl EditorSnapshot {
    pub(super) fn prepare_rename(
        &self,
        path: &Path,
        position: Position,
        encoding: Encoding,
    ) -> Option<Value> {
        let (_, span, name) = self.rename_target(path, position, encoding)?;
        Some(json!({"range": range(self.origins.get(path)?, span, encoding)?, "placeholder": name}))
    }

    pub(super) fn rename(
        &self,
        path: &Path,
        position: Position,
        new_name: &str,
        encoding: Encoding,
    ) -> Result<Value, &'static str> {
        if !identifier(new_name) {
            return Err("The new name must be a Hirsel identifier");
        }
        let (key, _, original) = self
            .rename_target(path, position, encoding)
            .ok_or("Rename requires a writable, unambiguous binding and complete scope")?;
        let mut edits = Edits::new();
        for site in self.references.occurrences(key) {
            let source = self
                .origins
                .get(&site.path)
                .ok_or("Missing source for rename")?;
            if source.bytes.get(site.start as usize..site.end as usize) != Some(original.as_bytes())
            {
                return Err("Rename would edit a differently spelled occurrence");
            }
            edits.entry(site.path.clone()).or_default().push(Edit {
                span: site.span(),
                text: new_name.to_owned(),
            });
        }
        let mut changed = BTreeMap::new();
        for (path, edits) in &edits {
            let source = self
                .editable(path)
                .ok_or("Rename source changed or is read-only")?;
            let mut source = std::str::from_utf8(&source.bytes)
                .map_err(|_| "Rename source is invalid UTF-8")?
                .to_owned();
            for edit in edits.iter().rev() {
                source.replace_range(edit.span.start as usize..edit.span.end as usize, &edit.text);
            }
            changed.insert(path.clone(), source);
        }
        if !self.graphs.iter().all(|graph| graph.validates(&changed)) {
            return Err("Rename would change name resolution or shadow an active binding");
        }
        self.workspace_edit(&edits, encoding)
            .ok_or("Rename source changed")
    }

    fn rename_target(
        &self,
        path: &Path,
        position: Position,
        encoding: Encoding,
    ) -> Option<(&Key, Span, &str)> {
        let (key, span) = self
            .references
            .at(path, self.offset(path, position, encoding)?)?;
        let declaration = self
            .references
            .occurrences(key)
            .find(|site| site.declaration)?;
        self.editable(&declaration.path)?;
        let supported = match key {
            Key::Definition(def, _) => !matches!(def.0.name.as_str(), "main" | "_"),
            Key::Binder(..) => true,
            _ => false,
        };
        if !supported
            || !self.complete_workspace(&declaration.path)
            || self
                .references
                .occurrences(key)
                .any(|site| self.editable(&site.path).is_none())
            || self.graphs.iter().any(|graph| {
                graph.paths.values().any(|path| path == &declaration.path) && !graph.complete
            })
        {
            return None;
        }
        let written = self
            .origins
            .get(path)?
            .bytes
            .get(span.start as usize..span.end as usize)?;
        let written = std::str::from_utf8(written)
            .ok()
            .filter(|name| identifier(name))?;
        Some((key, span, written))
    }

    pub(super) fn workspace_edit(&self, edits: &Edits, encoding: Encoding) -> Option<Value> {
        let mut changes = Vec::new();
        for (path, edits) in edits {
            let text = self.editable(path)?;
            let version = self.inputs.overlay.get(path).map(|doc| doc.version);
            let mut ordered = edits.iter().collect::<Vec<_>>();
            ordered.sort_by_key(|edit| (edit.span.start, edit.span.end));
            if ordered.windows(2).any(|pair| {
                pair[0].span.end > pair[1].span.start || pair[0].span.start == pair[1].span.start
            }) {
                return None;
            }
            let encoded = ordered
                .into_iter()
                .map(|edit| {
                    Some(json!({
                        "range": range(text, edit.span, encoding)?, "newText": edit.text,
                    }))
                })
                .collect::<Option<Vec<_>>>()?;
            changes.push(json!({
                "textDocument": {"uri": uri(path, &self.inputs.overlay), "version": version},
                "edits": encoded,
            }));
        }
        Some(json!({"documentChanges": changes}))
    }
}

fn identifier(name: &str) -> bool {
    if name == "_" {
        return false;
    }
    let lexed = lex(name, &mut Symbols::default());
    lexed.diagnostics.is_empty()
        && matches!(lexed.tokens.as_slice(), [token, eof]
            if token.kind == TokenKind::Ident && token.span.start == 0
                && token.span.end as usize == name.len() && eof.kind == TokenKind::Eof)
}
