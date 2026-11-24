use super::*;

/// The link name of the revision carrier of the root whose identity digest is
/// `digest` ([`Module::revision_root`]): a `Bytes` carrier `{bytes, 32, 0}` of
/// its code revision. Each unit that describes an item's function defines it
/// weak as `{0, 0, 0}` (no revision), and a link that knows the program's
/// reach defines it; an intrinsic's carrier holds [`intrinsic_revision`].
pub fn revision_symbol(digest: ContentHash) -> String {
    symbol(digest) + "$revision"
}
/// Whose code revision a described function has (PROG-9 `code.revision`):
/// the code a call through the `StaticFn` runs.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum RevisionRoot {
    /// A reached instance: the described item, which a `$callable` adapter
    /// calls.
    Item(Identity),
    /// A core item without a body, which the adapter computes itself: no
    /// checked IR reaches out of it ([`intrinsic_revision`]).
    Intrinsic(Identity),
}
impl RevisionRoot {
    pub fn identity(&self) -> &Identity {
        match self {
            Self::Item(identity) | Self::Intrinsic(identity) => identity,
        }
    }
}
impl Module {
    /// The source instance behind a descriptor's callable adapter. Different
    /// bodies can prepare its callable ABI with different checked IR.
    fn described_instance(&self, function: FuncId) -> Option<Identity> {
        let instance = self.functions[function.index()].instance;
        let (source, adapter) = self.instances[instance.index()]
            .definition
            .0
            .name
            .split_once("$callable[")?;
        if adapter == "instance]" || adapter.starts_with("instance:") {
            let mut identity = self.identity(instance);
            identity.definition.0.name = source.to_owned();
            Some(identity)
        } else {
            None
        }
    }

    pub(crate) fn descriptor_identity(&self, function: FuncId) -> ContentHash {
        self.described_instance(function).map_or_else(
            || self.code_identity(function),
            |identity| identity.digest(),
        )
    }

    /// The weak descriptor symbol. Generic descriptors share one symbol per
    /// described instance, independently of the adapter that carries its ABI.
    pub fn descriptor_symbol(&self, function: FuncId, function_symbol: &str) -> String {
        match self.described_instance(function) {
            Some(identity) => format!("{}$static", identity.symbol()),
            None => format!("{function_symbol}$static"),
        }
    }

    /// The revision root of described function `function`: itself, or for a
    /// `$callable` adapter the item it calls. An instance adapter carries that
    /// instance's key, even in a unit with only a reference to the adapter.
    pub fn revision_root(&self, function: FuncId) -> RevisionRoot {
        if let Some(root) = self.described_instance(function) {
            return if crate::is_versioned_builtin(&root.definition) {
                RevisionRoot::Intrinsic(root)
            } else {
                RevisionRoot::Item(root)
            };
        }
        let f = &self.functions[function.index()];
        let identity = self.identity(f.instance);
        let Some((source, _)) = identity.definition.0.name.split_once("$callable[") else {
            return RevisionRoot::Item(identity);
        };
        let mut item = identity.definition.clone();
        item.0.name = source.to_owned();
        let callee = f
            .body
            .iter()
            .flat_map(|b| &b.calls)
            .find_map(|c| match c.callee {
                Callee::Direct(g) => {
                    let callee = self.identity(self.functions[g.index()].instance);
                    (callee.definition == item).then_some(callee)
                }
                _ => None,
            });
        match callee {
            Some(callee) => RevisionRoot::Item(callee),
            None => RevisionRoot::Intrinsic(Identity::plain(item)),
        }
    }
}
/// The code revision of a compiler-implemented core definition.
pub fn intrinsic_revision(root: &Identity) -> ContentHash {
    crate::code_revision(crate::builtin_address(root).expect("intrinsic has a semantics number"))
}
