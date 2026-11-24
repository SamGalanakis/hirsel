use super::*;
use hirsel_core_net::key_agreement::{Group, KeyShare};

pub(super) struct Share {
    _key: KeyShare,
    payer: Limit,
}

impl Machine<'_> {
    pub(super) fn key_share(&mut self, t: &Task, a: &[u64]) -> X<Vec<u64>> {
        let group = Group::from_case(a[1]).expect("a checked Group");
        let size = group.public_len() as u64;
        let (_, dst, len, cap) = self.carrier(a[2]);
        let spare = cap - len;
        if spare < size {
            return Ok(vec![size, 4, 0, spare]);
        }
        let payer = t.limits.last().unwrap().clone();
        if let Err(available) = self.admit(&payer, 64) {
            return Ok(vec![64, 4, 0, available]);
        }
        let mut point = [0; 65];
        let point = &mut point[..size as usize];
        let key = if let Some(cell) = self.cell(a[0]) {
            let VirtualCell::Random(stream) = &mut self.world.cells[cell] else {
                unreachable!("a Random cell");
            };
            match KeyShare::from_draws(
                group,
                |draw| stream.fill(draw).then_some(()).ok_or(()),
                point,
            ) {
                Ok(key) => key,
                Err(()) => {
                    self.refund(payer, 64);
                    return Err(Stop::Cause("Bounds"));
                }
            }
        } else {
            KeyShare::host(group, point).unwrap_or_else(|_| {
                std::panic::resume_unwind(Box::new(RunStop(
                    "secret.key_share: OS key generation: ring Unspecified",
                )))
            })
        };
        self.put(dst + len, point);
        self.store(a[2] + 8, 8, len + size);
        let id = self.world.key_shares.len() as u64;
        self.world.key_shares.push(Some(Share { _key: key, payer }));
        Ok(vec![id, 0, 0, 0])
    }

    pub(super) fn drop_media(&mut self, id: u64) {
        let share = self.world.key_shares[id as usize].take().unwrap();
        self.refund(share.payer, 64);
    }
}
