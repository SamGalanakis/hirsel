use crate::{
    CheckProduct, CheckRequest, ContentHash, CorpusManifest, Driver, Meter, reachability,
    request_record,
};
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
                self.check_inner::<true>(request, meter, record, false)
            })
        } else {
            hirsel_core_ids::published::catch_io(|| {
                self.check_inner::<false>(request, meter, record, false)
            })
        }
    }
    pub(crate) fn check_for_authority(
        &mut self,
        request: CheckRequest,
    ) -> io::Result<Arc<CheckProduct>> {
        hirsel_core_ids::published::catch_io(|| {
            self.check_inner::<false>(request, &mut Meter::new(false), None, true)?
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
