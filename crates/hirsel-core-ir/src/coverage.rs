//! Coverage sites (`docs/coverage.md`, "Sites"): the one definition of which
//! places of a body a coverage build flags, their order and their keys. Both
//! engines and the driver read a body's sites from [`sites`].

use crate::{Anchor, BlockId, Body, EdgeId, FuncId, Module, Terminator};
use hirsel_core_ids::{ContentHash, DefPath, PackageId};
use std::fmt::Write as _;

/// The packages whose items have sites: those given as source in the request.
/// A module without a scope ([`Module::coverage`] `None`) has no sites.
pub type Scope = std::collections::BTreeSet<PackageId>;

/// The domain tag of a record id.
pub const ID_TAG: &str = "hirsel.coverage.sites.v1";
/// The first word of every record in the `hirsel_cov` section.
pub const RECORD_MAGIC: u32 = 0x5643_4948;
/// The bytes before a record's flags: magic, site count and id.
pub const RECORD_HEADER: usize = 40;
/// The link section that holds every record.
pub const SECTION: &str = "hirsel_cov";

/// An arm of a source decision.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Arm {
    Yes,
    No,
    /// The case of a switch, numbered from 0 in terminator order.
    Case(u32),
    Otherwise,
}

impl std::fmt::Display for Arm {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Yes => f.write_str("yes"),
            Self::No => f.write_str("no"),
            Self::Case(n) => write!(f, "{n}"),
            Self::Otherwise => f.write_str("otherwise"),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Kind {
    Entry,
    /// An edge of the decision that ends `block`.
    Arm {
        block: BlockId,
        arm: Arm,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Site {
    /// The anchor of the decision an arm belongs to; none for an entry.
    pub anchor: Option<Anchor>,
    pub key: String,
    /// The item that holds the site: the function's definition for an
    /// entry, the anchor's item for an arm.
    pub definition: DefPath,
    pub kind: Kind,
}

/// A body's sites in their one order: the entry, then each decision's arms in
/// block order.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Sites {
    pub sites: Vec<Site>,
    /// SHA-256 over [`ID_TAG`] and each key in order, each framed by its
    /// length in eight little-endian bytes ([`ContentHash::of_parts`]).
    pub id: ContentHash,
    /// By block, the flag of the first arm of the decision that ends it. A
    /// branch's arms are `no` then `yes`, so its taken arm is that flag plus
    /// the condition. A switch's are its cases in order, then `otherwise`.
    pub decisions: Vec<Option<u32>>,
}

impl Sites {
    /// The record image the section starts with: magic, count, id, zeroed
    /// flags, padded to eight bytes.
    pub fn record(&self) -> Vec<u8> {
        let n = self.sites.len();
        let mut bytes = Vec::with_capacity((RECORD_HEADER + n).next_multiple_of(8));
        bytes.extend_from_slice(&RECORD_MAGIC.to_le_bytes());
        bytes.extend_from_slice(&(n as u32).to_le_bytes());
        bytes.extend_from_slice(&self.id.0);
        bytes.resize((RECORD_HEADER + n).next_multiple_of(8), 0);
        bytes
    }
}

/// Whether `module` flags `function`'s body: its definition's package is in
/// the scope and the body has source, so generated glue has no sites.
pub fn in_scope(module: &Module, function: FuncId) -> bool {
    let Some(scope) = &module.coverage else {
        return false;
    };
    let row = &module.functions[function.index()];
    let Some(body) = &row.body else {
        return false;
    };
    let definition = &module.instances[row.instance.index()].definition;
    scope.contains(&definition.0.package)
        && (body.blocks.iter().any(|b| b.anchor.is_some())
            || body.inst_anchors.iter().any(Option::is_some))
}

/// `function`'s sites, or none when the module does not flag it
/// ([`in_scope`]). Every instance and code variant of one item's body gives
/// the same keys, since keys name the source item and offsets within it.
pub fn sites(module: &Module, function: FuncId) -> Option<Sites> {
    if !in_scope(module, function) {
        return None;
    }
    let row = &module.functions[function.index()];
    let body = row.body.as_ref()?;
    let definition = module.instances[row.instance.index()].definition.clone();
    let mut sites = vec![Site {
        anchor: None,
        key: format!("{definition}#e"),
        definition,
        kind: Kind::Entry,
    }];
    let mut shared = std::collections::HashMap::<(u32, u32, u32), u32>::new();
    let mut decisions = vec![None; body.blocks.len()];
    for (index, block) in body.blocks.iter().enumerate() {
        let Some(anchor) = block.anchor else {
            continue;
        };
        let arms: Vec<Arm> = match &body.terminators[index] {
            Terminator::Branch { .. } => vec![Arm::No, Arm::Yes],
            Terminator::Switch { arms, .. } => (0..arms.len)
                .map(Arm::Case)
                .chain(std::iter::once(Arm::Otherwise))
                .collect(),
            _ => continue,
        };
        let item = module.items[anchor.item.index()].clone();
        let n = shared
            .entry((anchor.item.0, anchor.start, anchor.end))
            .or_default();
        decisions[index] = Some(sites.len() as u32);
        let block = BlockId(index as u32);
        for arm in arms {
            let mut key = String::new();
            write!(key, "{item}@{}-{}.{n}:{arm}", anchor.start, anchor.end).unwrap();
            sites.push(Site {
                anchor: Some(anchor),
                key,
                definition: item.clone(),
                kind: Kind::Arm { block, arm },
            });
        }
        *n += 1;
    }
    let id = ContentHash::of_parts(ID_TAG, sites.iter().map(|s| s.key.as_bytes()));
    Some(Sites {
        sites,
        id,
        decisions,
    })
}

/// The edge an arm takes.
pub fn edge(body: &Body, block: BlockId, arm: Arm) -> EdgeId {
    match (&body.terminators[block.index()], arm) {
        (Terminator::Branch { yes, .. }, Arm::Yes) => *yes,
        (Terminator::Branch { no, .. }, Arm::No) => *no,
        (Terminator::Switch { arms, .. }, Arm::Case(n)) => {
            arms.get(&body.pools.arms).unwrap()[n as usize].edge
        }
        (Terminator::Switch { otherwise, .. }, Arm::Otherwise) => *otherwise,
        _ => unreachable!("an arm of a block that does not decide it"),
    }
}

/// A source range: byte offsets relative to the start of `definition`.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Position {
    pub definition: DefPath,
    pub start: u32,
    pub end: u32,
}

/// One site as a map fragment holds it.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SiteRow {
    pub key: String,
    /// An arm's name; none for an entry.
    pub arm: Option<String>,
    /// An arm's decision; none for an entry, whose place is its function's.
    pub decision: Option<Position>,
    /// The first anchored position of an arm's target block, when it has one.
    pub target: Option<Position>,
}

