use super::support::{Fixture, function_signature, id, owner_sig, path, session};
use hirsel_syntax::ContentHash;
use hirsel_tir::BodyId;
use hirsel_types::{SignatureRef, TypeInterner, TypeKind};
use hirsel_validate::{
    PackageViews, SignatureLookup, VRead, ValidatedSignatures, View, ViewTable, validate_definition,
};
use std::sync::Arc;

fn table(
    types: &Arc<TypeInterner>,
    rows: impl IntoIterator<Item = SignatureRef>,
) -> Arc<ViewTable> {
    Arc::new(ViewTable::new(types, rows, None, &mut ValidatedSignatures::default(), None).unwrap())
}

#[test]
fn checked_evidence_binds_body_type_policy_and_private_snapshot() {
    let mut f = Fixture::unit();
    owner_sig(&mut f, &[]);
    let types = session();
    let rows: Vec<_> = f.signatures.values().cloned().map(Arc::new).collect();
    let first = table(&types, rows.clone());
    let definition = Arc::new(f.definition);
    let policy = ContentHash::of_bytes(b"checked-source-policy");
    let (report, ran, _, evidence) = validate_definition(
        &definition,
        types.clone(),
        |_| Some((path("main"), 0)),
        &*first as &dyn hirsel_validate::CheckedEnvironment,
        policy,
        None,
    )
    .unwrap();
    assert!(ran && report.violations.is_empty(), "{report:?}");
    let evidence = evidence.unwrap();
    assert!(evidence.holds(
        &definition,
        &types,
        policy,
        &*first as &dyn hirsel_validate::CheckedEnvironment
    ));
    assert!(evidence.holds(
        &definition,
        &types,
        policy,
        &*first as &dyn hirsel_validate::CheckedEnvironment
    ));
    assert!(!evidence.holds(
        &Arc::new((*definition).clone()),
        &types,
        policy,
        &*first as &dyn hirsel_validate::CheckedEnvironment
    ));
    assert!(!evidence.holds(
        &definition,
        &TypeInterner::new(None),
        policy,
        &*first as &dyn hirsel_validate::CheckedEnvironment
    ));
    assert!(!evidence.holds(
        &definition,
        &types,
        ContentHash::default(),
        &*first as &dyn hirsel_validate::CheckedEnvironment
    ));
    let equal = table(&types, rows.clone());
    assert!(evidence.holds(
        &definition,
        &types,
        policy,
        &*equal as &dyn hirsel_validate::CheckedEnvironment
    ));
    let mut changed = (*rows
        .iter()
        .find(|s| s.definition == definition.path)
        .unwrap())
    .as_ref()
    .clone();
    changed.metadata.visibility = hirsel_syntax::Visibility::Public;
    assert_eq!(changed.fingerprint, rows[0].fingerprint);
    let private = PackageViews::new(
        &types,
        first.clone(),
        [View::new(&types, Arc::new(changed), None)],
        &mut ValidatedSignatures::default(),
        None,
    )
    .unwrap();
    assert!(
        !evidence.environment_holds(policy, &private as &dyn hirsel_validate::CheckedEnvironment)
    );
    assert!(
        evidence.environment_holds(policy, &*first as &dyn hirsel_validate::CheckedEnvironment)
    );
    let mut malformed = (*definition).clone();
    malformed.entry = BodyId(u32::MAX);
    let (report, ran, accepted, proof) = validate_definition(
        &Arc::new(malformed),
        types.clone(),
        |_| Some((path("main"), 0)),
        &*first as &dyn hirsel_validate::CheckedEnvironment,
        policy,
        None,
    )
    .unwrap();
    assert!(ran && !report.violations.is_empty());
    assert!(accepted.is_none() && proof.is_none());
    let copy = Arc::new((*definition).clone());
    drop(definition);
    assert!(!evidence.holds(
        &copy,
        &types,
        policy,
        &*first as &dyn hirsel_validate::CheckedEnvironment
    ));
}

#[test]
fn checked_snapshot_stamp_cannot_hide_unrelated_universe_errors_or_missing_owner() {
    let mut f = Fixture::unit();
    owner_sig(&mut f, &[]);
    let types = session();
    let rows: Vec<_> = f.signatures.values().cloned().map(Arc::new).collect();
    let first = table(&types, rows.clone());
    let definition = Arc::new(f.definition);
    let policy = ContentHash::default();
    let (report, _, _, evidence) = validate_definition(
        &definition,
        types.clone(),
        |_| Some((path("main"), 0)),
        &*first as &dyn hirsel_validate::CheckedEnvironment,
        policy,
        None,
    )
    .unwrap();
    assert!(report.violations.is_empty());
    let evidence = evidence.unwrap();
    assert!(
        evidence
            .reads()
            .iter()
            .any(|r| matches!(r, VRead::UniverseErrors(_)))
    );
    let invalid = function_signature("unrelated", vec![], TypeKind::Error);
    let changed = table(&types, rows.iter().cloned().chain([Arc::new(invalid)]));
    assert!(!changed.universe_errors().1.is_empty());
    assert!(!evidence.environment_holds(
        policy,
        &*changed as &dyn hirsel_validate::CheckedEnvironment
    ));
    let missing = table(&types, []);
    assert!(
        evidence
            .reads()
            .iter()
            .any(|r| matches!(r, VRead::View(d, _) if *d == id("main")))
    );
    assert!(!evidence.environment_holds(
        policy,
        &*missing as &dyn hirsel_validate::CheckedEnvironment
    ));
    assert!(
        evidence.environment_holds(policy, &*first as &dyn hirsel_validate::CheckedEnvironment)
    );
}

