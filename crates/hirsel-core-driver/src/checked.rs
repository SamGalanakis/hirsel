use crate::{CheckProduct, ContentHash, request_record};
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
    pub(crate) product: CheckProduct,
    error: io::Error,
}

impl RejectedCheck {
    pub(crate) fn with_product(product: CheckProduct, error: io::Error) -> io::Error {
        if product.accepted() {
            error
        } else {
            io::Error::new(error.kind(), Self { product, error })
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
