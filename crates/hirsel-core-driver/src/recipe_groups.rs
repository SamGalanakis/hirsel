//! The canonical recipe groups of the previous resident build.

use crate::instances::{Adapter, CodeSignatures, InstanceBody, InstanceSet};
use hirsel_core_ids::ContentHash;
use hirsel_core_ir::{Identity, address::GroupPlan};
use std::collections::BTreeMap;

pub(crate) struct Groups {
    instances: Vec<Identity>,
    rows: Vec<Row>,
    foreign: BTreeMap<Identity, ContentHash>,
    pub(super) plan: GroupPlan,
}

struct Row {
    record: Option<ContentHash>,
    slots: Vec<Identity>,
    signatures: CodeSignatures,
    adapters: Vec<Adapter>,
}

fn record(body: &InstanceBody) -> Option<ContentHash> {
    body.product.as_ref().map(|product| product.record)
}

impl Groups {
    pub(super) fn new(
        entry: &Identity,
        instances: &[Identity],
        materialized: &InstanceSet,
    ) -> Option<Self> {
        let plan = materialized.group_plan(entry, instances)?;
        let rows = instances
            .iter()
            .map(|identity| {
                let body = materialized.get(identity);
                Row {
                    record: record(body),
                    slots: body.slots.clone(),
                    signatures: body.signatures.clone(),
                    adapters: body.adapters.clone(),
                }
            })
            .collect();
        Some(Self {
            instances: instances.to_vec(),
            rows,
            foreign: materialized.foreign_addresses.clone(),
            plan,
        })
    }

    /// The same slots, signatures and shared adapters keep the prior
    /// validation and the prior strongly connected components. Read those
    /// fields only on changed definitions; a materialised instance is a
    /// function of its identity and published definition record.
    pub(super) fn update(&mut self, instances: &[Identity], materialized: &InstanceSet) -> bool {
        if self.instances != instances || self.foreign != materialized.foreign_addresses {
            return false;
        }
        let mut changed = Vec::new();
        for (i, (identity, row)) in instances.iter().zip(&self.rows).enumerate() {
            let body = materialized.get(identity);
            let live = record(body);
            // A constant's value can change under an unchanged record; the
            // signatures carry the value hashes the group binds.
            if live == row.record
                && live != Some(ContentHash::default())
                && body.signatures.values == row.signatures.values
            {
                continue;
            }
            if body.slots != row.slots
                || body.signatures != row.signatures
                || body.adapters != row.adapters
            {
                return false;
            }
            changed.push((i, identity, body.body, &body.signatures.values, live));
        }
        self.plan.update(
            changed
                .iter()
                .map(|(_, identity, body, values, _)| (*identity, *body, &values[..])),
        );
        for (i, _, _, _, live) in changed {
            self.rows[i].record = live;
        }
        true
    }
}
