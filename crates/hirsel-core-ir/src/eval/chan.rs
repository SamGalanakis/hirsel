//! Channels on the evaluator (CONC-9): a bounded queue, its counted Senders and
//! its one receiver end. The last end to go refunds the channel's admission.
use super::*;

pub(super) struct Chan {
    pub(super) id: u64,
    pub(super) item: TypeRef,
    pub(super) cap: u64,
    pub(super) queue: VecDeque<Vec<u8>>,
    pub(super) senders: u64,
    pub(super) receiver: End,
    pub(super) charge: u64,
    pub(super) limit: Option<Limit>,
}
/// A channel's receiver end. While `Cleaning` its queued items are dropped, and
/// a Sender among them never ends the channel: the receiver's cleanup does, last.
#[derive(Clone, Copy, PartialEq)]
pub(super) enum End {
    Open,
    Cleaning,
    Gone,
}
impl Machine<'_> {
    /// Take channel `i`'s front item into `dest`, or return false when its queue is
    /// empty. The slot it frees wakes this channel's senders in park order.
    pub(super) fn take(&mut self, i: usize, dest: u64) -> bool {
        let Some(bytes) = self.chans[i].queue.pop_front() else {
            return false;
        };
        self.put(dest, &bytes);
        self.event(RECEIVE, self.chan_id(i), 0);
        self.wake(Condition::Send(i));
        true
    }
    /// Put an item on channel `i` unless it is closed (2) or full (1): native
    /// `publish`, which wakes a receiver parked on this channel.
    pub(super) fn publish(&mut self, i: usize, value: u64, item: TypeId) -> u64 {
        if self.chans[i].receiver != End::Open {
            return 2;
        }
        if self.chans[i].queue.len() as u64 == self.chans[i].cap {
            return 1;
        }
        let n = self.held(TypeRef(item.0)) as usize;
        let bytes = self.get(value, n).to_vec();
        self.chans[i].queue.push_back(bytes);
        self.event(PUBLISH, self.chan_id(i), 0);
        self.wake(Condition::Receive(i));
        0
    }
    pub(super) fn chan_freed(&mut self, i: usize) {
        let c = &mut self.chans[i];
        if c.senders == 0 && c.receiver == End::Gone {
            let (l, n) = (c.limit.take().unwrap(), c.charge);
            self.refund(l, n);
        }
    }
}
