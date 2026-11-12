use hirsel_core_ids::{DefPath, ItemPath, PackageId};
use hirsel_core_syntax::{Diagnostic, Item, NodeId, NodeKind, Span, TokenKind};
use std::collections::BTreeMap;

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, serde::Serialize, serde::Deserialize)]
pub struct ModuleId {
    pub package: PackageId,
    pub modules: Vec<String>,
}
impl ModuleId {
    pub fn definition(&self, name: &str) -> DefPath {
        DefPath(ItemPath {
            package: self.package.clone(),
            modules: self.modules.clone(),
            name: name.into(),
        })
    }
}
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Site {
    pub module: ModuleId,
    pub item: u32,
    pub span: Span,
}
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Refusal {
    pub rule: String,
    pub primary: Site,
    pub causal: Vec<Site>,
    pub declared: Option<String>,
    pub required: Option<String>,
    pub message: String,
}
impl Refusal {
    pub(crate) fn new(rule: &str, primary: Site, message: impl Into<String>) -> Self {
        Self {
            rule: rule.into(),
            primary,
            causal: vec![],
            declared: None,
            required: None,
            message: message.into(),
        }
    }
    pub fn syntax(module: ModuleId, diagnostic: &Diagnostic) -> Self {
        Self {
            rule: diagnostic.rule.clone(),
            primary: Site {
                module,
                item: u32::MAX,
                span: diagnostic.primary,
            },
            causal: vec![],
            declared: diagnostic.declared.clone(),
            required: diagnostic.required.clone(),
            message: diagnostic.message.clone(),
        }
    }
}
#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum DefinitionKind {
    Function,
    Constant,
    Type,
}
/// Index entries contain only declared headers. No body, AST or signature provider.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Definition {
    pub path: DefPath,
    pub kind: DefinitionKind,
    pub public: bool,
    pub opaque: bool,
    pub site: Site,
    pub fields: Vec<String>,
    pub variants: Vec<String>,
}
/// Explicit dependency exposures/prelude are supplied by the driver, never guessed from spelling.
#[derive(Clone, Debug, Default)]
pub struct Environment {
    pub prelude: BTreeMap<String, DefPath>,
    pub dependencies: BTreeMap<PackageId, BTreeMap<String, ModuleId>>,
}
#[derive(Clone, Debug, Default)]
pub struct NameIndex {
    definitions: BTreeMap<DefPath, Definition>,
    variants: BTreeMap<String, Vec<DefPath>>,
    pub environment: Environment,
    pub diagnostics: Vec<Refusal>,
}
impl NameIndex {
    pub fn new(environment: Environment) -> Self {
        Self {
            environment,
            ..Self::default()
        }
    }
    pub fn definitions(&self) -> impl Iterator<Item = &Definition> {
        self.definitions.values()
    }
    pub fn get(&self, path: &DefPath) -> Option<&Definition> {
        self.definitions.get(path)
    }
    pub fn variants(&self, name: &str) -> &[DefPath] {
        self.variants.get(name).map_or(&[], Vec::as_slice)
    }
    pub fn add(&mut self, definition: Definition) {
        if let Some(old) = self.definitions.get(&definition.path) {
            let mut d = Refusal::new("LANG-7", definition.site.clone(), "duplicate definition");
            d.causal.push(old.site.clone());
            self.diagnostics.push(d);
            return;
        }
        for name in &definition.variants {
            self.variants
                .entry(name.clone())
                .or_default()
                .push(definition.path.clone());
        }
        self.definitions.insert(definition.path.clone(), definition);
    }
    pub fn index_item(&mut self, module: &ModuleId, ordinal: u32, item: &Item) {
        let root = item.tree.node(item.root);
        let kind = match root.kind {
            NodeKind::Function => DefinitionKind::Function,
            NodeKind::Constant => DefinitionKind::Constant,
            NodeKind::TypeDecl
            | NodeKind::StableType
            | NodeKind::ResourceType
            | NodeKind::ScopedType => DefinitionKind::Type,
            _ => return,
        };
        let Some(name) = root.token else { return };
        let name = item.tree.token_text(&item.text, name);
        let mut definition = Definition {
            path: module.definition(name),
            kind,
            public: false,
            opaque: false,
            site: Site {
                module: module.clone(),
                item: ordinal,
                span: root.origin.span,
            },
            fields: vec![],
            variants: vec![],
        };
        for &child in item.tree.children(item.root) {
            let n = item.tree.node(child);
            match n.kind {
                NodeKind::Public => definition.public = true,
                NodeKind::Opaque => definition.opaque = true,
                NodeKind::Fields | NodeKind::Variants => {
                    for &field in item.tree.children(child) {
                        let n = item.tree.node(field);
                        if let Some(t) = n.token {
                            let name = item.tree.token_text(&item.text, t).into();
                            if n.kind == NodeKind::Field {
                                definition.fields.push(name);
                            } else if n.kind == NodeKind::Variant {
                                definition.variants.push(name);
                            }
                        }
                    }
                }
                _ => {}
            }
        }
        self.add(definition);
    }
}
pub(crate) fn path_parts(item: &Item, node: NodeId) -> Vec<String> {
    let r = &item.tree.node(node).origin.tokens;
    item.tree.tokens[r.start as usize..r.end as usize]
        .iter()
        .filter(|t| t.kind == TokenKind::Ident)
        .map(|t| item.text[t.span.start as usize..t.span.end as usize].to_owned())
        .collect()
}
