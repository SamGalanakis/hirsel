//! Rust-only inputs for compiling the shipping verifier before an iOS runtime
//! archive is available. The verifier itself supplies every Apple FFI call.

extern crate self as rustls;

#[path = "apple.rs"]
mod apple;

mod pki_types {
    pub type CertificateDer<'a> = &'a [u8];

    pub struct ServerName<'a>(pub &'a str);

    impl ServerName<'_> {
        pub fn to_str(&self) -> &str {
            self.0
        }
    }
}

#[allow(dead_code)]
pub enum CertificateError {
    BadEncoding,
    NotValidForName,
    NotValidForNameContext {},
}

#[allow(dead_code)]
pub enum Error {
    InvalidCertificate(CertificateError),
}

mod server {
    pub struct ParsedCertificate;

    impl TryFrom<&&[u8]> for ParsedCertificate {
        type Error = crate::Error;

        fn try_from(_: &&[u8]) -> Result<Self, Self::Error> {
            Ok(Self)
        }
    }
}

mod client {
    pub fn verify_server_name(
        _: &crate::server::ParsedCertificate,
        _: &crate::pki_types::ServerName<'_>,
    ) -> Result<(), crate::Error> {
        Ok(())
    }
}

mod os_tls {
    pub mod platform {
        pub struct Input {
            pub chain: Vec<crate::pki_types::CertificateDer<'static>>,
            pub name: crate::pki_types::ServerName<'static>,
            pub now: std::time::Duration,
        }
    }
}

mod provider {
    pub struct Outcome {
        pub value: u64,
        pub detail: u8,
    }
}

mod net {
    pub fn tls_failure(_: u8, detail: u8) -> crate::provider::Outcome {
        crate::provider::Outcome { value: 0, detail }
    }
}

pub fn evaluate(input: &os_tls::platform::Input) -> Result<(), provider::Outcome> {
    apple::evaluate(input)
}
