//! Blocking calls leave their unit on a Reply while the call runs on a host thread.
use super::{POOL, Pool, Reply, in_unit, pool, pool_hooks, untied, wake_slot};
use std::{
    collections::VecDeque,
    io,
    sync::{Condvar, Mutex, atomic::Ordering},
    thread,
    time::Duration,
};

#[derive(Clone, Copy, Eq, PartialEq)]
pub(crate) enum HostCall {
    /// A standalone call that can finish without another unit's host call.
    Bounded,
    /// Runtime signal relay, entropy or diagnostics.
    System,
    /// A call such as a FIFO open that can depend on another queued caller.
    PeerWait,
}

/// Leave the slot only when running inside the pool, then return the call's result.
pub(crate) fn off_slot<T: Send>(class: HostCall, call: impl FnOnce() -> T + Send) -> T {
    let mut call = Some(call);
    let mut answer = None;
    let mut run = || answer = Some((call.take().unwrap())());
    if !(untied() && (pool_hooks().off_slot)(class, &mut run)) {
        run();
    }
    answer.unwrap()
}

struct HostJob {
    call: *mut (dyn FnMut() + Send),
    reply: *const Reply,
}
// SAFETY: the parked unit pins the call and reply until completion.
unsafe impl Send for HostJob {}

#[derive(Default)]
struct Lane {
    jobs: VecDeque<HostJob>,
    live: usize,
    idle: usize,
}

#[derive(Default)]
struct State {
    lanes: [Lane; 2],
    peers: usize,
    peak_live: usize,
    peak_queued: usize,
    spawn_deferred: u64,
}

impl State {
    fn live(&self) -> usize {
        self.lanes[0].live + self.lanes[1].live + self.peers
    }

    fn queued(&self) -> usize {
        self.lanes[0].jobs.len() + self.lanes[1].jobs.len()
    }

    fn unoccupied_system(&self, reserve: usize) -> usize {
        reserve.saturating_sub(self.lanes[1].live)
    }

    fn update_peaks(&mut self) {
        self.peak_live = self.peak_live.max(self.live());
        self.peak_queued = self.peak_queued.max(self.queued());
    }
}

#[derive(Default)]
pub(super) struct Helpers {
    state: Mutex<State>,
    ready: [Condvar; 2],
}

impl Helpers {
    fn try_enqueue(
        p: &'static Pool,
        class: HostCall,
        job: HostJob,
    ) -> Result<(), crate::run_stop::Stop> {
        let pool = &p.helpers;
        let mut s = pool.state.lock().unwrap();
        let reserve = p.plan.reserved_system_threads;
        let total = p.plan.host_thread_capacity;
        let stack = p.plan.host_stack_bytes;
        if class == HostCall::PeerWait {
            // A peer cannot borrow an unused system seat.
            let spare = s.unoccupied_system(reserve);
            if s.live() >= total.saturating_sub(spare) {
                return Err(crate::run_stop::Stop::from_static(b"HostThreads"));
            }
            s.peers += 1;
            s.update_peaks();
            thread::Builder::new()
                .name("hirsel-peer".into())
                .stack_size(stack)
                .spawn(move || {
                    execute(&job);
                    pool.state.lock().unwrap().peers -= 1;
                    complete(job);
                })
                .map_err(|_| {
                    s.peers -= 1;
                    crate::run_stop::Stop::from_static(b"HostThreads")
                })?;
            return Ok(());
        }

        let which = usize::from(class == HostCall::System && reserve != 0);
        let (lane_cap, keep_clear) = if which == 0 {
            (
                p.plan.max_host_threads - reserve,
                s.unoccupied_system(reserve),
            )
        } else {
            (reserve, 0)
        };
        let headroom = s.live() < total.saturating_sub(keep_clear);
        s.lanes[which].jobs.push_back(job);
        s.update_peaks();
        let lane = &mut s.lanes[which];
        if !headroom && lane.live == 0 {
            // The host envelope is full of peer calls and has no thread
            // that could drain this lane.
            lane.jobs.pop_back();
            return Err(crate::run_stop::Stop::from_static(b"HostThreads"));
        }
        if lane.jobs.len() > lane.idle && lane.live < lane_cap && headroom {
            match thread::Builder::new()
                .name("hirsel-host".into())
                .stack_size(stack)
                .spawn(move || Self::run(p, which))
            {
                Ok(_) => {
                    lane.live += 1;
                    s.update_peaks();
                }
                Err(error) if lane.live > 0 && transient(&error) => s.spawn_deferred += 1,
                Err(_) => {
                    lane.jobs.pop_back();
                    return Err(crate::run_stop::Stop::from_static(b"HostThreads"));
                }
            }
        }
        pool.ready[which].notify_one();
        Ok(())
    }

    fn enqueue(p: &'static Pool, class: HostCall, job: HostJob) {
        if let Err(stop) = Self::try_enqueue(p, class, job) {
            crate::run_stop::raise(stop);
        }
    }

    fn run(p: &Pool, which: usize) {
        let pool = &p.helpers;
        let mut s = pool.state.lock().unwrap();
        loop {
            if let Some(job) = s.lanes[which].jobs.pop_front() {
                drop(s);
                execute(&job);
                complete(job);
                s = pool.state.lock().unwrap();
                continue;
            }
            if s.lanes.iter().map(|lane| lane.idle).sum::<usize>() >= p.plan.kept_host_threads {
                break;
            }
            s.lanes[which].idle += 1;
            let (next, waited) = pool.ready[which]
                .wait_timeout_while(s, Duration::from_millis(p.plan.host_idle_ms), |s| {
                    s.lanes[which].jobs.is_empty()
                })
                .unwrap();
            s = next;
            s.lanes[which].idle -= 1;
            if waited.timed_out() && s.lanes[which].jobs.is_empty() {
                break;
            }
        }
        s.lanes[which].live -= 1;
    }

    pub(super) fn report(&self) -> (usize, usize, u64) {
        let s = self.state.lock().unwrap();
        (s.peak_live, s.peak_queued, s.spawn_deferred)
    }
}

fn transient(error: &io::Error) -> bool {
    matches!(error.raw_os_error(), Some(libc::EAGAIN | libc::ENOMEM))
}

fn execute(job: &HostJob) {
    // SAFETY: the job's unit does not resume before completion.
    unsafe { (*job.call)() };
}

fn complete(job: HostJob) {
    // SAFETY: the parked unit outlives this job's only completion.
    unsafe { Reply::complete(job.reply) };
}

pub(super) fn run_off_slot(class: HostCall, call: &mut (dyn FnMut() + Send)) -> bool {
    if !in_unit() {
        return false;
    }
    let reply = Reply::new();
    // SAFETY: only the lifetime is erased. The unit pins the call and reply
    // while it waits, and the host thread never touches them after completion.
    let call: *mut (dyn FnMut() + Send + 'static) =
        unsafe { std::mem::transmute(call as *mut (dyn FnMut() + Send + '_)) };
    Helpers::enqueue(
        pool(),
        class,
        HostJob {
            call,
            reply: &reply,
        },
    );
    reply.wait();
    true
}

pub(super) fn stop_pool(p: &'static Pool) {
    if !p.stop.swap(true, Ordering::SeqCst) {
        for w in std::mem::take(&mut *p.idle.lock().unwrap()) {
            p.nsleep.fetch_sub(1, Ordering::SeqCst);
            wake_slot(p, w);
        }
    }
}

pub(crate) fn wake_all_for_stop() {
    if let Some(p) = POOL.get() {
        stop_pool(p);
    }
}

#[cfg(test)]
#[path = "units_host_tests.rs"]
mod tests;
