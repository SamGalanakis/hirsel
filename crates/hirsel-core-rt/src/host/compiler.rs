//! Each designation has two compiler helpers and a bounded FIFO. Only owned
//! input and completion state cross to helpers. The task owns its reply.
use crate::provider::{Outcome, Request, STEP_PENDING, Step};
use crate::tasks::Outside;
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};

type Answer = (u64, Vec<u8>);
const WORKERS: usize = 2;
const QUEUE: usize = 64;

#[repr(C)]
#[derive(Default)]
pub(crate) struct Call {
    pub(crate) reply: Vec<u8>,
    pending: Option<Arc<Completion>>,
}

struct Completion {
    cancelled: AtomicBool,
    state: Mutex<(Option<Outside>, Option<Answer>)>,
}

struct Job {
    token: u64,
    input: Vec<u8>,
    completion: Arc<Completion>,
}

#[derive(Default)]
struct Queue {
    helpers: usize,
    admitted: usize,
    jobs: VecDeque<Job>,
}

struct Lane {
    queue: Mutex<Queue>,
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    service: hirsel_compiler::grant::Designation,
}

pub(crate) struct Binding(Arc<Lane>);

impl Binding {
    pub(crate) fn new(worker: PathBuf) -> Box<Self> {
        #[cfg(target_os = "ios")]
        let _ = worker;
        Box::new(Self(Arc::new(Lane {
            queue: Mutex::default(),
            #[cfg(any(target_os = "linux", target_os = "macos"))]
            service: hirsel_compiler::grant::Designation::new(worker),
        })))
    }

    pub(crate) fn handle(&self) -> u64 {
        std::ptr::from_ref(self) as u64
    }
}

impl Lane {
    fn submit(self: &Arc<Self>, token: u64, input: &[u8], call: &mut Call) -> Result<(), Answer> {
        {
            let mut q = self.queue.lock().unwrap();
            if q.admitted == WORKERS + QUEUE {
                return Err((8, Vec::new()));
            }
            q.admitted += 1;
        }
        let input = input.to_vec();
        let mut q = self.queue.lock().unwrap();
        if q.helpers < WORKERS {
            let lane = self.clone();
            std::thread::Builder::new()
                .name("hirsel-compiler".into())
                .spawn(move || lane.work())
                .map_err(|e| {
                    q.admitted -= 1;
                    (9, e.to_string().into_bytes())
                })?;
            q.helpers += 1;
        }
        let completion = Arc::new(Completion {
            cancelled: AtomicBool::new(false),
            state: Mutex::new((Some(Outside::begin()), None)),
        });
        q.jobs.push_back(Job {
            token,
            input,
            completion: completion.clone(),
        });
        call.pending = Some(completion);
        Ok(())
    }

    fn work(&self) {
        loop {
            let job = {
                let mut q = self.queue.lock().unwrap();
                let Some(job) = q.jobs.pop_front() else {
                    q.helpers -= 1;
                    return;
                };
                job
            };
            #[cfg(any(target_os = "linux", target_os = "macos"))]
            let answer = self
                .service
                .check_cancellable(&job.input, &job.completion.cancelled);
            #[cfg(target_os = "ios")]
            let answer = (9, b"compiler workers are unavailable on iOS".to_vec());
            self.queue.lock().unwrap().admitted -= 1;
            let mut state = job.completion.state.lock().unwrap();
            if let Some(waiter) = state.0.take() {
                state.1 = Some(answer);
                waiter.complete(job.token);
            }
        }
    }
}

/// The adapter pins Call until delivery or abort. Helpers never borrow it.
pub(crate) unsafe fn step(row: &mut Request, token: u64, first: bool) -> Step {
    let call = unsafe { &mut *row.path.cast_mut().cast::<Call>() };
    let binding = unsafe { (row.handle as *const Binding).as_ref() };
    let answer = if let Some(binding) = binding {
        if first {
            let input = if row.buffer.len == 0 {
                &[]
            } else {
                unsafe { std::slice::from_raw_parts(row.buffer.data, row.buffer.len) }
            };
            binding.0.submit(token, input, call).err()
        } else {
            call.pending
                .as_ref()
                .unwrap()
                .state
                .lock()
                .unwrap()
                .1
                .take()
        }
    } else {
        Some((9, b"compiler worker was not designated".to_vec()))
    };
    if let Some((value, reply)) = answer {
        call.reply = reply;
        call.pending = None;
        Step {
            outcome: Outcome {
                value,
                ..Outcome::default()
            },
            ..Step::default()
        }
    } else {
        Step {
            kind: STEP_PENDING,
            ..Step::default()
        }
    }
}

pub(crate) unsafe fn abort(row: &mut Request) -> Outcome {
    let call = unsafe { &mut *row.path.cast_mut().cast::<Call>() };
    if let Some(pending) = call.pending.take() {
        pending.cancelled.store(true, Ordering::Release);
        pending.state.lock().unwrap().0.take();
        let binding = unsafe { &*(row.handle as *const Binding) };
        let mut q = binding.0.queue.lock().unwrap();
        if let Some(index) = q
            .jobs
            .iter()
            .position(|job| Arc::ptr_eq(&job.completion, &pending))
        {
            q.admitted -= 1;
            let removed = q.jobs.remove(index);
            drop(q);
            drop(removed);
        }
    }
    crate::provider::CANCELLED
}

#[cfg(test)]
#[path = "compiler_tests.rs"]
mod tests;
