//! Scalar locals with no address or representation observation can use the same
//! delayed NaN representation as internal ValueIds. This is local escape/use
//! analysis of the concrete body, not an ownership fact about borrowed storage.
use super::*;

pub(super) fn addresses(body: &Body) -> Vec<bool> {
    if !body
        .slots
        .iter()
        .any(|t| matches!(t, PlaceTy::Value(t) if t.is_float()))
    {
        return Vec::new();
    }
    let mut private: Vec<_> = body
        .slots
        .iter()
        .map(|t| matches!(t, PlaceTy::Value(t) if t.is_float()))
        .collect();
    let roots: Vec<_> = body
        .addresses
        .iter()
        .map(|a| match a.base {
            AddressBase::Slot(s) => Some(s),
            _ => None,
        })
        .collect();
    // Any address chain starts with a direct slot address. Reject its first
    // projection, even an unused one; no recursive pointer analysis is needed.
    for (i, a) in body.addresses.iter().enumerate() {
        match a.base {
            AddressBase::Slot(s) if a.steps.len != 0 || a.ty != body.slots[s.index()] => {
                private[s.index()] = false;
            }
            AddressBase::Address(parent) => reject(&mut private, &roots, parent),
            _ => {}
        }
        if let Some(s) = roots[i] {
            private[s.index()] &= a.steps.len == 0;
        }
    }
    for inst in &body.insts {
        match *inst {
            Inst::Pointer { address, .. }
            | Inst::Case { address, .. }
            | Inst::SetCase { address, .. }
            | Inst::Drop { address, .. }
            | Inst::Mark { address, .. }
            | Inst::Unmark { address, .. }
            | Inst::Grow {
                carrier: address, ..
            } => {
                reject(&mut private, &roots, address);
            }
            Inst::Copy { to, from, .. } => {
                reject(&mut private, &roots, to);
                reject(&mut private, &roots, from);
            }
            Inst::Read {
                result, address, ..
            } => {
                if let Some(s) = roots[address.index()] {
                    private[s.index()] &=
                        body.slots[s.index()] == PlaceTy::Value(body.values[result.index()]);
                }
            }
            Inst::Store { address, value } => {
                if let Some(s) = roots[address.index()] {
                    private[s.index()] &=
                        body.slots[s.index()] == PlaceTy::Value(body.values[value.index()]);
                }
            }
            _ => {}
        }
    }
    for call in &body.calls {
        for arg in call.args.get(&body.pools.args).unwrap() {
            if let Arg::Address(a) = *arg {
                reject(&mut private, &roots, a);
            }
        }
    }
    roots
        .into_iter()
        .map(|s| s.is_some_and(|s| private[s.index()]))
        .collect()
}

fn reject(private: &mut [bool], roots: &[Option<SlotId>], address: AddressId) {
    if let Some(s) = roots[address.index()] {
        private[s.index()] = false;
    }
}
