//! System trust roots for platforms with a readable root store.
use rustls::RootCertStore;
use std::sync::Arc;

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) fn system_roots() -> Result<Arc<RootCertStore>, i32> {
    let mut store = RootCertStore::empty();
    store.add_parsable_certificates(rustls_native_certs::load_native_certs().certs);
    Ok(Arc::new(store))
}

#[cfg(target_os = "ios")]
pub(crate) fn system_roots() -> Result<Arc<RootCertStore>, i32> {
    // iOS uses platform trust evaluation, which cannot return a root store.
    Err(libc::ENOTSUP)
}
