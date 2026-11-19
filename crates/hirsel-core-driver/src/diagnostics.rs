//! Tool encoding of PROG-7 sites. Cached item-local anchors bind to current file bytes.
use super::*;
use hirsel_core_syntax::Span;

#[derive(Serialize)]
pub struct SourceSite {
    /// The file: its path relative to the request's manifest, or without a
    /// manifest its module (`app@1/main`).
    pub module: String,
    pub span: Span,
}
#[derive(Serialize)]
pub struct Diagnostic<'a> {
    pub rule: &'a str,
    pub primary: SourceSite,
    pub causal: Vec<SourceSite>,
    pub declared: Option<&'a str>,
    pub required: Option<&'a str>,
    pub message: &'a str,
}
#[derive(Serialize)]
pub struct IncompleteDiagnostic<'a> {
    pub primary: SourceSite,
    pub causal: Vec<SourceSite>,
    pub message: &'a str,
}
/// A resource refusal (PROG-6) names no rule: only its site and the limit it passes.
#[derive(Serialize)]
pub struct ResourceDiagnostic<'a> {
    pub primary: SourceSite,
    pub message: &'a str,
}
impl CheckProduct {
    pub(crate) fn source_site(&self, site: &Site) -> io::Result<SourceSite> {
        hirsel_core_ids::published::catch_io(|| self.source_site_inner(site))
    }
    fn source_site_inner(&self, site: &Site) -> io::Result<SourceSite> {
        let own = site
            .source
            .and_then(|source| self.files.iter().find(|f| f.file == source));
        let (base, name) = if let Some(file) = own {
            let base = file
                .syntax
                .items
                .get(site.item as usize)
                .map_or(0, |i| i.file_offset);
            (base, file.name.clone())
        } else if let (Some(image), Some(at)) = (&self.image, item_at(site)) {
            (image.file_offset(at)?, image.source_name(at.0)?)
        } else {
            (0, None)
        };
        let id = self.index.module_path(site.module);
        Ok(SourceSite {
            module: name.unwrap_or_else(|| {
                format!(
                    "{}@{}/{}",
                    id.package.name,
                    id.package.version,
                    id.modules.join("/")
                )
            }),
            span: Span {
                start: base + site.span.start,
                end: base + site.span.end,
            },
        })
    }
    pub fn machine_diagnostics(&self) -> io::Result<Vec<Diagnostic<'_>>> {
        self.diagnostics_for(self.diagnostics.iter())
    }
    pub fn diagnostics_for<'a>(
        &self,
        rows: impl IntoIterator<Item = &'a Refusal>,
    ) -> io::Result<Vec<Diagnostic<'a>>> {
        rows.into_iter()
            .map(|r| {
                Ok(Diagnostic {
                    rule: &r.rule,
                    primary: self.source_site(&r.primary)?,
                    causal: r
                        .causal
                        .iter()
                        .map(|s| self.source_site(s))
                        .collect::<io::Result<_>>()?,
                    declared: r.declared.as_deref(),
                    required: r.required.as_deref(),
                    message: &r.message,
                })
            })
            .collect()
    }
    pub fn machine_incomplete(&self) -> io::Result<Vec<IncompleteDiagnostic<'_>>> {
        self.incomplete_for(self.incomplete.iter())
    }
    pub fn machine_resources(&self) -> io::Result<Vec<ResourceDiagnostic<'_>>> {
        self.resources_for(self.resources.iter())
    }
    pub fn resources_for<'a>(
        &self,
        rows: impl IntoIterator<Item = &'a ResourceRefusal>,
    ) -> io::Result<Vec<ResourceDiagnostic<'a>>> {
        rows.into_iter()
            .map(|r| {
                Ok(ResourceDiagnostic {
                    primary: self.source_site(&r.primary)?,
                    message: &r.message,
                })
            })
            .collect()
    }
    pub fn incomplete_for<'a>(
        &self,
        rows: impl IntoIterator<Item = &'a Incomplete>,
    ) -> io::Result<Vec<IncompleteDiagnostic<'a>>> {
        rows.into_iter()
            .map(|g| {
                Ok(IncompleteDiagnostic {
                    primary: self.source_site(&g.primary)?,
                    causal: g
                        .causal
                        .iter()
                        .map(|s| self.source_site(s))
                        .collect::<io::Result<_>>()?,
                    message: &g.message,
                })
            })
            .collect()
    }
}

/// The terminal form of PROG-7 sites: `path:line:column`, from the request's
/// own source files. A site whose file the request did not read (an image's
/// library source) keeps its module and byte span.
pub struct SourceFiles(Vec<(String, PathBuf)>);
impl SourceFiles {
    /// Name each request file as `source_site` names it: by its path relative
    /// to the manifest, or without a manifest by its module.
    pub fn of(request: &CheckRequest) -> Self {
        let mut files = Vec::new();
        for package in &request.inputs.packages {
            for source in &package.sources {
                let name = match &request.manifest {
                    Some(root) => match hirsel_core_resolve::manifest_name(root, &source.file) {
                        Ok(name) => name,
                        Err(_) => continue,
                    },
                    None => format!(
                        "{}@{}/{}",
                        package.package.name,
                        package.package.version,
                        source.modules.join("/")
                    ),
                };
                files.push((name, source.file.clone()));
            }
        }
        Self(files)
    }
    pub fn none() -> Self {
        Self(Vec::new())
    }
    /// `path:line:column`, with `-line:column` for the span's end when it
    /// differs. The bytes are read now, so a file changed since the check
    /// shows the current text's positions.
    pub fn render(&self, site: &SourceSite) -> String {
        let found = self
            .0
            .iter()
            .find(|(name, _)| *name == site.module)
            .and_then(|(_, path)| Some((path, std::fs::read(path).ok()?)));
        let Some((path, text)) = found else {
            return format!("{} {}..{}", site.module, site.span.start, site.span.end);
        };
        let start = line_column(&text, site.span.start as usize);
        let end = line_column(&text, site.span.end as usize);
        if site.span.end <= site.span.start || start == end {
            format!("{}:{}:{}", path.display(), start.0, start.1)
        } else if start.0 == end.0 {
            format!("{}:{}:{}-{}", path.display(), start.0, start.1, end.1)
        } else {
            format!(
                "{}:{}:{}-{}:{}",
                path.display(),
                start.0,
                start.1,
                end.0,
                end.1
            )
        }
    }
}

/// The 1-based line and column of byte `offset` (docs/spec/diagnostics.md,
/// "Terminal sites"). Lines end at LF; a CR immediately before that LF belongs
/// to the line end, not to a column. Columns count Unicode scalar values: every
/// byte except a UTF-8 continuation byte starts one, so an invalid byte counts
/// as one column.
pub fn line_column(text: &[u8], offset: usize) -> (usize, usize) {
    let prefix = &text[..offset.min(text.len())];
    let line_start = prefix
        .iter()
        .rposition(|&b| b == b'\n')
        .map_or(0, |at| at + 1);
    let line = 1 + prefix.iter().filter(|&&b| b == b'\n').count();
    let mut columns = &prefix[line_start..];
    if columns.last() == Some(&b'\r') && text.get(prefix.len()) == Some(&b'\n') {
        columns = &columns[..columns.len() - 1];
    }
    let column = 1 + columns.iter().filter(|&&b| b & 0xc0 != 0x80).count();
    (line, column)
}
