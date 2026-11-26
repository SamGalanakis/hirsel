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
        body.extend([state.retired, u8::from(state.exported)]);
        put_varint(body, state.domains.len() as u64);
        for (space, domain) in state.domains {
            body.push(space);
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
        body.push(self.space);
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
        body.push(self.space);
    }
}

impl Resource for Mask {
    const TAG: u8 = 12;

    fn origin(&mut self) -> &mut Origin {
        &mut self.origin
    }

    fn write_snapshot(&self, body: &mut Vec<u8>) {
        body.extend(self.profile);
        body.extend([self.space, u8::from(self.local)]);
    }
}
