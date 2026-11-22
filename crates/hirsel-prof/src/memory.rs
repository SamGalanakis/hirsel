use std::collections::{BTreeMap, HashMap};

use rusqlite::Connection;

use crate::profile::{Result, Unsigned};

const ALLOC: u16 = 16;
const PEAK: u16 = 17;
const BUDGET: u16 = 18;
const SCOPE_STACK: u16 = 19;
const META: u16 = 20;
const SLACK: u16 = 21;

const BACKING: &[&str] = &[
    "block",
    "task_heap",
    "region",
    "task_storage",
    "channel",
    "cause_cell",
];
const METRICS: &[&str] = &["charged_bytes", "retained_bytes", "slack_bytes"];
const SCOPES: &[&str] = &["bytes", "steps", "capacity_unattributed"];

struct Allocation {
    site: i64,
    kind: &'static str,
    bytes: u64,
    probability: f64,
    start: u64,
    end: Option<u64>,
}

#[derive(Clone, Copy)]
pub(crate) struct Snapshot {
    metric: &'static str,
    scope: Option<i64>,
    ts: u64,
    bytes: u64,
    epoch: u64,
}

#[derive(Clone, Copy)]
pub(crate) struct Budget {
    id: i64,
    task: i64,
    parent: Option<i64>,
    kind: &'static str,
    limit: Option<u64>,
    peak: Option<u64>,
    enter: u64,
    exit: Option<u64>,
    stack: u64,
    refused: bool,
}

pub(crate) enum MemoryEvent {
    Allocate {
        id: u64,
        bytes: u64,
        kind: &'static str,
        probability: f64,
        epoch: u64,
    },
    Free {
        id: u64,
        epoch: u64,
    },
    Resize {
        id: u64,
        bytes: u64,
        epoch: u64,
    },
    Peak(Snapshot),
    ScopeStack {
        id: u64,
    },
    Budget(Box<Budget>),
    Meta {
        metric_version: u64,
        state_loss: u64,
    },
    Slack {
        epoch: u64,
        id: u64,
        bytes: u64,
    },
}

impl MemoryEvent {
    pub(crate) fn decode(kind: u16, words: &[u64]) -> Result<Self> {
        let event = match (kind, words) {
            (ALLOC, &[0, id, bytes, backing, probability, epoch]) => {
                let probability = f64::from_bits(probability);
                if probability <= 0.0 || probability > 1.0 || !probability.is_finite() {
                    return Err("invalid memory sample inclusion probability".into());
                }
                Self::Allocate {
                    id,
                    bytes,
                    kind: named(BACKING, backing)?,
                    probability,
                    epoch,
                }
            }
            (ALLOC, &[1, id, _, _, _, epoch]) => Self::Free { id, epoch },
            (ALLOC, &[2, id, bytes, _, _, epoch]) => Self::Resize { id, bytes, epoch },
            (PEAK, &[metric, scope, ts, bytes, epoch]) => Self::Peak(Snapshot {
                metric: named(METRICS, metric)?,
                scope: (scope != 0).then(|| i64::try_from(scope)).transpose()?,
                ts,
                bytes,
                epoch,
            }),
            (SCOPE_STACK, &[id]) => Self::ScopeStack { id },
            (
                BUDGET,
                &[
                    id,
                    task,
                    parent,
                    kind,
                    limit,
                    peak,
                    enter,
                    exit,
                    stack,
                    refused,
                ],
            ) => {
                let kind = named(SCOPES, kind)?;
                Self::Budget(Box::new(Budget {
                    id: i64::try_from(id)?,
                    task: i64::try_from(task)?,
                    parent: (parent != 0).then(|| i64::try_from(parent)).transpose()?,
                    kind,
                    limit: (kind != "capacity_unattributed").then_some(limit),
                    peak: (kind != "capacity_unattributed").then_some(peak),
                    enter,
                    exit: (exit != 0).then_some(exit),
                    stack,
                    refused: refused != 0,
                }))
            }
            (META, &[metric_version, state_loss]) => Self::Meta {
                metric_version,
                state_loss,
            },
            (SLACK, &[epoch, id, bytes]) => Self::Slack { epoch, id, bytes },
            _ => return Err(format!("invalid memory profile record {kind}").into()),
        };
        Ok(event)
    }
}

