use hirsel_run_schema::BudgetKind;

/// The most retained values a CPU publication keeps.
pub const RETAINED_MAX: usize = 1024;
/// The most pieces one stretch keeps.
pub const PIECES_MAX: usize = 64;

/// Which budgets a run has, so a summary keeps only what settlement reads.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Budgets {
    pub memory: bool,
}

/// An event a stretch cannot summarise within its bounds: the segment pauses until head.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Pause {
    Pieces,
}

/// One piece: the charges between two credit events.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Piece {
    /// Net bytes: allocations less frees.
    pub net: i64,
    /// Maximum positive storage above the piece's starting level.
    pub high: i64,
    /// The running level relative to the piece's start.
    pub(super) level: i64,
}

/// A non-head turn's charges since its last retired or head point.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Stretch {
    budgets: Budgets,
    first: Piece,
    /// Pieces following credit events. The first piece is always inline.
    pub(super) pieces: Vec<(u64, Piece)>,
    // Filled only by an explicitly enabled runtime witness (or protocol tests).
    pub(crate) exact: Vec<super::stretch_trace::Charge>,
}

impl Stretch {
    pub fn new(budgets: Budgets) -> Stretch {
        Stretch {
            budgets,
            first: Piece::default(),
            pieces: Vec::new(),
            exact: Vec::new(),
        }
    }
    pub(super) fn piece(&mut self) -> &mut Piece {
        self.pieces
            .last_mut()
            .map_or(&mut self.first, |(_, piece)| piece)
    }
    /// An allocation of `bytes`.
    pub fn alloc(&mut self, bytes: u64) -> Result<(), Pause> {
        let memory = self.budgets.memory;
        let piece = self.piece();
        piece.level = piece.level.saturating_add(bytes as i64);
        piece.net = piece.level;
        if memory {
            piece.high = piece.high.max(piece.level);
        }
        #[cfg(test)]
        self.exact.push(super::stretch_trace::Charge::Alloc(bytes));
        Ok(())
    }
    /// A free of `bytes` within the segment.
    pub fn free(&mut self, bytes: u64) {
        let piece = self.piece();
        piece.level = piece.level.saturating_sub(bytes as i64);
        piece.net = piece.level;
        #[cfg(test)]
        self.exact.push(super::stretch_trace::Charge::Free(bytes));
    }
    /// A credit event of `bytes`, computed when its op commits: it opens a new piece.
    pub fn credit(&mut self, bytes: u64) -> Result<(), Pause> {
        if self.pieces.len() + 1 == PIECES_MAX {
            return Err(Pause::Pieces);
        }
        self.pieces.push((bytes, Piece::default()));
        #[cfg(test)]
        self.exact.push(super::stretch_trace::Charge::Credit(bytes));
        Ok(())
    }
    /// The pieces, each with the credit event before it (0 for the first).
    pub fn pieces(&self) -> impl Iterator<Item = (u64, &Piece)> {
        std::iter::once((0, &self.first))
            .chain(self.pieces.iter().map(|(credit, piece)| (*credit, piece)))
    }

    /// Join two consecutive charge prefixes without introducing a credit at
    /// their physical settlement boundary. Failure leaves the receiver intact.
    pub(crate) fn append(&mut self, next: &Self) -> Result<(), Pause> {
        assert_eq!(self.budgets, next.budgets);
        if self.pieces.len() + next.pieces.len() + 1 > PIECES_MAX {
            return Err(Pause::Pieces);
        }
        let memory = self.budgets.memory;
        let left = self.piece();
        let right = &next.first;
        if memory {
            left.high = left.high.max(left.level.saturating_add(right.high));
        }
        left.level = left.level.saturating_add(right.net);
        left.net = left.level;
        self.pieces.extend_from_slice(&next.pieces);
        #[cfg(test)]
        self.exact.extend_from_slice(&next.exact);
        Ok(())
    }
}

/// The run's settled budget state at the retirement frontier.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Settled {
    /// Settled logical bytes.
    pub level: u64,
    /// The memory budget, `u64::MAX` for none.
    pub limit: u64,
}

