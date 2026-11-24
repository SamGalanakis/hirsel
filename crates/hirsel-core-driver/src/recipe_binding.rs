//! Bind recipe units' diagnostic sites to the current source map.

use super::{Hits, Unit, UnitModule};
use crate::native::{Module, NativeStop, key};
use hirsel_core_ids::{ContentHash, DefPath};
use hirsel_core_ir::{
    Identity, ItemId,
    normal::{Annex, Site},
    source::SourceMap,
};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Arc;

fn site_item(site: &Site) -> DefPath {
    crate::source_delta::site_item(&site.file, site.line, site.column)
}

/// The anchor of `site` in a unit whose source items are `items`, adding its
/// item on first sight: the unit's own with a unit `symbol`, else the site's.
/// Keyed by the borrowed site, so each distinct site builds its DefPath once.
fn anchor<'a>(
    seen: &mut HashMap<(&'a str, u32, u32), ItemId>,
    added: &mut Vec<(ItemId, &'a Site)>,
    items: &mut Vec<DefPath>,
    unit: Option<&str>,
    site: Option<&'a Site>,
) -> Option<hirsel_core_ir::Anchor> {
    let site = site?;
    let index = *seen
        .entry((site.file.as_str(), site.line, site.column))
        .or_insert_with(|| {
            let index = ItemId(items.len() as u32);
            items.push(match unit {
                Some(unit) => unit_item(unit, index),
                None => site_item(site),
            });
            added.push((index, site));
            index
        });
    Some(hirsel_core_ir::Anchor {
        item: index,
        start: 0,
        end: 0,
    })
}

/// Bind each unit's annex sites to source items, and the source map of every
/// site the units, `memos` and `extra` (a separately lowered std's sites)
/// name, with each relocated unit's and memo's own items bound as their
/// sites. Also each relocated unit's binding as its memo keeps it ([`Memo`]).
/// A memo's sites and items enter the map as its unit's binding gave them;
/// its module is bound only if it is decoded ([`Publication::memo_module`]).
/// With `remember` (the key of `extra`'s std, if any) a relocated map is
/// keyed by what it is a function of, and a resident compiler remembers it.
pub(in crate::native) fn bind_annexes(
    units: &mut BTreeMap<Identity, Unit>,
    memos: &Hits,
    extra: &BTreeMap<String, Vec<(u32, u32)>>,
    relocate: bool,
    remember: Option<Option<ContentHash>>,
) -> Result<Bindings, NativeStop> {
    if !relocate && !memos.is_empty() {
        return Err(NativeStop::Entry(
            "unit memos bind only relocated units".into(),
        ));
    }
    let mut bound = BTreeMap::new();
    for (identity, unit) in units.iter_mut() {
        // Cached objects reference a unit's local site numbers. The final link
        // binds those symbols to the current recipe's absolute positions.
        match &mut unit.module {
            UnitModule::Decoded(module) => {
                let symbol = relocate.then(|| hirsel_core_ir::symbol(identity.digest()));
                let added = bind_module(module, &unit.annex, symbol.as_deref());
                if relocate {
                    bound.insert(identity.clone(), Bound::of(&unit.annex, Some(added)));
                }
            }
            UnitModule::Encoded(_) => {
                debug_assert!(relocate);
                bound.insert(identity.clone(), Bound::of(&unit.annex, None));
            }
        }
    }
    // A unit's sites and items are its binding's; a memo's are its stored
    // body's and annex's.
    let key = remember
        .filter(|_| relocate)
        .map(|std| {
            let prepared: Vec<_> = bound
                .iter()
                .map(|(identity, bound)| (identity, &bound.points, &bound.anchored))
                .collect();
            let remembered: Vec<_> = memos
                .iter()
                .map(|(identity, (_, memo))| (identity, memo.body.0, memo.annex.0))
                .collect();
            key("hirsel.recipe-source-map.v1", &(std, prepared, remembered))
        })
        .transpose()?;
    let contributions: Vec<_> = bound
        .iter()
        .map(|(identity, bound)| (identity, None, &bound.points, &bound.anchored))
        .chain(memos.iter().map(|(identity, (_, memo))| {
            let of = Some((memo.body.0, memo.annex.0));
            (identity, of, &memo.points, &memo.anchored)
        }))
        .map(
            |(identity, of, points, anchored)| crate::source_delta::Contribution {
                symbol: hirsel_core_ir::symbol(identity.digest()),
                of,
                points,
                anchored,
            },
        )
        .collect();
    let (sources, made) =
        crate::source_delta::map(key, remember.flatten(), extra, &contributions, || {
            source_map(units, &bound, memos, extra)
        })?;
    Ok(Bindings {
        sources,
        key,
        rebased: made == crate::source_delta::Made::Rebased,
        bound,
    })
}

/// A program's bound source map, and its key when it is remembered
/// ([`bind_annexes`]); and each relocated unit's binding.
pub(in crate::native) struct Bindings {
    pub(in crate::native) sources: Arc<SourceMap>,
    pub(super) key: Option<ContentHash>,
    /// Whether a resident compiler rebased its last map to make `sources`.
    pub(super) rebased: bool,
    pub(super) bound: BTreeMap<Identity, Bound>,
}

/// The source map of every site `units`, `memos` and `extra` name, with the
/// items of each unit's binding in `bound` and of each memo bound as their
/// sites.
fn source_map(
    units: &BTreeMap<Identity, Unit>,
    bound: &BTreeMap<Identity, Bound>,
    memos: &Hits,
    extra: &BTreeMap<String, Vec<(u32, u32)>>,
) -> Result<SourceMap, NativeStop> {
    let mut points = BTreeMap::<String, BTreeSet<(u32, u32)>>::new();
    for (file, at) in extra {
        points
            .entry(file.clone())
            .or_default()
            .extend(at.iter().copied());
    }
    let mut add = |file: &String, at: (u32, u32)| match points.get_mut(file) {
        Some(file) => {
            file.insert(at);
        }
        None => {
            points.insert(file.clone(), BTreeSet::from([at]));
        }
    };
    for unit in units.values() {
        for site in annex_sites(&unit.annex) {
            add(&site.file, (site.line, site.column));
        }
    }
    for (_, memo) in memos.values() {
        for (file, line, column) in &memo.points {
            add(file, (*line, *column));
        }
    }
    let bindings = bound
        .iter()
        .map(|(identity, bound)| (identity, &bound.points[..], &bound.anchored[..]))
        .chain(
            memos
                .iter()
                .map(|(identity, (_, memo))| (identity, &memo.points[..], &memo.anchored[..])),
        )
        .map(|(identity, points, anchored)| {
            (hirsel_core_ir::symbol(identity.digest()), points, anchored)
        });
    crate::source_delta::assemble(points, bindings).map_err(NativeStop::Entry)
}

/// Every site of `annex`, in body order.
fn annex_sites(annex: &Annex) -> impl Iterator<Item = &Site> {
    annex.0.iter().flatten().flat_map(|hints| {
        hints
            .instructions
            .iter()
            .chain(&hints.terminators)
            .flatten()
    })
}

/// The source item of a relocated unit's item `index`, named by the unit's
/// `symbol`.
fn unit_item(symbol: &str, index: ItemId) -> DefPath {
    crate::source_delta::unit_item(symbol, index.0.to_string())
}

/// Anchor `module`'s sites to source items and apply its annex's hints; with
/// a unit `symbol`, each site's item is the unit's own. The items added, by
/// index, with their sites.
pub(super) fn bind_module<'a>(
    module: &mut Module,
    annex: &'a Annex,
    symbol: Option<&str>,
) -> Vec<(ItemId, &'a Site)> {
    let mut items = HashMap::new();
    let mut added = Vec::new();
    for (function, hints) in module.functions.iter_mut().zip(&annex.0) {
        let (Some(body), Some(hints)) = (function.body.as_mut(), hints) else {
            continue;
        };
        for (at, site) in body.inst_anchors.iter_mut().zip(&hints.instructions) {
            *at = anchor(
                &mut items,
                &mut added,
                &mut module.items,
                symbol,
                site.as_ref(),
            );
        }
        for (block, site) in body.blocks.iter_mut().zip(&hints.terminators) {
            block.anchor = anchor(
                &mut items,
                &mut added,
                &mut module.items,
                symbol,
                site.as_ref(),
            );
        }
        for (call, &cold) in body.calls.iter_mut().zip(&hints.cold_calls) {
            call.cold = cold;
        }
        for (edge, &cold) in body.edges.iter_mut().zip(&hints.cold_edges) {
            edge.cold = cold;
        }
        body.callable_parameters = hints
            .callable_parameters
            .iter()
            .copied()
            .map(hirsel_core_ir::ValueId)
            .collect();
    }
    added
}

/// A prepared unit's binding as its memo keeps it.
pub(in crate::native) struct Bound {
    pub(super) points: Vec<(String, u32, u32)>,
    pub(super) anchored: Vec<(u32, u32)>,
}

impl Bound {
    fn of(annex: &Annex, added: Option<Vec<(ItemId, &Site)>>) -> Self {
        let mut index = HashMap::new();
        let mut points = Vec::new();
        for site in annex_sites(annex) {
            index
                .entry((site.file.as_str(), site.line, site.column))
                .or_insert_with(|| {
                    points.push((site.file.clone(), site.line, site.column));
                    points.len() as u32 - 1
                });
        }
        let anchored = match added {
            Some(added) => added
                .into_iter()
                .map(|(item, site)| (item.0, index[&(site.file.as_str(), site.line, site.column)]))
                .collect(),
            // A normal body has no source items. Binding adds one per distinct
            // site in annex order, the same order as `points`.
            None => (0..points.len() as u32).map(|site| (site, site)).collect(),
        };
        Self { points, anchored }
    }
}
