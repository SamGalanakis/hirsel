//! Apple's SSL trust policy. All Core Foundation objects stay on the evaluator.
use crate::{net::tls_failure, os_tls::platform::Input, provider::Outcome};
use rustls::{
    CertificateError, Error, client::verify_server_name, pki_types::CertificateDer,
    server::ParsedCertificate,
};
use std::{
    ffi::c_void,
    ptr::{self, NonNull},
};

type Ref = *const c_void;
type Status = i32;

#[repr(C)]
struct ArrayCallbacks {
    version: isize,
    retain: Option<unsafe extern "C" fn(Ref, Ref) -> Ref>,
    release: Option<unsafe extern "C" fn(Ref, Ref)>,
    copy_description: Option<unsafe extern "C" fn(Ref) -> Ref>,
    equal: Option<unsafe extern "C" fn(Ref, Ref) -> u8>,
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    static kCFTypeArrayCallBacks: ArrayCallbacks;
    fn CFRelease(value: Ref);
    fn CFDataCreate(allocator: Ref, bytes: *const u8, length: isize) -> Ref;
    fn CFArrayCreate(
        allocator: Ref,
        values: *const Ref,
        count: isize,
        callbacks: *const ArrayCallbacks,
    ) -> Ref;
    fn CFStringCreateWithBytes(
        allocator: Ref,
        bytes: *const u8,
        count: isize,
        encoding: u32,
        external: u8,
    ) -> Ref;
    fn CFDateCreate(allocator: Ref, seconds: f64) -> Ref;
    fn CFDateGetAbsoluteTime(date: Ref) -> f64;
    fn CFErrorGetCode(error: Ref) -> isize;
}

#[link(name = "Security", kind = "framework")]
unsafe extern "C" {
    fn SecCertificateCreateWithData(allocator: Ref, data: Ref) -> Ref;
    fn SecCertificateCopyNotValidBeforeDate(cert: Ref) -> Ref;
    fn SecCertificateCopyNotValidAfterDate(cert: Ref) -> Ref;
    fn SecPolicyCreateSSL(server: u8, name: Ref) -> Ref;
    fn SecTrustCreateWithCertificates(certificates: Ref, policies: Ref, trust: *mut Ref) -> Status;
    fn SecTrustSetVerifyDate(trust: Ref, date: Ref) -> Status;
    fn SecTrustSetNetworkFetchAllowed(trust: Ref, allowed: u8) -> Status;
    fn SecTrustEvaluateWithError(trust: Ref, error: *mut Ref) -> bool;
    #[cfg(test)]
    fn SecTrustSetAnchorCertificates(trust: Ref, anchors: Ref) -> Status;
    #[cfg(test)]
    fn SecTrustSetAnchorCertificatesOnly(trust: Ref, only: u8) -> Status;
}

struct Owned(NonNull<c_void>);

impl Owned {
    // The caller must pass ownership of a Create/Copy result or null.
    unsafe fn new(value: Ref, on_null: i64) -> Result<Self, Outcome> {
        NonNull::new(value.cast_mut())
            .map(Self)
            .ok_or_else(|| failure(4, on_null))
    }

    fn get(&self) -> Ref {
        self.0.as_ptr()
    }
}

impl Drop for Owned {
    fn drop(&mut self) {
        // SAFETY: the owner holds one Create/Copy reference.
        unsafe { CFRelease(self.get()) };
    }
}

// SecBase.h codes. Null Copy/Create returns carry no status, so use the
// corresponding Security failure code for the failed operation.
const ALLOCATE: i64 = -108;
const INTERNAL_COMPONENT: i64 = -2070;
const DECODE: i64 = -26275;
const CREATE_CHAIN_FAILED: i64 = -25318;
const NOT_TRUSTED: i64 = -67843;
const HOST_NAME_MISMATCH: i64 = -67602;
const CERTIFICATE_EXPIRED: i64 = -67818;
const CERTIFICATE_NOT_VALID_YET: i64 = -67819;

fn failure(detail: u8, code: i64) -> Outcome {
    // On certificate failure the TLS outcome has no stream key. Keep the
    // full signed Apple diagnostic in that word, including in the tape.
    Outcome {
        value: code as u64,
        ..tls_failure(0, detail)
    }
}

fn status(code: Status) -> Result<(), Outcome> {
    if code == 0 {
        Ok(())
    } else {
        Err(failure(4, i64::from(code)))
    }
}

fn certificate(der: &CertificateDer<'_>) -> Result<Owned, Outcome> {
    // SAFETY: CFDataCreate copies the supplied bytes and the certificate
    // constructor borrows the resulting data for the call.
    unsafe {
        let data = Owned::new(
            CFDataCreate(ptr::null(), der.as_ptr(), der.len() as isize),
            ALLOCATE,
        )?;
        Owned::new(
            SecCertificateCreateWithData(ptr::null(), data.get()),
            DECODE,
        )
    }
}