impl Settled {
    /// Settle `stretch` at its turn's retirement: the stop the serial run reaches inside it,
    /// or the state after it.
    pub fn settle(&mut self, stretch: &Stretch) -> Result<(), BudgetKind> {
        for (credit, piece) in stretch.pieces() {
            self.level = self.level.saturating_sub(credit);
            if piece.high as u64 > self.limit.saturating_sub(self.level) {
                return Err(BudgetKind::Memory);
            }
            self.level = self.level.saturating_add_signed(piece.net);
        }
        Ok(())
    }
}

/// The first stopped job, indexed in the batch's serial order.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct JobStop {
    pub job: usize,
    pub kind: BudgetKind,
}

/// Completed job summaries awaiting serial retirement. Publication order is
/// physical only; neither publication nor a missing older job settles charges.
/// Replay publishes the recorded summaries through this same interface.
#[derive(Debug)]
pub struct Jobs {
    pending: Vec<Option<(Stretch, bool)>>,
    next: usize,
    settled: Settled,
    stopped: Option<JobStop>,
}

impl Jobs {
    /// One bounded CPU batch (at most twice its admitted width).
    pub fn new(width: u32, count: usize, settled: Settled) -> Self {
        assert!(width > 0, "admitted CPU width");
        assert!(count <= width as usize * 2, "bounded CPU batch");
        Self {
            pending: vec![None; count],
            next: 0,
            settled,
            stopped: None,
        }
    }

    /// CPU settlement also enforces the batch grant. A stricter run memory
    /// limit remains stricter; physical admission is checked independently.
    pub fn cpu(
        grant: &hirsel_run_schema::CpuGrant,
        width: u32,
        count: usize,
        mut settled: Settled,
    ) -> Self {
        assert!(width <= grant.max_workers, "granted CPU width");
        settled.limit = settled.limit.min(grant.max_batch_bytes);
        Self::new(width, count, settled)
    }

    /// Publish one completed job, including its cleanup.
    /// The caller retains a recording copy before transferring the summary.
    pub fn publish(&mut self, job: usize, charges: Stretch) {
        self.submit(job, charges, true);
    }

    /// Publish a prefix paused at a physical or summary bound. Settlement
    /// checks the prefix but cannot advance to the next job until this job
    /// publishes its continuation and cleanup. A denied allocation may be
    /// included only when it is the next serial charge; resumption must not
    /// charge it again after successful settlement.
    pub fn pause(&mut self, job: usize, charges: Stretch) {
        self.submit(job, charges, false);
    }

    fn submit(&mut self, job: usize, charges: Stretch, complete: bool) {
        assert!(job >= self.next, "a job retires once");
        assert!(
            self.settled.limit == u64::MAX || charges.budgets.memory,
            "memory-limited jobs retain allocation envelopes"
        );
        let slot = self.pending.get_mut(job).expect("job belongs to batch");
        assert!(slot.is_none(), "a job publishes once");
        *slot = Some((charges, complete));
    }

    /// Settle the available serial prefix. A stop is sticky: discarded younger
    /// work cannot replace its job or kind, even when published afterward.
    pub fn retire(&mut self) -> Result<usize, JobStop> {
        if let Some(stop) = self.stopped {
            return Err(stop);
        }
        while let Some(Some((_, complete))) = self.pending.get(self.next) {
            let complete = *complete;
            self.retire_one(self.next)?;
            if !complete {
                break;
            }
        }
        Ok(self.next)
    }

    fn retire_one(&mut self, job: usize) -> Result<(), JobStop> {
        assert_eq!(job, self.next, "CPU prefixes retire in job order");
        let (charges, complete) = self.pending[job].as_ref().expect("a published prefix");
        if let Err(kind) = self.settled.settle(charges) {
            let stop = JobStop { job, kind };
            self.stopped = Some(stop);
            return Err(stop);
        }
        let complete = *complete;
        self.pending[job] = None;
        if complete {
            self.next += 1;
        }
        Ok(())
    }

    pub fn withdraw_younger(&mut self, job: usize) -> Vec<(usize, Stretch, bool)> {
        assert!(
            job >= self.next && job < self.pending.len(),
            "live CPU ordinal"
        );
        self.pending
            .iter_mut()
            .enumerate()
            .skip(job + 1)
            .filter_map(|(ordinal, slot)| {
                slot.take()
                    .map(|(charges, complete)| (ordinal, charges, complete))
            })
            .collect()
    }

