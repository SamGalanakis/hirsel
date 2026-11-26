//! System trust roots for platforms with a readable root store.
use rustls::RootCertStore;
use std::sync::Arc;

// macOS exercises the verifier in the native laws. Its shipping archive
// uses native roots and must have no imports from the iOS verifier.
#[cfg(any(target_os = "ios", all(test, target_vendor = "apple")))]
mod apple;
#[cfg(any(target_os = "ios", all(test, target_vendor = "apple")))]
pub(crate) use apple::evaluate;
const PLATFORM_TLS: bool = cfg!(target_os = "ios");

pub(crate) fn platform_tls() -> bool {
    #[cfg(all(test, target_vendor = "apple"))]
    if crate::os_tls::platform::test_trust().is_some() {
        return true;
    }
    PLATFORM_TLS
}

// Checked with the actual rustc cfg for the device, simulator and macOS.
#[cfg(all(target_vendor = "apple", target_abi = "sim"))]
const _: () = assert!(PLATFORM_TLS);
#[cfg(all(target_vendor = "apple", target_os = "ios", not(target_abi = "sim")))]
const _: () = assert!(PLATFORM_TLS);
#[cfg(target_os = "macos")]
const _: () = assert!(!PLATFORM_TLS);

#[cfg(not(any(target_os = "ios", all(test, target_vendor = "apple"))))]
pub(crate) fn evaluate(_: &crate::os_tls::platform::Input) -> Result<(), crate::provider::Outcome> {
    Err(crate::net::tls_failure(0, 4))
}

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
