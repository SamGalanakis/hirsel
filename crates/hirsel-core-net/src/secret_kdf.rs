//! TLS-5 derivations shared by the native provider and the evaluator.
use crate::SecretUse;
use crate::argon2id::{self, Error, Plan};
use crate::tls_kdf::{self, TlsHash};
use base64ct::{Base64, Encoding as _};
use md5::{Digest as _, Md5};
use ring::digest;
use zeroize::{Zeroize as _, Zeroizing};

#[derive(Clone, Copy)]
pub enum DigestAlg {
    Md5 = 0,
    Sha256 = 1,
}

#[derive(Clone, Copy)]
pub enum Inner {
    Raw = 0,
    Base64 = 1,
}

pub enum Derivation {
    Hmac,
    Digest {
        alg: DigestAlg,
        inner: Inner,
    },
    Hkdf {
        len: usize,
    },
    Argon2id {
        len: usize,
        cost: [u32; 3],
        plan: Plan,
    },
}

impl Derivation {
    pub fn digest(alg: u32, inner: u32, empty_affixes: bool) -> Option<Self> {
        let alg = match alg {
            0 => DigestAlg::Md5,
            1 => DigestAlg::Sha256,
            _ => return None,
        };
        let inner = match inner {
            0 => Inner::Raw,
            1 => Inner::Base64,
            _ => return None,
        };
        if empty_affixes && matches!((alg, inner), (DigestAlg::Sha256, Inner::Raw)) {
            return None;
        }
        Some(Self::Digest { alg, inner })
    }

    pub fn parent_use(&self) -> SecretUse {
        match self {
            Self::Digest { .. } => SecretUse::DIGEST,
            _ => SecretUse::KDF,
        }
    }

    pub fn hkdf(len: u64) -> Option<Self> {
        (1..=255 * 32)
            .contains(&len)
            .then_some(Self::Hkdf { len: len as usize })
    }

    pub fn argon2id(cost: [u32; 3], len: u64, salt_len: u64) -> Option<Self> {
        if !argon2id::valid_input_lengths([0, salt_len, 0, 0]) {
            return None;
        }
        let [memory_kib, passes, lanes] = cost;
        let plan = Plan::new(memory_kib, passes, lanes, len).ok()?;
        Some(Self::Argon2id {
            len: len as usize,
            cost,
            plan,
        })
    }

    /// Child length, extra steps and working bytes.
    pub fn charges(&self) -> (u64, u64, u64) {
        match self {
            Self::Hmac => (32, 0, 0),
            Self::Digest { alg, .. } => match alg {
                DigestAlg::Md5 => (16, 0, 0),
                DigestAlg::Sha256 => (32, 0, 0),
            },
            Self::Hkdf { len } => (*len as u64, 0, 0),
            Self::Argon2id { len, plan, .. } => (*len as u64, plan.steps(), plan.memory_bytes()),
        }
    }

    /// Run only after admission, with output held in zeroizing host storage.
    pub fn derive(
        self,
        parent: &[u8],
        salt_or_prefix: &[u8],
        info_or_data: &[u8],
    ) -> Result<Zeroizing<Vec<u8>>, Error> {
        match self {
            Self::Hmac => Ok(crate::derive_hmac_sha256(
                parent,
                salt_or_prefix,
                info_or_data,
            )),
            Self::Digest { alg, inner } => Ok(derive_digest(
                alg,
                inner,
                salt_or_prefix,
                parent,
                info_or_data,
            )),
            Self::Hkdf { len } => {
                let mut out = Zeroizing::new(vec![0; len]);
                tls_kdf::hkdf(
                    TlsHash::Sha256,
                    salt_or_prefix,
                    parent,
                    &[info_or_data],
                    &mut out,
                )
                .expect("validated HKDF length");
                Ok(out)
            }
            Self::Argon2id { len, plan, .. } => {
                if !argon2id::valid_input_lengths([
                    parent.len() as u64,
                    salt_or_prefix.len() as u64,
                    0,
                    0,
                ]) {
                    return Err(Error::Params);
                }
                let mut out = Zeroizing::new(Vec::new());
                out.try_reserve_exact(len).map_err(|_| Error::HostMemory)?;
                out.resize(len, 0);
                plan.hash(parent, salt_or_prefix, &[], &[], &mut out)?;
                Ok(out)
            }
        }
    }
}

fn derive_digest(
    alg: DigestAlg,
    inner: Inner,
    prefix: &[u8],
    parent: &[u8],
    suffix: &[u8],
) -> Zeroizing<Vec<u8>> {
    let feed = |update: &mut dyn FnMut(&[u8])| {
        update(prefix);
        match inner {
            Inner::Raw => update(parent),
            Inner::Base64 => {
                let mut encoded = Zeroizing::new([0; 1024]);
                // Only the final chunk can need padding.
                for chunk in parent.chunks(768) {
                    let text = Base64::encode(chunk, encoded.as_mut()).unwrap();
                    update(text.as_bytes());
                }
            }
        }
        update(suffix);
    };
    match alg {
        DigestAlg::Md5 => {
            let mut hash = Md5::new();
            feed(&mut |bytes| hash.update(bytes));
            let mut derived = hash.finalize();
            let out = Zeroizing::new(derived.to_vec());
            derived.as_mut_slice().zeroize();
            out
        }
        DigestAlg::Sha256 => {
            let mut hash = digest::Context::new(&digest::SHA256);
            feed(&mut |bytes| hash.update(bytes));
            Zeroizing::new(hash.finish().as_ref().to_vec())
        }
    }
}
