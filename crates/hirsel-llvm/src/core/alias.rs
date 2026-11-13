//! FORM storage roles are disjoint within one activation. Scope declarations
//! let LLVM clone that activation's domains when it inlines the function.
use super::*;
use hirsel_core_ir::roles::{Role, roles};

pub(super) struct Scopes {
    addresses: Vec<Option<usize>>,
    scopes: Vec<usize>,
}

impl Scopes {
    pub fn new(
        module: &Module,
        function: &Function,
        name: &str,
        next: &mut usize,
        declarations: &mut BTreeSet<String>,
    ) -> Option<Self> {
        let addresses = roles(module, function);
        let mut unique = Vec::<Role>::new();
        let addresses = addresses
            .into_iter()
            .map(|role| {
                role.map(|role| {
                    if let Some(index) = unique.iter().position(|old| *old == role) {
                        index
                    } else {
                        unique.push(role);
                        unique.len() - 1
                    }
                })
            })
            .collect();
        if unique.len() < 2 {
            return None;
        }
        let domain = *next;
        *next += 1;
        declarations.insert(format!("!{domain} = distinct !{{!{domain}, !\"{name}\"}}"));
        let mut scopes = Vec::new();
        for role in unique {
            let scope = *next;
            *next += 1;
            declarations.insert(format!(
                "!{scope} = distinct !{{!{scope}, !{domain}, !\"{role:?}\"}}"
            ));
            scopes.push(scope);
        }
        declarations.insert("declare void @llvm.experimental.noalias.scope.decl(metadata)".into());
        Some(Self { addresses, scopes })
    }

    pub fn declaration(&self) -> String {
        self.scopes
            .iter()
            .map(|scope| {
                format!("call void @llvm.experimental.noalias.scope.decl(metadata !{{!{scope}}})")
            })
            .collect::<Vec<_>>()
            .join("\n  ")
    }

    pub fn access(&self, addresses: &[AddressId]) -> String {
        // A memcpy touches both operands. An unclassified side could touch any
        // role, so it prevents an assertion about the instruction as a whole.
        let Some(roles) = addresses
            .iter()
            .map(|a| self.addresses[a.index()])
            .collect::<Option<Vec<_>>>()
        else {
            return String::new();
        };
        let included: Vec<_> = self
            .scopes
            .iter()
            .enumerate()
            .filter(|(i, _)| roles.contains(i))
            .map(|(_, scope)| *scope)
            .collect();
        let excluded: Vec<_> = self
            .scopes
            .iter()
            .enumerate()
            .filter(|(i, _)| !roles.contains(i))
            .map(|(_, scope)| *scope)
            .collect();
        let mut result = format!(", !alias.scope !{{{}}}", list(included));
        if !excluded.is_empty() {
            write!(result, ", !noalias !{{{}}}", list(excluded)).unwrap();
        }
        result
    }
}

fn list(scopes: impl IntoIterator<Item = usize>) -> String {
    scopes
        .into_iter()
        .map(|id| format!("!{id}"))
        .collect::<Vec<_>>()
        .join(", ")
}
