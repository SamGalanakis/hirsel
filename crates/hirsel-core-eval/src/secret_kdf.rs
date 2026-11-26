use super::*;

impl Machine<'_> {
    pub(super) fn secret_derive(&mut self, t: &mut Task, op: HostOp, a: &[u64]) -> X<Vec<u64>> {
        use hirsel_core_net::{argon2id::Error, secret_kdf::Derivation};

        let invalid = || Ok(vec![0, 1, 0, 0]);
        let derivation = match op {
            HostOp::SecretDeriveHmac => Some(Derivation::Hmac),
            HostOp::SecretDeriveDigest => {
                let empty_affixes = [a[2], a[4]]
                    .into_iter()
                    .all(|ptr| self.load(ptr + 8, 8) == 0);
                Derivation::digest(a[1] as u32, a[3] as u32, empty_affixes)
            }
            HostOp::SecretDeriveHkdf => Derivation::hkdf(a[3]),
            HostOp::SecretDeriveArgon2id => Derivation::argon2id(
                [a[2] as u32, a[3] as u32, a[4] as u32],
                a[5],
                self.load(a[1] + 8, 8),
            ),
            _ => unreachable!(),
        };
        let Some(derivation) = derivation else {
            return invalid();
        };
        let parent = self.secret(a[0]).unwrap();
        let Some(policy) = std::str::from_utf8(self.carrier_bytes(*a.last().unwrap()))
            .ok()
            .and_then(|uses| parent.policy.derived(derivation.parent_use(), uses))
        else {
            return invalid();
        };
        let (len, steps, working) = derivation.charges();
        if steps != 0 {
            self.take_steps(t, steps)?;
        }
        let child = SecretGrant::logical_size(policy.send(), len).unwrap();
        let charge = child + working;
        let payer = t.limits.last().unwrap().clone();
        if let Err(available) = self.admit(&payer, charge) {
            return Ok(vec![charge, 2, 0, available]);
        }
        let parent = self.secret(a[0]).unwrap();
        let (prefix, data) = match op {
            HostOp::SecretDeriveDigest => (self.carrier_bytes(a[2]), self.carrier_bytes(a[4])),
            HostOp::SecretDeriveArgon2id => (self.carrier_bytes(a[1]), &[][..]),
            _ => (self.carrier_bytes(a[1]), self.carrier_bytes(a[2])),
        };
        let derived = derivation.derive(&parent.bytes, prefix, data);
        let opaque = parent.opaque;
        self.refund(payer.clone(), working);
        match derived {
            Ok(bytes) => {
                let key = self.store_sealed(policy, bytes, payer, child, opaque);
                Ok(vec![key, 0, 0, 0])
            }
            Err(Error::Params) => {
                self.refund(payer, child);
                invalid()
            }
            Err(Error::HostMemory) => std::panic::resume_unwind(Box::new(RunStop("HostMemory"))),
        }
    }
}
