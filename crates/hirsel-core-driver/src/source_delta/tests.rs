use super::*;

/// A contributor: its unit symbol and its sites in body order. Each distinct
/// site anchors one item, numbered from 2 so item and point indices differ.
#[derive(Clone)]
struct Unit {
    symbol: &'static str,
    sites: Vec<(&'static str, u32, u32)>,
}

struct Owned {
    symbol: String,
    points: Vec<(String, u32, u32)>,
    anchored: Vec<(u32, u32)>,
}

fn owned(units: &[Unit]) -> Vec<Owned> {
    units
        .iter()
        .map(|unit| {
            let mut points: Vec<(String, u32, u32)> = Vec::new();
            for &(file, line, column) in &unit.sites {
                let site = (file.to_owned(), line, column);
                if !points.contains(&site) {
                    points.push(site);
                }
            }
            let anchored = (0..points.len() as u32).map(|p| (p + 2, p)).collect();
            Owned {
                symbol: unit.symbol.to_owned(),
                points,
                anchored,
            }
        })
        .collect()
}

fn contributions(owned: &[Owned]) -> Vec<Contribution<'_>> {
    owned
        .iter()
        .map(|unit| Contribution {
            symbol: unit.symbol.clone(),
            of: None,
            points: &unit.points,
            anchored: &unit.anchored,
        })
        .collect()
}

/// The full build's map: the oracle a rebase must equal.
fn full(owned: &[Owned], extra: &BTreeMap<String, Vec<(u32, u32)>>) -> SourceMap {
    let mut points = BTreeMap::<String, BTreeSet<(u32, u32)>>::new();
    for (file, at) in extra {
        points.entry(file.clone()).or_default().extend(at);
    }
    for unit in owned {
        for (file, line, column) in &unit.points {
            points
                .entry(file.clone())
                .or_default()
                .insert((*line, *column));
        }
    }
    let bindings = owned
        .iter()
        .map(|unit| (unit.symbol.clone(), &unit.points[..], &unit.anchored[..]));
    assemble(points, bindings).unwrap()
}

/// Every item whose binding differs between `old` and `new`, and every file
/// whose sites differ, is one the transition reports, so a lookup it keeps
/// resolves the same in both maps.
fn check_transition(old: &SourceMap, new: &SourceMap, transition: &Transition) {
    let mut items = old.items.clone();
    for item in &transition.removed {
        assert!(
            items.remove(item).is_some(),
            "removed {item:?} was not bound"
        );
    }
    for item in &transition.bound {
        items.insert(item.clone(), new.items[item]);
    }
    assert_eq!(
        items, new.items,
        "the transition's items give the new map's"
    );
    let keys: BTreeSet<_> = old.items.keys().chain(new.items.keys()).collect();
    for item in keys {
        let (before, after) = (old.items.get(item), new.items.get(item));
        let mut uses = Uses::default();
        uses.looked_up(item);
        for at in [before, after].into_iter().flatten() {
            uses.landed(at.file);
        }
        if before != after {
            assert!(!transition.keeps(&uses), "{item:?} moved but is kept");
        }
    }
    for (index, file) in new.files.iter().enumerate() {
        if old.points[&file.path] != new.points[&file.path] {
            let mut uses = Uses::default();
            uses.landed(index as u32);
            assert!(
                !transition.keeps(&uses),
                "{} renumbered but kept",
                file.path
            );
        }
    }
}

