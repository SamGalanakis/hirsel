//! Evaluator adapters for the deterministic QUIC Initial schedule and packet operations.
use super::*;
use hirsel_core_net::{
    packet_aead::{self, PacketError, PacketLengths},
    packet_cipher::{HeaderMask, PacketCipher, TAG_LEN},
    packet_epoch::{self, Epoch, IssueError},
    packet_nonce::{ReceiverNonce, SenderCounter},
    secret_aead::AeadAlg,
    tls_kdf::Side,
};
use std::mem::MaybeUninit;

pub(super) enum Payload {
    Share(key_share::Share),
    Schedule { epoch: Epoch<Limit>, space: u64 },
    Sealer(SenderCounter, AeadAlg, PacketCipher),
    Opener(ReceiverNonce, AeadAlg, PacketCipher),
    Mask(HeaderMask),
}

pub(super) struct Media {
    owner: Limit,
    payload: Payload,
}

fn issue_error(error: IssueError) -> Vec<u64> {
    match error {
        IssueError::Invalid => vec![0, 2, 0, 0],
        IssueError::Issued => vec![0, 3, 0, 0],
        IssueError::Spent => vec![0, 4, 0, 0],
        IssueError::Capacity {
            requested,
            available,
        } => vec![requested, 5, 0, available],
    }
}

