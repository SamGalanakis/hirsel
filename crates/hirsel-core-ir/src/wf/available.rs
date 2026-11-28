//! A value or address is available at a block's entry if every incoming edge
//! defines it. A failing instruction defines its results only on success.
use super::{Body, Error, Terminator};
use crate::decode::{Debit, DecodeError};
use crate::extent::{visit_operands, visit_terminator_uses};

impl From<DecodeError> for Error {
    fn from(error: DecodeError) -> Self {
        match error {
            DecodeError::Limit(limit) => Self::Limit(limit),
            DecodeError::Malformed(reason) | DecodeError::Unsupported(reason) => {
                Self::Invalid(reason.into())
            }
        }
    }
}

pub(super) fn check(b: &Body, budget: &Debit<'_>) -> Result<(), Error> {
    let scratch = budget.scratch();
    let words = (b.values.len() + b.addresses.len()).div_ceil(64);
    let blocks = b.blocks.len();
    let mut states = Vec::<Option<Vec<u64>>>::new();
    scratch.reserve(&mut states, blocks)?;
    states.resize_with(blocks, || None);
    let mut current = Vec::new();
    scratch.reserve(&mut current, words)?;
    current.resize(words, 0u64);
    let mut initial = Vec::new();
    scratch.reserve(&mut initial, words)?;
    initial.resize(words, 0u64);
    if blocks > 0 {
        states[0] = Some(initial);
    }
    let mut queued = Vec::new();
    scratch.reserve(&mut queued, blocks)?;
    queued.resize(blocks, false);
    let mut work = Vec::new();
    scratch.reserve(&mut work, blocks)?;
    if blocks > 0 {
        work.push(0usize);
    }

    let define = |set: &mut [u64], k: usize| set[k / 64] |= 1 << (k % 64);
    let params = |bi: usize| b.blocks[bi].params.get(&b.pools.values).unwrap_or_default();
    let instructions = |bi: usize| b.blocks[bi].insts.get(&b.insts).unwrap_or_default();
    while let Some(bi) = work.pop() {
        scratch.check()?;
        queued[bi] = false;
        current.copy_from_slice(states[bi].as_ref().unwrap());
        for p in params(bi) {
            define(&mut current, p.index());
        }
        let mut send = |target: usize, current: &[u64]| -> Result<(), Error> {
            scratch.check()?;
            if target == 0 {
                return Ok(());
            }
            let changed = if let Some(old) = &mut states[target] {
                let mut changed = false;
                for (word, incoming) in old.iter_mut().zip(current) {
                    let next = *word & incoming;
                    changed |= *word != next;
                    *word = next;
                }
                changed
            } else {
                let mut state = Vec::new();
                scratch.reserve(&mut state, words)?;
                state.extend_from_slice(current);
                states[target] = Some(state);
                true
            };
            if changed && !queued[target] {
                queued[target] = true;
                work.push(target);
            }
            Ok(())
        };
        for inst in instructions(bi) {
            scratch.check()?;
            if let Some(fail) = inst.fail(b) {
                send(fail.index(), &current)?;
            }
            visit_operands(b, inst, |_| {}, |d| define(&mut current, d));
        }
        let mut edge = |e: crate::EdgeId| send(b.edges[e.index()].target.index(), &current);
        match b.terminators[bi] {
            Terminator::Jump(e) => edge(e)?,
            Terminator::Branch { yes, no, .. } => {
                edge(yes)?;
                edge(no)?;
            }
            Terminator::Switch {
                arms, otherwise, ..
            } => {
                for arm in arms.get(&b.pools.arms).unwrap_or_default() {
                    edge(arm.edge)?;
                }
                edge(otherwise)?;
            }
            Terminator::Raise {
                cleanup: Some(target),
                ..
            } => send(target.index(), &current)?,
            _ => {}
        }
    }

    for (bi, state) in states.iter().enumerate() {
        let Some(state) = state else { continue };
        current.copy_from_slice(state);
        for p in params(bi) {
            define(&mut current, p.index());
        }
        let validate = |missing: Option<usize>| -> Result<(), Error> {
            if let Some(k) = missing {
                let (prefix, index) = if k < b.values.len() {
                    ('v', k)
                } else {
                    ('a', k - b.values.len())
                };
                return Err(Error::Invalid(format!(
                    "b{bi}: {prefix}{index} is used on a path that does not define it"
                )));
            }
            Ok(())
        };
        for inst in instructions(bi) {
            scratch.check()?;
            let mut missing = None;
            visit_operands(
                b,
                inst,
                |u| {
                    if current[u / 64] >> (u % 64) & 1 == 0 {
                        missing.get_or_insert(u);
                    }
                },
                |_| {},
            );
            validate(missing)?;
            visit_operands(b, inst, |_| {}, |d| define(&mut current, d));
        }
        let mut missing = None;
        visit_terminator_uses(b, bi, |u| {
            if current[u / 64] >> (u % 64) & 1 == 0 {
                missing.get_or_insert(u);
            }
        });
        validate(missing)?;
    }
    scratch.check()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::decode::DecodeLimit;
    use crate::decode::{DecodeBudget, DecodeLimits};
    use std::time::Instant;

    fn valid_body() -> Body {
        let text = "core v1\nsig g0 = (u64) -> (u64)\ninst n0 = app@1/check/id [] []\n\
                    fn f0 = n0 g0 {\nb0(v0: u64):\njump b1(v0)\nb1(v1: u64):\nreturn v1\n}\n";
        crate::text::parse(text).unwrap().functions[0]
            .body
            .take()
            .unwrap()
    }

    #[test]
    fn valid_body_requires_scratch_and_an_unexpired_deadline() {
        let body = valid_body();
        let limits = DecodeLimits {
            bytes: 4096,
            members: 16,
            slots: 16,
            depth: 16,
            deadline: None,
        };
        for (bytes, deadline, expected) in [
            (0, None, DecodeLimit::Bytes),
            (limits.bytes, Some(Instant::now()), DecodeLimit::Deadline),
        ] {
            let budget = DecodeBudget::new(DecodeLimits {
                bytes,
                deadline,
                ..limits
            });
            assert_eq!(
                check(&body, &budget.debit()),
                Err(super::super::Error::Limit(expected))
            );
        }
        let budget = DecodeBudget::new(limits);
        check(&body, &budget.debit()).unwrap();
        budget
            .debit()
            .charge_bytes(limits.bytes)
            .expect("scratch is released");
    }
}
