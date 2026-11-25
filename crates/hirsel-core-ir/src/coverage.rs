//! Coverage sites (`docs/coverage.md`, "Sites"): the one definition of which
//! places of a body a coverage build flags, their order and their keys. Both
//! engines and the driver read a body's sites from [`sites`].

use crate::source::numbers_unit_sites;
use crate::{Anchor, BlockId, Body, EdgeId, FuncId, Module, Terminator};
use hirsel_core_ids::{ContentHash, DefPath, PackageId};
use std::fmt::Write as _;

/// The packages whose items have sites: those given as source in the request.
/// A module without a scope ([`Module::coverage`] `None`) has no sites.
pub type Scope = std::collections::BTreeSet<PackageId>;

/// A body's decisions by block, each as its arms' rows, read off the body
/// before its anchors are rebound ([`decisions`]).
pub type Decisions = Vec<Option<Vec<SiteRow>>>;

/// A unit's decisions by the definition of each body it holds (its root and
/// its closures, whose anchors binding rebinds to the unit's sites too).
pub type UnitDecisions = std::collections::BTreeMap<String, Decisions>;

/// The decisions of units whose anchors were rebound to their unit's sites,
/// by unit symbol. A release unit's anchors name `source@1/unit/<symbol>/<n>`,
/// a site of the unit at no offset, so the keys cannot be read off them; and
/// binding drops the anchor of a decision whose place has no source position.
/// A body's decisions are found by block index, which holds only while a
/// pass keeps the body's blocks in their canonical order; a body whose
/// blocks no longer end with its source's decisions is refused
/// ([`Mismatch`]).
pub type Rebound = std::collections::BTreeMap<String, UnitDecisions>;

/// The bodies a pass after binding rebuilt with blocks of their own, by
/// function: for each block of the body it was built from, the block that
/// ends it, with that block's terminator and anchor. A rebound body found
/// here takes its source's decisions through this map; any other keeps its
/// source's blocks, and a body that does not refuses ([`Mismatch`]).
pub type Moved = std::collections::BTreeMap<FuncId, Vec<BlockId>>;

/// The domain tag of a record id.
pub const ID_TAG: &str = "hirsel.coverage.sites.v1";
/// The first word of every record in the `hirsel_cov` section.
pub const RECORD_MAGIC: u32 = 0x5643_4948;
/// The bytes before a record's flags: magic, site count and id.
pub const RECORD_HEADER: usize = 40;
/// The link section that holds every record.
pub const SECTION: &str = "hirsel_cov";
/// The name a coverage build's scope enters the native environment under.
/// It names the map fragment's meaning too, so a fragment built under an
/// earlier rule is never joined into a later map.
pub const KEY_TAG: &str = "coverage.v2";

/// A rebound body whose blocks are not its source's: a pass between the
/// unit's materialisation and its emission changed the body's blocks, so its
/// carried decisions ([`Rebound`]) would flag the wrong arms. The build stops
/// with it, naming the unit.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Mismatch {
    /// The symbol of the unit the body's anchors were rebound to.
    pub unit: String,
    /// The body's definition.
    pub definition: String,
    /// What differs from the source.
    pub reason: String,
}

impl std::fmt::Display for Mismatch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "coverage: a body of {} in unit {} {}",
            self.definition, self.unit, self.reason
        )
    }
}

impl std::error::Error for Mismatch {}

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
    /// An arm's row as its source gave it, for a rebound body.
    pub row: Option<SiteRow>,
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
    module
        .coverage
        .as_ref()
        .is_some_and(|scope| in_scope_of(module, function, scope))
}

fn in_scope_of(module: &Module, function: FuncId, scope: &Scope) -> bool {
    let row = &module.functions[function.index()];
    let Some(body) = &row.body else {
        return false;
    };
    let definition = &module.instances[row.instance.index()].definition;
    scope.contains(&definition.0.package)
        && (body.blocks.iter().any(|b| b.anchor.is_some())
            || body.inst_anchors.iter().any(Option::is_some)
            || rooted_unit(module, function).is_some())
}

/// The unit a bound body with no anchors roots, when `coverage_rebound`
/// holds that unit's decisions of its definition. Binding gives a core@1 body
/// no anchors (its failure sites are its caller's, so its items have no
/// native source positions), but its unit is keyed by its own instance's
/// symbol, which every code variant of the root keeps.
fn rooted_unit(module: &Module, function: FuncId) -> Option<String> {
    let rebound = module.coverage_rebound.as_deref()?;
    let row = &module.functions[function.index()];
    let symbol = module.identity(row.instance).symbol();
    let definition = module.instances[row.instance.index()]
        .definition
        .to_string();
    rebound
        .get(&symbol)?
        .contains_key(&definition)
        .then_some(symbol)
}

