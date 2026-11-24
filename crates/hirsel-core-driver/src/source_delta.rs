//! A recipe's source map, and a resident compiler's rebase of it after an
//! edit.
//!
//! The map is the union of its contributors' sites: each relocated unit's
//! binding, each unit memo's, and a separately lowered std's points. A site
//! is a source item named by its file, line and column, numbered by its rank
//! among its file's sites; a contributor's anchored items are named by its
//! unit and bound to their site's rank. An edit changes only the
//! contributors whose sites it moves. A resident compiler keeps each
//! contributor's sites by file, renumbers only the files whose sites
//! changed, and rebinds only the items in them, as rust-analyzer keeps one
//! line index per file and Zig's incremental compiler re-derives only what
//! an edit reaches. The rebased map equals the full build's, item for item.
//!
//! A [`Transition`] names what a rebase changed, so the site filter and the
//! debug positions remembered for the previous map carry over to this one.

use hirsel_core_ids::{ContentHash, DefPath, ItemPath, PackageId};
use hirsel_core_ir::source::{ItemSource, SourceFile, SourceMap};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::sync::{Arc, Mutex};

/// The source item of a site.
pub(crate) fn site_item(file: &str, line: u32, column: u32) -> DefPath {
    source_item(vec![file.to_owned()], format!("{line}_{column}"))
}

/// The source item `name` of the relocated unit whose symbol is `symbol`.
pub(crate) fn unit_item(symbol: &str, name: String) -> DefPath {
    source_item(vec!["unit".into(), symbol.into()], name)
}

fn source_item(modules: Vec<String>, name: String) -> DefPath {
    DefPath(ItemPath {
        package: PackageId {
            name: "source".into(),
            version: "1".into(),
        },
        modules,
        name,
    })
}

/// A contributor's distinct sites, and each item its binding adds: the
/// item's index and its site in `points`.
pub(crate) struct Contribution<'a> {
    /// The unit's symbol, which names its items.
    pub(crate) symbol: String,
    /// What `points` and `anchored` are a function of, when known: a memo's
    /// stored body and annex.
    pub(crate) of: Option<(ContentHash, ContentHash)>,
    pub(crate) points: &'a [(String, u32, u32)],
    pub(crate) anchored: &'a [(u32, u32)],
}

/// The map of `points` by file, with each binding's anchored items bound to
/// their sites. A later binding of an item replaces an earlier one.
pub(crate) fn assemble<'a>(
    points: BTreeMap<String, BTreeSet<(u32, u32)>>,
    bindings: impl Iterator<Item = (String, &'a [(String, u32, u32)], &'a [(u32, u32)])>,
) -> Result<SourceMap, String> {
    let mut sources = SourceMap::default();
    // Site items differ from each other only in file and name. Files come in
    // order; ordering each file's items by name gives the map's own order,
    // so it is built in one pass rather than by a search per site.
    let mut items = Vec::new();
    for (path, points) in points {
        let file = u32::try_from(sources.files.len())
            .map_err(|_| "source file table exceeds u32".to_owned())?;
        let points: Vec<_> = points.into_iter().collect();
        let first = items.len();
        for (offset, &(line, column)) in points.iter().enumerate() {
            let file_offset = u32::try_from(offset)
                .map_err(|_| "source position table exceeds u32".to_owned())?;
            items.push((
                site_item(&path, line, column),
                ItemSource { file, file_offset },
            ));
        }
        items[first..].sort_unstable_by(|(a, _), (b, _)| a.0.name.cmp(&b.0.name));
        sources.files.push(SourceFile {
            path: path.clone(),
            text: String::new(),
        });
        sources.points.insert(path, points);
    }
    // Bind unit items by their site's file and rank. Unit items share their
    // package and first module, so their symbol and then their name order
    // them as the map does; sorted, they join the site items as one more run
    // rather than by a search per item.
    let files: HashMap<_, _> = sources
        .files
        .iter()
        .enumerate()
        .map(|(index, file)| {
            (
                file.path.as_str(),
                (index as u32, &sources.points[&file.path]),
            )
        })
        .collect();
    let mut units = Vec::new();
    for (symbol, sites, anchored) in bindings {
        let symbol: std::rc::Rc<str> = symbol.into();
        for &(index, point) in anchored {
            let (file, line, column) = &sites[point as usize];
            let (file, points) = files[file.as_str()];
            let file_offset = points
                .binary_search(&(*line, *column))
                .expect("the unit's points were added to the source map");
            units.push((
                std::rc::Rc::clone(&symbol),
                index.to_string(),
                ItemSource {
                    file,
                    file_offset: file_offset as u32,
                },
            ));
        }
    }
    // Stable, so a later binding of an item replaces an earlier one, as the
    // map's construction from a sorted sequence keeps the last.
    units.sort_by(|a, b| (&a.0, &a.1).cmp(&(&b.0, &b.1)));
    items.extend(
        units
            .into_iter()
            .map(|(symbol, name, at)| (unit_item(&symbol, name), at)),
    );
    sources.items = items.into_iter().collect();
    Ok(sources)
}

