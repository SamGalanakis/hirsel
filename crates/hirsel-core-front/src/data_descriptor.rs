//! Bind a data schema to the declarations selected by normal name resolution.
use super::*;
use hirsel_data::{DataError, Descriptor, ErrorKind, ResolvedType};
use std::collections::HashMap;

impl Program<'_> {
    /// Describe an authored concrete record in a checked program. The document
    /// uses the caller's module alias. Other modules retain their full identities.
    pub fn data_descriptor(&self, root: &DefPath, alias: &str) -> Result<Descriptor, DataError> {
        let unavailable = || DataError {
            kind: ErrorKind::Invalid("data root declaration is unavailable"),
            span: Span::default(),
            related: None,
            path: Vec::new(),
        };
        let id = self
            .index
            .module_at(&root.0.package, &root.0.modules)
            .ok_or_else(unavailable)?;
        let name = self
            .index
            .symbols
            .find(&root.0.name)
            .ok_or_else(unavailable)?;
        let def = self.index.definition(id, name).ok_or_else(unavailable)?;
        let Some(Def::Type(root_id)) = self.by_definition[def.0 as usize] else {
            return Err(unavailable());
        };
        if root_id.index() < self.decls.base_len() {
            return Err(unavailable());
        }
        let root_decl = &self.decls[root_id.index()];
        let modules: HashMap<_, _> = self
            .modules
            .iter()
            .enumerate()
            .map(|(i, m)| (std::ptr::from_ref(&m.syntax.tree), i))
            .collect();
        Descriptor::lower_resolved(alias, self.item(root_decl.at), |item, node| {
            let module = *modules.get(&std::ptr::from_ref(item.tree))?;
            let ordinal = self.modules[module]
                .syntax
                .items
                .partition_point(|r| r.nodes.end <= node.0);
            let at = ItemRef {
                module: module as u32,
                item: ordinal as u32,
            };
            match self.target(at, node)? {
                Target::Builtin(symbol) => {
                    Some(ResolvedType::Core(self.index.symbols.text(*symbol)))
                }
                Target::Definition(def) => {
                    let Def::Type(d) = self.by_definition[def.0 as usize]? else {
                        return None;
                    };
                    let decl = &self.decls[d.index()];
                    if is_core_prelude(&decl.path, &[]) {
                        return Some(ResolvedType::Core(&decl.path.0.name));
                    }
                    if d.index() < self.decls.base_len() {
                        return None;
                    }
                    let path = &decl.path.0;
                    let module = if path.package == root.0.package && path.modules == root.0.modules
                    {
                        alias.into()
                    } else {
                        format!(
                            "{}@{}/{}",
                            path.package.name,
                            path.package.version,
                            path.modules.join(".")
                        )
                    };
                    Some(ResolvedType::Nominal {
                        module,
                        item: self.item(decl.at),
                    })
                }
                _ => None,
            }
        })
    }
}

#[cfg(test)]
#[path = "data_descriptor_tests.rs"]
mod tests;