fn check_leaf(
    leaf: &CertificateDer<'_>,
    certificate: &Owned,
    name: &rustls::pki_types::ServerName<'_>,
    date: f64,
) -> Result<(), Outcome> {
    let parsed = ParsedCertificate::try_from(leaf).map_err(|_| failure(4, DECODE))?;
    // SAFETY: both Copy calls take a live SecCertificate. Each returned date
    // remains live for the corresponding CFDate call.
    let (before, after) = unsafe {
        let before = Owned::new(
            SecCertificateCopyNotValidBeforeDate(certificate.get()),
            DECODE,
        )?;
        let after = Owned::new(
            SecCertificateCopyNotValidAfterDate(certificate.get()),
            DECODE,
        )?;
        (
            CFDateGetAbsoluteTime(before.get()),
            CFDateGetAbsoluteTime(after.get()),
        )
    };
    if before > after || !before.is_finite() || !after.is_finite() {
        return Err(failure(4, DECODE));
    }
    verify_server_name(&parsed, name).map_err(|error| match error {
        Error::InvalidCertificate(
            CertificateError::NotValidForName | CertificateError::NotValidForNameContext { .. },
        ) => failure(1, HOST_NAME_MISMATCH),
        _ => failure(4, DECODE),
    })?;
    if date > after {
        return Err(failure(2, CERTIFICATE_EXPIRED));
    }
    if date < before {
        return Err(failure(3, CERTIFICATE_NOT_VALID_YET));
    }
    Ok(())
}

pub(crate) fn evaluate(input: &Input) -> Result<(), Outcome> {
    const UNIX_TO_CF_EPOCH: f64 = 978_307_200.0;
    const UTF8: u32 = 0x0800_0100;
    let (leaf, intermediates) = input
        .chain
        .split_first()
        .ok_or_else(|| failure(4, DECODE))?;
    let leaf_cert = certificate(leaf)?;
    let seconds = input.now.as_secs() as f64 - UNIX_TO_CF_EPOCH;
    check_leaf(leaf, &leaf_cert, &input.name, seconds)?;
    let intermediates = intermediates
        .iter()
        .map(certificate)
        .collect::<Result<Vec<_>, _>>()?;
    let chain: Vec<_> = std::iter::once(&leaf_cert)
        .chain(&intermediates)
        .map(Owned::get)
        .collect();
    let name = input.name.to_str();
    // SAFETY: the array retains each certificate. The string, date, policy
    // and array are owned for as long as Security uses them.
    unsafe {
        let chain = Owned::new(
            CFArrayCreate(
                ptr::null(),
                chain.as_ptr(),
                chain.len() as isize,
                &raw const kCFTypeArrayCallBacks,
            ),
            ALLOCATE,
        )?;
        let name = Owned::new(
            CFStringCreateWithBytes(ptr::null(), name.as_ptr(), name.len() as isize, UTF8, 0),
            ALLOCATE,
        )?;
        let policy = Owned::new(SecPolicyCreateSSL(1, name.get()), INTERNAL_COMPONENT)?;
        let mut trust = ptr::null();
        status(SecTrustCreateWithCertificates(
            chain.get(),
            policy.get(),
            &mut trust,
        ))?;
        let trust = Owned::new(trust, INTERNAL_COMPONENT)?;
        let date = Owned::new(CFDateCreate(ptr::null(), seconds), ALLOCATE)?;
        status(SecTrustSetVerifyDate(trust.get(), date.get()))?;
        status(SecTrustSetNetworkFetchAllowed(trust.get(), 0))?;
        #[cfg(test)]
        if let Some(test) = &input.test {
            if let Some(anchor) = &test.anchors {
                test_anchor(&trust, anchor)?;
            }
            if let Some(hook) = &test.on_evaluate {
                hook();
            }
        }
        let mut error = ptr::null();
        let accepted = SecTrustEvaluateWithError(trust.get(), &mut error);
        let error = Owned::new(error, INTERNAL_COMPONENT);
        if accepted {
            return Ok(());
        }
        let code = CFErrorGetCode(error?.get()) as i64;
        let detail = match code {
            CREATE_CHAIN_FAILED | NOT_TRUSTED => 0,
            HOST_NAME_MISMATCH => 1,
            CERTIFICATE_EXPIRED => 2,
            CERTIFICATE_NOT_VALID_YET => 3,
            _ => 4,
        };
        Err(failure(detail, code))
    }
}

#[cfg(test)]
fn test_anchor(trust: &Owned, der: &CertificateDer<'_>) -> Result<(), Outcome> {
    let anchor = certificate(der)?;
    let entry = anchor.get();
    // SAFETY: the array contains one live SecCertificate. Security retains
    // the anchor array, so use a retaining CF array for this override.
    unsafe {
        let anchors = Owned::new(
            CFArrayCreate(ptr::null(), &entry, 1, &raw const kCFTypeArrayCallBacks),
            ALLOCATE,
        )?;
        status(SecTrustSetAnchorCertificates(trust.get(), anchors.get()))?;
        status(SecTrustSetAnchorCertificatesOnly(trust.get(), 1))
    }
}
