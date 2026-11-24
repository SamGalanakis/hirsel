use super::{OptionId, OptionSpec};

pub(super) static OPTIONS: &[OptionSpec] = &[
    option!(
        AddRoot,
        ["--root"],
        Some("sha256:HEX"),
        ["add"],
        "Require this archive head root."
    ),
    option!(
        AddUpdate,
        ["--update"],
        None,
        ["add"],
        "Accept a changed root at the same source."
    ),
    option!(
        AddDryRun,
        ["--dry-run"],
        None,
        ["add"],
        "Resolve the change without writing the manifest or lock."
    ),
    option!(
        PackageStore,
        ["--package-store"],
        Some("DIR"),
        [
            "check",
            "build",
            "publish",
            "run",
            "test",
            "inspect",
            "replay",
            "sdk",
            "lock",
            "fetch",
            "add",
            "pack",
            "laws check",
            "laws write"
        ],
        "Select the verified library store."
    ),
    option!(
        Locked,
        ["--locked"],
        None,
        [
            "check",
            "build",
            "publish",
            "run",
            "test",
            "inspect",
            "replay",
            "sdk",
            "lock",
            "fetch",
            "add",
            "pack",
            "laws check",
            "laws write"
        ],
        "Require a current workspace lock."
    ),
    option!(
        Frozen,
        ["--frozen"],
        None,
        [
            "check",
            "build",
            "publish",
            "run",
            "test",
            "inspect",
            "replay",
            "sdk",
            "lock",
            "fetch",
            "add",
            "pack",
            "laws check",
            "laws write"
        ],
        "Require a current workspace lock and use only the package store."
    ),
];
