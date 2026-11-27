use crate::launcher::{admit::grant_diff, session::Grants};
use hirsel_core_ids::{ContentHash, DefPath};
use hirsel_core_ir::Identity;
use hirsel_core_ir::release_recipe::Recipe;
use hirsel_core_ir::release_recipe::{Backend, GrantBinding, GrantKind, Launch};

fn recipe(grants: &[(GrantKind, &[&str])]) -> Recipe {
    Recipe {
        entry: Identity::plain(DefPath::from_full("app@1/main/main").unwrap()),
        address: ContentHash::default(),
        groups: vec![],
        bodies: vec![],
        values: vec![],
        foreign_modules: vec![],
        foreign_archives: vec![],
        toolchain: vec![],
        launch: Launch {
            backend: Backend::Cranelift,
            target: "t".into(),
            root_limit: 1,
            n_place: 1,
            exit_status: false,
            grants: grants
                .iter()
                .enumerate()
                .map(|(ordinal, (kind, values))| GrantBinding {
                    ordinal: ordinal as u32,
                    kind: *kind,
                    values: values.iter().map(|v| (*v).into()).collect(),
                    archive: None,
                    policy: None,
                })
                .collect(),
        },
    }
}

// The deploy gate's diff (launch.md "Deploy preview") names every authority gained and
// every one lost, value by value. Hand-derived: NEW adds one Net rule beside OLD's,
// narrows a Secret's patterns, drops Clock and keeps Dir and Args; so the diff is the
// new rule and the new Secret binding gained, the old Secret binding and Clock lost.
#[test]
fn grant_diff_names_each_authority() {
    let old = recipe(&[
        (GrantKind::Net, &["allow tcp a:1"]),
        (GrantKind::Dir, &["/data"]),
        (GrantKind::Clock, &["Clock"]),
        (GrantKind::Secret, &["vault", "api_*"]),
        (GrantKind::Args, &["Args"]),
    ]);
    let new = recipe(&[
        (GrantKind::Net, &["allow tcp a:1", "allow tcp b:2"]),
        (GrantKind::Dir, &["/data"]),
        (GrantKind::Secret, &["vault", "api_key"]),
        (GrantKind::Args, &["Args"]),
    ]);
    let initial = grant_diff(None, &new);
    assert_eq!(
        initial.added,
        [
            "Dir=/data",
            "Net=allow tcp a:1",
            "Net=allow tcp b:2",
            "Secret=vault api_key"
        ]
    );
    assert!(initial.removed.is_empty());
    let diff = grant_diff(Some(&old), &new);
    assert_eq!(diff.added, ["Net=allow tcp b:2", "Secret=vault api_key"]);
    assert_eq!(diff.removed, ["Clock", "Secret=vault api_*"]);
    assert_eq!(grant_diff(Some(&new), &new), Grants::default());
}
