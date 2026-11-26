//! Adapts rustls's synchronous certificate callback to a pending provider step.
use super::*;
use crate::{host, tasks::Outside};
use rustls::{
    DigitallySignedStruct, SignatureScheme,
    client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier},
    crypto::{WebPkiSupportedAlgorithms, verify_tls12_signature, verify_tls13_signature},
    pki_types::{CertificateDer, UnixTime},
    time_provider::TimeProvider,
};
use std::{fmt, sync::Mutex};

pub(crate) struct Input {
    pub(crate) chain: Vec<CertificateDer<'static>>,
    pub(crate) name: ServerName<'static>,
    pub(crate) now: UnixTime,
    #[cfg(all(test, target_vendor = "apple"))]
    pub(crate) test: Option<Arc<TestTrust>>,
}

pub(super) struct Deferred {
    algorithms: WebPkiSupportedAlgorithms,
    input: Mutex<Option<Input>>,
    #[cfg(all(test, target_vendor = "apple"))]
    test: Option<Arc<TestTrust>>,
}

impl fmt::Debug for Deferred {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Deferred")
    }
}

impl Deferred {
    pub(super) fn has_input(&self) -> bool {
        self.input.lock().unwrap().is_some()
    }
}

impl ServerCertVerifier for Deferred {
    fn verify_server_cert(
        &self,
        leaf: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        name: &ServerName<'_>,
        _: &[u8],
        now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        let chain = std::iter::once(leaf)
            .chain(intermediates)
            .map(|der| CertificateDer::from(der.as_ref().to_vec()))
            .collect();
        *self.input.lock().unwrap() = Some(Input {
            chain,
            name: name.to_owned(),
            now,
            #[cfg(all(test, target_vendor = "apple"))]
            test: self.test.clone(),
        });
        // This lets rustls check CertificateVerify and encode into its local
        // buffer. advance() parks before writing or completing the handshake.
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        signature: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls12_signature(message, cert, signature, &self.algorithms)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        signature: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls13_signature(message, cert, signature, &self.algorithms)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.algorithms.supported_schemes()
    }
}

#[derive(Debug)]
pub(super) struct ProviderClock {
    #[cfg(all(test, target_vendor = "apple"))]
    pub(super) now: Option<UnixTime>,
}

impl TimeProvider for ProviderClock {
    fn current_time(&self) -> Option<UnixTime> {
        #[cfg(all(test, target_vendor = "apple"))]
        if self.now.is_some() {
            return self.now;
        }
        host::clock::wall_ms()
            .map(|ms| UnixTime::since_unix_epoch(std::time::Duration::from_millis(ms)))
    }
}

pub(super) fn configure(alpn: Vec<Vec<u8>>) -> Result<(Arc<ClientConfig>, Arc<Deferred>), Outcome> {
    let crypto = Arc::new(rustls::crypto::ring::default_provider());
    #[cfg(all(test, target_vendor = "apple"))]
    let test = test_trust();
    let clock = Arc::new(ProviderClock {
        #[cfg(all(test, target_vendor = "apple"))]
        now: test.as_ref().and_then(|t| t.now),
    });
    let verifier = Arc::new(Deferred {
        algorithms: crypto.signature_verification_algorithms,
        input: Mutex::new(None),
        #[cfg(all(test, target_vendor = "apple"))]
        test,
    });
    let mut config = ClientConfig::builder_with_details(crypto, clock)
        .with_safe_default_protocol_versions()
        .map_err(protocol_error)?
        .dangerous()
        .with_custom_certificate_verifier(verifier.clone())
        .with_no_client_auth();
    config.resumption = Resumption::disabled();
    config.alpn_protocols = alpn;
    config.enable_secret_extraction = true;
    Ok((Arc::new(config), verifier))
}

struct Completion {
    waiter: Option<Outside>,
    answer: Option<Result<(), Outcome>>,
}

pub(super) struct Job(Arc<Mutex<Completion>>);

impl Job {
    pub(super) fn begin(verifier: &Deferred, token: u64) -> Result<Option<Self>, Outcome> {
        let Some(input) = verifier.input.lock().unwrap().take() else {
            return Ok(None);
        };
        let job = Self(Arc::new(Mutex::new(Completion {
            waiter: Some(Outside::begin()),
            answer: None,
        })));
        let completion = Arc::downgrade(&job.0);
        std::thread::Builder::new()
            .name("Hirsel SecTrust".into())
            .spawn(move || {
                let answer = host::trust::evaluate(&input);
                let published = if let Some(completion) = completion.upgrade()
                    && let mut state = completion.lock().unwrap()
                    && let Some(waiter) = state.waiter.take()
                {
                    state.answer = Some(answer);
                    waiter.complete(token);
                    true
                } else {
                    false
                };
                #[cfg(all(test, target_vendor = "apple"))]
                if let Some(hook) = input
                    .test
                    .as_ref()
                    .and_then(|test| test.on_complete.as_ref())
                {
                    hook(published);
                }
                #[cfg(not(all(test, target_vendor = "apple")))]
                let _ = published;
            })
            .map_err(transport)?;
        Ok(Some(job))
    }

    pub(super) fn answer(&self) -> Option<Result<(), Outcome>> {
        self.0.lock().unwrap().answer
    }
}

impl Drop for Job {
    fn drop(&mut self) {
        // The worker never holds this lock during Security evaluation. Closing
        // the handoff also releases its scheduler waiter before teardown.
        self.0.lock().unwrap().waiter.take();
    }
}

#[cfg(all(test, target_vendor = "apple"))]
#[derive(Default)]
pub(crate) struct TestTrust {
    pub(crate) anchors: Option<CertificateDer<'static>>,
    pub(crate) now: Option<UnixTime>,
    pub(crate) on_evaluate: Option<Box<dyn Fn() + Send + Sync>>,
    pub(crate) on_complete: Option<Box<dyn Fn(bool) + Send + Sync>>,
}

#[cfg(all(test, target_vendor = "apple"))]
thread_local! {
    pub(crate) static TEST_TRUST: std::cell::RefCell<Option<Arc<TestTrust>>> = const { std::cell::RefCell::new(None) };
}

#[cfg(all(test, target_vendor = "apple"))]
pub(crate) fn test_trust() -> Option<Arc<TestTrust>> {
    TEST_TRUST.with_borrow(Clone::clone)
}
