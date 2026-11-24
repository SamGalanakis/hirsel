use super::*;

impl Machine<'_> {
    /// `secret.wrap`, `unwrap` and `sign` (native `secret.rs` key
    /// operations): error tag 1 Denied, 2 Invalid, 3 Tampered, 4 Capacity.
    /// Every evaluator Secret is sealed, so no KEK or secret is opaque.
    pub(super) fn secret_key(&mut self, t: &Task, op: HostOp, a: &[u64]) -> Vec<u64> {
        use hirsel_core_net::key_wrap::{self, KeyFailure, SignAlg};
        let fail = |f: KeyFailure| {
            let tag = match f {
                KeyFailure::Denied => 1,
                KeyFailure::Invalid => 2,
                KeyFailure::Tampered => 3,
                KeyFailure::Provider(reason) => {
                    std::panic::resume_unwind(Box::new(RunStop(reason)))
                }
            };
            vec![0, tag, 0, 0]
        };
        let key = self.secret(a[0]).unwrap().clone();
        let needed = match op {
            HostOp::SecretSign => SignAlg::from_case(a[1]).expect("a SignAlg case").needed(),
            _ => SecretUse::WRAP,
        };
        if !key.policy.uses.permits(needed) {
            return fail(KeyFailure::Denied);
        }
        let (produced, out) = match op {
            HostOp::SecretWrap => {
                let s = self.secret(a[1]).unwrap();
                let blob = key_wrap::wrap_with_salt(
                    &key.bytes,
                    &s.policy.text(),
                    &s.bytes,
                    key_wrap::salt(),
                );
                (blob, a[2])
            }
            HostOp::SecretSign => {
                let alg = SignAlg::from_case(a[1]).unwrap();
                (
                    key_wrap::sign(alg, &key.bytes, self.carrier_bytes(a[2])),
                    a[3],
                )
            }
            _ => {
                let Some(requested) = std::str::from_utf8(self.carrier_bytes(a[2]))
                    .ok()
                    .and_then(SecretPolicy::parse)
                else {
                    return fail(KeyFailure::Invalid);
                };
                let blob = self.carrier_bytes(a[1]).to_vec();
                let len = key_wrap::unwrapped_len(&blob).unwrap_or(0) as u64;
                let size = SecretGrant::logical_size(&requested.send, len);
                let charge = size.unwrap_or(u64::MAX);
                let payer = t.limits.last().unwrap().clone();
                if let Err(available) = self.admit_charge(&payer, charge, size.is_none()) {
                    return vec![charge, 4, 0, available];
                }
                let mut plain = Zeroizing::new(Vec::new());
                if let Err(f) = key_wrap::unwrap_into(&key.bytes, &blob, &mut plain)
                    .and_then(|bound| key_wrap::within(&requested, bound))
                {
                    self.refund(payer, charge);
                    return fail(f);
                }
                let seal = Arc::new(());
                self.world
                    .seals
                    .push((Arc::downgrade(&seal), payer.clone(), charge));
                let key = self.designate(
                    EvalSecret {
                        policy: requested,
                        bytes: Arc::new(SecretBytes::new(plain)),
                        seal,
                    },
                    (payer, 0),
                );
                return vec![key, 0, 0, 0];
            }
        };
        let bytes = match produced {
            Ok(bytes) => bytes,
            Err(f) => return fail(f),
        };
        let (_, data, len, cap) = self.carrier(out);
        let spare = cap.saturating_sub(len);
        let n = bytes.len() as u64;
        if n > spare {
            return vec![n, 4, 0, spare];
        }
        self.put(data + len, &bytes);
        self.store(out + 8, 8, len + n);
        vec![n, 0, 0, 0]
    }

    pub(super) fn secret_aead(&mut self, op: HostOp, a: &[u64]) -> Vec<u64> {
        use hirsel_core_net::key_wrap::KeyFailure;
        use hirsel_core_net::secret_aead::{self, AeadAlg, AeadInput, AeadMode, AeadRefusal};

        let key = self.secret(a[0]).unwrap().clone();
        let alg = AeadAlg::from_case(a[1]).expect("an AeadAlg case");
        let mode = if op == HostOp::SecretAeadSeal {
            AeadMode::Seal
        } else {
            AeadMode::Open
        };
        let out = a[5];
        let len = self.load(out + 8, 8);
        let spare = self.load(out + 16, 8) - len;
        let size = match secret_aead::output_len(
            key.policy.uses,
            alg,
            mode,
            self.load(a[2] + 8, 8) as usize,
            self.load(a[4] + 8, 8) as usize,
            spare as usize,
        ) {
            Ok(size) => size,
            Err(AeadRefusal::Denied) => return vec![0, 1, 0, 0],
            Err(AeadRefusal::Invalid) => return vec![0, 2, 0, 0],
            Err(AeadRefusal::Capacity {
                requested,
                available,
            }) => {
                return vec![requested as u64, 4, 0, available as u64];
            }
        };
        let apply = |machine: &Self, spare| {
            let input = AeadInput {
                alg,
                mode,
                nonce: machine.carrier_bytes(a[2]),
                aad: machine.carrier_bytes(a[3]),
                bytes: machine.carrier_bytes(a[4]),
            };
            input.apply(&key.bytes, spare)
        };
        let result = if size == 0 {
            apply(self, &mut [])
        } else {
            let (block, offset) = self.check_write(self.load(out, 8) + len, size);
            // Lend the destination's allocation to the sink while reading the
            // other carriers. The exclusive destination cannot alias any input.
            let mut bytes = std::mem::take(&mut self.mem[block].bytes);
            let spare = &mut bytes[offset..offset + size];
            // SAFETY: MaybeUninit<u8> has the layout of u8. The sink only writes
            // initialized bytes, and the allocation remains exclusively borrowed.
            let spare = unsafe { std::slice::from_raw_parts_mut(spare.as_mut_ptr().cast(), size) };
            let result = apply(self, spare);
            self.mem[block].bytes = bytes;
            result
        };
        match result {
            Ok(()) => {
                self.store(out + 8, 8, len + size as u64);
                vec![size as u64, 0, 0, 0]
            }
            Err(KeyFailure::Invalid) => vec![0, 2, 0, 0],
            Err(KeyFailure::Tampered) => vec![0, 3, 0, 0],
            Err(KeyFailure::Denied | KeyFailure::Provider(_)) => unreachable!(),
        }
    }

    pub(super) fn secret_mac(&mut self, t: &mut Task, op: HostOp, a: &[u64]) -> X<Vec<u64>> {
        let secret = self.secret(a[0]).unwrap();
        let signing = op.secret_mac_signs();
        let needed = if signing {
            SecretUse::MAC
        } else {
            SecretUse::VERIFY
        };
        if !secret.policy.uses.permits(needed) {
            return Ok(vec![0, 1, 0, 0]);
        }
        self.take_steps(t, self.load(a[1] + 8, 8))?;
        let secret = self.secret(a[0]).unwrap();
        let data = self.carrier_bytes(a[1]);
        use hirsel_core_net::HmacSha2;
        let alg = match op {
            HostOp::SecretHmac | HostOp::SecretVerifyHmac => HmacSha2::Sha256,
            HostOp::SecretHmacSha384 | HostOp::SecretVerifyHmacSha384 => HmacSha2::Sha384,
            HostOp::SecretHmacSha512 | HostOp::SecretVerifyHmacSha512 => HmacSha2::Sha512,
            HostOp::SecretJwsHmacSign | HostOp::SecretJwsHmacVerify => {
                HmacSha2::from_jws_case(a[3])
            }
            _ => unreachable!(),
        };
        let jws = matches!(op, HostOp::SecretJwsHmacSign | HostOp::SecretJwsHmacVerify);
        if jws && secret.bytes.len() < alg.tag_len() {
            return Ok(vec![0, 2, 0, 0]);
        }
        if signing {
            let tag = alg.sign(&secret.bytes, data);
            let bytes = tag.as_ref();
            let len = if jws { 64 } else { bytes.len() };
            for (i, byte) in bytes
                .iter()
                .copied()
                .chain(std::iter::repeat(0))
                .take(len)
                .enumerate()
            {
                self.store(a[2] + i as u64, 1, u64::from(byte));
            }
            Ok(vec![0, 0, 0, 0])
        } else {
            let verdict = alg.verify(&secret.bytes, data, self.carrier_bytes(a[2]));
            Ok(vec![u64::from(verdict), 0, 0, 0])
        }
    }
}
