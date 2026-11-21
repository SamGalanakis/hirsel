//! System name lookup on a helper with no access to the caller's list.
use crate::{
    os_net::{Operations, pending_deadline, signal},
    provider::*,
    resolve::{ResolveRequest, append, spare},
    tasks::Outside,
};
use hirsel_core_net::Name;
use std::{
    ffi::{CStr, CString},
    net::{IpAddr, Ipv4Addr, Ipv6Addr},
    os::fd::OwnedFd,
    ptr,
    sync::{Arc, Mutex},
};

pub(crate) static OPERATIONS: Operations = Operations {
    step,
    abort,
    adopt_listener: |_, _: OwnedFd| unreachable!("resolve cannot adopt a listener"),
};

type Answer = Result<Vec<IpAddr>, i32>;

pub(crate) struct Job {
    completion: Arc<Mutex<Completion>>,
    deadline: Option<u64>,
}

struct Completion {
    active: Option<Outside>,
    answer: Option<Answer>,
}

impl Drop for Job {
    fn drop(&mut self) {
        self.completion.lock().unwrap().active.take();
    }
}

unsafe fn step(request: *mut Request, token: u64, first: bool) -> Step {
    // SAFETY: RESOLVE pins its adapter extension and writable List[Ip].
    let (request, resolve) = unsafe {
        let request = &mut *request;
        let resolve = &mut *request.path.cast_mut().cast::<ResolveRequest>();
        (request, resolve)
    };
    let deadline = if first {
        // SAFETY: the adapter pins the optional clock for the request.
        unsafe { pending_deadline(&resolve.network) }
    } else {
        resolve.job.as_ref().unwrap().deadline
    };
    if deadline.is_some_and(|at| at <= crate::os::monotonic_ns()) {
        return done(abandon(resolve, ABORT_DEADLINE));
    }
    if first {
        match resolve.name.as_ref().unwrap() {
            Name::Ip(address) => {
                let count = usize::from(spare(&request.buffer) != 0);
                let addresses = &std::slice::from_ref(address)[..count];
                // SAFETY: write at most one address into a spare slot.
                return done(unsafe { append(&mut request.buffer, addresses) });
            }
            Name::Dns(_) => {}
        }
        // SAFETY: the adapter pins the name, which authorize parsed as DNS.
        let name = unsafe { crate::tls::bytes(&resolve.raw_name) };
        let mut submit = Submit {
            token,
            deadline,
            name: CString::new(name).unwrap(),
        };
        // SAFETY: submission is synchronous, does not yield, and the helper
        // receives owned data. Spawn on the host stack, like file sync.
        let outcome = unsafe {
            crate::tasks::host_operation((&raw mut submit).cast(), request, submit_on_stack)
        };
        if outcome.error != IoError::OK {
            return done(outcome);
        }
    } else {
        let job = resolve.job.as_ref().unwrap();
        let answer = job.completion.lock().unwrap().answer.take();
        if let Some(answer) = answer {
            resolve.job.take();
            return done(match answer {
                // SAFETY: the worker bounded its answer by this request's spare.
                Ok(addresses) => unsafe { append(&mut request.buffer, &addresses) },
                Err(code) => resolver_error(code),
            });
        }
    }
    Step {
        kind: STEP_PENDING,
        deadline_ns: deadline.unwrap_or(0),
        ..Step::default()
    }
}

struct Submit {
    token: u64,
    deadline: Option<u64>,
    name: CString,
}

unsafe extern "C" fn submit_on_stack(state: *mut u8, request: *mut Request) -> Outcome {
    // SAFETY: step pins the arguments until this callback returns.
    let (submit, capacity, resolve) = unsafe {
        let submit = &mut *state.cast::<Submit>();
        let request = &mut *request;
        let resolve = &mut *request.path.cast_mut().cast::<ResolveRequest>();
        (submit, spare(&request.buffer), resolve)
    };
    let token = submit.token;
    let name = std::mem::take(&mut submit.name);
    let completion = Arc::new(Mutex::new(Completion {
        active: Some(Outside::begin()),
        answer: None,
    }));
    let result = completion.clone();
    #[cfg(test)]
    let lookup = LOOKUP.with(|entry| entry.borrow().clone());
    let helper = std::thread::Builder::new()
        .name("hirsel-resolve".into())
        .spawn(move || {
            #[cfg(test)]
            let answer = lookup.map_or_else(
                || getaddrinfo(&name, capacity),
                |lookup| lookup(&name, capacity),
            );
            #[cfg(not(test))]
            let answer = getaddrinfo(&name, capacity);
            let mut result = result.lock().unwrap();
            if let Some(active) = result.active.take() {
                result.answer = Some(answer);
                active.ready(token);
            }
        });
    match helper {
        Ok(helper) => {
            #[cfg(test)]
            if LOOKUP.with(|entry| entry.borrow().is_some()) {
                WORKERS.with(|workers| workers.borrow_mut().push(helper));
            }
            #[cfg(not(test))]
            drop(helper);
            resolve.job = Some(Job {
                completion,
                deadline: submit.deadline,
            });
            Outcome::default()
        }
        Err(error) => other(error.raw_os_error().unwrap_or(libc::EAGAIN)),
    }
}