/// What a rebase changed between the maps keyed `from` and `to`, whose files
/// are the same.
pub(crate) struct Transition {
    pub(crate) from: ContentHash,
    pub(crate) to: ContentHash,
    /// The files whose sites were renumbered, by path and by map index.
    touched: HashSet<String>,
    touched_index: Vec<bool>,
    /// The symbols of the contributors that changed, came or went.
    changed: HashSet<String>,
    /// The items `to` no longer holds.
    pub(crate) removed: Vec<DefPath>,
    /// The items `to` binds that `from` did not, or bound elsewhere.
    pub(crate) bound: Vec<DefPath>,
}

impl Transition {
    /// Whether positions resolved through `uses` in `from` resolve the same
    /// in `to`.
    pub(crate) fn keeps(&self, uses: &Uses) -> bool {
        !uses.opaque
            && !uses
                .files
                .iter()
                .any(|&file| self.touched_index[file as usize])
            && !uses.paths.iter().any(|path| self.touched.contains(path))
            && !uses
                .symbols
                .iter()
                .any(|symbol| self.changed.contains(symbol))
    }
}

/// What lookups in a source map read: the files their positions landed in,
/// and the bindings of the items they looked up, as the contributors and
/// site files that make those bindings. An item that is neither a unit's nor
/// a site's is opaque: no rebase keeps it.
#[derive(Default)]
pub(crate) struct Uses {
    files: BTreeSet<u32>,
    paths: BTreeSet<String>,
    symbols: BTreeSet<String>,
    opaque: bool,
}

impl Uses {
    pub(crate) fn landed(&mut self, file: u32) {
        self.files.insert(file);
    }
    pub(crate) fn looked_up(&mut self, item: &DefPath) {
        let path = &item.0;
        if path.package.name != "source" || path.package.version != "1" {
            self.opaque = true;
            return;
        }
        match path.modules.as_slice() {
            [unit, symbol] if unit == "unit" => {
                if !self.symbols.contains(symbol) {
                    self.symbols.insert(symbol.clone());
                }
            }
            [file] => {
                if !self.paths.contains(file) {
                    self.paths.insert(file.clone());
                }
            }
            _ => self.opaque = true,
        }
    }
    pub(crate) fn extend(&mut self, other: &Uses) {
        self.files.extend(&other.files);
        self.paths.extend(other.paths.iter().cloned());
        self.symbols.extend(other.symbols.iter().cloned());
        self.opaque |= other.opaque;
    }
}

/// A contributor as a resident compiler holds it.
struct Held {
    of: Option<(ContentHash, ContentHash)>,
    points: Vec<(String, u32, u32)>,
    anchored: Vec<(u32, u32)>,
    /// `points` by file, each file's sorted.
    files: BTreeMap<String, Vec<(u32, u32)>>,
}

impl Held {
    fn of(contribution: &Contribution<'_>) -> Self {
        let mut files = BTreeMap::<String, Vec<(u32, u32)>>::new();
        for (file, line, column) in contribution.points {
            match files.get_mut(file) {
                Some(points) => points.push((*line, *column)),
                None => {
                    files.insert(file.clone(), vec![(*line, *column)]);
                }
            }
        }
        for points in files.values_mut() {
            points.sort_unstable();
            points.dedup();
        }
        Self {
            of: contribution.of,
            points: contribution.points.to_vec(),
            anchored: contribution.anchored.to_vec(),
            files,
        }
    }
    fn holds(&self, contribution: &Contribution<'_>) -> bool {
        match (self.of, contribution.of) {
            (Some(held), Some(now)) => held == now,
            _ => self.points == contribution.points && self.anchored == contribution.anchored,
        }
    }
}

/// A resident compiler's last recipe map and what made it.
pub(crate) struct State {
    key: ContentHash,
    std: Option<ContentHash>,
    map: Arc<SourceMap>,
    extra: BTreeMap<String, Vec<(u32, u32)>>,
    held: HashMap<String, Held>,
    /// The contributors with sites in each file.
    by_file: HashMap<String, HashSet<String>>,
    transition: Option<Arc<Transition>>,
}