/// `function`'s sites, or none when the module does not flag it
/// ([`in_scope`]). Every instance and code variant of one item's body gives
/// the same keys, since keys name the source item and offsets within it.
pub fn sites(module: &Module, function: FuncId) -> Result<Option<Sites>, Mismatch> {
    match module.coverage.as_ref() {
        Some(scope) => sites_of(module, function, scope),
        None => Ok(None),
    }
}

/// `function`'s decisions under `scope` by block, or none when `scope` does
/// not flag it: what [`Rebound`] keeps of a body before its anchors are
/// rebound.
pub fn decisions(
    module: &Module,
    function: FuncId,
    scope: &Scope,
) -> Result<Option<Decisions>, Mismatch> {
    let Some(sites) = sites_of(module, function, scope)? else {
        return Ok(None);
    };
    let Some(body) = module.functions[function.index()].body.as_ref() else {
        return Ok(None);
    };
    let blocks = body.blocks.len();
    let mut decisions: Decisions = vec![None; blocks];
    for (site, row) in sites.sites.iter().zip(rows(module, function, &sites)) {
        if let Kind::Arm { block, .. } = site.kind {
            decisions[block.index()]
                .get_or_insert_with(Vec::new)
                .push(row);
        }
    }
    Ok(Some(decisions))
}

/// The decisions of every body of a unit's materialised `module` that
/// `scope` flags, by definition: what [`Rebound`] keeps of a unit.
pub fn unit_decisions(module: &Module, scope: &Scope) -> Result<UnitDecisions, Mismatch> {
    let mut unit = UnitDecisions::new();
    for (f, row) in module.functions.iter().enumerate() {
        let Some(decisions) = decisions(module, FuncId(f as u32), scope)? else {
            continue;
        };
        let definition = module.instances[row.instance.index()]
            .definition
            .to_string();
        if let Some(other) = unit.insert(definition, decisions.clone()) {
            // Two bodies of one definition in a unit are told apart by
            // nothing a rebound body keeps.
            assert!(
                other == decisions,
                "coverage: a unit holds two bodies of one definition"
            );
        }
    }
    Ok(unit)
}

/// The arms of the decision `terminator` ends a block with, if it is one.
fn arms(terminator: &Terminator) -> Option<Vec<Arm>> {
    match terminator {
        Terminator::Branch { .. } => Some(vec![Arm::No, Arm::Yes]),
        Terminator::Switch { arms, .. } => Some(
            (0..arms.len)
                .map(Arm::Case)
                .chain(std::iter::once(Arm::Otherwise))
                .collect(),
        ),
        _ => None,
    }
}

/// The symbol of the unit whose sites `body`'s anchors were rebound to.
fn rebound_unit<'m>(module: &'m Module, body: &Body) -> Option<&'m str> {
    let blocks = body.blocks.iter().map(|block| block.anchor);
    blocks
        .chain(body.inst_anchors.iter().copied())
        .flatten()
        .map(|anchor| &module.items[anchor.item.index()])
        .find(|item| numbers_unit_sites(item))
        .map(|item| item.0.modules[1].as_str())
}