#[test]
fn rebased_maps_equal_full_builds_and_their_transitions_name_every_change() {
    let extra = BTreeMap::from([("std.hirsel".to_owned(), vec![(1, 1), (9, 4)])]);
    let a = |line| ("a.hirsel", line, 5);
    let b = |line| ("b.hirsel", line, 3);
    let main = Unit {
        symbol: "main",
        sites: vec![a(3), a(7), b(2), a(3), ("std.hirsel", 9, 4)],
    };
    let tail = Unit {
        symbol: "tail",
        sites: vec![a(20), a(22)],
    };
    let helper = Unit {
        symbol: "helper",
        sites: vec![b(5), b(9)],
    };
    let step = |main: &Unit, tail: &Unit, helper: Option<&Unit>| {
        let mut units = vec![main.clone(), tail.clone()];
        units.extend(helper.cloned());
        units
    };
    let shift = |unit: &Unit, file: &str, from: u32, by: i64| Unit {
        symbol: unit.symbol,
        sites: unit
            .sites
            .iter()
            .map(|&(f, line, column)| {
                let moved = f == file && line >= from;
                (
                    f,
                    if moved {
                        (line as i64 + by) as u32
                    } else {
                        line
                    },
                    column,
                )
            })
            .collect(),
    };
    let steps: Vec<(&str, Vec<Unit>, bool)> = vec![
        // A line inserted at the top of a.hirsel moves every unit in it;
        // b.hirsel's sites stay, though main has sites there too.
        (
            "insert at the top",
            step(
                &shift(&main, "a.hirsel", 0, 1),
                &shift(&tail, "a.hirsel", 0, 1),
                Some(&helper),
            ),
            true,
        ),
        // A line inserted between main and tail moves only tail.
        (
            "insert in the middle",
            step(
                &shift(&main, "a.hirsel", 0, 1),
                &shift(&tail, "a.hirsel", 0, 2),
                Some(&helper),
            ),
            true,
        ),
        // A new site ahead of tail's renumbers tail's unchanged items.
        (
            "site added before an unchanged unit",
            step(
                &Unit {
                    symbol: "main",
                    sites: vec![a(4), a(8), a(15), b(2), a(4), ("std.hirsel", 9, 4)],
                },
                &shift(&tail, "a.hirsel", 0, 2),
                Some(&helper),
            ),
            true,
        ),
        // A site on a std line only the std had, and a unit that goes.
        (
            "std site shared, unit gone",
            step(
                &Unit {
                    symbol: "main",
                    sites: vec![a(4), a(8), a(15), b(2), ("std.hirsel", 1, 1)],
                },
                &shift(&tail, "a.hirsel", 0, 2),
                None,
            ),
            true,
        ),
        // Lines deleted at the end of b.hirsel, and helper comes back.
        (
            "delete at the end, unit back",
            step(
                &Unit {
                    symbol: "main",
                    sites: vec![a(4), a(8), a(15), b(2), ("std.hirsel", 1, 1)],
                },
                &shift(&tail, "a.hirsel", 0, 2),
                Some(&shift(&helper, "b.hirsel", 9, -3)),
            ),
            true,
        ),
        // The last site in b.hirsel goes with helper's: the files change.
        (
            "file goes",
            step(
                &Unit {
                    symbol: "main",
                    sites: vec![a(4), a(8), a(15), ("std.hirsel", 1, 1)],
                },
                &shift(&tail, "a.hirsel", 0, 2),
                None,
            ),
            false,
        ),
    ];
    let first = owned(&step(&main, &tail, Some(&helper)));
    let mut state = State::new(
        old_key(0),
        None,
        &extra,
        &contributions(&first),
        Arc::new(full(&first, &extra)),
    )
    .unwrap();
    for (n, (name, units, rebases)) in steps.into_iter().enumerate() {
        let owned = owned(&units);
        let key = ContentHash::of_bytes(&[n as u8 + 1]);
        let old = Arc::clone(state.map());
        let rebased = state.rebase(key, &contributions(&owned)).is_some();
        assert_eq!(rebased, rebases, "{name}");
        if !rebased {
            assert_eq!(
                state.map(),
                &old,
                "{name}: a refused rebase changes nothing"
            );
            continue;
        }
        assert_eq!(**state.map(), full(&owned, &extra), "{name}");
        let transition = state.transition.clone().unwrap();
        assert_eq!((transition.from, transition.to), (old_key(n), key));
        check_transition(&old, state.map(), &transition);
    }
}

fn old_key(n: usize) -> ContentHash {
    ContentHash::of_bytes(&[n as u8])
}
