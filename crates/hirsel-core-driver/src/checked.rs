use crate::{
    CheckProduct, CheckRequest, ContentHash, CorpusManifest, Meter, ModuleInput, Site,
    reachability, request_record,
};
use hirsel_core_ids::DefPath;
use std::io;
use std::sync::Arc;

/// A request's check: its product with the key of its request record, or
/// the record's answer.
pub(crate) enum Checked {
    Product(Arc<CheckProduct>, Option<ContentHash>),
    Hit(request_record::Hit),
}
impl Checked {
    pub(super) fn product(self) -> io::Result<Arc<CheckProduct>> {
        match self {
            Self::Product(product, _) => Ok(product),
            Self::Hit(_) => Err(io::Error::other(
                "internal error: a request record answered a request that names none",
            )),
        }
    }
}

/// A check that refused after producing ordinary source diagnostics.
pub(crate) struct RejectedCheck {
    pub(crate) product: Arc<CheckProduct>,
    error: io::Error,
}

impl RejectedCheck {
    pub(crate) fn with_product(product: CheckProduct, error: io::Error) -> io::Error {
        if product.accepted() {
            error
        } else {
            io::Error::new(
                error.kind(),
                Self {
                    product: Arc::new(product),
                    error,
                },
            )
        }
    }
}

impl std::fmt::Debug for RejectedCheck {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RejectedCheck")
            .field("error", &self.error)
            .finish_non_exhaustive()
    }
}

impl std::fmt::Display for RejectedCheck {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(&self.error, f)
    }
}

impl std::error::Error for RejectedCheck {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.error)
    }
}

impl Driver {
    pub fn check(
        &mut self,
        request: CheckRequest,
        meter: &mut Meter,
    ) -> io::Result<Arc<CheckProduct>> {
        self.check_recorded(request, meter, None)?.product()
    }
    /// Check `request` and seal PACK-14/15 export adapters for the named closed `exports`.
    pub fn check_pack_exports(
        &mut self,
        request: CheckRequest,
        exports: &[DefPath],
        meter: &mut Meter,
    ) -> io::Result<Arc<CheckProduct>> {
        hirsel_core_ids::published::catch_io(|| {
            self.check_inner::<false>(request, meter, None, false, exports, None)?
                .product()
        })
    }
    /// Check `request`, answered from its request record when `record` names
    /// one that holds its answer.
    pub(crate) fn check_recorded(
        &mut self,
        request: CheckRequest,
        meter: &mut Meter,
        record: Option<&request_record::Request>,
    ) -> io::Result<Checked> {
        if meter.enabled() {
            hirsel_core_ids::published::catch_io(|| {
                self.check_inner::<true>(request, meter, record, false, &[], None)
            })
        } else {
            hirsel_core_ids::published::catch_io(|| {
                self.check_inner::<false>(request, meter, record, false, &[], None)
            })
        }
    }
    pub(crate) fn check_for_authority(
        &mut self,
        request: CheckRequest,
    ) -> io::Result<Arc<CheckProduct>> {
        hirsel_core_ids::published::catch_io(|| {
            self.check_inner::<false>(request, &mut Meter::new(false), None, true, &[], None)?
                .product()
        })
    }

    pub(crate) fn check_for_editor(
        &mut self,
        request: CheckRequest,
        cache: &mut crate::lsp::cache::Cache,
        editor: &mut crate::lsp::cache::Check,
    ) -> io::Result<Arc<CheckProduct>> {
        hirsel_core_ids::published::catch_io(|| {
            self.check_inner::<false>(
                request,
                &mut Meter::new(false),
                None,
                true,
                &[],
                Some((cache, editor)),
            )?
            .product()
        })
    }
}

pub(super) fn finish_authority(
    mut result: CheckProduct,
    resolution_inputs: &CorpusManifest,
    report_authority: bool,
) -> io::Result<CheckProduct> {
    let mut cards = std::mem::take(&mut result.authority_cards);
    match reachability::authority::collect(
        &result,
        resolution_inputs,
        report_authority.then_some(&mut cards),
    ) {
        Ok(facts) => result.authority = facts,
        Err(error) => return Err(RejectedCheck::with_product(result, error)),
    }
    result.authority_cards = cards;
    if let Err(error) = reachability::authority::enforce(&result.authority, resolution_inputs) {
        if !report_authority {
            return Err(RejectedCheck::with_product(result, error));
        }
        result.authority_refusal = Some(error.to_string());
    }
    Ok(result)
}

