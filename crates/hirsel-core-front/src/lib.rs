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
mod decls;
mod layout;
mod own;
mod types;

pub use body::{CoreCheck, Counts, Emitter};
pub use decls::*;
pub use types::*;

use hirsel_core_ids::DefPath;
use hirsel_core_resolve::{
    ModuleId, NameIndex, Refusal, Resolution, ResolvedUse, Site, Target, resolve,
};
use hirsel_core_syntax::{Item, NodeId, Span, SyntaxFile};
use std::collections::HashMap;

/// One module of the program, already parsed.
pub struct ModuleInput {
    pub id: ModuleId,
    pub syntax: SyntaxFile,
}

/// An item of a module.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
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
    pub resolution: Resolution,
    /// Node index to its entry in `resolution.names`, or `u32::MAX`.
    pub uses: Vec<u32>,
}

/// The declarations of one program: summaries and signatures, published once.
pub struct Program<'a> {
    pub index: &'a NameIndex,
    pub modules: &'a [ModuleInput],
    pub(crate) items: Vec<Vec<ItemInfo>>,
    pub types: Types,
    pub decls: Vec<TypeDecl>,
    pub fns: Vec<FnDecl>,
    pub by_path: HashMap<DefPath, Def>,
    /// Refusals of declarations and signatures, and the resolver's.
    pub refusals: Vec<Refusal>,
    pub gaps: Vec<Gap>,
    /// The program's entry, as its manifest names it (PROG-3); `main` is always `waits`
    /// (LANG-8).
    pub entry: Option<DefPath>,
    pub(crate) pending: Vec<decls::Pending>,
    pub(crate) alias_state: Vec<u8>,
    /// Aliases being lowered, each with its reference continuing the chain.
    pub(crate) alias_stack: Vec<(DeclId, Option<Site>)>,
}

impl<'a> Program<'a> {
    /// Resolve every item, then publish summaries and signatures (slice a).
    pub fn declare(index: &'a NameIndex, modules: &'a [ModuleInput]) -> Self {
        let mut refusals = index.diagnostics.clone();
        let items = modules
            .iter()
            .map(|m| {
                m.syntax
                    .items
                    .iter()
                    .enumerate()
                    .map(|(i, item)| {
                        let resolution = resolve(index, &m.id, i as u32, item);
                        refusals.extend(resolution.diagnostics.iter().cloned());
                        let mut uses = vec![u32::MAX; item.tree.nodes.len()];
                        for (k, u) in resolution.names.iter().enumerate() {
                            uses[u.node.0 as usize] = k as u32;
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
            types: Types::new(),
            decls: vec![],
            fns: vec![],
            by_path: HashMap::new(),
            refusals,
            gaps: vec![],
            entry: None,
            pending: vec![],
            alias_state: vec![],
            alias_stack: vec![],
        };
        p.register();
        p.lower_headers();
        p.summarize();
        p
    }

    pub(crate) fn item(&self, at: ItemRef) -> &'a Item {
        let modules: &'a [ModuleInput] = self.modules;
        &modules[at.module as usize].syntax.items[at.item as usize]
    }
    pub(crate) fn site(&self, at: ItemRef, n: NodeId) -> Site {
        self.site_span(at, self.item(at).tree.node(n).origin.span)
    }
    pub(crate) fn site_span(&self, at: ItemRef, span: Span) -> Site {
        Site {
            module: self.modules[at.module as usize].id.clone(),
            item: at.item,
            span,
        }
    }
    pub(crate) fn target(&self, at: ItemRef, n: NodeId) -> Option<&Target> {
        Some(&self.resolved(at, n)?.target)
    }
    pub(crate) fn resolved(&self, at: ItemRef, n: NodeId) -> Option<&ResolvedUse> {
        let info = &self.items[at.module as usize][at.item as usize];
        let k = *info.uses.get(n.0 as usize)?;
        (k != u32::MAX).then(|| &info.resolution.names[k as usize])
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
