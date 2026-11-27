use super::diagnostics::{InputOrigins, uri};
use super::documents::Encoding;
use super::navigation::range;
use super::rename::{Edit, Edits};
use super::snapshot::EditorSnapshot;
use crate::CheckProduct;
use crate::diagnostics::{Applicability, source_name};
use hirsel_core_syntax::Span;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

pub(super) struct Offer {
    primary: Span,
    code: String,
    message: String,
    description: Option<String>,
    applicability: Applicability,
    edits: Edits,
}

pub(super) fn collect(
    product: &CheckProduct,
    origins: &InputOrigins,
    wanted: &BTreeSet<PathBuf>,
    immutable: &BTreeSet<PathBuf>,
    offers: &mut BTreeMap<PathBuf, Vec<Offer>>,
) {
    let mut names = BTreeMap::new();
    let mut sources = BTreeMap::new();
    for file in &product.files {
        let module = product.index.module_path(file.module);
        let name = source_name(file.name.as_deref(), &module);
        let origin = origins.get(&(module.package, name.clone()));
        names
            .entry(name)
            .and_modify(|entry| *entry = None)
            .or_insert(origin);
        if let Some(origin) = origin {
            sources.insert(file.file, origin);
        }
    }
    for refusal in &product.diagnostics {
        let Some(origin) = refusal
            .primary
            .source
            .and_then(|source| sources.get(&source))
        else {
            continue;
        };
        if !wanted.contains(&origin.path) || immutable.contains(&origin.path) {
            continue;
        }
        let Ok(diagnostics) = product.diagnostics_for([refusal]) else {
            continue;
        };
        for diagnostic in diagnostics {
            let primary = diagnostic.primary.span;
            if !fits(origin.text.bytes.as_ref(), primary) {
                continue;
            }
            for fix in diagnostic.fixes {
                let description = fix.signature.or_else(|| {
                    let [edit] = fix.edits.as_slice() else {
                        return None;
                    };
                    (!edit.text.is_empty() && !edit.text.contains('\n'))
                        .then(|| format!("{:?}", edit.text))
                });
                let mut edits = Edits::new();
                let complete = fix.edits.into_iter().all(|edit| {
                    let Some(target) = names.get(&edit.module).copied().flatten() else {
                        return false;
                    };
                    if immutable.contains(&target.path) || !fits(&target.text.bytes, edit.span) {
                        return false;
                    }
                    edits.entry(target.path.clone()).or_default().push(Edit {
                        span: edit.span,
                        text: edit.text,
                    });
                    true
                });
                if complete && !edits.is_empty() {
                    offers.entry(origin.path.clone()).or_default().push(Offer {
                        primary,
                        code: diagnostic.rule.to_owned(),
                        message: diagnostic.message.to_owned(),
                        description,
                        applicability: fix.applicability,
                        edits,
                    });
                }
            }
        }
    }
}

fn fits(bytes: &[u8], span: Span) -> bool {
    std::str::from_utf8(bytes)
        .is_ok_and(|text| text.get(span.start as usize..span.end as usize).is_some())
}

impl EditorSnapshot {
    pub(super) fn code_actions(
        &self,
        path: &Path,
        requested: Span,
        context: &Value,
        encoding: Encoding,
    ) -> Vec<Value> {
        if context["only"]
            .as_array()
            .is_some_and(|kinds| !kinds.iter().any(|kind| kind == "quickfix" || kind == ""))
        {
            return Vec::new();
        }
        let requested_diagnostics = context["diagnostics"].as_array();
        self.fixes
            .get(path)
            .into_iter()
            .flatten()
            .filter_map(|offer| {
                if offer.primary.start > requested.end || requested.start > offer.primary.end {
                    return None;
                }
                let primary = range(self.origins.get(path)?, offer.primary, encoding)?;
                let mut diagnostic = self
                    .diagnostics
                    .iter()
                    .filter(|d| d.path() == path)
                    .filter_map(|d| {
                        d.lsp(&self.origins, encoding, |path| {
                            uri(path, &self.inputs.overlay)
                        })
                    })
                    .find(|d| {
                        d["range"] == primary
                            && d["code"] == offer.code
                            && d["message"] == offer.message
                    })?;
                diagnostic["data"] = json!({"generation": self.inputs.generation});
                if requested_diagnostics.is_some_and(|rows| {
                    !rows.is_empty()
                        && !rows.iter().any(|row| {
                            ["data", "range", "code", "message"]
                                .iter()
                                .all(|key| row[key] == diagnostic[key])
                        })
                }) {
                    return None;
                }
                let preferred = offer.applicability == Applicability::Safe
                    && offer
                        .edits
                        .keys()
                        .all(|path| self.inputs.overlay.contains_key(path));
                let title = match offer.applicability {
                    Applicability::Safe => "Apply fix",
                    Applicability::Maybe => "Review suggested fix",
                    Applicability::Placeholder => "Insert placeholder",
                };
                Some(json!({
                    "title": match &offer.description {
                    Some(description) => format!("{title} ({description}) for {}", offer.code),
                    None => format!("{title} for {}", offer.code),
                },
                    "kind": "quickfix", "diagnostics": [diagnostic], "isPreferred": preferred,
                    "data": {"applicability": offer.applicability},
                    "edit": self.workspace_edit(&offer.edits, encoding)?,
                }))
            })
            .collect()
    }
}
