/// Readiness batch and completion ring sizes for a worker's poller.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ReactorPlan {
    pub poll_batch: u32,
    pub sq_entries: u32,
    pub readiness_cq: u32,
    pub completion_cq: u32,
    pub iowq_bounded: u32,
    pub iowq_unbounded: u32,
    pub force_epoll: bool,
}

impl ReactorPlan {
    pub const DEFAULT: Self = Self::for_batch(128);

    pub const fn for_batch(poll_batch: u32) -> Self {
        let sq_entries = poll_batch / 2;
        Self {
            poll_batch,
            sq_entries,
            readiness_cq: 2 * poll_batch,
            completion_cq: 2 * sq_entries,
            iowq_bounded: 2,
            iowq_unbounded: 1,
            force_epoll: false,
        }
    }
}
