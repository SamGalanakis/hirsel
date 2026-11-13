//! Tool encoding of PROG-7 sites. Cached item-local anchors bind to current file bytes.
use super::*;
use hirsel_core_syntax::Span;

#[derive(Serialize)]
pub struct SourceSite {
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
    pub message: &'a str,
}
impl CheckProduct {
    pub(crate) fn source_site(&self, site: &Site) -> io::Result<SourceSite> {
        let file = if let Some(file) = self.files.iter().find(|f| f.module == site.module) {
            Some(Arc::clone(file))
        } else if let Some(image) = &self.image {
            image
                .directory
                .files
                .iter()
                .position(|f| f.module == site.module)
                .map(|i| image.file(i))
                .transpose()?
        } else {
            None
        };
        let base = file
            .as_ref()
            .and_then(|f| f.syntax.items.get(site.item as usize))
            .map_or(0, |i| i.file_offset);
        let id = self.index.module_path(site.module);
        Ok(SourceSite {
            module: format!(
                "{}@{}/{}",
                id.package.name,
                id.package.version,
                id.modules.join("/")
            ),
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
    pub fn incomplete_for<'a>(
        &self,
        rows: impl IntoIterator<Item = &'a Incomplete>,
    ) -> io::Result<Vec<IncompleteDiagnostic<'a>>> {
        rows.into_iter()
            .map(|g| {
                Ok(IncompleteDiagnostic {
                    primary: self.source_site(&g.primary)?,
                    message: &g.message,
                })
            })
            .collect()
    }
}
