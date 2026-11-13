//! Build acceptance uses demanded definitions; package check keeps every diagnostic.
use super::*;
use hirsel_core_ir::InstanceKey;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Serialize)]
pub struct GapStop {
    pub definition: String,
    pub rule: String,
    pub construct: String,
    pub primary: SourceSite,
}
/// A census counts the first stop of each demanded definition, not every message.
/// Behind an incomplete body, resolved source names expose the remaining closure.
/// Completed bodies use their emitted calls and function references.
pub struct Reachable<'a> {
    pub bodies: BTreeSet<DefPath>,
    pub declarations: BTreeSet<DefPath>,
    pub diagnostics: Vec<&'a Refusal>,
    pub incomplete: Vec<&'a Incomplete>,
    pub first_stops: Vec<GapStop>,
    pub instances: Result<Vec<InstanceKey>, InstanceStop>,
    pub(crate) materialized: crate::instances::InstanceSet,
}
impl Reachable<'_> {
    pub fn accepted(&self) -> bool {
        self.diagnostics.is_empty() && self.incomplete.is_empty() && self.instances.is_ok()
    }
    pub(crate) fn require(&self) -> Result<&[InstanceKey], NativeStop> {
        if !self.diagnostics.is_empty() || !self.incomplete.is_empty() {
            let stop = self.first_stops.first().unwrap();
            return Err(NativeStop::Check(format!(
                "{}: {} {}",
                stop.definition, stop.rule, stop.construct
            )));
        }
        self.instances
            .as_deref()
            .map_err(|e| NativeStop::Instances(e.clone()))
    }
    pub fn census(&self) -> BTreeMap<&str, usize> {
        let mut counts = BTreeMap::new();
        for stop in &self.first_stops {
            *counts.entry(stop.construct.as_str()).or_default() += 1;
        }
        counts
    }
}
impl CheckProduct {
    pub fn reachable(&self, entry: &DefPath, limit: usize) -> io::Result<Reachable<'_>> {
        hirsel_core_ids::published::catch_io(|| self.reachable_inner(entry, limit))
    }
    fn reachable_inner(&self, entry: &DefPath, limit: usize) -> io::Result<Reachable<'_>> {
        let mut materialized = crate::instances::InstanceSet::default();
        let instances = self.instances_into(
            [InstanceKey {
                definition: entry.clone(),
                types: vec![],
                dictionaries: vec![],
            }],
            limit,
            &mut materialized,
        );
        let image_directory = self.image.as_ref().map(|i| i.directory()).transpose()?;
        let mut bodies = BTreeSet::new();
        let mut unavailable = BTreeSet::new();
        let mut declarations = BTreeSet::new();
        let mut pending = BTreeSet::from([(entry.clone(), true)]);
        // LANG-20 evaluates every source constant before a body, including unused
        // ones. Installed constants already carry their published value.
        for (id, definition) in self
            .index
            .definitions
            .own_iter()
            .enumerate()
            .map(|(i, d)| (self.index.definitions.base_len() + i, d))
        {
            if definition.kind == hirsel_core_resolve::DefinitionKind::Constant {
                let path = self
                    .index
                    .definition_path(hirsel_core_resolve::DefinitionId(id as u32));
                if !image_directory
                    .as_ref()
                    .is_some_and(|i| i.headers.contains_key(&path))
                {
                    pending.insert((path, false));
                }
            }
        }
        while let Some((path, body)) = pending.pop_first() {
            if body {
                if !bodies.insert(path.clone()) {
                    continue;
                }
                pending.insert((path.clone(), false));
                if let Some(header) = self.headers.get(&path) {
                    // Named types/signatures are needed even when folding removes a call.
                    pending.extend(header.body_reads.iter().cloned().map(|p| (p, false)));
                }
                if !self.definitions.contains_key(&path)
                    && !image_directory
                        .as_ref()
                        .is_some_and(|i| i.definitions.contains_key(&path))
                {
                    unavailable.insert(path);
                    continue;
                }
                let product = self.demand_body(&path)?;
                if let Some(function) = product.check.function {
                    let ir = &product.ir;
                    let functions = crate::instances::external_functions(ir, function);
                    pending.extend(functions.into_iter().map(|f| {
                        (
                            ir.instances[ir.functions[f.index()].instance.index()]
                                .definition
                                .clone(),
                            true,
                        )
                    }));
                } else if let Some(header) = self.headers.get(&path) {
                    // Failed lowering has no call graph. Keep its resolved references
                    // for diagnostics/census rather than hiding everything after its gap.
                    pending.extend(
                        header
                            .body_reads
                            .iter()
                            .filter(|p| self.headers.get(*p).is_some_and(|h| h.has_body))
                            .cloned()
                            .map(|p| (p, true)),
                    );
                }
            } else {
                if !declarations.insert(path.clone()) {
                    continue;
                }
                if let Some(header) = self.headers.get(&path) {
                    pending.extend(
                        header
                            .reads
                            .iter()
                            .map(|&id| (self.index.definition_path(id), false)),
                    );
                }
            }
        }
        let owners: BTreeMap<_, _> = self
            .headers
            .available()
            .into_iter()
            .map(|(p, h)| (h.anchor, (p, h)))
            .collect();
        let needed = |site: &Site| -> bool {
            if let Some((path, header)) = owners.get(&(site.module, site.item)) {
                bodies.contains(*path)
                    || declarations.contains(*path) && site.span.start < header.header_end
            } else {
                // Module-wide decoding/resource failures affect each demand from it.
                site.item == u32::MAX
                    && owners.values().any(|(p, h)| {
                        h.anchor.0 == site.module
                            && (bodies.contains(*p) || declarations.contains(*p))
                    })
            }
        };
        let mut library_diagnostics = vec![];
        let mut library_incomplete = vec![];
        if let Some(image) = &self.image {
            for path in &bodies {
                if !self.definitions.contains_key(path) && image.has_body(path)? {
                    let body = image.demand_ref(path)?;
                    library_diagnostics.extend(
                        body.check
                            .refusals
                            .iter()
                            .filter(|d| !self.diagnostics.contains(d)),
                    );
                    library_incomplete.extend(body.incomplete.iter().filter(|d| {
                        !self
                            .incomplete
                            .iter()
                            .any(|other| other.primary == d.primary && other.message == d.message)
                    }));
                }
            }
        }
        let diagnostics: Vec<_> = self
            .diagnostics
            .iter()
            .chain(library_diagnostics)
            .filter(|d| needed(&d.primary))
            .collect();
        let incomplete: Vec<_> = self
            .incomplete
            .iter()
            .chain(library_incomplete)
            .filter(|d| needed(&d.primary))
            .collect();
        let mut first_stops = BTreeMap::new();
        for (site, rule, message) in diagnostics
            .iter()
            .map(|d| (&d.primary, d.rule.as_str(), d.message.as_str()))
            .chain(
                incomplete
                    .iter()
                    .map(|d| (&d.primary, "checker gap", d.message.as_str())),
            )
        {
            let primary = self.source_site(site)?;
            let name = owners
                .get(&(site.module, site.item))
                .map(|(p, _)| p.to_string())
                .unwrap_or_else(|| primary.module.clone());
            if let std::collections::btree_map::Entry::Vacant(slot) =
                first_stops.entry(name.clone())
            {
                slot.insert(GapStop {
                    definition: name,
                    rule: rule.to_owned(),
                    construct: message.to_owned(),
                    primary,
                });
            }
        }
        for path in unavailable {
            if let Some(h) = self.headers.get(&path) {
                first_stops.entry(path.to_string()).or_insert(GapStop {
                    definition: path.to_string(),
                    rule: "instances".into(),
                    construct: "native body is unavailable".into(),
                    primary: self.source_site(&h.site)?,
                });
            }
        }
        if let Err(stop) = &instances {
            let (path, construct) = match stop {
                InstanceStop::Incomplete(p) => {
                    (p, "body has no complete native product".to_owned())
                }
                InstanceStop::OutsideReached(p) => (p, "body is unavailable".to_owned()),
                InstanceStop::MaterializationRequired(k) => {
                    (&k.definition, "generic instance materialization".to_owned())
                }
                InstanceStop::Input {
                    definition,
                    message,
                } => (definition, message.clone()),
                InstanceStop::Resource { next, .. } => (
                    &next.definition,
                    "instance collection resource refusal".to_owned(),
                ),
            };
            if let Some(h) = self.headers.get(path)
                && let std::collections::btree_map::Entry::Vacant(slot) =
                    first_stops.entry(path.to_string())
            {
                slot.insert(GapStop {
                    definition: path.to_string(),
                    rule: "instances".into(),
                    construct,
                    primary: self.source_site(&h.site)?,
                });
            }
        }
        Ok(Reachable {
            bodies,
            declarations,
            diagnostics,
            incomplete,
            first_stops: first_stops.into_values().collect(),
            instances,
            materialized,
        })
    }
}
