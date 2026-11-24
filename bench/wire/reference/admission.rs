use super::{Bucket, Limits, Principal};
use crate::codec::{Bad, Done, Frame, Outcome, Refusal, StreamId};
use crate::{BITS, Q};
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio::time::Instant;

struct HeldSend {
    unit: OwnedSemaphorePermit,
    until: Instant,
    settled: bool,
}
type Owed = HashMap<(StreamId, u64), VecDeque<HeldSend>>;
pub(super) type Rates = Arc<Mutex<HashMap<Principal, Bucket>>>;

pub(super) struct Calls {
    slots: Arc<Semaphore>,
    refusals: Arc<Semaphore>,
    owed: Mutex<Owed>,
    principal: Principal,
    entries: Vec<Option<usize>>,
    limits: Limits,
    rates: Rates,
}

pub(super) struct HeldRefusal {
    pub done: Done,
    pub unit: OwnedSemaphorePermit,
}
pub(super) enum Admission {
    Forward,
    Refused(HeldRefusal),
    Close,
}

impl Calls {
    pub fn new(
        principal: Principal,
        entries: Vec<Option<usize>>,
        limits: Limits,
        rates: Rates,
    ) -> Self {
        Self {
            slots: Arc::new(Semaphore::new(1 + Q)),
            refusals: Arc::new(Semaphore::new(1 + Q)),
            owed: Mutex::new(HashMap::new()),
            principal,
            entries,
            limits,
            rates,
        }
    }

    pub fn admit(
        &self,
        stream: StreamId,
        p: u64,
        slot: u32,
        deadline: u32,
        arg: &[u8],
    ) -> Admission {
        let refusal = if self.entries.get(slot as usize) != Some(&Some(0)) {
            Some(Refusal::NoEntry)
        } else if arg.len() != 4 || u32::from_le_bytes(arg.try_into().unwrap()) >= BITS {
            Some(Refusal::Malformed(Bad { kind: 6, at: 0 }))
        } else if deadline == 0 {
            Some(Refusal::Deadline)
        } else {
            let mut rates = self.rates.lock().unwrap();
            let bucket = rates.entry(self.principal).or_insert_with(|| Bucket {
                tokens: f64::from(self.limits.principal_burst),
                at: std::time::Instant::now(),
            });
            (!bucket.take(self.limits.principal_rate, self.limits.principal_burst))
                .then_some(Refusal::Overloaded)
        };
        if let Some(refusal) = refusal {
            return self.refuse(stream, p, refusal);
        }
        let Ok(unit) = self.slots.clone().try_acquire_owned() else {
            return self.refuse(stream, p, Refusal::Overloaded);
        };
        self.owed
            .lock()
            .unwrap()
            .entry((stream, p))
            .or_default()
            .push_back(HeldSend {
                unit,
                until: Instant::now() + Duration::from_millis(u64::from(deadline)),
                settled: false,
            });
        Admission::Forward
    }

    fn refuse(&self, stream: StreamId, p: u64, reason: Refusal) -> Admission {
        let Ok(unit) = self.refusals.clone().try_acquire_owned() else {
            return Admission::Close;
        };
        Admission::Refused(HeldRefusal {
            done: Done {
                stream,
                p,
                outcome: Outcome::Refused(reason),
            },
            unit,
        })
    }

    pub fn settle_at_delivery(&self, frame: &mut Frame) {
        let done = match frame {
            Frame::Batch(_, _, _, d) | Frame::Reset(_, d) => d,
            _ => return,
        };
        let mut book = self.owed.lock().unwrap();
        for d in done {
            let send = book
                .get_mut(&(d.stream, d.p))
                .unwrap()
                .iter_mut()
                .find(|s| !s.settled)
                .unwrap();
            if Instant::now() >= send.until {
                d.outcome = Outcome::Abandoned;
            }
            send.settled = true;
        }
    }

