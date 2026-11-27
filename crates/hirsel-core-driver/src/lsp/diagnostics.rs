use super::documents::{Encoding, Text, file_uri};
use crate::{CheckProduct, Site};
use hirsel_core_syntax::{ParseMode, Span, Symbols, parse_bytes};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

pub(super) struct Origin {
    pub(super) path: PathBuf,
    pub(super) text: Text,
}

#[derive(Clone)]
struct Location {
    path: PathBuf,
    span: Span,
}

pub(super) struct Diagnostic {
    primary: Location,
    code: Option<String>,
    message: String,
    causes: Vec<(Location, &'static str)>,
}

impl Diagnostic {
    pub(super) fn configuration(path: &Path, message: String) -> Self {
        Self {
            primary: Location {
                path: path.to_owned(),
                span: Span::default(),
            },
            code: None,
            message,
            causes: Vec::new(),
        }
    }

    pub(super) fn path(&self) -> &Path {
        &self.primary.path
    }

    pub(super) fn lsp(
        &self,
        origins: &BTreeMap<PathBuf, Text>,
        encoding: Encoding,
        uri: impl Fn(&Path) -> String,
    ) -> Option<Value> {
        let mut diagnostic = json!({
            "range": self.primary.range(origins, encoding)?,
            "severity": 1,
            "source": "hirsel",
            "message": self.message,
        });
        if let Some(code) = &self.code {
            diagnostic["code"] = json!(code);
            if let Some(rule) = hirsel_core_diag::rule(code) {
                diagnostic["codeDescription"] =
                    json!({"href": crate::installation::spec_url(rule.spec_file, rule.anchor)});
            }
        }
        if !self.causes.is_empty() {
            diagnostic["relatedInformation"] = Value::Array(
                self.causes
                    .iter()
                    .filter_map(|(site, role)| Some(json!({
                        "location": {"uri": uri(&site.path), "range": site.range(origins, encoding)?},
                        "message": role,
                    })))
                    .collect(),
            );
        }
        Some(diagnostic)
    }
}

impl Location {
    fn range(&self, origins: &BTreeMap<PathBuf, Text>, encoding: Encoding) -> Option<Value> {
        if let Some(text) = origins.get(&self.path) {
            Some(json!({
                "start": text.position(self.span.start, encoding)?,
                "end": text.position(self.span.end, encoding)?,
            }))
        } else if self.span == Span::default() {
            let origin = json!({"line": 0, "character": 0});
            Some(json!({"start": origin, "end": origin}))
        } else {
            None
        }
    }
}

pub(super) type InputOrigins = BTreeMap<(hirsel_core_ids::PackageId, String), Origin>;

pub(super) fn checked(
    product: &CheckProduct,
    origins: &InputOrigins,
    wanted: &BTreeSet<PathBuf>,
    immutable: &BTreeSet<PathBuf>,
) -> Vec<Diagnostic> {
    let by_source: BTreeMap<_, _> = product
        .files
        .iter()
        .filter_map(|file| {
            let package = product.index.module_path(file.module).package;
            let origin = origins.get(&(package, file.name.clone()?))?;
            if immutable.contains(&origin.path) {
                return None;
            }
            Some((file.file, origin))
        })
        .collect();
    let locate = |site: &Site| {
        let origin = by_source.get(&site.source?)?;
        let span = product.source_site(site).ok()?.span;
        (span.end as usize <= origin.text.bytes.len()).then(|| Location {
            path: origin.path.clone(),
            span,
        })
    };
    let primary = |site: &Site| locate(site).filter(|location| wanted.contains(&location.path));
    let mut diagnostics = Vec::new();
    diagnostics.extend(product.diagnostics.iter().filter_map(|refusal| {
        Some(Diagnostic {
            primary: primary(&refusal.primary)?,
            code: Some(refusal.rule.clone()),
            message: refusal.message.clone(),
            causes: refusal
                .causal
                .iter()
                .filter_map(|cause| Some((locate(&cause.site)?, cause.role.label())))
                .collect(),
        })
    }));
    diagnostics.extend(product.incomplete.iter().filter_map(|gap| {
        Some(Diagnostic {
            primary: primary(&gap.primary)?,
            code: None,
            message: format!("Incomplete: {}", gap.message),
            causes: gap
                .causal
                .iter()
                .filter_map(|site| Some((locate(site)?, "cause")))
                .collect(),
        })
    }));
    diagnostics.extend(product.resources.iter().filter_map(|refusal| {
        Some(Diagnostic {
            primary: primary(&refusal.primary)?,
            code: None,
            message: refusal.message.clone(),
            causes: Vec::new(),
        })
    }));
    diagnostics
}

pub(super) fn syntax(path: &Path, text: &Text, mode: ParseMode) -> Vec<Diagnostic> {
    let output = parse_bytes(&text.bytes, mode, &mut Symbols::default());
    let location = |span| Location {
        path: path.to_owned(),
        span,
    };
    let mut diagnostics: Vec<_> = output
        .diagnostics
        .into_iter()
        .map(|error| Diagnostic {
            primary: location(error.primary),
            code: Some(error.rule),
            message: error.message,
            causes: error
                .causal
                .into_iter()
                .map(|span| (location(span), "cause"))
                .collect(),
        })
        .collect();
    if let Some(refusal) = output.resource_refusal {
        diagnostics.push(Diagnostic {
            primary: location(refusal.site()),
            code: None,
            message: format!("parser resource refusal: {refusal}"),
            causes: Vec::new(),
        });
    }
    diagnostics
}

pub(super) fn uri(path: &Path, overlay: &super::documents::SourceOverlay) -> String {
    overlay
        .get(path)
        .map_or_else(|| file_uri(path), |document| document.uri.clone())
}
