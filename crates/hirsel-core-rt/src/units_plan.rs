//! Fund the untied scheduler's resources before it starts any units.
use super::Unit;
pub(super) use crate::host::envelope::HostEnvelope;
use crate::host::envelope::{HAS_IO_URING, min_limit};
use hirsel_core_net::{REACTOR_ENV, RESERVED_UNIT, ReactorPlan, ready::Event};

const KIB: usize = 1024;
const MIB: usize = KIB * KIB;
const THREAD_HEADROOM: usize = 4;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ReactorChoice {
    Auto,
    Readiness,
    IoUring,
}

impl ReactorChoice {
    pub(super) fn from_env() -> Self {
        match std::env::var_os(REACTOR_ENV).as_deref() {
            Some(v) if HAS_IO_URING && v == "io_uring" => Self::IoUring,
            Some(v) if v == "epoll" => Self::Readiness,
            _ if HAS_IO_URING => Self::Auto,
            _ => Self::Readiness,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct UntiedPlan {
    pub(super) initial_units: usize,
    pub(super) workers: usize,
    pub(super) root_limit: u64,
    pub(super) worker_stack_bytes: usize,
    pub(super) host_stack_bytes: usize,
    pub(super) memory_stack_bytes: usize,
    pub(super) unit_stack_bytes: usize,
    pub(super) task_stack_reserve: usize,
    pub(super) max_units: u32,
    pub(super) physical_unit_limit: bool,
    pub(super) host_thread_capacity: usize,
    pub(super) max_host_threads: usize,
    pub(super) kept_host_threads: usize,
    pub(super) reserved_system_threads: usize,
    pub(super) host_idle_ms: u64,
    pub(super) reactor: ReactorPlan,
    pub(super) retained_bytes: usize,
    pub(super) reclaim_bytes: usize,
    pub(super) reclaim_slots: usize,
    pub(super) ready_scratch_bytes: usize,
    pub(super) idle_release_ms: u64,
    pub(super) fixed_memory_bytes: usize,
    pub(super) pinned_ring_bytes: usize,
    pub(super) fixed_threads: usize,
}

fn reactor_plan(
    w: usize,
    host: HostEnvelope,
    choice: ReactorChoice,
) -> Result<(ReactorPlan, usize, usize), String> {
    let readiness = || {
        (
            ReactorPlan {
                force_epoll: true,
                ..ReactorPlan::DEFAULT
            },
            0,
            0,
        )
    };
    if choice == ReactorChoice::Readiness {
        return Ok(readiness());
    }

    let page = host.page_bytes;
    let round = |bytes: usize| bytes.div_ceil(page) * page;
    let Some((mut reactor, pinned)) = [128, 64, 32].into_iter().find_map(|batch| {
        let plan = ReactorPlan::for_batch(batch);
        let ring = |cq| round(cq as usize * 16) + page + round(plan.sq_entries as usize * 64);
        let pinned = w * (ring(plan.readiness_cq) + ring(plan.completion_cq));
        host.memlock_limit
            .is_none_or(|m| pinned as u64 <= m / 2)
            .then_some((plan, pinned))
    }) else {
        return match choice {
            ReactorChoice::IoUring => Err(format!(
                "untied plan for {w} workers exceeds RLIMIT_MEMLOCK"
            )),
            _ => Ok(readiness()),
        };
    };

    let base = w + 1 + THREAD_HEADROOM;
    if let Some(limit) = host.thread_limit {
        if base + 2 * w + 1 > limit {
            return match choice {
                ReactorChoice::IoUring => Err(format!(
                    "untied io_uring plan for {w} workers exceeds pids.max/RLIMIT_NPROC ({limit})"
                )),
                _ => Ok(readiness()),
            };
        }
        if base + 3 * w + (2 * w).min(4) > limit {
            reactor.iowq_bounded = 1;
        }
    }
    let io_workers = w * (reactor.iowq_bounded + reactor.iowq_unbounded) as usize;
    Ok((reactor, pinned, io_workers))
}

impl UntiedPlan {
    pub(super) fn derive(
        k: usize,
        w: usize,
        root_limit: u64,
        host: HostEnvelope,
        choice: ReactorChoice,
    ) -> Result<Self, String> {
        if k == 0 || k > RESERVED_UNIT as usize || w == 0 || w > usize::MAX / (4 * MIB) {
            return Err(format!("invalid untied plan for {k} roots and {w} workers"));
        }
        let page = host.page_bytes;
        let worker_stack = MIB;
        let host_stack = MIB;
        let memory_stack = 256 * KIB;
        let unit_stack = MIB;
        let commit = crate::fiber::commit_for_page(page);
        let task_reserve = host.address_space_limit.map_or(8 * MIB, |limit| {
            let share = (limit / (16 * w as u64)).min((8 * MIB) as u64) as usize;
            (share / page * page).max(256 * KIB)
        });
        let (reactor, pinned, io_workers) = reactor_plan(w, host, choice)?;
        let fixed_threads = w + 1 + io_workers;
        let occupied = fixed_threads + THREAD_HEADROOM;
        if let Some(limit) = host.thread_limit
            && occupied + 1 > limit
        {
            return Err(format!(
                "untied plan for {w} workers needs {fixed_threads} fixed threads and {} spare, pids.max/RLIMIT_NPROC allows {limit}",
                THREAD_HEADROOM + 1
            ));
        }

        let poll_events = w * (2 * reactor.poll_batch as usize) * size_of::<Event>();
        let fixed_memory = w * (worker_stack + commit + page) + memory_stack + pinned + poll_events;
        let fixed_physical = w * (2 * commit + page) + 64 * KIB + pinned + poll_events;
        for (limit, bytes, name) in [
            (host.address_space_limit, fixed_memory, "RLIMIT_AS"),
            (host.memory_limit, fixed_physical, "memory.max/RLIMIT_DATA"),
        ] {
            if let Some(limit) = limit
                && bytes as u64 > limit
            {
                return Err(format!(
                    "untied plan for {w} workers needs {bytes} bytes, {name} allows {limit}"
                ));
            }
        }

        let memory_left = host
            .memory_limit
            .map(|m| m.saturating_sub(fixed_physical as u64) as usize);
        let address_left = host
            .address_space_limit
            .map(|a| a.saturating_sub(fixed_memory as u64) as usize);
        let memory_threads = address_left
            .map(|a| a / (4 * host_stack))
            .or_else(|| memory_left.map(|m| m / (8 * 128 * KIB)));
        let host_thread_capacity = host.thread_limit.map_or(usize::MAX, |t| t - occupied);
        let max_host_threads = (8 * w)
            .clamp(16, 256)
            .min(host_thread_capacity)
            .min(memory_threads.unwrap_or(usize::MAX))
            .max(1);
        let physical_units = min_limit(
            memory_left.map(|m| m / (page + commit + size_of::<Unit>())),
            address_left.map(|a| a / (unit_stack + task_reserve + 2 * page)),
        )
        .unwrap_or(usize::MAX);
        // The language charge bounds active language jobs, but a replay starts
        // historical streams together and a flow can use extra engine units.
        let max_units = physical_units.min(RESERVED_UNIT as usize).max(k) as u32;
        let physical_unit_limit = physical_units < RESERVED_UNIT as usize;

        let scale = host.memory_limit.map_or(root_limit, |m| root_limit.min(m));
        let worker_retention = (64 * MIB)
            .saturating_add((16 * MIB).saturating_mul(w))
            .min(256 * MIB);
        let host_retention = memory_left.map_or(usize::MAX, |m| m / 4);
        let retained = ((scale / 8) as usize)
            .clamp(2 * MIB, worker_retention)
            .min(host_retention);
        let retained = retained / page * page;
        let reclaim = if retained == 0 {
            0
        } else {
            (retained / 4).max(2 * unit_stack).min(host_retention / 2)
        };
        let reclaim_slots = if reclaim == 0 {
            0
        } else {
            (reclaim / crate::task_heap::SLAB).clamp(64, 4096)
        };
        Ok(Self {
            initial_units: k,
            workers: w,
            root_limit,
            worker_stack_bytes: worker_stack,
            host_stack_bytes: host_stack,
            memory_stack_bytes: memory_stack,
            unit_stack_bytes: unit_stack,
            task_stack_reserve: task_reserve,
            max_units,
            physical_unit_limit,
            host_thread_capacity,
            max_host_threads,
            kept_host_threads: (2 * w).min(max_host_threads).min(16),
            reserved_system_threads: if max_host_threads >= 4 { 2 } else { 0 },
            host_idle_ms: 10_000,
            reactor,
            retained_bytes: retained,
            reclaim_bytes: reclaim,
            reclaim_slots,
            ready_scratch_bytes: 2
                * reactor.poll_batch as usize
                * size_of::<crate::tasks::ReadyArm>(),
            idle_release_ms: 100,
            fixed_memory_bytes: fixed_memory,
            pinned_ring_bytes: pinned,
            fixed_threads,
        })
    }
}

#[cfg(test)]
#[path = "units_plan_tests.rs"]
mod tests;
