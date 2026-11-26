//! UDP pool refunds are tape inputs of the owner that folds the payer's mailbox.
use super::{schedule::Group, *};
use crate::{limit::Payer, udp_net::SocketCharge};
use std::sync::Arc;

/// A pool's `RETIRED`: the unit that records it, and the pool's name, which
/// is fixed where the pool was made.
#[derive(Clone, Copy)]
pub(crate) struct Retirement {
    group: Group,
    /// Survivor kind: 5 a socket's pool, 6 a group member's queue.
    kind: u8,
    /// The pool's home stream, whose object ordinal it is.
    stream: u32,
    pool: u64,
}

impl Retirement {
    /// A pool of the unit whose provider made `handle`; `queue` for a group
    /// member's queue. Taken before any host lock: it reads the unit's cell.
    pub(crate) fn new(provider: Provider, handle: u64, intake: bool, queue: bool) -> Self {
        let group = Group::root(cell_of(provider.state), generation_of(handle));
        Self {
            group,
            kind: if queue && !intake { 6 } else { 5 },
            stream: group.with(|live| units::stream(live)).unwrap_or(0),
            pool: pool_id(handle, intake),
        }
    }

    pub(crate) fn record(self) {
        if let Some(owner) = crate::tasks::current_group().filter(|g| g.same(self.group)) {
            owner.record_retirement(self.stream, self.pool);
        }
    }

    /// An owner adopts the pool at a helper's end take: its `RETIRED` is the
    /// owner's from then on, and still names the pool's home.
    pub(crate) fn adopt(&mut self) {
        if let Some(owner) = crate::tasks::current_group() {
            self.group = owner;
        }
    }

    /// Its name in a helper end's survivor list: kind, stream, ordinal.
    pub(crate) fn survivor(self) -> (u8, u32, u64) {
        (self.kind, self.stream, self.pool >> 1)
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
    group.with(|live| live.register_udp_pools(first, members, intake));
}

#[derive(Clone, Copy)]
pub(crate) struct ServedPoolSet {
    owner: Group,
    first: u64,
}

impl ServedPoolSet {
    pub(crate) fn member(self, index: usize) -> ServedPool {
        ServedPool {
            owner: self.owner,
            ordinal: ordinal_of(self.first) as u64 + index as u64,
        }
    }
}

pub(crate) struct ServedPool {
    owner: Group,
    ordinal: u64,
}

impl Drop for ServedPool {
    fn drop(&mut self) {
        self.owner
            .with(|live| live.close_udp_pool(self.ordinal).unwrap());
    }
}

pub(crate) fn served_udp_pools(
    members: &[Arc<SocketCharge>],
    intake: Option<Arc<SocketCharge>>,
) -> Option<ServedPoolSet> {
    let owner = crate::tasks::current_group()?;
    owner
        .with(|live| {
            let first = handle(owner.generation, live.objects.len() + 1);
            for _ in members {
                live.objects.push(Object {
                    inner: None,
                    open_resource: false,
                });
            }
            if live.mode == Mode::Replay {
                live.register_udp_pools(first, members, intake);
                return Some(ServedPoolSet { owner, first });
            }
            let stream = units::stream(live);
            let kind = if intake.is_some() { 6 } else { 5 };
            for (index, member) in members.iter().enumerate() {
                member.record_retirement(Retirement {
                    group: owner,
                    kind,
                    stream,
                    pool: pool_id(first + index as u64, false),
                });
            }
            if let Some(intake) = intake {
                intake.record_retirement(Retirement {
                    group: owner,
                    kind: 5,
                    stream,
                    pool: pool_id(first, true),
                });
            }
            None
        })
        .flatten()
}

impl Live {
    fn register_udp_pools(
        &mut self,
        first: u64,
        members: &[Arc<SocketCharge>],
        intake: Option<Arc<SocketCharge>>,
    ) {
        let intake_id = intake.map(|charge| {
            let id = pool_id(first, true);
            self.udp_retirements.insert(
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
            self.udp_retirements.insert(
                pool_id(first + index as u64, false),
                Pool {
                    charge: charge.clone(),
                    remaining_closes: 1,
                    intake: intake_id,
                },
            );
        }
    }

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
        let own = units::stream(self);
        let Some(replay) = self.replayer.as_mut().filter(|_| self.fault.is_none()) else {
            return;
        };
        while let Ok(&Frame::Retired {
            gap,
            task,
            stream,
            pool,
        }) = replay.peek()
        {
            if gap.checked_add(self.schedule.last) != Some(self.schedule.events) || task != owner {
                break;
            }
            // A pool adopted from a helper is named in its home stream, whose
            // pools a replay of this unit alone does not hold.
            if stream != own {
                self.schedule_fault(None);
                return;
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
