//! UDP pool refunds are tape inputs of the owner that folds the payer's mailbox.
use super::{schedule::Group, *};
use crate::{limit::Payer, udp_net::SocketCharge};
use std::sync::Arc;

#[derive(Clone, Copy)]
pub(crate) struct Retirement {
    group: Group,
    pool: u64,
}

impl Retirement {
    pub(crate) fn new(provider: Provider, handle: u64, intake: bool) -> Self {
        Self {
            group: Group::root(cell_of(provider.state), generation_of(handle)),
            pool: pool_id(handle, intake),
        }
    }

    pub(crate) fn record(self) {
        if let Some(owner) = crate::tasks::current_group().filter(|g| g.same(self.group)) {
            owner.record_retirement(self.pool);
        }
    }
}

fn pool_id(handle: u64, intake: bool) -> u64 {
    ((ordinal_of(handle) as u64) << 1) | u64::from(intake)
}

pub(super) struct Pool {
    charge: Arc<SocketCharge>,
    remaining_closes: usize,
    intake: Option<u64>,
}

pub(crate) fn register_udp_retirements(
    provider: Provider,
    first: u64,
    members: &[Arc<SocketCharge>],
    intake: Option<Arc<SocketCharge>>,
) {
    let group = Group::root(cell_of(provider.state), generation_of(first));
    group.with(|live| {
        let intake_id = intake.map(|charge| {
            let id = pool_id(first, true);
            live.udp_retirements.insert(
                id,
                Pool {
                    charge,
                    remaining_closes: members.len(),
                    intake: None,
                },
            );
            id
        });
        for (index, charge) in members.iter().enumerate() {
            live.udp_retirements.insert(
                pool_id(first + index as u64, false),
                Pool {
                    charge: charge.clone(),
                    remaining_closes: 1,
                    intake: intake_id,
                },
            );
        }
    });
}

impl Live {
    pub(super) fn close_udp_pool(&mut self, ordinal: u64) -> Result<(), Fault> {
        let entry = self
            .udp_retirements
            .get_mut(&(ordinal << 1))
            .ok_or(Fault::Corrupt)?;
        entry.remaining_closes = 0;
        if let Some(intake) = entry.intake {
            self.udp_retirements
                .get_mut(&intake)
                .unwrap()
                .remaining_closes -= 1;
        }
        Ok(())
    }

    pub(super) fn replay_retirements(&mut self, owner: u64, payer: Payer) {
        let Some(replay) = self.replayer.as_mut().filter(|_| self.fault.is_none()) else {
            return;
        };
        while let Ok(&Frame::Retired { gap, task, pool }) = replay.peek() {
            if gap.checked_add(self.schedule.last) != Some(self.schedule.events) || task != owner {
                break;
            }
            let Some(entry) = self
                .udp_retirements
                .get(&pool)
                .filter(|p| p.remaining_closes == 0)
            else {
                self.schedule_fault(None);
                return;
            };
            if !entry.charge.refund_replayed(payer) {
                self.schedule_fault(None);
                return;
            }
            replay.next().unwrap();
            self.schedule.last = self.schedule.events;
            self.udp_retirements.remove(&pool);
        }
    }
}
