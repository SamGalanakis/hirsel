//! Evaluator adapters for the deterministic QUIC Initial schedule and packet operations.
use super::*;
use hirsel_core_net::{
    agreement_registry::RegistryVerdict,
    packet_aead::{self, PacketError, PacketLengths},
    packet_cipher::{HeaderMask, PacketCipher},
    packet_epoch::{self, Epoch, IssueError},
    packet_nonce::{ReceiverNonce, SenderCounter},
    packet_protection::{self, Algorithm, Key},
    packet_schedule::{self, ImportProfile},
    packet_sframe::Sframe,
    secret_aead::AeadAlg,
    srtp_cipher::SrtpProfile,
    tls_kdf::{Side, Tls13Handshake, Tls13PacketKeys, TlsHash, dtls12::Dtls12Keys},
};
use std::mem::MaybeUninit;

type Cipher = packet_protection::Cipher<PacketCipher>;

pub(super) enum Payload {
    Share(key_share::Share),
    Schedule(packet_schedule::Schedule<Limit>),
    Sealer(SenderCounter, Algorithm, Cipher),
    Opener(ReceiverNonce, Algorithm, Cipher),
    Mask(HeaderMask),
}

fn opener_payload(issued: packet_epoch::Opener<'_, Limit>) -> Payload {
    Payload::Opener(
        issued.nonce,
        issued.algorithm,
        Cipher::new(issued.algorithm, issued.key.unwrap()).unwrap(),
    )
}

pub(super) struct Media {
    owner: Limit,
    payload: Payload,
}

pub(super) struct RegistryCharge {
    payer: Limit,
    queue: Arc<Mutex<Vec<Limit>>>,
}

impl Drop for RegistryCharge {
    fn drop(&mut self) {
        self.queue.lock().unwrap().push(self.payer.clone());
    }
}

const REGISTRY_CHARGE: u64 = 48;

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

fn return_share(error: IssueError) -> Vec<u64> {
    let mut row = issue_error(error);
    row[2] = 1;
    row
}

fn sender(tag: u64) -> Side {
    if tag == 0 { Side::Client } else { Side::Server }
}

