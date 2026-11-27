# Workspace files each Cargo-style build script reads at run time.

BUILD_SCRIPT_INPUTS = {
    "hirsel-config-schema": ["crates/hirsel-config-schema/schemas/*.hirsel"],
    "hirsel-core-diag": ["docs/spec/*.md", "core/front/cases/**/*.hirsel"],
    "hirsel-core-front": [
        "core/core/core.hirsel",
        "crates/hirsel-core-front/src/body/core_ops/net_class.rs",
        "crates/hirsel-core-net/src/class.rs",
    ],
}