/// Publish codec failures through the same declaration/arguments/operation/member path,
/// retaining the underlying type declaration that made a field lack `codec`.
pub(super) fn derived_sites(result: &mut CheckProduct, modules: &[ModuleInput]) {
    let context = |site: &Site| {
        let module = modules.iter().find(|m| Some(m.source) == site.source)?;
        hirsel_core_syntax::codec_context(&module.syntax, site.item as usize, site.span)
    };
    let generated = |site: &Site| {
        modules
            .iter()
            .find(|m| Some(m.source) == site.source)
            .and_then(|m| hirsel_core_syntax::declaration_of(&m.syntax, site.item as usize))
            .is_some()
    };
    let own: std::collections::HashSet<_> = result
        .diagnostics
        .iter()
        .filter(|r| !generated(&r.primary))
        .map(|r| (r.primary.source, r.primary.item))
        .collect();
    let project = |site: Site| {
        if generated(&site) {
            context(&site).map_or(site, |c| Site {
                item: c.item as u32,
                span: c.primary,
                ..site
            })
        } else {
            site
        }
    };
    let apply = |primary: &mut Site, ctx: &hirsel_core_syntax::DeriveContext| {
        primary.item = ctx.item as u32;
        primary.span = ctx.primary;
        let at = |span| Site { span, ..*primary };
        let owner = |span| Site {
            item: ctx.owner as u32,
            span,
            ..*primary
        };
        let mut path = vec![owner(ctx.declaration)];
        path.extend(ctx.arguments.iter().map(|&(item, span)| Site {
            item: item as u32,
            span,
            ..*primary
        }));
        path.push(owner(ctx.operation));
        path.extend(ctx.path.iter().copied().map(at));
        path
    };
    let mut seen = std::collections::HashSet::new();
    result.diagnostics.retain_mut(|r| {
        let is_generated = generated(&r.primary);
        if !is_generated
            && !(r.rule == "OWN-4"
                && !r.causal.is_empty()
                && r.required.as_deref().is_some_and(|b| b.contains("codec")))
        {
            return true;
        }
        let Some(ctx) = context(&r.primary) else {
            return true;
        };
        if is_generated && own.contains(&(r.primary.source, ctx.owner as u32)) {
            return false;
        }
        let path = apply(&mut r.primary, &ctx);
        let mut causal: Vec<_> = path
            .into_iter()
            .map(|site| hirsel_core_resolve::Cause {
                site,
                role: hirsel_core_resolve::CausalRole::DerivedHere,
            })
            .collect();
        for mut cause in std::mem::take(&mut r.causal) {
            cause.site = project(cause.site);
            if !causal.iter().any(|c| c.site == cause.site) {
                causal.push(cause);
            }
        }
        r.causal = causal;
        r.detail = None;
        r.unresolved = None;
        let keep = seen.insert((
            r.rule.clone(),
            r.primary.source,
            r.primary.item,
            r.primary.span,
            r.message.clone(),
        ));
        if is_generated {
            r.message = format!("{}: {}", ctx.description, r.message);
        }
        keep
    });
    let mut seen = std::collections::HashSet::new();
    result.incomplete.retain_mut(|g| {
        if !generated(&g.primary) {
            return true;
        }
        let Some(ctx) = context(&g.primary) else {
            return true;
        };
        if own.contains(&(g.primary.source, ctx.owner as u32)) {
            return false;
        }
        let mut path = apply(&mut g.primary, &ctx);
        for cause in g.causal.iter().copied().map(project) {
            if !path.contains(&cause) {
                path.push(cause);
            }
        }
        g.causal = path;
        let keep = seen.insert((
            g.primary.source,
            g.primary.item,
            g.primary.span,
            g.message.clone(),
        ));
        g.message = format!("{}: {}", ctx.description, g.message);
        keep
    });
}

impl Driver {
    pub(super) fn check_store(&self, path: &std::path::Path) -> io::Result<crate::Store> {
        if self.package_check {
            Ok(crate::Store::for_package_check())
        } else {
            crate::Store::new(path)
        }
    }
}

#[derive(Default)]
pub struct Driver {
    pub(crate) package_check: bool,
    #[cfg(test)]
    pub(crate) layers: crate::layers::Probe,
    /// Each body builds every row it names, as `closure_laws` compares copies with.
    #[cfg(test)]
    pub(crate) fresh_rows: bool,
}