impl State {
    /// The state of `map`, keyed `key`, made from `contributions` over the
    /// std keyed `std` and its points `extra`; none if two contributions
    /// name the same unit, whose later binding a rebase cannot order.
    pub(crate) fn new(
        key: ContentHash,
        std: Option<ContentHash>,
        extra: &BTreeMap<String, Vec<(u32, u32)>>,
        contributions: &[Contribution<'_>],
        map: Arc<SourceMap>,
    ) -> Option<Self> {
        let mut held = HashMap::with_capacity(contributions.len());
        let mut by_file = HashMap::<String, HashSet<String>>::new();
        for contribution in contributions {
            let contributor = Held::of(contribution);
            for file in contributor.files.keys() {
                by_file
                    .entry(file.clone())
                    .or_default()
                    .insert(contribution.symbol.clone());
            }
            if held
                .insert(contribution.symbol.clone(), contributor)
                .is_some()
            {
                return None;
            }
        }
        Some(Self {
            key,
            std,
            map,
            extra: extra.clone(),
            held,
            by_file,
            transition: None,
        })
    }

    pub(crate) fn map(&self) -> &Arc<SourceMap> {
        &self.map
    }

    /// Rebase this state's map onto `contributions`, the map keyed `key`.
    /// None, with this state unchanged, when the files would change (a file
    /// comes or goes, and every later file's index with it) or two
    /// contributions name the same unit.
    pub(crate) fn rebase(
        &mut self,
        key: ContentHash,
        contributions: &[Contribution<'_>],
    ) -> Option<()> {
        let mut now = HashMap::with_capacity(contributions.len());
        for contribution in contributions {
            if now
                .insert(contribution.symbol.as_str(), contribution)
                .is_some()
            {
                return None;
            }
        }
        let mut fresh = HashMap::new();
        for contribution in contributions {
            if !self
                .held
                .get(&contribution.symbol)
                .is_some_and(|held| held.holds(contribution))
            {
                fresh.insert(contribution.symbol.clone(), Held::of(contribution));
            }
        }
        let gone: Vec<String> = self
            .held
            .keys()
            .filter(|symbol| !now.contains_key(symbol.as_str()))
            .cloned()
            .collect();
        let changed: HashSet<String> = fresh.keys().chain(&gone).cloned().collect();
        let none = BTreeMap::new();
        let mut touched = BTreeSet::new();
        for symbol in &changed {
            let old = self.held.get(symbol).map_or(&none, |held| &held.files);
            let new = fresh.get(symbol).map_or(&none, |held| &held.files);
            for (file, points) in old.iter().chain(new) {
                if old.get(file) != Some(points) || new.get(file) != Some(points) {
                    touched.insert(file.clone());
                }
            }
        }
        // Each touched file's sites, from every contributor with sites there.
        let mut renumbered = Vec::with_capacity(touched.len());
        for file in &touched {
            if !self.map.points.contains_key(file) {
                return None;
            }
            let mut contributors = self.by_file.get(file).cloned().unwrap_or_default();
            for symbol in &changed {
                if fresh
                    .get(symbol)
                    .is_some_and(|held| held.files.contains_key(file))
                {
                    contributors.insert(symbol.clone());
                } else {
                    contributors.remove(symbol);
                }
            }
            let mut points: BTreeSet<(u32, u32)> = self
                .extra
                .get(file)
                .into_iter()
                .flatten()
                .copied()
                .collect();
            for symbol in &contributors {
                let held = fresh.get(symbol).or_else(|| self.held.get(symbol))?;
                points.extend(held.files.get(file)?.iter().copied());
            }
            if points.is_empty() {
                return None;
            }
            let points: Vec<_> = points.into_iter().collect();
            if points.len() > u32::MAX as usize {
                return None;
            }
            renumbered.push((file.clone(), points, contributors));
        }
        // Nothing below refuses: the state changes as a whole.
        let index: HashMap<String, u32> = self
            .map
            .files
            .iter()
            .enumerate()
            .map(|(index, file)| (file.path.clone(), index as u32))
            .collect();
        let map = Arc::make_mut(&mut self.map);
        let mut removed = Vec::new();
        let mut bound = Vec::new();
        let mut bind = |map: &mut SourceMap, item: DefPath, at: ItemSource| {
            if map.items.get(&item) != Some(&at) {
                map.items.insert(item.clone(), at);
                bound.push(item);
            }
        };
        for (file, points, _) in &renumbered {
            let old = std::mem::replace(
                map.points.get_mut(file).expect("a touched file is mapped"),
                points.clone(),
            );
            for &(line, column) in &old {
                if points.binary_search(&(line, column)).is_err() {
                    let item = site_item(file, line, column);
                    map.items.remove(&item);
                    removed.push(item);
                }
            }
            for (rank, &(line, column)) in points.iter().enumerate() {
                let at = ItemSource {
                    file: index[file],
                    file_offset: rank as u32,
                };
                bind(map, site_item(file, line, column), at);
            }
        }
        let site = |map: &SourceMap, (file, line, column): &(String, u32, u32)| ItemSource {
            file: index[file],
            file_offset: map.points[file]
                .binary_search(&(*line, *column))
                .expect("the unit's points were added to the source map")
                as u32,
        };
        for symbol in &changed {
            let mut kept = HashSet::new();
            if let Some(held) = fresh.get(symbol) {
                for &(item, point) in &held.anchored {
                    kept.insert(item);
                    let at = site(map, &held.points[point as usize]);
                    bind(map, unit_item(symbol, item.to_string()), at);
                }
            }
            for &(item, _) in self.held.get(symbol).map_or(&[][..], |held| &held.anchored) {
                if !kept.contains(&item) {
                    let item = unit_item(symbol, item.to_string());
                    map.items.remove(&item);
                    removed.push(item);
                }
            }
        }
        // An unchanged contributor's items in a renumbered file move with
        // their sites' ranks.
        for (file, _, contributors) in &renumbered {
            for symbol in contributors.iter().filter(|s| !changed.contains(*s)) {
                let held = &self.held[symbol];
                for &(item, point) in &held.anchored {
                    let point = &held.points[point as usize];
                    if point.0 == *file {
                        let at = site(map, point);
                        bind(map, unit_item(symbol, item.to_string()), at);
                    }
                }
            }
        }
        let mut touched_index = vec![false; map.files.len()];
        for (file, _, contributors) in renumbered {
            touched_index[index[&file] as usize] = true;
            self.by_file.insert(file, contributors);
        }
        for symbol in &gone {
            self.held.remove(symbol);
        }
        self.held.extend(fresh);
        self.transition = Some(Arc::new(Transition {
            from: self.key,
            to: key,
            touched: touched.into_iter().collect(),
            touched_index,
            changed,
            removed,
            bound,
        }));
        self.key = key;
        Some(())
    }
}

/// Each resident working set's last recipe map.
pub(super) static STATE: crate::resident::WorkingMemo<(), Arc<Mutex<Option<State>>>> =
    crate::resident::WorkingMemo::new();

/// How a recipe's map was made.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Made {
    Full,
    Remembered,
    Rebased,
}

/// The map keyed `key` of `contributions` over the std keyed `std`, whose
/// points are `extra`: a resident compiler's last map, or that map rebased,
/// or else `full`'s.
pub(crate) fn map<E>(
    key: Option<ContentHash>,
    std: Option<ContentHash>,
    extra: &BTreeMap<String, Vec<(u32, u32)>>,
    contributions: &[Contribution<'_>],
    full: impl FnOnce() -> Result<SourceMap, E>,
) -> Result<(Arc<SourceMap>, Made), E> {
    let Some(key) = key.filter(|_| crate::resident::serving()) else {
        return Ok((Arc::new(full()?), Made::Full));
    };
    let slot = STATE.previous_or_default();
    let mut slot = slot.lock().unwrap();
    if let Some(state) = slot.as_mut().filter(|state| state.std == std) {
        if state.key == key {
            return Ok((Arc::clone(state.map()), Made::Remembered));
        }
        if state.rebase(key, contributions).is_some() {
            return Ok((Arc::clone(state.map()), Made::Rebased));
        }
    }
    *slot = None;
    let map = Arc::new(full()?);
    *slot = State::new(key, std, extra, contributions, Arc::clone(&map));
    Ok((map, Made::Full))
}

/// What the rebase that made the map keyed `to` changed, if one did.
pub(crate) fn transition(to: ContentHash) -> Option<Arc<Transition>> {
    let slot = STATE.previous()?;
    let slot = slot.lock().unwrap();
    let transition = slot.as_ref()?.transition.as_ref()?;
    (transition.to == to).then(|| Arc::clone(transition))
}

#[cfg(test)]
mod tests;