    /// Called when the writer takes the frame, before any socket await.
    pub fn hand_to_writer(&self, frame: &mut Frame) {
        let done = match frame {
            Frame::Batch(_, _, _, d) | Frame::Reset(_, d) => d,
            _ => return,
        };
        let mut book = self.owed.lock().unwrap();
        for d in done {
            let sends = book.get_mut(&(d.stream, d.p)).unwrap();
            let send = sends.pop_front().unwrap();
            drop(send.unit);
            if sends.is_empty() {
                book.remove(&(d.stream, d.p));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn stream(n: u64) -> StreamId {
        StreamId { t: 0, n }
    }
    #[test]
    fn writer_delay_does_not_rewrite_a_chosen_outcome() {
        let calls = Calls::new(
            [0; 16],
            vec![Some(0)],
            Limits::benchmark(),
            Arc::new(Mutex::new(HashMap::new())),
        );
        assert!(matches!(
            calls.admit(stream(0), 0, 0, 30000, &0u32.to_le_bytes()),
            Admission::Forward
        ));
        let answer = Outcome::Returned(vec![0, 0, 0, 0, 1]);
        let mut frame = Frame::Batch(
            1,
            vec![],
            vec![],
            vec![Done {
                stream: stream(0),
                p: 0,
                outcome: answer.clone(),
            }],
        );
        // The answer is chosen before expiry, then waits in the outgoing queue.
        calls.settle_at_delivery(&mut frame);
        calls.owed.lock().unwrap().get_mut(&(stream(0), 0)).unwrap()[0].until =
            Instant::now() - Duration::from_millis(1);
        calls.hand_to_writer(&mut frame);
        assert_eq!(
            frame,
            Frame::Batch(
                1,
                vec![],
                vec![],
                vec![Done {
                    stream: stream(0),
                    p: 0,
                    outcome: answer
                }]
            )
        );
    }
    #[test]
    fn writer_handoff_frees_units_and_sixth_unsent_refusal_closes() {
        let calls = Calls::new(
            [0; 16],
            vec![Some(0)],
            Limits::benchmark(),
            Arc::new(Mutex::new(HashMap::new())),
        );
        for p in 0..=4 {
            assert!(matches!(
                calls.admit(stream(0), p, 0, 30000, &0u32.to_le_bytes()),
                Admission::Forward
            ));
        }
        let mut held = Vec::new();
        for p in 5..=9 {
            let Admission::Refused(r) = calls.admit(stream(0), p, 0, 30000, &0u32.to_le_bytes())
            else {
                panic!()
            };
            assert_eq!(r.done.outcome, Outcome::Refused(Refusal::Overloaded));
            held.push(r);
        }
        assert!(matches!(
            calls.admit(stream(0), 10, 0, 30000, &0u32.to_le_bytes()),
            Admission::Close
        ));
        let mut frame = Frame::Batch(
            1,
            vec![],
            vec![],
            vec![Done {
                stream: stream(0),
                p: 0,
                outcome: Outcome::Returned(vec![0, 0, 0, 0, 1]),
            }],
        );
        calls.hand_to_writer(&mut frame);
        // The next send can enter before the writer has flushed its current socket frame.
        assert!(matches!(
            calls.admit(stream(0), 11, 0, 30000, &0u32.to_le_bytes()),
            Admission::Forward
        ));
        drop(held);
        assert!(matches!(
            calls.admit(stream(0), 12, 99, 30000, &0u32.to_le_bytes()),
            Admission::Refused(_)
        ));
    }

    #[test]
    fn principal_rate_is_shared_and_a_refusal_does_not_take_a_call_unit() {
        let limits = Limits {
            principal_rate: 0,
            principal_burst: 1,
            ..Limits::default()
        };
        let rates = Arc::new(Mutex::new(HashMap::new()));
        let a = Calls::new([1; 16], vec![Some(0)], limits, rates.clone());
        let b = Calls::new([1; 16], vec![Some(0)], limits, rates);
        assert!(matches!(
            a.admit(stream(0), 0, 0, 30000, &0u32.to_le_bytes()),
            Admission::Forward
        ));
        let Admission::Refused(r) = b.admit(stream(0), 0, 0, 30000, &0u32.to_le_bytes()) else {
            panic!()
        };
        assert_eq!(r.done.outcome, Outcome::Refused(Refusal::Overloaded));
        assert_eq!(b.slots.available_permits(), 1 + Q);
    }
}
