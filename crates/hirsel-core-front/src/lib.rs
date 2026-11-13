//! The new core language's single check-and-lower traversal.
//!
//! [`Program::declare`] publishes one summary per type declaration (OWN-2), in
//! dependency order, and checks every signature once (LANG-8); it reads no body.
//! [`Program::check_lower`] then visits one definition's body once, forward, typing it,
//! checking ownership with the state OWN names, and emitting CoreIR as it goes. No fact
//! crosses definitions except through signatures and summaries, nothing is iterated to a
//! fixpoint, and nothing the pass emits is checked again.

#[cfg(test)]
hirsel_guard::install!();

mod body;
mod constants;
pub use constants::{ConstData, ConstValue, Constant};
mod decls;
mod layout;
mod library;
pub use library::{Library, Rows};
mod own;
mod types;

pub use body::{CoreCheck, Counts, Emitter, GenericTemplate, materialize};
pub use decls::*;
pub use types::*;

use hirsel_core_ids::DefPath;
use hirsel_core_resolve::{
    DefinitionId, ModuleId, NameIndex, Refusal, Resolution, ResolvedUse, Site, Target, resolve,
};
use hirsel_core_syntax::{Item, NodeId, Span, SyntaxFile};
use std::collections::HashMap;

/// One module of the program, already parsed.
pub struct ModuleInput {
    pub id: ModuleId,
    pub syntax: SyntaxFile,
}

/// An item of a module.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, serde::Serialize, serde::Deserialize)]
pub struct ItemRef {
    pub module: u32,
    pub item: u32,
}

/// A construct this implementation does not build yet: not a refusal of the program,
/// and never silently accepted either.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Gap {
    pub site: Site,
    pub what: &'static str,
}
impl Gap {
    pub fn new(site: Site, what: &'static str) -> Self {
        Self { site, what }
    }
}

pub(crate) fn refusal(rule: &str, primary: Site, message: impl Into<String>) -> Refusal {
    Refusal {
        rule: rule.into(),
        primary,
        causal: vec![],
        declared: None,
        required: None,
        message: message.into(),
    }
}

pub(crate) struct ItemInfo {
    pub resolution: std::sync::Arc<Resolution>,
    /// Node index to its entry in `resolution.names`, or `u32::MAX`.
    pub uses: Vec<u32>,
}

/// The declarations of one program: summaries and signatures, published once.
pub struct Program<'a> {
    pub index: &'a NameIndex,
    pub modules: &'a [ModuleInput],
    pub(crate) items: Vec<Vec<ItemInfo>>,
    pub types: Types,
    pub decls: Rows<TypeDecl>,
    pub fns: Rows<FnDecl>,
    pub constants: Rows<Constant>,
    pub constant_parts: Rows<ConstValue>,
    pub(crate) constant_ids: Vec<Option<u32>>,
    pub by_path: HashMap<DefPath, Def>,
    pub by_definition: Vec<Option<Def>>,
    pub(crate) core_operations: Vec<Option<body::core_ops::Kernel>>,
    /// Refusals of declarations and signatures, and the resolver's.
    pub refusals: Vec<Refusal>,
    pub gaps: Vec<Gap>,
    /// The program's entry, as its manifest names it (PROG-3); `main` is always `waits`
    /// (LANG-8).
    pub entry: Option<DefPath>,
    pub(crate) library_modules: usize,
    pub(crate) pending: Vec<decls::Pending>,
    pub(crate) alias_state: Vec<u8>,
    /// Aliases being lowered, each with its reference continuing the chain.
    pub(crate) alias_stack: Vec<(DeclId, Option<Site>)>,
}

impl<'a> Program<'a> {
    /// The core prelude type named `name`, when the program has core.
    pub(crate) fn core_decl(&self, name: &str) -> Option<DeclId> {
        self.decls
            .iter()
            .position(|d| decls::is_core_prelude(&d.path, &[name]))
            .map(|i| DeclId(i as u32))
    }

    /// Resolve every item, then publish summaries and signatures (slice a).
    pub fn declare(index: &'a NameIndex, modules: &'a [ModuleInput]) -> Self {
        let resolutions = modules
            .iter()
            .map(|m| {
                m.syntax
                    .items()
                    .enumerate()
                    .map(|(i, item)| std::sync::Arc::new(resolve(index, m.id, i as u32, item)))
                    .collect()
            })
            .collect();
        Self::declare_resolved(index, modules, resolutions)
    }

    /// Publish declarations from the resolver's completed rows without resolving again.
    /// The driver uses this boundary to attribute resolution and declaration work separately.
    pub fn declare_resolved(
        index: &'a NameIndex,
        modules: &'a [ModuleInput],
        resolutions: Vec<Vec<std::sync::Arc<Resolution>>>,
    ) -> Self {
        Self::declare_with_library(index, modules, resolutions, None)
    }