/// The sites of one record id, in [`sites`] order.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Record {
    pub id: ContentHash,
    pub function: DefPath,
    pub sites: Vec<SiteRow>,
}

/// The map fragment of one object (`docs/coverage.md`, "The site map"): each
/// record id its bodies write, once, with what the driver needs to name and
/// place its sites. The driver joins the fragments of a link into its map.
#[derive(Clone, Debug, Default, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Fragment {
    pub records: Vec<Record>,
}

impl Fragment {
    /// Add `function`'s record unless one with its id is already here.
    pub fn add(&mut self, module: &Module, function: FuncId, sites: &Sites) {
        if self.records.iter().any(|r| r.id == sites.id) {
            return;
        }
        let row = &module.functions[function.index()];
        let body = row.body.as_ref().unwrap();
        let position = |anchor: Anchor| Position {
            definition: module.items[anchor.item.index()].clone(),
            start: anchor.start,
            end: anchor.end,
        };
        let rows = sites
            .sites
            .iter()
            .map(|site| match site.kind {
                Kind::Entry => SiteRow {
                    key: site.key.clone(),
                    arm: None,
                    decision: None,
                    target: None,
                },
                Kind::Arm { block, arm } => {
                    let target = body.edges[edge(body, block, arm).index()].target;
                    let target = &body.blocks[target.index()];
                    let (start, len) = (target.insts.start as usize, target.insts.len as usize);
                    let first = (start..start + len)
                        .find_map(|i| body.inst_anchors.get(i).copied().flatten())
                        .or(target.anchor);
                    SiteRow {
                        key: site.key.clone(),
                        arm: Some(arm.to_string()),
                        decision: site.anchor.map(position),
                        target: first.map(position),
                    }
                }
            })
            .collect();
        self.records.push(Record {
            id: sites.id,
            function: module.instances[row.instance.index()].definition.clone(),
            sites: rows,
        });
    }
}