fn sites_of(module: &Module, function: FuncId, scope: &Scope) -> Result<Option<Sites>, Mismatch> {
    if !in_scope_of(module, function, scope) {
        return Ok(None);
    }
    let row = &module.functions[function.index()];
    let Some(body) = row.body.as_ref() else {
        return Ok(None);
    };
    let definition = module.instances[row.instance.index()].definition.clone();
    let mut sites = vec![Site {
        anchor: None,
        key: format!("{definition}#e"),
        definition,
        kind: Kind::Entry,
        row: None,
    }];
    let mut decisions = vec![None; body.blocks.len()];
    let unit = rebound_unit(module, body)
        .map(str::to_owned)
        .or_else(|| rooted_unit(module, function));
    if let Some(symbol) = unit.as_deref() {
        let definition = module.instances[row.instance.index()]
            .definition
            .to_string();
        // A mismatch would misattribute flags, so it stops the build.
        let mismatch = |reason: String| Mismatch {
            unit: symbol.to_owned(),
            definition: definition.clone(),
            reason,
        };
        let unit = (module.coverage_rebound.as_deref())
            .and_then(|rebound| rebound.get(symbol)?.get(&definition))
            .ok_or_else(|| mismatch("has no decisions".into()))?;
        // The block that ends each source block: itself, unless a declared
        // rebuild moved it.
        let moved = module.coverage_moved.get(&function);
        let blocks = moved.map_or(body.blocks.len(), Vec::len);
        if unit.len() != blocks {
            return Err(mismatch(format!(
                "has {blocks} blocks and its source {}",
                unit.len()
            )));
        }
        for (source, rows) in unit.iter().enumerate() {
            let Some(rows) = rows else {
                continue;
            };
            let index = moved.map_or(source, |moved| moved[source].index());
            if index >= body.blocks.len() {
                return Err(mismatch(format!(
                    "moves block {source} to {index}, past its {} blocks",
                    body.blocks.len()
                )));
            }
            let arms = arms(&body.terminators[index]).filter(|arms| arms.len() == rows.len());
            let arms = arms.ok_or_else(|| {
                mismatch(format!(
                    "does not end block {index} with its source's {}-arm decision",
                    rows.len()
                ))
            })?;
            decisions[index] = Some(sites.len() as u32);
            for (arm, row) in arms.into_iter().zip(rows) {
                sites.push(Site {
                    anchor: body.blocks[index].anchor,
                    key: row.key.clone(),
                    definition: row.decision.as_ref().unwrap().definition.clone(),
                    kind: Kind::Arm {
                        block: BlockId(index as u32),
                        arm,
                    },
                    row: Some(row.clone()),
                });
            }
        }
    } else {
        let mut shared = std::collections::HashMap::<(u32, u32, u32), u32>::new();
        for (index, block) in body.blocks.iter().enumerate() {
            let anchor = block.anchor.filter(|_| block.decision);
            let (Some(anchor), Some(arms)) = (anchor, arms(&body.terminators[index])) else {
                continue;
            };
            let item = &module.items[anchor.item.index()];
            decisions[index] = Some(sites.len() as u32);
            let n = shared
                .entry((anchor.item.0, anchor.start, anchor.end))
                .or_default();
            for arm in arms {
                let mut key = String::new();
                write!(key, "{item}@{}-{}.{n}:{arm}", anchor.start, anchor.end).unwrap();
                sites.push(Site {
                    anchor: Some(anchor),
                    key,
                    definition: item.clone(),
                    kind: Kind::Arm {
                        block: BlockId(index as u32),
                        arm,
                    },
                    row: None,
                });
            }
            *n += 1;
        }
    }
    let id = ContentHash::of_parts(ID_TAG, sites.iter().map(|s| s.key.as_bytes()));
    Ok(Some(Sites {
        sites,
        id,
        decisions,
    }))
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
#[derive(Clone, Debug, Eq, Hash, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Position {
    pub definition: DefPath,
    pub start: u32,
    pub end: u32,
}

/// One site as a map fragment holds it.
#[derive(Clone, Debug, Eq, Hash, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SiteRow {
    pub key: String,
    /// An arm's name; none for an entry.
    pub arm: Option<String>,
    /// An arm's decision; none for an entry, whose place is its function's.
    pub decision: Option<Position>,
    /// The first anchored position of an arm's target block; none for an
    /// entry and for an empty arm.
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
        self.records.push(Record {
            id: sites.id,
            function: module.instances[row.instance.index()].definition.clone(),
            sites: rows(module, function, sites),
        });
    }
}

/// The rows of `function`'s `sites`: a rebound arm's as its source gave it.
fn rows(module: &Module, function: FuncId, sites: &Sites) -> Vec<SiteRow> {
    let body = module.functions[function.index()].body.as_ref().unwrap();
    let position = |anchor: Anchor| Position {
        definition: module.items[anchor.item.index()].clone(),
        start: anchor.start,
        end: anchor.end,
    };
    sites
        .sites
        .iter()
        .map(|site| match (&site.row, &site.kind) {
            (Some(row), _) => row.clone(),
            (None, Kind::Entry) => SiteRow {
                key: site.key.clone(),
                arm: None,
                decision: None,
                target: None,
            },
            (&None, &Kind::Arm { block, arm }) => {
                let index = body.edges[edge(body, block, arm).index()].target.index();
                let target = &body.blocks[index];
                let (start, len) = (target.insts.start as usize, target.insts.len as usize);
                let first =
                    (start..start + len).find_map(|i| body.inst_anchors.get(i).copied().flatten());
                // An empty arm, such as the `no` of an `if` without `else`,
                // reaches a block with no source code of its own that only
                // jumps on: it has no position ("The site map", `arm_line`).
                let empty =
                    first.is_none() && matches!(body.terminators[index], Terminator::Jump(_));
                let first = first.or(target.anchor).filter(|_| !empty);
                SiteRow {
                    key: site.key.clone(),
                    arm: Some(arm.to_string()),
                    decision: site.anchor.map(position),
                    target: first.map(position),
                }
            }
        })
        .collect()
}

/// The map fragment of an object `module` is emitted into whole, in a
/// coverage build: each flagged function's record, in function order, as an
/// engine that emits every body adds them.
pub fn fragment(module: &Module) -> Result<Option<Fragment>, Mismatch> {
    if module.coverage.is_none() {
        return Ok(None);
    }
    let mut fragment = Fragment::default();
    for f in 0..module.functions.len() {
        let function = FuncId(f as u32);
        if let Some(sites) = sites(module, function)? {
            fragment.add(module, function, &sites);
        }
    }
    Ok(Some(fragment))
}