    /// Append only the request's declarations to a source-checked immutable library.
    pub fn declare_with_library(
        index: &'a NameIndex,
        modules: &'a [ModuleInput],
        resolutions: Vec<Vec<std::sync::Arc<Resolution>>>,
        library: Option<&Library>,
    ) -> Self {
        let mut refusals = index.diagnostics.clone();
        let items = modules
            .iter()
            .zip(resolutions)
            .map(|(m, rows)| {
                m.syntax
                    .items()
                    .zip(rows)
                    .map(|(item, resolution)| {
                        refusals.extend(resolution.diagnostics.iter().cloned());
                        let mut uses = vec![
                            u32::MAX;
                            (item.range.nodes.end - item.range.nodes.start) as usize
                        ];
                        for (k, u) in resolution.names.iter().enumerate() {
                            uses[(u.node.0 - item.range.nodes.start) as usize] = k as u32;
                        }
                        ItemInfo { resolution, uses }
                    })
                    .collect()
            })
            .collect();
        let mut p = Program {
            index,
            modules,
            items,
            types: library.map_or_else(Types::new, |l| {
                Types::overlay(std::sync::Arc::clone(&l.types))
            }),
            decls: library.map_or_else(Rows::default, |l| {
                Rows::overlay(std::sync::Arc::clone(&l.decls))
            }),
            fns: library.map_or_else(Rows::default, |l| {
                Rows::overlay(std::sync::Arc::clone(&l.fns))
            }),
            constants: library.map_or_else(Rows::default, |l| {
                Rows::overlay(std::sync::Arc::clone(&l.constants))
            }),
            constant_parts: library.map_or_else(Rows::default, |l| {
                Rows::overlay(std::sync::Arc::clone(&l.constant_parts))
            }),
            constant_ids: {
                let mut ids = library.map_or_else(Vec::new, |l| l.constant_ids.to_vec());
                ids.resize(index.definitions.len(), None);
                ids
            },
            by_path: library.map_or_else(HashMap::new, |l| {
                l.paths.iter().map(|(p, d)| (p.clone(), *d)).collect()
            }),
            by_definition: {
                let mut rows = library.map_or_else(Vec::new, |l| l.definitions.to_vec());
                rows.resize(index.definitions.len(), None);
                rows
            },
            core_operations: vec![],
            refusals,
            gaps: vec![],
            entry: None,
            library_modules: library.map_or(0, |l| l.modules),
            pending: vec![],
            alias_state: vec![],
            alias_stack: vec![],
        };
        p.register();
        p.lower_headers();
        p.register_constants();
        p.summarize();
        p.evaluate_constants();

        p.core_operations = body::core_ops::bind(&p.fns);
        p
    }

    pub(crate) fn item(&self, at: ItemRef) -> Item<'a> {
        let modules: &'a [ModuleInput] = self.modules;
        modules[at.module as usize].syntax.item(at.item as usize)
    }
    pub(crate) fn site(&self, at: ItemRef, n: NodeId) -> Site {
        self.site_span(at, self.item(at).tree.node(n).origin.span)
    }
    pub(crate) fn site_span(&self, at: ItemRef, span: Span) -> Site {
        Site {
            module: self.modules[at.module as usize].id,
            item: at.item,
            span,
        }
    }
    pub(crate) fn target(&self, at: ItemRef, n: NodeId) -> Option<&Target> {
        Some(&self.resolved(at, n)?.target)
    }
    pub(crate) fn resolved(&self, at: ItemRef, n: NodeId) -> Option<&ResolvedUse> {
        let info = &self.items[at.module as usize][at.item as usize];
        let k = *info
            .uses
            .get((n.0 - self.item(at).range.nodes.start) as usize)?;
        (k != u32::MAX).then(|| &info.resolution.names[k as usize])
    }

    pub(crate) fn core_named(&self, id: DefinitionId, names: &[&str]) -> bool {
        let d = self.index.get(id);
        let m = self.index.modules[d.site.module.0 as usize];
        Some(m.package) == self.index.core
            && m.depth == 1
            && self.index.symbols.text(m.name) == "core"
            && names
                .iter()
                .any(|n| self.index.symbols.find(n) == Some(d.name))
    }
    pub fn decl(&self, path: &DefPath) -> Option<&TypeDecl> {
        match self.by_path.get(path)? {
            Def::Type(d) => Some(&self.decls[d.index()]),
            Def::Fn(_) => None,
        }
    }
    pub fn function(&self, path: &DefPath) -> Option<&FnDecl> {
        match self.by_path.get(path)? {
            Def::Fn(f) => Some(&self.fns[*f as usize]),
            Def::Type(_) => None,
        }
    }
    /// The kind of a closed type (OWN-3).
    pub fn kind(&self, ty: TyId) -> Kind {
        decls::kind(&self.decls, &self.types, ty, &[])
    }
}

#[cfg(test)]
mod tests;