unsafe fn abort(request: *mut Request, _: u64, reason: u8) -> Outcome {
    // SAFETY: the scheduler exclusively owns the pinned adapter during abort.
    let resolve = unsafe { &mut *(*request).path.cast_mut().cast::<ResolveRequest>() };
    abandon(resolve, reason)
}

fn abandon(resolve: &mut ResolveRequest, reason: u8) -> Outcome {
    resolve.job.take();
    signal(if reason == ABORT_CANCEL { 7 } else { 6 })
}

fn done(outcome: Outcome) -> Step {
    Step {
        outcome,
        ..Step::default()
    }
}

fn other(code: i32) -> Outcome {
    Outcome {
        error: IoError {
            tag: 8,
            code: code as u32,
        },
        ..Outcome::default()
    }
}

fn resolver_error(code: i32) -> Outcome {
    if code == libc::EAI_AGAIN {
        signal(3)
    } else {
        other(code)
    }
}

fn getaddrinfo(name: &CStr, capacity: usize) -> Answer {
    struct AddrInfo(*mut libc::addrinfo);
    impl Drop for AddrInfo {
        fn drop(&mut self) {
            if !self.0.is_null() {
                // SAFETY: this owns the head returned by a successful getaddrinfo.
                unsafe { libc::freeaddrinfo(self.0) };
            }
        }
    }
    // SAFETY: zero initializes the unused C hint fields. The C string and
    // hints stay live for the call, which writes only to the out pointer.
    let hints = libc::addrinfo {
        ai_family: libc::AF_UNSPEC,
        ai_socktype: libc::SOCK_STREAM,
        ..unsafe { std::mem::zeroed() }
    };
    let mut head = ptr::null_mut();
    let code = unsafe { libc::getaddrinfo(name.as_ptr(), ptr::null(), &hints, &mut head) };
    match code {
        0 => {}
        libc::EAI_NONAME | libc::EAI_NODATA => return Ok(Vec::new()),
        code => return Err(code),
    }
    let list = AddrInfo(head);
    let mut entry = list.0;
    let mut addresses = Vec::new();
    let mut seen = std::collections::HashSet::new();
    while addresses.len() < capacity && !entry.is_null() {
        // SAFETY: getaddrinfo owns a list of live records, each pointing to
        // its initialized address structure for the reported family.
        let item = unsafe { &*entry };
        let address = match item.ai_family {
            libc::AF_INET => {
                let v4 = unsafe { &*item.ai_addr.cast::<libc::sockaddr_in>() };
                Some(Ipv4Addr::from(u32::from_be(v4.sin_addr.s_addr)).into())
            }
            libc::AF_INET6 => {
                let v6 = unsafe { &*item.ai_addr.cast::<libc::sockaddr_in6>() };
                Some(Ipv6Addr::from(v6.sin6_addr.s6_addr).into())
            }
            _ => None,
        };
        if let Some(address) = address
            && seen.insert(address)
        {
            addresses.push(address);
        }
        entry = item.ai_next;
    }
    Ok(addresses)
}

#[cfg(test)]
type Lookup = Arc<dyn Fn(&CStr, usize) -> Answer + Send + Sync>;

#[cfg(test)]
thread_local! {
    pub(crate) static WORKERS: std::cell::RefCell<Vec<std::thread::JoinHandle<()>>> = const { std::cell::RefCell::new(Vec::new()) };
    pub(crate) static LOOKUP: std::cell::RefCell<Option<Lookup>> = const { std::cell::RefCell::new(None) };
}
