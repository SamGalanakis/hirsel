//! TLS-5 derivations shared by the native provider and the evaluator.
use crate::argon2id::{self, Error, Plan};
use ring::hkdf;
use zeroize::Zeroizing;

pub enum Derivation {
    Hmac,
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
            Self::Hkdf { len } => {
                let prk = hkdf::Salt::new(hkdf::HKDF_SHA256, salt_or_prefix).extract(parent);
                let info = [info_or_data];
                let okm = prk.expand(&info, Len(len)).expect("validated HKDF length");
                let mut out = Zeroizing::new(vec![0; len]);
                okm.fill(&mut out).expect("exact HKDF output length");
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

struct Len(usize);

impl hkdf::KeyType for Len {
    fn len(&self) -> usize {
        self.0
    }
}
