//! The memo engine's data model: typed reads, revisions and durability
//! (design §1b).
use hirsel_query_products::reads::{Durability, ReadKey, ReadKind, Reads, Revision};
mod graph;
pub(crate) use graph::SourceHeaderGraph;
pub use graph::{
    ChangeSet, Invalidation, Memo, MemoGraph, MemoKey, MemoStats, Verdict, invalidation_count,
};
mod coordinator;
pub use coordinator::{CoordinatorStats, Entered, Frame, S1Coordinator, S1Worker, WorkerId};
mod holds;
pub use holds::{HoldStats, Holds, Retention, default_hold_bytes};
pub(crate) mod retained;