    /// Retire exactly this already-published native prefix, never a younger
    /// available job. The global frontier may contain orchestration or cleanup
    /// charges between the two job prefixes; import those before settlement.
    pub(crate) fn retire_published_at(
        &mut self,
        job: usize,
        frontier: &mut Settled,
    ) -> (Stretch, Result<(), JobStop>) {
        assert_eq!(job, self.next, "native CPU prefixes retire in job order");
        let charges = self.pending[job]
            .as_ref()
            .expect("a published native prefix")
            .0
            .clone();
        if let Some(stop) = self.stopped {
            return (charges, Err(stop));
        }
        self.settled = Settled {
            limit: frontier.limit.min(self.settled.limit),
            ..*frontier
        };
        let result = self.retire_one(job);
        frontier.level = self.settled.level;
        (charges, result)
    }

    pub fn settled(&self) -> Settled {
        self.settled
    }

    /// Retire a prefix at the native frontier. Between job prefixes the
    /// caller may have retired orchestration work or result cleanup. Import
    /// that state without losing this batch's stricter memory limit.
    pub(crate) fn retire_prefix_at(
        &mut self,
        job: usize,
        charges: Stretch,
        complete: bool,
        frontier: &mut Settled,
    ) -> Result<(), JobStop> {
        if let Some(stop) = self.stopped {
            return Err(stop);
        }
        assert_eq!(job, self.next, "native CPU prefixes retire in job order");
        assert!(
            self.pending[job].is_none(),
            "published CPU prefix retires first"
        );
        self.settled = Settled {
            limit: frontier.limit.min(self.settled.limit),
            ..*frontier
        };
        self.submit(job, charges, complete);
        let result = self.retire_one(job);
        frontier.level = self.settled.level;
        result
    }
}

/// One event of a turn, in program order: the exact charge stream the summaries replace.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Event {
    Alloc(u64),
    Free(u64),
    Credit(u64),
}

/// The serial run's meter over `events`: the reference settlement must equal.
pub fn reference(state: &mut Settled, events: &[Event]) -> Result<(), BudgetKind> {
    for &event in events {
        match event {
            Event::Alloc(bytes) => {
                if state.level.saturating_add(bytes) > state.limit {
                    return Err(BudgetKind::Memory);
                }
                state.level += bytes;
            }
            Event::Free(bytes) | Event::Credit(bytes) => {
                state.level = state.level.saturating_sub(bytes);
            }
        }
    }
    Ok(())
}

/// The stretch summarising `events`, or the pause one of them causes.
pub fn summarise(budgets: Budgets, events: &[Event]) -> Result<Stretch, Pause> {
    let mut stretch = Stretch::new(budgets);
    for &event in events {
        match event {
            Event::Alloc(bytes) => stretch.alloc(bytes)?,
            Event::Free(bytes) => stretch.free(bytes),
            Event::Credit(bytes) => stretch.credit(bytes)?,
        }
    }
    Ok(stretch)
}

#[cfg(test)]
mod tests {
    use super::*;
    use Event::*;

    const MEMORY: Budgets = Budgets { memory: true };

    #[test]
    fn joining_physically_settled_prefixes_matches_the_unsplit_charge_stream() {
        let vectors = [
            vec![Alloc(16), Alloc(32), Free(24), Alloc(64)],
            vec![Free(32), Alloc(16), Alloc(32), Credit(8), Alloc(24)],
            vec![Alloc(8), Credit(8), Credit(0), Alloc(16)],
            vec![Alloc(16), Alloc(32), Free(48), Alloc(64), Alloc(16)],
        ];

        for memory in [false, true] {
            let budgets = Budgets { memory };
            for events in &vectors {
                let expected = summarise(budgets, events).unwrap();
                for first in 0..=events.len() {
                    for second in first..=events.len() {
                        let mut joined = summarise(budgets, &events[..first]).unwrap();
                        joined
                            .append(&summarise(budgets, &events[first..second]).unwrap())
                            .unwrap();
                        joined
                            .append(&summarise(budgets, &events[second..]).unwrap())
                            .unwrap();
                        assert_eq!(joined, expected, "{events:?}, {first}, {second}");
                    }
                }
            }
        }
    }

