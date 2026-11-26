use super::*;

/// `h` and the 64 hex digits of `digest`.
pub fn symbol(digest: ContentHash) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut name = String::with_capacity(65);
    name.push('h');
    for byte in digest.0 {
        name.push(HEX[(byte >> 4) as usize] as char);
        name.push(HEX[(byte & 15) as usize] as char);
    }
    name
}

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

    /// The identity a descriptor names for `function`: its source instance's for a
    /// callable adapter, else its own code identity.
    pub fn descriptor_identity(&self, function: FuncId) -> ContentHash {
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

    /// The key of a spawn entry of rows `spawn`: their closures' hashes, which
    /// two modules agree on exactly when their rows are one type.
    pub fn spawn_key(&self, spawn: SpawnTypes) -> ContentHash {
        let (x, r) = (self.closure(spawn.x).hash, self.closure(spawn.r).hash);
        ContentHash::of_parts("hirsel.spawn-entry.v1", [&x.0[..], &r.0[..]])
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

/// The rows of a spawnable function's entry: `x`, the task input row
/// `(fn(take X) -> R, X)` that `task.spawn_unit` hands its helper, and `r`.
/// The link keys the function's spawn entry by them ([`Module::spawn_key`]):
/// two modules' rows are one entry exactly when both closures are equal.
#[derive(serde::Serialize, serde::Deserialize, Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub struct SpawnTypes {
    pub x: TypeId,
    pub r: TypeId,
}

/// The link name of the spawn entry of key `key` ([`Module::spawn_key`]):
/// `{bridge, adapter, layout, x, r}`, what `task.spawn_unit` passes for a
/// helper of those rows. Each unit that spawns one defines it weak.
pub fn spawn_entry_symbol(key: ContentHash) -> String {
    symbol(key) + "$spawn_entry"
}
/// The link name that marks descriptor `descriptor` as a spawnable function's
/// whose entry has key `key`. The link joins each marker to its key's spawn
/// entry ([`spawnable_parts`]).
pub fn spawnable_symbol(descriptor: &str, key: ContentHash) -> String {
    format!("{descriptor}$spawnable${}", symbol(key))
}
/// A [`spawnable_symbol`]'s descriptor name and its spawn entry's name.
pub fn spawnable_parts(name: &str) -> Option<(&str, String)> {
    let (descriptor, key) = name.split_once("$spawnable$")?;
    Some((descriptor, key.to_owned() + "$spawn_entry"))
}
