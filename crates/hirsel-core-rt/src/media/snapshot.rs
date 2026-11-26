//! Public state of the five media handles that move between units.
use super::{
    Head, Operand, Origin, Resource, Slot,
    schedule::{Mask, Opener, Schedule, Sealer},
};
use crate::secret::key_share::Share;
use hirsel_core_net::SframeDirection;
use hirsel_tape_format::put_varint;

pub(super) type Part = Result<Vec<u8>, &'static str>;

/// An arrival assigns the operand name for later operations in the receiver.
/// Its birth and payer stay with it.
///
/// # Safety
/// `word` is an exclusively held live Media slot.
pub(crate) unsafe fn snapshot(word: u64, arrival: Option<u64>) -> Part {
    // SAFETY: every live Media slot starts with its own snapshot callback.
    let write = unsafe { (*(word as *const Head)).snapshot };
    unsafe { write(word, arrival) }
}

/// # Safety
/// `word` names an exclusively held live slot of T.
pub(super) unsafe fn write<T: Resource>(word: u64, arrival: Option<u64>) -> Part {
    let value = unsafe { &mut (*(word as *mut Slot<T>)).value };
    let origin = value.origin();
    let (stream, delivery, admission) = origin
        .birth
        .ok_or("a key object was made outside a recorded unit")?;
    let ordinal = if let Some(ordinal) = arrival {
        origin.operand = Operand::Arrival(ordinal);
        ordinal
    } else {
        origin.arrival().unwrap_or(0)
    };
    let mut body = vec![T::TAG];
    put_varint(&mut body, ordinal);
    body.extend_from_slice(&stream.to_le_bytes());
    put_varint(&mut body, delivery);
    if delivery == 0 {
        put_varint(&mut body, admission);
    }
    value.write_snapshot(&mut body);
    Ok(body)
}

impl Resource for Share {
    const TAG: u8 = 8;

    fn origin(&mut self) -> &mut Origin {
        &mut self.tape
    }

    fn write_snapshot(&self, body: &mut Vec<u8>) {
        body.push(self.group as u8);
    }
}

impl Schedule {
    pub(super) fn advance_generation(&mut self, local: bool, by: u64) {
        let generation = &mut self.generations[usize::from(!local)];
        *generation = generation
            .checked_add(by)
            .unwrap_or_else(|| crate::process::stop(b"MediaGenerations"));
    }
}

impl Resource for Schedule {
    const TAG: u8 = 9;

    fn origin(&mut self) -> &mut Origin {
        &mut self.origin
    }

    fn write_snapshot(&self, body: &mut Vec<u8>) {
        let state = self.state.snapshot();
        body.extend(self.profile);
        body.extend([self.side, u8::from(state.application)]);
        for generation in self.generations {
            body.extend_from_slice(&generation.to_le_bytes());
        }
        body.extend([state.retired_bits(), u8::from(state.exported)]);
        put_varint(body, state.domains.len() as u64);
        for (space, domain) in state.domains {
            body.push(space.tag());
            body.extend_from_slice(&domain.to_le_bytes());
        }
        if let Ok(state) = self.state.sframe()
            && let SframeDirection::Send { kid, bits } = state.direction()
        {
            body.extend_from_slice(&kid.to_le_bytes());
            body.push(bits);
        }
    }
}

impl Resource for Sealer {
    const TAG: u8 = 10;

    fn origin(&mut self) -> &mut Origin {
        &mut self.origin
    }

    fn write_snapshot(&self, body: &mut Vec<u8>) {
        let (domain, floor, highest) = self.counter.snapshot();
        body.extend(self.profile);
        body.push(self.space.tag());
        body.extend_from_slice(&self.sframe_kid.unwrap_or(domain).to_le_bytes());
        body.extend_from_slice(&floor.to_le_bytes());
        body.push(u8::from(highest.is_some()));
        if let Some(highest) = highest {
            body.extend_from_slice(&highest.to_le_bytes());
        }
    }
}

impl Resource for Opener {
    const TAG: u8 = 11;

    fn origin(&mut self) -> &mut Origin {
        &mut self.origin
    }

    fn write_snapshot(&self, body: &mut Vec<u8>) {
        body.extend(self.profile);
        body.push(self.space.tag());
    }
}

impl Resource for Mask {
    const TAG: u8 = 12;

    fn origin(&mut self) -> &mut Origin {
        &mut self.origin
    }

    fn write_snapshot(&self, body: &mut Vec<u8>) {
        body.extend(self.profile);
        body.extend([self.space.tag(), u8::from(self.local)]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        Context,
        media::{Charges, KEY_OBJECT, schedule::admit},
    };
    use hirsel_core_net::{
        ImportUse,
        packet_epoch::{CIPHER_CHARGE, DOMAIN_CHARGE, SCHEDULE_CHARGE},
        packet_nonce::NonceFormat,
        packet_schedule::{self, ImportProfile, Space},
        secret_aead::AeadAlg,
        srtp_cipher::SrtpProfile,
        tls_kdf::Side,
    };
    use hirsel_tape_format::Value;
    use std::sync::Arc;

    // docs/recording.md fixes the Schedule layout and the packet space case order.
    #[test]
    fn schedule_snapshot_preserves_space_tags_and_retirement_bits() {
        let context = Context::new(4096);
        let charges = Arc::new(Charges::default());
        let pay = |bytes| charges.track(admit(&context, bytes).unwrap(), KEY_OBJECT);
        let check = |state, profile, side, tail: &[u8]| {
            let schedule = Schedule::new(state, 1, true, pay(SCHEDULE_CHARGE), profile, side);
            // Empty prefix, one resource, Schedule of ordinal 1, born in stream/delivery 1.
            let prefix = [0, 1, 1, 9, 1, 1, 0, 0, 0, 1];
            let mut body = prefix.to_vec();
            schedule.write_snapshot(&mut body);

            // Profile, side, application present, and zero generations.
            let mut expected = prefix.to_vec();
            expected.extend(profile);
            expected.extend([side, 1]);
            expected.extend([0; 16]);
            expected.extend(tail);
            assert_eq!(body, expected);
            assert!(Value::decode(&body).is_ok());
        };

        let mut tls = packet_schedule::Schedule::tls13(
            NonceFormat::Quic,
            AeadAlg::Aes128Gcm,
            Side::Client,
            None,
        );
        tls.application(&[0; 32]);
        tls.check_retire(Space::Handshake).unwrap();
        drop(tls.retire());
        tls.epoch(Space::Application)
            .unwrap()
            .sealer(0, 0, |bytes| {
                Ok((pay(bytes - DOMAIN_CHARGE), pay(DOMAIN_CHARGE)))
            })
            .unwrap();
        // Handshake bit, no export, one Application domain (zero).
        check(tls, [0, 0], 0, &[2, 0, 1, 2, 0, 0, 0, 0, 0, 0, 0, 0]);

        let mut srtp = ImportProfile::Srtp(SrtpProfile::Aes128CmHmacSha1_80)
            .import(ImportUse::SrtpSend, None)
            .unwrap();
        srtp.check_sealer_pair(Space::Rtcp, Space::Rtp, 7)
            .unwrap()
            .issue(
                [0, 0],
                std::array::from_fn(|_| (pay(CIPHER_CHARGE), pay(DOMAIN_CHARGE))),
            );
        // No retired spaces or export, two domains. RTP precedes RTCP on tape.
        check(
            srtp,
            [5, 0],
            2,
            &[
                0, 0, 2, 3, 7, 0, 0, 0, 0, 0, 0, 0, 4, 7, 0, 0, 0, 0, 0, 0, 0,
            ],
        );
    }
}