    #[test]
    fn joined_prefixes_keep_the_original_piece_cap() {
        let mut full = summarise(MEMORY, &vec![Credit(1); PIECES_MAX - 1]).unwrap();
        let before = full.clone();
        assert_eq!(
            full.append(&summarise(MEMORY, &[Credit(1)]).unwrap()),
            Err(Pause::Pieces)
        );
        assert_eq!(full, before);
    }

    fn settle(
        state: Settled,
        budgets: Budgets,
        events: &[Event],
    ) -> (Result<(), BudgetKind>, Settled) {
        let mut settled = state;
        let stretch = summarise(budgets, events).expect("within bounds");
        let result = settled.settle(&stretch);
        (result, settled)
    }

    /// Review-2's counterexample: a younger summary whose record high 20 stops the run at its
    /// turn when 90 + 20 > 100, although its net is 0.
    #[test]
    fn record_high_stops_a_net_zero_turn() {
        let state = Settled {
            level: 90,
            limit: 100,
        };
        let memory = Budgets { memory: true };
        assert_eq!(
            settle(state, memory, &[Alloc(20), Free(20)]).0,
            Err(BudgetKind::Memory)
        );
    }

    fn next(seed: &mut u64) -> u64 {
        *seed = seed.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = *seed;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    #[test]
    fn summaries_equal_the_reference_charge_stream() {
        let mut seed = 20261020;
        for case in 0..20_000 {
            let budgets = Budgets {
                memory: case % 2 == 0,
            };
            let limit = if budgets.memory {
                64 + next(&mut seed) % 256
            } else {
                u64::MAX
            };
            let level = if budgets.memory {
                next(&mut seed) % (limit + 1)
            } else {
                next(&mut seed) % 512
            };
            let state = Settled { level, limit };
            let len = next(&mut seed) % 40;
            let mut held = level;
            let events: Vec<Event> = (0..len)
                .map(|_| match next(&mut seed) % 7 {
                    0 | 1 => Free(0),
                    2 | 3 => {
                        let bytes = 1 + next(&mut seed) % 48;
                        held += bytes;
                        Alloc(bytes)
                    }
                    4 | 5 => {
                        let bytes = next(&mut seed) % (held + 1);
                        held -= bytes;
                        Free(bytes)
                    }
                    _ => {
                        let bytes = next(&mut seed) % (held + 1);
                        held -= bytes;
                        Credit(bytes)
                    }
                })
                .collect();
            let mut expected = state;
            let expected_result = reference(&mut expected, &events);
            let (result, settled) = settle(state, budgets, &events);
            assert_eq!(result, expected_result, "case {case}: {state:?} {events:?}");
            if result.is_ok() {
                assert_eq!(settled, expected, "case {case}: {state:?} {events:?}");
            }
        }
    }

    /// A piece past its envelope bound and a stretch past its piece bound pause.
    #[test]
    fn piece_overflow_pauses() {
        let credits = vec![Credit(0); PIECES_MAX];
        assert_eq!(summarise(MEMORY, &credits), Err(Pause::Pieces));
        assert!(summarise(MEMORY, &credits[1..]).is_ok());
    }

    #[test]
    fn cpu_job_credits_and_a_stricter_run_limit_are_preserved() {
        let grant = hirsel_run_schema::CpuGrant {
            provider: "cpu.scoped@1".into(),
            max_workers: 4,
            max_domains: 1,
            max_physical_bytes: 1 << 30,
            max_batch_bytes: 100,
        };
        for (limit, expected) in [
            (u64::MAX, Ok(2)),
            (
                80,
                Err(JobStop {
                    job: 1,
                    kind: BudgetKind::Memory,
                }),
            ),
        ] {
            let mut jobs = Jobs::cpu(&grant, 4, 2, Settled { level: 60, limit });
            jobs.publish(1, summarise(MEMORY, &[Credit(40), Alloc(70)]).unwrap());
            assert_eq!(jobs.retire(), Ok(0));
            jobs.publish(0, summarise(MEMORY, &[]).unwrap());
            assert_eq!(jobs.retire(), expected);
            if expected.is_ok() {
                assert_eq!(
                    jobs.settled(),
                    Settled {
                        level: 90,
                        limit: 100,
                    }
                );
            }
        }
    }

    #[test]
    fn cpu_job_paused_prefix_resumes_before_younger_cleanup() {
        let mut jobs = Jobs::new(
            4,
            2,
            Settled {
                level: 0,
                limit: 100,
            },
        );
        jobs.publish(1, summarise(MEMORY, &[Alloc(100)]).unwrap());
        jobs.pause(0, summarise(MEMORY, &[Alloc(80)]).unwrap());
        assert_eq!(jobs.retire(), Ok(0));
        assert_eq!(
            jobs.settled(),
            Settled {
                level: 80,
                limit: 100,
            }
        );
        assert_eq!(jobs.retire(), Ok(0));
        jobs.publish(0, summarise(MEMORY, &[Credit(80)]).unwrap());
        assert_eq!(jobs.retire(), Ok(2));
        assert_eq!(
            jobs.settled(),
            Settled {
                level: 100,
                limit: 100,
            }
        );
    }

    #[test]
    fn native_cpu_publications_wait_for_each_global_retirement_without_duplicate_charge() {
        for width in 1..=16 {
            let mut state = Settled {
                level: 0,
                limit: 100,
            };
            let mut jobs = Jobs::new(width, 2, state);
            let younger = summarise(MEMORY, &[Alloc(60)]).unwrap();
            jobs.pause(1, younger.clone());
            jobs.retire_prefix_at(
                0,
                summarise(MEMORY, &[Alloc(30)]).unwrap(),
                false,
                &mut state,
            )
            .unwrap();
            assert_eq!(
                state,
                Settled {
                    level: 30,
                    limit: 100,
                }
            );
            jobs.retire_prefix_at(0, summarise(MEMORY, &[Free(30)]).unwrap(), true, &mut state)
                .unwrap();
            assert_eq!(
                state,
                Settled {
                    level: 0,
                    limit: 100,
                }
            );
            assert!(jobs.pending[1].is_some(), "job zero never retires job one");
            // Orchestration between mapper prefixes belongs to the run frontier.
            state.level = 10;
            let (recorded, result) = jobs.retire_published_at(1, &mut state);
            assert_eq!(recorded, younger);
            assert_eq!(result, Ok(()));
            assert_eq!(
                state,
                Settled {
                    level: 70,
                    limit: 100,
                }
            );
            jobs.retire_prefix_at(1, summarise(MEMORY, &[Free(60)]).unwrap(), true, &mut state)
                .unwrap();
            assert_eq!(
                state,
                Settled {
                    level: 10,
                    limit: 100,
                }
            );
            assert!(
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    jobs.retire_published_at(1, &mut state)
                }))
                .is_err(),
                "a summary cannot retire twice"
            );
        }
    }

    #[test]
    fn withdrawing_younger_summaries_returns_owner_charges_without_settling_them() {
        for width in 1..=16 {
            let mut state = Settled {
                level: 0,
                limit: 100,
            };
            let mut jobs = Jobs::new(width, 2, state);
            let younger = summarise(MEMORY, &[Alloc(60), Free(20)]).unwrap();
            jobs.publish(1, younger.clone());
            let withdrawn = jobs.withdraw_younger(0);
            assert_eq!(withdrawn, vec![(1, younger.clone(), true)]);
            assert!(
                jobs.withdraw_younger(0).is_empty(),
                "one withdrawal per prefix"
            );
            assert_eq!(jobs.settled(), state);
            jobs.retire_prefix_at(
                0,
                summarise(MEMORY, &[Alloc(30)]).unwrap(),
                true,
                &mut state,
            )
            .unwrap();
            jobs.publish(1, withdrawn.into_iter().next().unwrap().1);
            let (_, result) = jobs.retire_published_at(1, &mut state);
            assert_eq!(result, Ok(()));
            assert_eq!(
                state,
                Settled {
                    level: 70,
                    limit: 100,
                }
            );
        }
    }
}
