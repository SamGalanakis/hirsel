//! CONC-14 checks the same instantiated edges that supply a PROG-9 revision.
use super::*;
use crate::instances::InstanceSet;
use hirsel_core_ir::{Closure, RevisionRoot};
use hirsel_core_layout::{Body, FieldTy, Handle, Repr};
use hirsel_core_materialize::Resolver;

impl CheckProduct {
    pub(crate) fn flow_refusals(
        &mut self,
        mut bind: impl FnMut(&DefinitionProduct, &mut Site),
    ) -> Vec<Refusal> {
        let mut diagnostics = Vec::new();
        let mut checked = BTreeMap::new();
        for definition in self.definitions.values() {
            for (site, root) in &definition.check.flow.stages {
                let RevisionRoot::Item(root) = root else {
                    continue;
                };
                let message = checked
                    .entry(root.clone())
                    .or_insert_with(|| self.fragment_revision(root));
                if let Err(message) = message {
                    let mut site = *site;
                    bind(definition, &mut site);
                    diagnostics.push(Refusal {
                        rule: "CONC-14".into(),
                        primary: site,
                        causal: vec![],
                        declared: None,
                        required: None,
                        message: message.clone(),
                        unresolved: None,
                        detail: None,
                    });
                }
            }
        }
        diagnostics
    }

    fn fragment_revision(&self, root: &hirsel_core_ir::Identity) -> Result<ContentHash, String> {
        let mut instances = InstanceSet::default();
        let mut resolver = Resolver::default();
        let mut witness = None;
        let reached = crate::instances::collect_instances(
            [root.clone()],
            crate::native::DEFAULT_INSTANCE_LIMIT,
            |key| {
                if let Ok(definition) = self.demand_body(&key.definition) {
                    let facts = &definition.check.flow;
                    let arguments = resolver.arguments(key).ok();
                    let bad = arguments.as_ref().map_or(0, |args| {
                        args.iter().enumerate().fold(0, |bits, (i, arg)| {
                            bits | (u64::from(holds_authority(arg)) << i)
                        })
                    });
                    if facts.waits {
                        witness = Some(format!("`{}` is declared `waits`", key.definition));
                    } else if let Some(fact) = facts
                        .violations
                        .iter()
                        .filter(|fact| fact.when == 0 || fact.when & bad != 0)
                        .min_by_key(|fact| fact.at)
                    {
                        let mut message = fact.message.clone();
                        if fact.when != 0
                            && let Some(args) = arguments
                        {
                            let i = (fact.when & bad).trailing_zeros() as usize;
                            message.push_str(&format!(" at `{}`", args[i].rows[0].shape.name));
                        }
                        witness = Some(message);
                    }
                    if witness.is_some() {
                        return Err(crate::instances::InstanceStop::OutsideReached(
                            key.definition.clone(),
                        ));
                    }
                }
                self.instance_step(key, &mut instances, None, &mut resolver, &mut |_, _| {})
            },
        );
        let reached = reached.map_err(|stop| {
            witness.unwrap_or_else(|| {
                format!("the stage body's static reach cannot be closed ({stop:?})")
            })
        })?;
        instances
            .addresses(&reached, None)
            .and_then(|addresses| addresses.get(root).copied())
            .map(hirsel_core_ir::code_revision)
            .ok_or_else(|| "the stage body's static reach has no address for its root".into())
    }
}

fn holds_authority(closure: &Closure) -> bool {
    fn field(ty: &FieldTy, pending: &mut Vec<usize>) -> bool {
        match ty {
            FieldTy::Handle(Handle::Causes) => false,
            FieldTy::Handle(_) => true,
            FieldTy::Inline(r)
            | FieldTy::Box(r)
            | FieldTy::Rc(r)
            | FieldTy::Arc(r)
            | FieldTy::Shared(r)
            | FieldTy::RegionRef(r)
            | FieldTy::Link(r) => {
                pending.push(r.index());
                false
            }
            FieldTy::Seq(t)
            | FieldTy::Array(t, _)
            | FieldTy::View(t)
            | FieldTy::FixedSpan(t)
            | FieldTy::SeqSpan(t)
            | FieldTy::IndexedSpan(t)
            | FieldTy::Locator(t) => field(t, pending),
            FieldTy::Map(k, v) => field(k, pending) || field(v, pending),
            _ => false,
        }
    }
    let mut pending = vec![0];
    let mut seen = vec![false; closure.rows.len()];
    while let Some(row) = pending.pop() {
        if std::mem::replace(&mut seen[row], true) {
            continue;
        }
        let shape = &closure.rows[row].shape;
        if shape.repr == Repr::Resource {
            return true;
        }
        let mut field_holds = |f: &hirsel_core_layout::Field| field(&f.ty, &mut pending);
        let holds = match &shape.body {
            Body::Record(fields) => fields.iter().any(&mut field_holds),
            Body::Variant(cases) => cases.iter().flat_map(|c| &c.fields).any(&mut field_holds),
        };
        if holds {
            return true;
        }
    }
    false
}