#[derive(Default)]
pub(crate) struct MemoryImport {
    allocations: HashMap<u64, Allocation>,
    /// Epoch, allocation id, and the live size after the change. None frees it.
    changes: Vec<(u64, u64, Option<u64>)>,
    snapshots: Vec<Snapshot>,
    budgets: Vec<Budget>,
    scope_stacks: HashMap<u64, i64>,
    slack: BTreeMap<(u64, u64), u64>,
    meta: Option<(u64, u64)>,
    records: u64,
    unmatched: u64,
    stacks: u64,
    truncated: u64,
    unwind_failed: u64,
}

fn named(names: &[&'static str], code: u64) -> Result<&'static str> {
    names
        .get(usize::try_from(code)?)
        .copied()
        .ok_or_else(|| "invalid memory profile tag".into())
}

impl MemoryImport {
    pub(crate) fn stack(&mut self, status: &str) {
        self.stacks += 1;
        match status {
            "ok" | "fiber_root" => {}
            "truncated_stack" => self.truncated += 1,
            _ => self.unwind_failed += 1,
        }
    }

    pub(crate) fn record(&mut self, ts: u64, event: &MemoryEvent, site: Option<i64>) -> Result<()> {
        self.records += 1;
        match *event {
            MemoryEvent::Allocate {
                id,
                bytes,
                kind,
                probability,
                epoch,
            } => {
                let allocation = Allocation {
                    site: site.ok_or("missing allocation callsite")?,
                    kind,
                    bytes,
                    probability,
                    start: ts,
                    end: None,
                };
                if self.allocations.insert(id, allocation).is_some() {
                    return Err("duplicate memory allocation identity".into());
                }
                self.changes.push((epoch, id, Some(bytes)));
            }
            MemoryEvent::Free { id, epoch } => {
                if let Some(a) = self.allocations.get_mut(&id) {
                    a.end = Some(ts);
                    self.changes.push((epoch, id, None));
                } else {
                    self.unmatched += 1;
                }
            }
            MemoryEvent::Resize { id, bytes, epoch } => {
                if self.allocations.contains_key(&id) {
                    self.changes.push((epoch, id, Some(bytes)));
                } else {
                    self.unmatched += 1;
                }
            }
            MemoryEvent::Peak(snapshot) => self.snapshots.push(snapshot),
            MemoryEvent::ScopeStack { id } => {
                self.scope_stacks
                    .insert(id, site.ok_or("missing scope callsite")?);
            }
            MemoryEvent::Budget(ref budget) => {
                if let Some(site) = site {
                    self.scope_stacks.insert(budget.stack, site);
                }
                self.budgets.push(**budget);
            }
            MemoryEvent::Meta {
                metric_version,
                state_loss,
            } => {
                self.meta = Some((metric_version, state_loss));
            }
            MemoryEvent::Slack { epoch, id, bytes } => {
                self.slack.insert((epoch, id), bytes);
            }
        }
        Ok(())
    }

    pub(crate) fn finish(
        mut self,
        db: &Connection,
        mode: &str,
        drops: u64,
        trace_complete: bool,
        unknown: i64,
    ) -> Result<()> {
        self.allocations(db)?;
        self.budgets(db, unknown)?;
        let complete = self.peaks(db, unknown)?;
        let (version, state_loss) = self.meta.unwrap_or_default();
        let exact = mode == "exact"
            && trace_complete
            && version != 0
            && state_loss == 0
            && drops == 0
            && self.unmatched == 0
            && complete;
        let mut stmt = db.prepare("INSERT INTO run_meta VALUES (?1, ?2)")?;
        stmt.execute(("mem_mode", mode))?;
        stmt.execute(("metric_version", Unsigned(version)))?;
        stmt.execute(("mem_records", Unsigned(self.records)))?;
        stmt.execute(("mem_records_lost", Unsigned(drops)))?;
        stmt.execute(("mem_state_lost", Unsigned(state_loss)))?;
        stmt.execute(("mem_unmatched", Unsigned(self.unmatched)))?;
        stmt.execute(("mem_stacks", Unsigned(self.stacks)))?;
        stmt.execute(("mem_stacks_truncated", Unsigned(self.truncated)))?;
        stmt.execute(("mem_unwind_failed", Unsigned(self.unwind_failed)))?;
        stmt.execute(("mem_callsites_exact", i64::from(exact)))?;
        Ok(())
    }

    fn allocations(&self, db: &Connection) -> Result<()> {
        #[derive(Default)]
        struct Totals {
            count: u64,
            bytes: u64,
            estimated_count: f64,
            estimated_bytes: f64,
            freed: u64,
            lifetime: u64,
        }
        let mut totals = BTreeMap::<_, Totals>::new();
        for a in self.allocations.values() {
            let total = totals.entry((a.site, a.kind)).or_default();
            total.count += 1;
            total.bytes = total
                .bytes
                .checked_add(a.bytes)
                .ok_or("allocation bytes exceed u64")?;
            total.estimated_count += 1.0 / a.probability;
            total.estimated_bytes += a.bytes as f64 / a.probability;
            if let Some(end) = a.end {
                total.freed += 1;
                total.lifetime = total
                    .lifetime
                    .checked_add(end.saturating_sub(a.start))
                    .ok_or("allocation lifetime exceeds u64")?;
            }
        }
        let mut stmt = db.prepare("INSERT INTO mem_alloc_agg VALUES (?1,?2,?3,?4,?5,?6,?7,?8)")?;
        for ((site, kind), total) in totals {
            stmt.execute((
                site,
                kind,
                Unsigned(total.count),
                Unsigned(total.bytes),
                total.estimated_count,
                total.estimated_bytes,
                Unsigned(total.freed),
                Unsigned(total.lifetime),
            ))?;
        }
        Ok(())
    }

    fn budgets(&self, db: &Connection, unknown: i64) -> Result<()> {
        let mut stmt =
            db.prepare("INSERT INTO budget_scope_summary VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)")?;
        for b in &self.budgets {
            stmt.execute((
                b.id,
                b.parent,
                b.kind,
                b.task,
                self.scope_stacks.get(&b.stack).copied().unwrap_or(unknown),
                Unsigned(b.enter),
                b.exit.map(Unsigned),
                b.limit.map(Unsigned),
                b.peak.map(Unsigned),
                b.refused,
            ))?;
        }
        Ok(())
    }

    fn peaks(&mut self, db: &Connection, unknown: i64) -> Result<bool> {
        let mut complete = true;
        self.snapshots.sort_by_key(|snapshot| snapshot.epoch);
        self.changes.sort_unstable_by_key(|change| change.0);
        let mut changes = self.changes.iter().peekable();
        let mut live = HashMap::new();
        let mut snapshot_row = db.prepare(
            "INSERT INTO peak_snapshot(metric,scope_id,ts_ns,total_bytes) VALUES (?1,?2,?3,?4)",
        )?;
        let mut member_row = db.prepare("INSERT INTO peak_callsite VALUES (?1,?2,?3,?4)")?;
        for peak in &self.snapshots {
            while changes.peek().is_some_and(|change| change.0 <= peak.epoch) {
                let &(_, id, bytes) = changes.next().unwrap();
                if let Some(bytes) = bytes {
                    live.insert(id, bytes);
                } else {
                    live.remove(&id);
                }
            }
            let mut members = BTreeMap::<i64, (u64, u64)>::new();
            let mut add = |id, bytes: u64| -> Result<()> {
                let site = self.allocations.get(&id).map_or(unknown, |a| a.site);
                let member = members.entry(site).or_default();
                member.0 = member.0.checked_add(bytes).ok_or("peak bytes exceed u64")?;
                member.1 += 1;
                Ok(())
            };
            match peak.metric {
                "retained_bytes" => {
                    for (&id, &bytes) in &live {
                        add(id, bytes)?;
                    }
                }
                "slack_bytes" => {
                    for (&(_, id), &bytes) in
                        self.slack.range((peak.epoch, 0)..=(peak.epoch, u64::MAX))
                    {
                        add(id, bytes)?;
                    }
                }
                "charged_bytes" => {
                    members.insert(unknown, (peak.bytes, 0));
                }
                _ => unreachable!("validated metric"),
            }
            let total = members
                .values()
                .try_fold(0u64, |sum, &(bytes, _)| sum.checked_add(bytes))
                .ok_or("peak bytes exceed u64")?;
            if total != peak.bytes {
                complete = false;
                // A dropped free can leave a block looking live. Its size is
                // indistinguishable from the other members of this snapshot.
                members.clear();
                members.insert(unknown, (peak.bytes, 0));
            }
            snapshot_row.execute((
                peak.metric,
                peak.scope,
                Unsigned(peak.ts),
                Unsigned(peak.bytes),
            ))?;
            let id = db.last_insert_rowid();
            for (site, (bytes, count)) in members {
                member_row.execute((id, site, Unsigned(bytes), Unsigned(count)))?;
            }
        }
        Ok(complete)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_import_materializes_distinct_peak_memberships_and_unopened_refusal() {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch(include_str!("schema.sql")).unwrap();
        db.execute_batch(
            "INSERT INTO frame(id,inlined) VALUES (1,0),(2,0);
             INSERT INTO callsite(id,frame_id,depth) VALUES (1,1,0),(2,2,0);",
        )
        .unwrap();

        let mut memory = MemoryImport::default();
        let p = 1.0f64.to_bits();
        let mut record = |ts, kind, words: &[u64], site| {
            let event = MemoryEvent::decode(kind, words).unwrap();
            memory.record(ts, &event, site).unwrap();
        };
        record(1, ALLOC, &[0, 1, 100, 2, p, 1], Some(2));
        record(2, SLACK, &[1, 1, 92], None);
        record(3, ALLOC, &[0, 2, 24, 1, p, 2], Some(1));
        record(4, ALLOC, &[1, 2, 24, 0, 0, 3], None);
        record(5, ALLOC, &[1, 1, 100, 0, 0, 4], None);
        record(6, BUDGET, &[3, 1, 0, 0, 400, 0, 6, 0, 3, 1], Some(1));
        record(7, PEAK, &[0, 0, 4, 212, 3], None);
        record(7, PEAK, &[1, 0, 3, 124, 2], None);
        record(7, PEAK, &[2, 0, 2, 92, 1], None);
        record(7, META, &[2, 0], None);
        memory.finish(&db, "exact", 0, true, 1).unwrap();

        let peaks = db
            .prepare(
                "SELECT metric,total_bytes,sum(live_bytes) FROM peak_snapshot
             JOIN peak_callsite ON snapshot_id=id GROUP BY id ORDER BY metric",
            )
            .unwrap()
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Unsigned>(1)?.0,
                    row.get::<_, Unsigned>(2)?.0,
                ))
            })
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        assert_eq!(
            peaks,
            [
                ("charged_bytes".into(), 212, 212),
                ("retained_bytes".into(), 124, 124),
                ("slack_bytes".into(), 92, 92)
            ]
        );
        assert_eq!(
            db.query_row(
                "SELECT refused,limit_val,exit_ts FROM budget_scope_summary",
                [],
                |row| Ok((
                    row.get::<_, bool>(0)?,
                    row.get::<_, Unsigned>(1)?.0,
                    row.get::<_, Option<Unsigned>>(2)?.map(|n| n.0)
                )),
            )
            .unwrap(),
            (true, 400, None)
        );
        assert!(
            db.query_row(
                "SELECT value FROM run_meta WHERE key='mem_callsites_exact'",
                [],
                |row| row.get::<_, bool>(0)
            )
            .unwrap()
        );
        assert_eq!(
            db.query_row(
                "SELECT lifetime_ns FROM mem_alloc_agg WHERE callsite_id=1",
                [],
                |row| row.get::<_, Unsigned>(0).map(|n| n.0)
            )
            .unwrap(),
            1
        );
    }
}