impl Machine<'_> {
    pub(super) fn media_hold(&mut self, owner: Limit, payload: Payload) -> Vec<u64> {
        let id = self.world.media.len() as u64;
        self.world.media.push(Some(Media { owner, payload }));
        vec![id, 0, 0, 0]
    }

    pub(super) fn drop_media(&mut self, id: u64) {
        let Media { owner, payload } = self.world.media[id as usize].take().unwrap();
        let bytes = match payload {
            Payload::Share(_share) => 64,
            Payload::Schedule { epoch, .. } => {
                for paid in epoch.into_ledger() {
                    self.refund(paid, packet_epoch::DOMAIN_CHARGE);
                }
                packet_epoch::SCHEDULE_CHARGE
            }
            Payload::Sealer(..) | Payload::Opener(..) => packet_epoch::CIPHER_CHARGE,
            Payload::Mask(_) => packet_epoch::MASK_CHARGE,
        };
        self.refund(owner, bytes);
    }

    pub(super) fn media_call(&mut self, t: &Task, op: HostOp, a: &[u64]) -> X<Vec<u64>> {
        let payer = t.limits.last().unwrap().clone();
        match op {
            HostOp::ScheduleQuicInitial => {
                let side = if a[0] == 0 {
                    Side::Client
                } else {
                    Side::Server
                };
                if self.carrier_bytes(a[1]).len() > 20 {
                    return Ok(issue_error(IssueError::Invalid));
                };
                let requested = packet_epoch::SCHEDULE_CHARGE;
                if let Err(available) = self.admit(&payer, requested) {
                    return Ok(issue_error(IssueError::Capacity {
                        requested,
                        available,
                    }));
                }
                let epoch = Epoch::quic_initial(side, self.carrier_bytes(a[1])).unwrap();
                Ok(self.media_hold(payer, Payload::Schedule { epoch, space: 0 }))
            }
            HostOp::ScheduleImportTraffic => Ok(self.import_traffic(payer, a)),
            HostOp::ScheduleSealer | HostOp::ScheduleOpener | HostOp::ScheduleHeaderMask => {
                let slot = a[0] as usize;
                let mut held = self.world.media[slot].take().unwrap();
                let Payload::Schedule { epoch, space } = &mut held.payload else {
                    unreachable!()
                };
                if a[1] != *space {
                    self.world.media[slot] = Some(held);
                    return Ok(issue_error(IssueError::Invalid));
                }
                let child = match op {
                    HostOp::ScheduleSealer => epoch
                        .sealer(a[2], a[3], |n| {
                            self.admit(&payer, n)
                                .map(|()| (payer.clone(), payer.clone()))
                        })
                        .map(|issued| {
                            Payload::Sealer(
                                issued.counter,
                                issued.algorithm,
                                PacketCipher::new(issued.algorithm, issued.key.unwrap()).unwrap(),
                            )
                        }),
                    HostOp::ScheduleOpener => epoch
                        .opener(|n| self.admit(&payer, n).map(|()| payer.clone()))
                        .map(|issued| {
                            Payload::Opener(
                                issued.nonce,
                                issued.algorithm,
                                PacketCipher::new(issued.algorithm, issued.key.unwrap()).unwrap(),
                            )
                        }),
                    _ => epoch
                        .header_mask(a[2] != 0, |n| self.admit(&payer, n).map(|()| payer.clone()))
                        .map(|(key, _)| Payload::Mask(key.unwrap())),
                };
                self.world.media[slot] = Some(held);
                Ok(match child {
                    Ok(payload) => self.media_hold(payer, payload),
                    Err(error) => issue_error(error),
                })
            }
            HostOp::PacketMask => {
                let Payload::Mask(mask) =
                    &self.world.media[a[0] as usize].as_ref().unwrap().payload
                else {
                    unreachable!()
                };
                let sample = packet_aead::sample_at(self.carrier_bytes(a[1]), a[2])
                    .ok_or(Stop::Cause("Bounds"))?;
                let bytes = mask.mask(sample);
                self.put(a[3], &bytes);
                Ok(vec![0; 4])
            }
            _ => {
                let slot = a[0] as usize;
                let mut held = self.world.media[slot].take().unwrap();
                let outcome = self.packet_call(&payer, op, a, &mut held.payload);
                self.world.media[slot] = Some(held);
                let (value, error, written) = packet_aead::outcome(outcome?);
                Ok(vec![value, error as u64, 0, written])
            }
        }
    }

    fn import_traffic(&mut self, payer: Limit, args: &[u64]) -> Vec<u64> {
        use hirsel_core_net::{ImportKind, ImportRefusal, packet_epoch::traffic_profile};
        let key = args[2];
        let secret = self.secret(key).expect("a live Secret designation");
        if secret
            .policy
            .uses
            .import()
            .is_none_or(|use_| use_.kind() != ImportKind::Traffic)
        {
            return vec![0, 1, 1, 0];
        }
        let mark = secret
            .import
            .clone()
            .expect("import authority carries a spent mark");
        let bytes = secret.bytes.clone();
        let request = packet_epoch::SCHEDULE_CHARGE;
        let issued = mark.issue(ImportKind::Traffic, || {
            let parsed = traffic_profile(args[0], args[1]).ok_or(IssueError::Invalid)?;
            self.admit(&payer, request)
                .map_err(|available| IssueError::Capacity {
                    requested: request,
                    available,
                })?;
            Ok(parsed)
        });
        let (authority, (format, algorithm)) = match issued {
            Ok(issued) => issued,
            Err(error) => {
                let mut row = match error {
                    ImportRefusal::Denied => vec![0, 1, 0, 0],
                    ImportRefusal::Spent => issue_error(IssueError::Spent),
                    ImportRefusal::BeforeIssue(error) => issue_error(error),
                };
                row[2] = 1;
                return row;
            }
        };
        let epoch = Epoch::import_traffic(format, algorithm, authority, &bytes);
        drop(bytes);
        self.secret_drop(key);
        match epoch {
            Ok(epoch) => self.media_hold(payer, Payload::Schedule { epoch, space: 2 }),
            Err(_) => {
                self.refund(payer, request);
                issue_error(IssueError::Invalid)
            }
        }
    }

    fn packet_call(
        &mut self,
        payer: &Limit,
        op: HostOp,
        a: &[u64],
        payload: &mut Payload,
    ) -> X<Result<usize, PacketError>> {
        // All numbers are u64 until they have been checked against carrier lengths.
        let (src, dst, aad_at, at, extra) = match op {
            HostOp::PacketSeal => (a[2], a[2], a[3], a[4], a[5]),
            HostOp::PacketSealInto => (a[2], a[4], a[5], a[3], a[6]),
            HostOp::PacketOpen => (a[3], a[3], a[4], a[5], a[6]),
            HostOp::PacketOpenInto => (a[3], a[7], a[4], a[5], a[6]),
            _ => unreachable!(),
        };
        let (_, input, len, _) = self.carrier(src);
        let (_, output, written, capacity) = self.carrier(dst);
        let aad_end = if op == HostOp::PacketSealInto {
            written
        } else {
            at
        };
        let aad_source = if op == HostOp::PacketSealInto {
            dst
        } else {
            src
        };
        if at > len || aad_at > aad_end {
            return Err(Stop::Cause("Bounds"));
        }
        let header = &self.carrier_bytes(aad_source)[aad_at as usize..aad_end as usize];
        let extra_source = extra;
        let extra = self.carrier_bytes(extra_source);
        let seals = matches!(op, HostOp::PacketSeal | HostOp::PacketSealInto);
        let into = matches!(op, HostOp::PacketSealInto | HostOp::PacketOpenInto);
        let (nonce, algorithm, cipher) = match payload {
            Payload::Sealer(counter, algorithm, cipher) => {
                (counter.check(a[1]), *algorithm, &*cipher)
            }
            Payload::Opener(base, algorithm, cipher) => {
                (base.nonce(a[1], a[2]), *algorithm, &*cipher)
            }
            _ => unreachable!(),
        };
        let nonce = match nonce {
            Ok(nonce) => nonce,
            Err(error) => return Ok(Err(error.into())),
        };
        let body = (len - at) as usize;
        let plain = if seals {
            body
        } else {
            body.saturating_sub(TAG_LEN)
        };
        let lengths = PacketLengths::new(plain, header.len(), extra.len());
        if let Err(error) = lengths.check(algorithm) {
            return Ok(Err(error));
        }
        let spare = (capacity - written) as usize;
        let appended = match op {
            HostOp::PacketSeal => lengths.spare(TAG_LEN, spare).map(|()| TAG_LEN),
            HostOp::PacketSealInto => packet_aead::sealed_len(plain, spare),
            HostOp::PacketOpenInto => lengths.spare(plain, spare).map(|()| plain),
            _ => Ok(0),
        };
        let appended = match appended {
            Ok(n) => n,
            Err(error) => return Ok(Err(error)),
        };
        let scratch = lengths.scratch() as u64;
        if scratch != 0
            && let Err(available) = self.admit(payer, scratch)
        {
            return Ok(Err(PacketError::Capacity {
                requested: scratch,
                available,
            }));
        }
        let header = &self.carrier_bytes(aad_source)[aad_at as usize..aad_end as usize];
        let aad = [header, self.carrier_bytes(extra_source)].concat();
        let mut bytes = self.carrier_bytes(src)[at as usize..].to_vec();
        let outcome = if seals {
            let tag = cipher
                .seal(nonce, &aad, &mut bytes)
                .expect("validated packet sizes");
            if let Payload::Sealer(counter, ..) = payload {
                counter.commit(a[1]);
            }
            bytes.extend(tag);
            self.put(if into { output + written } else { input + at }, &bytes);
            self.store(dst + 8, 8, written + appended as u64);
            Ok(appended)
        } else if body < TAG_LEN {
            Err(PacketError::Tampered)
        } else if into {
            let mut out = vec![MaybeUninit::uninit(); plain];
            match cipher.open_into(nonce, &aad, &bytes, &mut out) {
                Ok(()) => {
                    // SAFETY: a successful open initializes the whole destination.
                    let out = unsafe { std::slice::from_raw_parts(out.as_ptr().cast(), plain) };
                    if !out.is_empty() {
                        self.put(output + written, out);
                    }
                    self.store(dst + 8, 8, written + plain as u64);
                    Ok(plain)
                }
                Err(_) => Err(PacketError::Tampered),
            }
        } else {
            let (body, tag) = bytes.split_at_mut(plain);
            let opened = cipher.open(nonce, &aad, body, (&*tag).try_into().unwrap());
            if !body.is_empty() {
                self.put(input + at, body);
            }
            match opened {
                Ok(()) => {
                    self.store(dst + 8, 8, len - TAG_LEN as u64);
                    Ok(plain)
                }
                Err(_) => Err(PacketError::Tampered),
            }
        };
        if scratch != 0 {
            self.refund(payer.clone(), scratch);
        }
        Ok(outcome)
    }
}