impl Machine<'_> {
    pub(super) fn media_hold(&mut self, owner: Limit, payload: Payload) -> Vec<u64> {
        let id = self.host.world.media.len() as u64;
        self.host.world.media.push(Some(Media { owner, payload }));
        vec![id, 0, 0, 0]
    }

    pub(super) fn drop_media(&mut self, id: u64) {
        let Media { owner, payload } = self.host.world.media[id as usize].take().unwrap();
        let bytes = match payload {
            Payload::Share(share) => {
                drop(share);
                self.refund_registry();
                64
            }
            Payload::Schedule(schedule) => {
                for paid in schedule.into_ledger() {
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
                let state = packet_schedule::Schedule::single(0, epoch);
                Ok(self.media_hold(payer, Payload::Schedule(state)))
            }
            HostOp::ScheduleImportTraffic
            | HostOp::ScheduleImportSrtp
            | HostOp::ScheduleImportSframe => Ok(self.import_schedule(payer, op, a)),
            HostOp::ScheduleSrtp => Ok(self.srtp_child(payer, a)),
            HostOp::ScheduleSpaceInfo => {
                let Payload::Schedule(schedule) = &self.host.world.media[a[0] as usize]
                    .as_ref()
                    .unwrap()
                    .payload
                else {
                    unreachable!()
                };
                Ok(schedule
                    .view_epoch(a[1])
                    .and_then(Epoch::info)
                    .map_or_else(issue_error, |(cipher, directions)| {
                        vec![cipher, 0, 0, directions]
                    }))
            }
            HostOp::ScheduleSealerPair => Ok(self.sealer_pair(payer, a)),
            HostOp::ScheduleTls13 => Ok(self.tls13(payer, a)),
            HostOp::ScheduleDtls12 => Ok(self.dtls12(payer, a)),
            HostOp::ScheduleSframeSealer | HostOp::ScheduleSframeOpener => {
                Ok(self.sframe_child(payer, op, a))
            }
            HostOp::ScheduleApplication
            | HostOp::ScheduleKeyUpdate
            | HostOp::ScheduleRetire
            | HostOp::ScheduleSframeRatchet => Ok(self.schedule_transition(op, a)),
            HostOp::ScheduleFinished | HostOp::ScheduleVerifyFinished => {
                Ok(self.schedule_finished(op, a))
            }
            HostOp::ScheduleSealer
            | HostOp::ScheduleOpener
            | HostOp::ScheduleNextOpener
            | HostOp::ScheduleHeaderMask => {
                let slot = a[0] as usize;
                let mut held = self.host.world.media[slot].take().unwrap();
                let Payload::Schedule(schedule) = &mut held.payload else {
                    unreachable!()
                };
                let child = match op {
                    HostOp::ScheduleSealer => schedule
                        .epoch(a[1])
                        .and_then(|epoch| {
                            epoch.sealer(a[2], a[3], |n| {
                                self.admit(&payer, n)
                                    .map(|()| (payer.clone(), payer.clone()))
                            })
                        })
                        .map(|issued| {
                            Payload::Sealer(
                                issued.counter,
                                issued.algorithm,
                                Cipher::new(issued.algorithm, issued.key.unwrap()).unwrap(),
                            )
                        }),
                    HostOp::ScheduleOpener => schedule
                        .view_epoch(a[1])
                        .and_then(|epoch| {
                            epoch.opener(|n| self.admit(&payer, n).map(|()| payer.clone()))
                        })
                        .map(opener_payload),
                    HostOp::ScheduleNextOpener => schedule.next_opener(
                        |n| self.admit(&payer, n).map(|()| payer.clone()),
                        opener_payload,
                    ),
                    _ => schedule
                        .view_epoch(a[1])
                        .and_then(|epoch| {
                            epoch.header_mask(a[2] != 0, |n| {
                                self.admit(&payer, n).map(|()| payer.clone())
                            })
                        })
                        .map(|(key, _)| Payload::Mask(key.unwrap())),
                };
                self.host.world.media[slot] = Some(held);
                Ok(match child {
                    Ok(payload) => self.media_hold(payer, payload),
                    Err(error) => issue_error(error),
                })
            }
            HostOp::PacketMask => {
                let Payload::Mask(mask) = &self.host.world.media[a[0] as usize]
                    .as_ref()
                    .unwrap()
                    .payload
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
                let mut held = self.host.world.media[slot].take().unwrap();
                let outcome = self.packet_call(&payer, op, a, &mut held.payload);
                self.host.world.media[slot] = Some(held);
                let (value, error, written) = packet_aead::outcome(outcome?);
                Ok(vec![value, error as u64, 0, written])
            }
        }
    }

    fn sealer_pair(&mut self, payer: Limit, a: &[u64]) -> Vec<u64> {
        let slot = a[0] as usize;
        let mut held = self.host.world.media[slot].take().unwrap();
        let Payload::Schedule(schedule) = &mut held.payload else {
            unreachable!()
        };
        let requested = 2 * (packet_epoch::CIPHER_CHARGE + packet_epoch::DOMAIN_CHARGE);
        let result = schedule
            .check_sealer_pair(a[1], a[2], a[3])
            .and_then(|pair| {
                self.admit(&payer, requested)
                    .map_err(|available| IssueError::Capacity {
                        requested,
                        available,
                    })?;
                Ok(pair
                    .issue(
                        [a[4], a[5]],
                        std::array::from_fn(|_| (payer.clone(), payer.clone())),
                    )
                    .map(|issued| {
                        Payload::Sealer(
                            issued.counter,
                            issued.algorithm,
                            Cipher::new(issued.algorithm, issued.key.unwrap()).unwrap(),
                        )
                    }))
            });
        self.host.world.media[slot] = Some(held);
        match result {
            Ok([first, second]) => {
                let a = self.media_hold(payer.clone(), first)[0];
                let b = self.media_hold(payer, second)[0];
                vec![a, 0, 0, b]
            }
            Err(error) => issue_error(error),
        }
    }

    /// `sframe_sealer` and `sframe_opener` (TLS-8, SFrame schedules).
    fn sframe_child(&mut self, payer: Limit, op: HostOp, a: &[u64]) -> Vec<u64> {
        let slot = a[0] as usize;
        let mut held = self.host.world.media[slot].take().unwrap();
        let Payload::Schedule(schedule) = &mut held.payload else {
            unreachable!()
        };
        let algorithm = Algorithm::Aead(AeadAlg::Aes128Gcm);
        let cipher = |keys: Option<Tls13PacketKeys>| {
            let keys = keys.expect("evaluator imports hold their bytes");
            Cipher::new(algorithm, Key::Aead(keys.payload())).unwrap()
        };
        let mut retired = Vec::new();
        let child = schedule.sframe_mut().and_then(|sframe| {
            if op == HostOp::ScheduleSframeSealer {
                sframe.check_sealer(a[1], a[2])?;
            } else {
                sframe.check_opener(a[1], a[2])?;
            }
            let requested = if op == HostOp::ScheduleSframeSealer {
                Sframe::<Limit>::SEALER_CHARGE
            } else {
                packet_epoch::CIPHER_CHARGE
            };
            self.admit(&payer, requested)
                .map_err(|available| IssueError::Capacity {
                    requested,
                    available,
                })?;
            Ok(if op == HostOp::ScheduleSframeSealer {
                let issued = sframe.sealer(a[1], a[2], a[3], (payer.clone(), payer.clone()));
                retired.extend(issued.retired.into_values());
                Payload::Sealer(issued.counter, algorithm, cipher(issued.keys))
            } else {
                let issued = sframe.opener(a[1], a[2], payer.clone());
                Payload::Opener(issued.nonce, algorithm, cipher(issued.keys))
            })
        });
        self.host.world.media[slot] = Some(held);
        for paid in retired {
            self.refund(paid, packet_epoch::DOMAIN_CHARGE);
        }
        match child {
            Ok(payload) => self.media_hold(payer, payload),
            Err(error) => issue_error(error),
        }
    }

    fn refund_registry(&mut self) {
        let refunds = std::mem::take(&mut *self.host.world.agreement_refunds.lock().unwrap());
        for payer in refunds {
            self.refund(payer, REGISTRY_CHARGE);
        }
    }

    fn tls13(&mut self, payer: Limit, a: &[u64]) -> Vec<u64> {
        let Some((format, algorithm)) = packet_epoch::traffic_profile(a[0], a[1]) else {
            return return_share(IssueError::Invalid);
        };
        let hash = TlsHash::for_aead(algorithm);
        if self.carrier_bytes(a[5]).len() != hash.output_len() {
            return return_share(IssueError::Invalid);
        }
        let side = sender(a[2]);
        let shared = match self.agree(&payer, a[3], side, a[4]) {
            Ok(shared) => shared,
            Err(error) => return error,
        };
        let labels = packet_epoch::tls13_labels(format).unwrap();
        let handshake =
            Tls13Handshake::new(hash, labels, &shared, self.carrier_bytes(a[5])).unwrap();
        let schedule = packet_schedule::Schedule::tls13(format, algorithm, side, Some(handshake));
        self.media_hold(payer, Payload::Schedule(schedule))
    }

    fn dtls12(&mut self, payer: Limit, a: &[u64]) -> Vec<u64> {
        let Some(algorithm) = packet_epoch::tls_suite(a[0]) else {
            return return_share(IssueError::Invalid);
        };
        let (Ok(client), Ok(server)) = (
            <[u8; 32]>::try_from(self.carrier_bytes(a[5])),
            <[u8; 32]>::try_from(self.carrier_bytes(a[6])),
        ) else {
            return return_share(IssueError::Invalid);
        };
        if a[7] != 0 && self.carrier_bytes(a[7]).len() != TlsHash::for_aead(algorithm).output_len()
        {
            return return_share(IssueError::Invalid);
        }
        let side = sender(a[1]);
        let shared = match self.agree(&payer, a[3], side, a[4]) {
            Ok(shared) => shared,
            Err(error) => return error,
        };
        let session = (a[7] != 0).then(|| self.carrier_bytes(a[7]));
        let keys = Dtls12Keys::new(algorithm, &shared, client, server, session).unwrap();
        let schedule = packet_schedule::Schedule::dtls12(a[2] as u16, algorithm, side, Some(keys));
        self.media_hold(payer, Payload::Schedule(schedule))
    }

    fn agree(
        &mut self,
        payer: &Limit,
        share: u64,
        side: Side,
        peer: u64,
    ) -> Result<zeroize::Zeroizing<[u8; 32]>, Vec<u64>> {
        let requested = packet_epoch::SCHEDULE_CHARGE + REGISTRY_CHARGE;
        if let Err(available) = self.admit(payer, requested) {
            return Err(return_share(IssueError::Capacity {
                requested,
                available,
            }));
        }
        let Media { owner, payload } = self.host.world.media[share as usize].take().unwrap();
        let Payload::Share(key_share::Share { key, ticket }) = payload else {
            unreachable!("checked KeyShare");
        };
        self.refund(owner, 64);
        let charge = RegistryCharge {
            payer: payer.clone(),
            queue: self.host.world.agreement_refunds.clone(),
        };
        let result = match key.agree(self.carrier_bytes(peer)) {
            Some(shared) => match ticket.resolve(side, &shared, charge) {
                RegistryVerdict::Accepted => Ok(shared),
                RegistryVerdict::Issued => Err(IssueError::Issued),
            },
            None => {
                drop((ticket, charge));
                Err(IssueError::Invalid)
            }
        };
        self.refund_registry();
        result.map_err(|error| {
            self.refund(payer.clone(), packet_epoch::SCHEDULE_CHARGE);
            issue_error(error)
        })
    }

    fn schedule_transition(&mut self, op: HostOp, a: &[u64]) -> Vec<u64> {
        let slot = a[0] as usize;
        let mut held = self.host.world.media[slot].take().unwrap();
        let Payload::Schedule(schedule) = &mut held.payload else {
            unreachable!()
        };
        let result = match op {
            HostOp::ScheduleApplication => {
                let hash = self.carrier_bytes(a[1]);
                schedule
                    .check_application(hash)
                    .map(|()| schedule.application(hash))
            }
            HostOp::ScheduleKeyUpdate => {
                let local = a[1] != 0;
                schedule.check_update(local).map(|()| {
                    for payer in schedule.key_update(local) {
                        self.refund(payer, packet_epoch::DOMAIN_CHARGE);
                    }
                })
            }
            HostOp::ScheduleRetire => schedule.check_retire(a[1]).map(|()| {
                for payer in schedule.retire() {
                    self.refund(payer, packet_epoch::DOMAIN_CHARGE);
                }
            }),
            HostOp::ScheduleSframeRatchet => schedule
                .sframe_mut()
                .and_then(|sframe| {
                    Sframe::<Limit>::check_ratchet(a[1])?;
                    Ok(sframe.advance(a[1]))
                })
                .map(|retired| {
                    for payer in retired.into_values() {
                        self.refund(payer, packet_epoch::DOMAIN_CHARGE);
                    }
                }),
            _ => unreachable!(),
        };
        self.host.world.media[slot] = Some(held);
        result.map_or_else(issue_error, |()| vec![0; 4])
    }

    fn schedule_finished(&mut self, op: HostOp, a: &[u64]) -> Vec<u64> {
        let Payload::Schedule(schedule) = &self.host.world.media[a[0] as usize]
            .as_ref()
            .unwrap()
            .payload
        else {
            unreachable!()
        };
        let hash = self.carrier_bytes(a[2]);
        let size = match schedule.finished_size(hash) {
            Ok(size) => size,
            Err(error) => return issue_error(error),
        };
        let of = sender(a[1]);
        if op == HostOp::ScheduleVerifyFinished {
            let valid = schedule.verify_finished(of, hash, self.carrier_bytes(a[3]), size);
            return vec![u64::from(valid), 0, 0, 0];
        }
        let (_, dst, len, cap) = self.carrier(a[3]);
        let available = cap - len;
        if available < size as u64 {
            return issue_error(IssueError::Capacity {
                requested: size as u64,
                available,
            });
        }
        let mut bytes = [0u8; 48];
        schedule.finished(of, hash, &mut bytes[..size]);
        self.put(dst + len, &bytes[..size]);
        self.store(a[3] + 8, 8, len + size as u64);
        vec![size as u64, 0, 0, 0]
    }

    fn import_schedule(&mut self, payer: Limit, op: HostOp, args: &[u64]) -> Vec<u64> {
        use hirsel_core_net::{ImportKind, ImportRefusal, ImportUse, SframeDirection};
        let (kind, profile, suite, key) = match op {
            HostOp::ScheduleImportTraffic => (ImportKind::Traffic, args[0], args[1], args[2]),
            HostOp::ScheduleImportSrtp => (ImportKind::Srtp, args[0], 0, args[1]),
            _ => (ImportKind::Sframe, 0, args[0], args[1]),
        };
        let secret = self.secret(key).expect("a live Secret designation");
        let authority = secret.policy.import();
        if authority.is_none_or(|use_| use_.kind() != kind) {
            return vec![0, 1, 1, 0];
        }
        let entry = match authority {
            Some(ImportUse::Sframe(SframeDirection::Send { .. })) => REGISTRY_CHARGE,
            _ => 0,
        };
        let mark = secret
            .import
            .clone()
            .expect("import authority carries a spent mark");
        let request = packet_epoch::SCHEDULE_CHARGE + entry;
        let issued = mark.issue(kind, || {
            let parsed =
                ImportProfile::from_case(kind, profile, suite).ok_or(IssueError::Invalid)?;
            self.admit(&payer, request)
                .map_err(|available| IssueError::Capacity {
                    requested: request,
                    available,
                })?;
            Ok(parsed)
        });
        let (authority, profile, spending) = match issued {
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
        let schedule = spending.bytes(|bytes| {
            let bytes = bytes.expect("evaluator imports hold their bytes");
            let schedule = profile
                .import(authority, Some(bytes))
                .map_err(|_| IssueError::Invalid)?;
            if entry != 0 {
                self.host
                    .world
                    .send_keys
                    .register(bytes, payer.clone())
                    .map_err(|_| IssueError::Spent)?;
            }
            Ok(schedule)
        });
        drop(spending);
        self.secret_drop(key);
        match schedule {
            Ok(schedule) => self.media_hold(payer, Payload::Schedule(schedule)),
            Err(error) => {
                self.refund(payer, request);
                issue_error(error)
            }
        }
    }

    fn srtp_child(&mut self, payer: Limit, args: &[u64]) -> Vec<u64> {
        let Some(profile) = SrtpProfile::from_case(args[1]) else {
            return issue_error(IssueError::Invalid);
        };
        let slot = args[0] as usize;
        let mut held = self.host.world.media[slot].take().unwrap();
        let Payload::Schedule(parent) = &mut held.payload else {
            unreachable!("checked schedule");
        };
        let requested = packet_epoch::SCHEDULE_CHARGE;
        let child = parent.check_srtp().and_then(|()| {
            self.admit(&payer, requested)
                .map_err(|available| IssueError::Capacity {
                    requested,
                    available,
                })?;
            Ok(parent.srtp(profile))
        });
        self.host.world.media[slot] = Some(held);
        match child {
            Ok(child) => self.media_hold(payer, Payload::Schedule(child)),
            Err(error) => issue_error(error),
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
                (counter.packet_nonce(a[1]), *algorithm, &*cipher)
            }
            Payload::Opener(base, algorithm, cipher) => {
                (base.packet_nonce(a[1], a[2]), *algorithm, &*cipher)
            }
            _ => unreachable!(),
        };
        let nonce = match nonce {
            Ok(nonce) => nonce,
            Err(error) => return Ok(Err(error.into())),
        };
        let tag_len = algorithm.tag_len();
        let body = (len - at) as usize;
        let plain = if seals {
            body
        } else {
            body.saturating_sub(tag_len)
        };
        let lengths = PacketLengths::new(plain, header.len(), extra.len());
        if let Err(error) = lengths.check(algorithm) {
            return Ok(Err(error));
        }
        let spare = (capacity - written) as usize;
        let appended = match op {
            HostOp::PacketSeal => lengths.spare(tag_len, spare).map(|()| tag_len),
            HostOp::PacketSealInto => packet_aead::sealed_len(plain, tag_len, spare),
            HostOp::PacketOpenInto => lengths.spare(plain, spare).map(|()| plain),
            _ => Ok(0),
        };
        let appended = match appended {
            Ok(n) => n,
            Err(error) => return Ok(Err(error)),
        };
        let scratch = lengths.scratch(algorithm) as u64;
        if scratch != 0
            && let Err(available) = self.admit(payer, scratch)
        {
            return Ok(Err(PacketError::Capacity {
                requested: scratch,
                available,
            }));
        }
        let header = &self.carrier_bytes(aad_source)[aad_at as usize..aad_end as usize];
        let extra = self.carrier_bytes(extra_source);
        let joined;
        let aad = match algorithm {
            Algorithm::Cm => (header, extra),
            Algorithm::Aead(_) if header.is_empty() => (extra, [].as_slice()),
            Algorithm::Aead(_) if extra.is_empty() => (header, [].as_slice()),
            Algorithm::Aead(_) => {
                joined = [header, extra].concat();
                (joined.as_slice(), [].as_slice())
            }
        };
        let mut bytes = self.carrier_bytes(src)[at as usize..].to_vec();
        let outcome = if seals {
            let tag = cipher
                .seal(nonce, aad, &mut bytes)
                .expect("validated packet sizes");
            if let Payload::Sealer(counter, ..) = payload {
                counter.commit(a[1]);
            }
            bytes.extend_from_slice(&tag[..tag_len]);
            self.put(if into { output + written } else { input + at }, &bytes);
            self.store(dst + 8, 8, written + appended as u64);
            Ok(appended)
        } else if body < tag_len {
            Err(PacketError::Tampered)
        } else if into {
            let mut out = vec![MaybeUninit::uninit(); plain];
            match cipher.open_into(nonce, aad, &bytes, &mut out) {
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
            let opened = cipher.open(nonce, aad, &mut bytes);
            if plain != 0 {
                self.put(input + at, &bytes[..plain]);
            }
            match opened {
                Ok(_) => {
                    self.store(dst + 8, 8, len - tag_len as u64);
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

impl crate::World {
    /// Sealed secrets and the SFrame send-key registry are held until the run ends.
    pub(crate) fn end_run_holds(&mut self) {
        for (_, l, n) in std::mem::take(&mut self.seals) {
            l.find().subtract(n);
        }
        for l in std::mem::take(&mut self.send_keys).end() {
            l.find()
                .subtract(hirsel_core_net::packet_sframe::REGISTRY_CHARGE);
        }
    }
}
