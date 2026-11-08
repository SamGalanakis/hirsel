use super::*;
use crate::{ItemPath, PackageId};
use hirsel_intern::Symbol;
use std::collections::BTreeMap;

fn path(module: &str, name: &str) -> DefPath {
    DefPath(ItemPath {
        package: PackageId {
            name: "ids".into(),
            version: "1".into(),
        },
        modules: vec![module.into()],
        name: name.into(),
    })
}

/// Spellings interned in a scrambled order, as a program's first uses would be.
fn spellings() -> Vec<String> {
    (0..200u32)
        .map(|i| format!("s{:03}", i.wrapping_mul(7919) % 200))
        .collect()
}

#[cfg(feature = "id-salt")]
#[test]
fn ordered_maps_iterate_identically_under_every_salt() {
    let Some(runs) = hirsel_intern::under_salts(
        "ids::tests::ordered_maps_iterate_identically_under_every_salt",
        &[0, 1, 2, 3],
        || {
            let names = spellings();
            let symbols: BTreeMap<Symbol, usize> = names
                .iter()
                .enumerate()
                .map(|(i, s)| (Symbol::intern(s), i))
                .collect();
            let defs: BTreeMap<DefId, usize> = names
                .iter()
                .enumerate()
                .map(|(i, s)| (DefId::intern(&path("salt", s)), i))
                .collect();
            let hashed: DefMap<()> = defs.keys().map(|&d| (d, ())).collect();
            let ordered = |keys: Vec<String>| keys.join(" ");
            format!(
                "{}\n{}\n{}",
                ordered(symbols.iter().map(|(s, i)| format!("{s}={i}")).collect()),
                ordered(defs.iter().map(|(d, i)| format!("{d}={i}")).collect()),
                ordered(hashed.keys().map(|d| d.path().0.name.clone()).collect()),
            )
        },
    ) else {
        return;
    };
    let lines: Vec<Vec<&str>> = runs.iter().map(|r| r.lines().collect()).collect();
    for run in &lines[1..] {
        assert_eq!(
            run[0], lines[0][0],
            "BTreeMap<Symbol, _> order moved with the salt"
        );
        assert_eq!(
            run[1], lines[0][1],
            "BTreeMap<DefId, _> order moved with the salt"
        );
    }
    let mut sorted = spellings();
    sorted.sort();
    assert!(lines[0][0].starts_with(&format!("{}=", sorted[0])));
    // The salt really moves id assignment: hashed iteration order differs.
    let hash_orders: std::collections::BTreeSet<&str> = lines.iter().map(|run| run[2]).collect();
    assert!(hash_orders.len() > 1, "every salt gave one DefMap order");
}

/// `DefId` compares precomputed keys; the oracle is `DefPath`'s derived order.
/// The cases are the boundaries an escaping or list encoding can get wrong:
/// prefixes, embedded NUL and 0x01/0xff bytes, empty strings, module lists of
/// different lengths, and a field difference behind an equal earlier field.
#[test]
fn id_order_is_derived_path_order() {
    let texts = [
        "", "\0", "\0\0", "\u{1}", "a", "a\0", "a\0b", "a\u{1}", "ab", "\u{ff}", "b",
    ];
    let mut paths = vec![];
    for package in ["", "a", "a\0", "ab"] {
        for version in ["", "1", "1\0"] {
            for modules in [
                vec![],
                vec![""],
                vec!["a"],
                vec!["a", ""],
                vec!["a", "b"],
                vec!["a\0"],
                vec!["ab"],
            ] {
                for name in texts {
                    paths.push(DefPath(ItemPath {
                        package: PackageId {
                            name: package.into(),
                            version: version.into(),
                        },
                        modules: modules.iter().map(|m| (*m).into()).collect(),
                        name: name.into(),
                    }));
                }
            }
        }
    }
    let ids: Vec<DefId> = paths.iter().map(DefId::intern).collect();
    for (a, x) in paths.iter().zip(&ids) {
        for (b, y) in paths.iter().zip(&ids) {
            assert_eq!(x.cmp(y), a.cmp(b), "{a:?} vs {b:?}");
        }
    }
}
