use super::*;

pub(crate) fn assert_declarations(p: &Program<'_>) {
    use std::collections::BTreeSet;
    let actual: BTreeSet<_> = p
        .fns
        .iter()
        .filter(|f| f.path.package_is("core", "1") && f.body.is_none())
        .map(|f| (f.path.0.modules[0].as_str(), f.path.0.name.as_str()))
        .collect();
    for entry in OPERATIONS {
        assert!(matches!(entry.class, "A" | "B" | "C"));
        if entry.private {
            assert!(
                entry.module == "string"
                    && matches!(
                        entry.name,
                        "of_bytes" | "to_bytes" | "view_string" | "shorten"
                    )
                    || entry.module == "map"
                    || entry.module == "list" && entry.name.starts_with("slots_")
                    || matches!(entry.module, "deque" | "heap")
            );
        }
    }
    let recorded: BTreeSet<_> = OPERATIONS.iter().map(|e| (e.module, e.name)).collect();
    assert_eq!(
        recorded.len(),
        OPERATIONS.len(),
        "duplicate core operation binding"
    );
    assert_eq!(
        actual, recorded,
        "assign each bodiless core function to its owning lane"
    );
}
