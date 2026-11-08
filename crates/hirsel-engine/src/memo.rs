//! The memo engine's data model: typed reads and memo records (design §1b).
use hirsel_query_products::reads::{ReadKey, Reads};
mod record;
pub use record::{Changes, Memo, MemoKey};
mod coordinator;
pub use coordinator::{CoordinatorStats, Entered, Frame, S1Coordinator, S1Worker, WorkerId};
mod holds;
pub use holds::{HoldStats, Holds, Retention, default_hold_bytes};
pub(crate) mod retained;
