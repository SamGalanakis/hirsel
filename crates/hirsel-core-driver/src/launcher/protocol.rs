//! Launcher protocol limits (`admit::protocol`) shared by the host and the operator CLI. Each is the Rust side
//! of a `pub const` in `core/programs/platform/launcher.hirsel` (launch.md "Source by
//! content", "Limits"); the source law `law_tree_limits_agree_with_launcher_hirsel` holds
//! the two sides equal.

/// The most content a source's files may hold in all (`launcher.SOURCE_BYTES`).
pub(crate) const SOURCE_BYTES: usize = 4 << 20;

/// The most codec bytes a source manifest may take (`launcher.MANIFEST_BYTES`).
pub(crate) const MANIFEST_BYTES: usize = 1 << 20;

/// The most codec bytes a tree may take (`launcher.TREE_BYTES`): a tree's fields cost no
/// more than its manifest's beyond the content, so every manifest within its limits
/// assembles into a tree within this.
pub(crate) const TREE_BYTES: usize = SOURCE_BYTES + MANIFEST_BYTES;
