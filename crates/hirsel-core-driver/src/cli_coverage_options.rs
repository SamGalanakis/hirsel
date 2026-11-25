//! The options of `hirsel test --coverage` and `hirsel coverage` (`docs/coverage.md`).
use super::{OptionId, OptionSpec};

pub(super) static OPTIONS: &[OptionSpec] = &[
    option!(
        Coverage,
        ["--coverage"],
        None,
        ["test"],
        "Report the function entries and source arms the selected tests reached."
    ),
    option!(
        CoverageOut,
        ["--coverage-out"],
        Some("FILE"),
        ["test"],
        "With --coverage (implied), also write the merged coverage profile to FILE."
    ),
    option!(
        CoverageBase,
        ["--coverage-base"],
        Some("FILE"),
        ["test"],
        "With --coverage (implied), add a diff against the coverage profile in FILE."
    ),
    option!(
        CoverageTargets,
        ["--coverage-targets"],
        Some("FILE"),
        ["test"],
        "With --coverage (implied), list only uncovered sites in the source ranges in FILE, one JSON object per line."
    ),
    option!(
        CoverageRuns,
        ["--coverage-runs"],
        None,
        ["test"],
        "With --coverage (implied), keep one row per run in the coverage profile."
    ),
    option!(
        SeedBase,
        ["--seed-base"],
        Some("K"),
        ["test"],
        "With --seeds N, run seeds K to K+N-1."
    ),
    option!(
        CoverageMap,
        ["--map"],
        Some("FILE"),
        ["coverage merge"],
        "Read raw coverage files against the site map in FILE. Repeat for more maps."
    ),
    option!(
        CoverageBase,
        ["--base"],
        Some("FILE"),
        ["coverage merge"],
        "Add a diff against the coverage profile in FILE."
    ),
    option!(
        CoverageTargets,
        ["--targets"],
        Some("FILE"),
        ["coverage merge"],
        "List only uncovered sites in the source ranges in FILE, one JSON object per line."
    ),
];
