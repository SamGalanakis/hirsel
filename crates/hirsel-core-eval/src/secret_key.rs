use super::*;
use hirsel_core_ids::work_steps::table;

impl Machine<'_> {
    /// `stdin.read_secret`: the policy, then seal's charge at 1024 bytes, both
    /// before the provider. The model's stdin is never a terminal, so a call
    /// past them refunds the charge and is `NotTerminal` (error 1). A policy
    /// refusal is error 9 with code 1 (Invalid) or 2 (Capacity).
    pub(super) fn stdin_secret(&mut self, t: &mut Task, a: &[u64]) -> X<Vec<u64>> {
        let Some(policy) = std::str::from_utf8(self.carrier_bytes(a[1]))
            .ok()
            .and_then(SecretPolicy::parse)
        else {
            return Ok(vec![0, 9, 1, 0]);
        };
        let size = SecretGrant::logical_size(policy.send(), 1024);
        let charge = size.unwrap_or(u64::MAX);
        let payer = t.limits.last().unwrap().clone();
        if let Err(available) = self.admit_charge(&payer, charge, size.is_none()) {
            return Ok(vec![charge, 9, 2, available]);
        }
        self.refund(payer, charge);
        self.check_cancel(t)?;
        Ok(vec![0, 1, 0, 0])
    }

    pub(super) fn secret_policy(&mut self, t: &Task, op: HostOp, a: &[u64]) -> Vec<u64> {
        let Some(policy) = std::str::from_utf8(self.carrier_bytes(a[1]))
            .ok()
            .and_then(SecretPolicy::parse)
        else {
            return vec![0, 1, 0, 0];
        };
        let (policy, len) = if op == HostOp::SecretSeal {
            (policy, self.load(a[0] + 8, 8))
        } else {
            let Some(parent) = self.secret(a[0]) else {
                return vec![0, 1, 0, 0];
            };
            let Some(intersection) = parent.policy.intersect(&policy) else {
                return vec![0, 1, 0, 0];
            };
            (intersection, 0)
        };
        let size = SecretGrant::logical_size(policy.send(), len);
        let charge = size.unwrap_or(u64::MAX);
        let payer = t.limits.last().unwrap();
        if let Err(available) = self.admit_charge(payer, charge, size.is_none()) {
            return vec![charge, 2, 0, available];
        }
        let (bytes, seal, opaque, import) = if op == HostOp::SecretSeal {
            let data = self.load(a[0], 8);
            let bytes = Zeroizing::new(self.carrier_bytes(a[0]).to_vec());
            if !bytes.is_empty() && self.load(a[0] + 16, 8) != 0 {
                let (block, offset) = split(data);
                use zeroize::Zeroize;
                self.mem[block].bytes[offset..offset + bytes.len()].zeroize();
            }
            let key = self.store_sealed(policy, bytes, payer.clone(), charge, false);
            return vec![key, 0, 0, 0];
        } else {
            let parent = self.secret(a[0]).unwrap();
            (
                parent.bytes.clone(),
                parent.seal.clone(),
                parent.opaque,
                parent.import.clone(),
            )
        };
        let payer = payer.clone();
        let key = self.designate(
            EvalSecret {
                opaque,
                policy,
                bytes,
                seal,
                import,
            },
            (payer, charge),
        );
        vec![key, 0, 0, 0]
    }

    pub(super) fn secret(&self, key: u64) -> Option<&EvalSecret> {
        let slot = self.host.world.secrets.get(key as usize)?.as_ref()?;
        Some(&slot.secret)
    }

    /// A new designation with one holder (native `Secrets::push`).
    pub(super) fn designate(&mut self, secret: EvalSecret, charge: (Limit, u64)) -> u64 {
        self.host.world.secrets.push(Some(EvalSlot {
            secret,
            holders: 1,
            charge,
        }));
        self.host.world.secrets.len() as u64 - 1
    }

    pub(super) fn store_sealed(
        &mut self,
        policy: SecretPolicy,
        bytes: Zeroizing<Vec<u8>>,
        payer: Limit,
        charge: u64,
        opaque: bool,
    ) -> u64 {
        let seal = Arc::new(());
        self.host
            .world
            .seals
            .push((Arc::downgrade(&seal), payer.clone(), charge));
        // An import secret's bytes live in its spent mark, which the import zeroes.
        let (bytes, import) = if policy.import().is_some() {
            let import = hirsel_core_net::ImportSpend::new(&policy, Some(bytes));
            (Zeroizing::new(Vec::new()), import.map(Arc::new))
        } else {
            (bytes, None)
        };
        self.designate(
            EvalSecret {
                opaque,
                import,
                policy,
                bytes: Arc::new(SecretBytes::new(bytes)),
                seal,
            },
            (payer, 0),
        )
    }

    pub(super) fn secret_p256_key(&mut self, t: &Task, a: &[u64]) -> X<Vec<u64>> {
        use hirsel_core_net::{
            key_wrap::KeyFailure,
            p256_key::{self, DOCUMENT_LEN, PUBLIC_LEN},
        };
        let Some(policy) = SecretPolicy::p256_generation(self.carrier_bytes(a[1])) else {
            return Ok(vec![0, 2, 0, 0]);
        };
        let (_, data, len, cap) = self.carrier(a[2]);
        let spare = cap - len;
        if spare < PUBLIC_LEN as u64 {
            return Ok(vec![PUBLIC_LEN as u64, 4, 0, spare]);
        }
        let charge = SecretGrant::logical_size(&[], DOCUMENT_LEN as u64).unwrap();
        let payer = t.limits.last().unwrap().clone();
        if let Err(available) = self.admit(&payer, charge) {
            return Ok(vec![charge, 4, 0, available]);
        }
        let mut point = [0; PUBLIC_LEN];
        let cell = self.cell(a[0]);
        let document = if let Some(cell) = cell {
            let VirtualCell::Random(stream) = &mut self.host.world.cells[cell] else {
                unreachable!("a Random cell");
            };
            match p256_key::generate_virtual(
                |draw| stream.fill(draw).then_some(()).ok_or(()),
                &mut point,
            ) {
                Ok(document) => document,
                Err(()) => {
                    self.refund(payer, charge);
                    return Err(Stop::Cause("Bounds"));
                }
            }
        } else {
            match p256_key::generate_host(&mut point) {
                Ok(document) => document,
                Err(KeyFailure::Provider(reason)) => {
                    std::panic::resume_unwind(Box::new(RunStop(reason)))
                }
                Err(_) => unreachable!("ring generation only fails at the provider"),
            }
        };
        let key = self.store_sealed(
            policy,
            Zeroizing::new(document.to_vec()),
            payer,
            charge,
            cell.is_none(),
        );
        self.put(data + len, &point);
        self.store(a[2] + 8, 8, len + PUBLIC_LEN as u64);
        Ok(vec![key, 0, 0, 0])
    }

    /// `secret.wrap`, `unwrap` and `sign` (native `secret.rs` key
    /// operations): error tag 1 Denied, 2 Invalid, 3 Tampered, 4 Capacity.
    /// Host-generated signing keys are opaque even to the evaluator.
    pub(super) fn secret_key(&mut self, t: &mut Task, op: HostOp, a: &[u64]) -> X<Vec<u64>> {
        use hirsel_core_net::key_wrap::{self, KeyFailure, SignAlg, SigningRandom};
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
        if !key.policy.uses().permits(needed) {
            return Ok(fail(KeyFailure::Denied));
        }
        let (produced, out) = match op {
            HostOp::SecretWrap => {
                let s = self.secret(a[1]).unwrap();
                if s.policy.import().is_some() || (s.opaque && !key.opaque) {
                    return Ok(fail(KeyFailure::Denied));
                }
                let len = s.bytes.len() as u64;
                self.take_work(t, table::AEAD, &[len])?;
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
                let source = if self.cell(a[2]).is_some() {
                    SigningRandom::Virtual
                } else {
                    SigningRandom::Host
                };
                if alg == SignAlg::P256 && source == SigningRandom::Virtual && key.opaque {
                    return Ok(fail(KeyFailure::Denied));
                }
                self.take_work(t, alg.work(), &[self.load(a[3] + 8, 8)])?;
                (
                    key_wrap::sign(alg, &key.bytes, self.carrier_bytes(a[3]), source),
                    a[4],
                )
            }
            _ => {
                let Some(requested) = std::str::from_utf8(self.carrier_bytes(a[2]))
                    .ok()
                    .and_then(SecretPolicy::parse)
                    .filter(|policy| policy.import().is_none())
                else {
                    return Ok(fail(KeyFailure::Invalid));
                };
                self.take_work(t, table::AEAD, &[self.load(a[1] + 8, 8)])?;
                let blob = self.carrier_bytes(a[1]).to_vec();
                let len = key_wrap::unwrapped_len(&blob).unwrap_or(0) as u64;
                let size = SecretGrant::logical_size(requested.send(), len);
                let charge = size.unwrap_or(u64::MAX);
                let payer = t.limits.last().unwrap().clone();
                if let Err(available) = self.admit_charge(&payer, charge, size.is_none()) {
                    return Ok(vec![charge, 4, 0, available]);
                }
                let mut plain = Zeroizing::new(Vec::new());
                if let Err(f) = key_wrap::unwrap_into(&key.bytes, &blob, &mut plain)
                    .and_then(|bound| key_wrap::within(&requested, bound))
                {
                    self.refund(payer, charge);
                    return Ok(fail(f));
                }
                let seal = Arc::new(());
                self.host
                    .world
                    .seals
                    .push((Arc::downgrade(&seal), payer.clone(), charge));
                let key = self.designate(
                    EvalSecret {
                        import: None,
                        opaque: key.opaque,
                        policy: requested,
                        bytes: Arc::new(SecretBytes::new(plain)),
                        seal,
                    },
                    (payer, 0),
                );
                return Ok(vec![key, 0, 0, 0]);
            }
        };
        let bytes = match produced {
            Ok(bytes) => bytes,
            Err(f) => return Ok(fail(f)),
        };
        let (_, data, len, cap) = self.carrier(out);
        let spare = cap.saturating_sub(len);
        let n = bytes.len() as u64;
        if n > spare {
            return Ok(vec![n, 4, 0, spare]);
        }
        self.put(data + len, &bytes);
        self.store(out + 8, 8, len + n);
        Ok(vec![n, 0, 0, 0])
    }

    pub(super) fn secret_aead(&mut self, t: &mut Task, op: HostOp, a: &[u64]) -> X<Vec<u64>> {
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
            key.policy.uses(),
            alg,
            mode,
            self.load(a[2] + 8, 8) as usize,
            self.load(a[4] + 8, 8) as usize,
            spare as usize,
        ) {
            Ok(size) => size,
            Err(AeadRefusal::Denied) => return Ok(vec![0, 1, 0, 0]),
            Err(AeadRefusal::Invalid) => return Ok(vec![0, 2, 0, 0]),
            Err(AeadRefusal::Capacity {
                requested,
                available,
            }) => {
                return Ok(vec![requested as u64, 4, 0, available as u64]);
            }
        };
        let units = [self.load(a[3] + 8, 8), self.load(a[4] + 8, 8)];
        self.take_work(t, table::AEAD, &units)?;
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
                Ok(vec![size as u64, 0, 0, 0])
            }
            Err(KeyFailure::Invalid) => Ok(vec![0, 2, 0, 0]),
            Err(KeyFailure::Tampered) => Ok(vec![0, 3, 0, 0]),
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
        if !secret.policy.uses().permits(needed) {
            return Ok(vec![0, 1, 0, 0]);
        }
        use hirsel_core_net::HmacAlgorithm;
        let alg = match op {
            HostOp::SecretHmacSha1 | HostOp::SecretVerifyHmacSha1 => HmacAlgorithm::Sha1,
            HostOp::SecretHmac | HostOp::SecretVerifyHmac => HmacAlgorithm::Sha256,
            HostOp::SecretHmacSha384 | HostOp::SecretVerifyHmacSha384 => HmacAlgorithm::Sha384,
            HostOp::SecretHmacSha512 | HostOp::SecretVerifyHmacSha512 => HmacAlgorithm::Sha512,
            HostOp::SecretJwsHmacSign | HostOp::SecretJwsHmacVerify => {
                HmacAlgorithm::from_jws_case(a[3])
            }
            _ => unreachable!(),
        };
        self.take_work(t, alg.work(), &[self.load(a[1] + 8, 8)])?;
        let secret = self.secret(a[0]).unwrap();
        let data = self.carrier_bytes(a[1]);
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

    pub(super) fn secret_share(&mut self, key: u64) -> u64 {
        self.host.world.secrets[key as usize]
            .as_mut()
            .unwrap()
            .holders += 1;
        key
    }

    /// Release one holder; the last refunds the designation's charge, and the
    /// last designation of sealed bytes refunds the seal (TLS-5).
    pub(super) fn secret_drop(&mut self, key: u64) {
        let slot = self.host.world.secrets[key as usize].as_mut().unwrap();
        slot.holders -= 1;
        if slot.holders != 0 {
            return;
        }
        let slot = self.host.world.secrets[key as usize].take().unwrap();
        let (l, n) = slot.charge;
        self.refund(l, n);
        drop(slot.secret);
        let (released, held): (Vec<_>, Vec<_>) = std::mem::take(&mut self.host.world.seals)
            .into_iter()
            .partition(|(token, ..)| token.strong_count() == 0);
        self.host.world.seals = held;
        for (_, l, n) in released {
            self.refund(l, n);
        }
    }
}