#[test]
fn foreign_interner_cannot_reuse_a_clean_environment() {
    let mut f = Fixture::unit();
    owner_sig(&mut f, &[]);
    let types = session();
    let rows: Vec<_> = f.signatures.values().cloned().map(Arc::new).collect();
    let first = table(&types, rows.clone());
    let definition = Arc::new(f.definition);
    let policy = ContentHash::default();
    let (report, _, _, evidence) = validate_definition(
        &definition,
        types.clone(),
        |_| Some((path("main"), 0)),
        &*first as &dyn hirsel_validate::CheckedEnvironment,
        policy,
        None,
    )
    .unwrap();
    assert!(report.violations.is_empty());
    let evidence = evidence.unwrap();
    // A foreign universe is rejected even if all of its numeric rows happen to
    // match. Its views retain their actual originating type owner.
    let foreign = TypeInterner::new(None);
    let foreign = table(&foreign, rows);
    assert!(!evidence.environment_holds(
        policy,
        &*foreign as &dyn hirsel_validate::CheckedEnvironment
    ));
    assert!(
        evidence.environment_holds(policy, &*first as &dyn hirsel_validate::CheckedEnvironment)
    );
}

#[test]
fn checked_environment_preserves_equal_heap_roles_and_rejects_changed_evidence() {
    use super::support::arena_slot;
    let mut f = Fixture::call();
    arena_slot(&mut f, hirsel_tir::Storage::Temporary);
    owner_sig(&mut f, &[]);
    let types = session();
    let rows: Vec<_> = f.signatures.values().cloned().map(Arc::new).collect();
    let first = table(&types, rows.clone());
    let definition = Arc::new(f.definition);
    let policy = ContentHash::default();
    let (report, _, _, evidence) = validate_definition(
        &definition,
        types.clone(),
        |_| Some((path("main"), 0)),
        &*first as &dyn hirsel_validate::CheckedEnvironment,
        policy,
        None,
    )
    .unwrap();
    assert!(report.violations.is_empty(), "{report:?}");
    let evidence = evidence.unwrap();
    assert!(evidence.reads().iter().any(|read| matches!(
        read,
        VRead::HeapRole(hirsel_validate::HeapObservation::SelectedHeap { .. })
    )));
    let mut earlier = function_signature("0provider", vec![], TypeKind::Tuple(vec![].into()));
    earlier.metadata.roles = rows
        .iter()
        .find(|s| s.definition == id("Arena"))
        .unwrap()
        .metadata
        .roles
        .clone();
    let changed = table(&types, rows.iter().cloned().chain([Arc::new(earlier)]));
    assert!(changed.universe_errors().1.is_empty());
    assert!(
        evidence
            .reads()
            .iter()
            .filter(|r| matches!(r, VRead::View(_, _)))
            .all(|r| hirsel_validate::read_holds(r, &*changed))
    );
    assert!(evidence.environment_holds(
        policy,
        &*changed as &dyn hirsel_validate::CheckedEnvironment
    ));
    assert!(
        evidence.environment_holds(policy, &*first as &dyn hirsel_validate::CheckedEnvironment)
    );
    for mutation in [
        "arena",
        "operation",
        "private shape",
        "version",
        "absence",
        "arity",
    ] {
        let mut changed: Vec<_> = rows.iter().map(|row| (**row).clone()).collect();
        for row in &mut changed {
            if mutation == "absence" {
                Arc::make_mut(&mut row.metadata.roles).heap = None;
            } else if mutation == "version" {
                if let Some(role) = Arc::make_mut(&mut row.metadata.roles).heap.as_mut() {
                    role.arena.0.package.version = "different".into();
                    role.operation.0.package.version = "different".into();
                }
            } else if row.definition
                == id(if mutation == "operation" {
                    "heap"
                } else {
                    "Arena"
                })
            {
                match mutation {
                    "private shape" => {
                        let hirsel_types::SignatureKind::Type(declaration) = &mut row.kind else {
                            unreachable!()
                        };
                        declaration.representation = Some(types.intern(hirsel_types::Type::unit()));
                    }
                    "arity" => {
                        let hirsel_types::SignatureKind::Type(declaration) = &mut row.kind else {
                            unreachable!()
                        };
                        declaration.generics =
                            vec![hirsel_types::GenericParameter::Size { name: "N".into() }];
                    }
                    _ => row.metadata.visibility = hirsel_syntax::Visibility::Public,
                }
            }
        }
        if let Ok(changed) = ViewTable::new(
            &types,
            changed.into_iter().map(Arc::new),
            None,
            &mut ValidatedSignatures::default(),
            None,
        ) {
            assert!(
                !evidence.environment_holds(
                    policy,
                    &changed as &dyn hirsel_validate::CheckedEnvironment
                ),
                "changed {mutation}"
            );
        }
    }
    let base = hirsel_types::TypeBase::build(types.len(), |_| {
        types.ids().map(|id| types.row(id).clone()).collect()
    });
    let foreign = table(&TypeInterner::new(Some(base)), rows);
    assert!(!evidence.environment_holds(
        policy,
        &*foreign as &dyn hirsel_validate::CheckedEnvironment
    ));
}
