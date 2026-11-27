use super::{Graph, reached};
use crate::native::recipe_grants::GRANT_NAMES;
use hirsel_core_front::{DeclBody, Program, TyId, TyKind};
use hirsel_core_ids::{DefPath, PackageId};
use hirsel_core_ir::Identity;
use hirsel_core_resolve::{CorpusManifest, SourceId};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};

pub(crate) type Cards = BTreeMap<PackageId, Card>;

#[derive(Clone, Default, Eq, PartialEq)]
pub(crate) struct Card {
    pub(crate) exports: BTreeMap<String, Export>,
    pub(crate) reached: Reached,
}

#[derive(Clone, Default, Eq, PartialEq)]
pub(crate) struct Export {
    pub(crate) carried: Carried,
    pub(crate) reached: Reached,
}

#[derive(Clone, Copy, Default, Eq, PartialEq)]
pub(crate) struct Carried {
    pub(crate) takes: u32,
    pub(crate) returns: u32,
}

const FUNCTION: u32 = 1 << 15;

#[derive(Clone, Default, Eq, PartialEq)]
pub(crate) struct Reached {
    pub(crate) kinds: u16,
    pub(crate) provided: u16,
    pub(crate) contained: BTreeSet<String>,
    pub(crate) unresolved: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(untagged)]
pub(crate) enum Verdict {
    Status(&'static str),
    Kinds(Vec<&'static str>),
}

pub(crate) fn names(mask: u16) -> Vec<&'static str> {
    GRANT_NAMES
        .iter()
        .copied()
        .enumerate()
        .filter_map(|(i, name)| (mask & (1 << i) != 0).then_some(name))
        .collect()
}

// The carried-function bit predates Compiler's grant ordinal (15).
fn carried_bit(ordinal: usize) -> u32 {
    1 << (ordinal + usize::from(ordinal >= 15))
}

pub(crate) fn carried_names(mask: u32) -> Vec<&'static str> {
    GRANT_NAMES
        .iter()
        .enumerate()
        .filter_map(|(i, &name)| (mask & carried_bit(i) != 0).then_some(name))
        .chain((mask & FUNCTION != 0).then_some("function"))
        .collect()
}

fn export_name(path: &DefPath) -> String {
    let path = &path.0;
    path.modules
        .iter()
        .map(String::as_str)
        .chain([path.name.as_str()])
        .collect::<Vec<_>>()
        .join("/")
}

impl Card {
    pub(super) fn set_reach(
        &mut self,
        graph: &Graph,
        closure: &BTreeSet<PackageId>,
        roots: &BTreeSet<Identity>,
        exports: &BTreeSet<DefPath>,
        facts: &Result<BTreeMap<u8, String>, String>,
    ) {
        let mut grouped = BTreeMap::<_, BTreeSet<_>>::new();
        for root in roots {
            grouped
                .entry(&root.definition)
                .or_default()
                .insert(root.clone());
        }
        for (path, roots) in grouped {
            let mut detail = Reached::default();
            detail.unresolved = reached(graph, closure, &roots, Some(&mut detail)).err();
            self.reached.kinds |= detail.kinds;
            self.reached.provided |= detail.provided;
            self.reached
                .contained
                .extend(detail.contained.iter().cloned());
            if exports.contains(path) {
                self.exports.entry(export_name(path)).or_default().reached = detail;
            }
        }
        self.reached.unresolved = facts.as_ref().err().cloned();
    }

    pub(crate) fn verdict(&self) -> Verdict {
        if self.reached.unresolved.is_some() {
            Verdict::Status("unknown")
        } else if self.reached.kinds != 0 {
            Verdict::Kinds(names(self.reached.kinds))
        } else if self
            .exports
            .values()
            .any(|e| e.carried.takes | e.carried.returns != 0)
        {
            Verdict::Status("conditional")
        } else {
            Verdict::Status("pure")
        }
    }

    pub(crate) fn merge(&mut self, other: &Self) {
        self.reached.merge(&other.reached);
        for (name, export) in &other.exports {
            let here = self.exports.entry(name.clone()).or_default();
            here.carried.takes |= export.carried.takes;
            here.carried.returns |= export.carried.returns;
            here.reached.merge(&export.reached);
        }
    }
}

impl Reached {
    fn merge(&mut self, other: &Self) {
        self.kinds |= other.kinds;
        self.provided |= other.provided;
        self.contained.extend(other.contained.iter().cloned());
        if self.unresolved.is_none() {
            self.unresolved.clone_from(&other.unresolved);
        }
    }
}

pub(crate) fn carried(
    program: &Program<'_>,
    inputs: &CorpusManifest,
    test_sources: &BTreeSet<SourceId>,
) -> Cards {
    let mut cards: Cards = inputs
        .packages
        .iter()
        .filter(|p| p.namespace.is_some() && !p.toolchain)
        .map(|p| (p.package.clone(), Card::default()))
        .collect();
    let public: BTreeSet<_> = program
        .index
        .definitions()
        .filter(|(_, def)| {
            def.public && !def.site.source.is_some_and(|s| test_sources.contains(&s))
        })
        .map(|(key, _)| program.index.definition_path(key))
        .collect();
    for fun in program.fns.iter() {
        let Some(card) = cards.get_mut(&fun.path.0.package) else {
            continue;
        };
        if !public.contains(&fun.path) || fun.is_test || fun.drop_of.is_some() {
            continue;
        }
        card.exports
            .entry(export_name(&fun.path))
            .or_default()
            .carried = Carried {
            takes: fun.params.iter().fold(0, |kinds, p| {
                kinds | held(program, p.ty, &mut BTreeSet::new())
            }),
            returns: held(program, fun.result, &mut BTreeSet::new()),
        };
    }
    cards
}

fn core_kind(path: &DefPath) -> u32 {
    if !path.package_is("core", "1") {
        return 0;
    }
    let name = match path.0.name.as_str() {
        "HostFile" | "HostEntries" => "Dir",
        "Socket" | "ReadHalf" | "WriteHalf" | "Listener" | "UnixListener" | "UnixDatagram"
        | "UdpSocket" => "Net",
        "ClockHand" => "Clock",
        "StopTrigger" => "Process",
        "Task" => "Tasks",
        "Proc" => "Exec",
        "StaticFn" => return FUNCTION,
        name => name,
    };
    GRANT_NAMES
        .iter()
        .position(|kind| *kind == name)
        .map_or(0, carried_bit)
}

fn held(program: &Program<'_>, ty: TyId, active: &mut BTreeSet<TyId>) -> u32 {
    if !active.insert(ty) {
        return 0;
    }
    let result = match program.types.kind(ty) {
        TyKind::Fn(..) => FUNCTION,
        TyKind::Tuple(items) => items
            .iter()
            .fold(0, |mask, t| mask | held(program, *t, active)),
        TyKind::Array(t, _) | TyKind::View(t) | TyKind::Anchored(t, _) => held(program, *t, active),
        TyKind::Adt(id, args) => {
            let decl = &program.decls[id.0 as usize];
            let arguments = args
                .iter()
                .enumerate()
                .filter(|(i, _)| decl.opaque || decl.summary.holds_params & (1 << i) != 0)
                .fold(0, |mask, (_, t)| mask | held(program, *t, active));
            core_kind(&decl.path)
                | arguments
                | match decl.body {
                    DeclBody::Alias(t) => held(program, t, active),
                    _ => decl
                        .all_fields()
                        .fold(0, |mask, field| mask | held(program, field.ty, active)),
                }
        }
        _ => 0,
    };
    active.remove(&ty);
    result
}
