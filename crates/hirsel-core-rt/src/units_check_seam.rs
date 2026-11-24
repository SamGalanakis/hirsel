//! The checker's seam: the pool's atomics, locks, thread parks, clock and
//! epoll/eventfd calls as scheduling points (see `units_check.rs`).
use super::{Wait, lock, me, note, point, quiet, shown};
use std::panic::Location;
use std::sync::atomic::Ordering;

fn at_point(addr: usize, op: &'static str, at: &'static Location<'static>) {
    if let Some(m) = me() {
        let q = quiet(addr);
        super::LAST_QUIET.set(q);
        if !q {
            point(m, Wait::Run, |st| format!("{op} {}", st.name(addr)), at);
        }
    }
}

macro_rules! int_atomic {
    ($name:ident, $t:ty) => {
        #[repr(transparent)]
        pub(crate) struct $name(std::sync::atomic::$name);
        impl $name {
            pub(crate) const fn new(v: $t) -> Self {
                Self(std::sync::atomic::$name::new(v))
            }
            fn a(&self) -> usize {
                self as *const Self as usize
            }
            pub(crate) fn raw(&self) -> $t {
                self.0.load(Ordering::SeqCst)
            }
            #[track_caller]
            pub(crate) fn load(&self, o: Ordering) -> $t {
                at_point(self.a(), "load", Location::caller());
                let v = self.0.load(o);
                note(|| format!("{}", shown(v as u64)));
                v
            }
            #[track_caller]
            pub(crate) fn store(&self, v: $t, o: Ordering) {
                at_point(self.a(), "store", Location::caller());
                self.0.store(v, o);
                note(|| format!("{}", shown(v as u64)));
            }
            #[track_caller]
            pub(crate) fn swap(&self, v: $t, o: Ordering) -> $t {
                at_point(self.a(), "swap", Location::caller());
                let r = self.0.swap(v, o);
                note(|| format!("{} => {}", shown(r as u64), shown(v as u64)));
                r
            }
            #[track_caller]
            pub(crate) fn compare_exchange(
                &self,
                c: $t,
                n: $t,
                s: Ordering,
                f: Ordering,
            ) -> Result<$t, $t> {
                at_point(self.a(), "compare_exchange", Location::caller());
                let r = self.0.compare_exchange(c, n, s, f);
                note(|| format!("{c}=>{n}: {r:?}"));
                r
            }
            #[track_caller]
            pub(crate) fn fetch_update<F: FnMut($t) -> Option<$t>>(
                &self,
                s: Ordering,
                f: Ordering,
                g: F,
            ) -> Result<$t, $t> {
                at_point(self.a(), "fetch_update", Location::caller());
                let r = self.0.fetch_update(s, f, g);
                let now = self.0.load(Ordering::SeqCst);
                note(|| format!("{r:?} => {now}"));
                r
            }
            #[track_caller]
            pub(crate) fn try_update<F: FnMut($t) -> Option<$t>>(
                &self,
                s: Ordering,
                f: Ordering,
                g: F,
            ) -> Result<$t, $t> {
                at_point(self.a(), "try_update", Location::caller());
                let r = self.0.try_update(s, f, g);
                let now = self.0.load(Ordering::SeqCst);
                note(|| format!("{r:?} => {now}"));
                r
            }
            #[track_caller]
            pub(crate) fn fetch_add(&self, v: $t, o: Ordering) -> $t {
                at_point(self.a(), "fetch_add", Location::caller());
                let r = self.0.fetch_add(v, o);
                note(|| format!("{} +{}", shown(r as u64), shown(v as u64)));
                r
            }
            #[track_caller]
            pub(crate) fn fetch_sub(&self, v: $t, o: Ordering) -> $t {
                at_point(self.a(), "fetch_sub", Location::caller());
                let r = self.0.fetch_sub(v, o);
                note(|| format!("{} -{}", shown(r as u64), shown(v as u64)));
                r
            }
            #[track_caller]
            pub(crate) fn fetch_min(&self, v: $t, o: Ordering) -> $t {
                at_point(self.a(), "fetch_min", Location::caller());
                let r = self.0.fetch_min(v, o);
                note(|| format!("{} min {}", shown(r as u64), shown(v as u64)));
                r
            }
            #[track_caller]
            pub(crate) fn fetch_max(&self, v: $t, o: Ordering) -> $t {
                at_point(self.a(), "fetch_max", Location::caller());
                let r = self.0.fetch_max(v, o);
                note(|| format!("{} max {}", shown(r as u64), shown(v as u64)));
                r
            }
            #[track_caller]
            pub(crate) fn fetch_or(&self, v: $t, o: Ordering) -> $t {
                at_point(self.a(), "fetch_or", Location::caller());
                let r = self.0.fetch_or(v, o);
                note(|| format!("{} or {}", shown(r as u64), shown(v as u64)));
                r
            }
        }
    };
}
int_atomic!(AtomicU8, u8);
int_atomic!(AtomicU32, u32);
int_atomic!(AtomicU64, u64);
int_atomic!(AtomicUsize, usize);

#[repr(transparent)]
pub(crate) struct AtomicBool(std::sync::atomic::AtomicBool);
impl AtomicBool {
    pub(crate) const fn new(v: bool) -> Self {
        Self(std::sync::atomic::AtomicBool::new(v))
    }
    fn a(&self) -> usize {
        self as *const Self as usize
    }
    pub(crate) fn raw(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
    #[track_caller]
    pub(crate) fn load(&self, o: Ordering) -> bool {
        at_point(self.a(), "load", Location::caller());
        let v = self.0.load(o);
        note(|| format!("{v}"));
        v
    }
    #[track_caller]
    pub(crate) fn store(&self, v: bool, o: Ordering) {
        at_point(self.a(), "store", Location::caller());
        self.0.store(v, o);
        note(|| format!("{v}"));
    }
    #[track_caller]
    pub(crate) fn swap(&self, v: bool, o: Ordering) -> bool {
        at_point(self.a(), "swap", Location::caller());
        let r = self.0.swap(v, o);
        note(|| format!("{r} => {v}"));
        r
    }
}

#[repr(transparent)]
pub(crate) struct AtomicPtr<T>(std::sync::atomic::AtomicPtr<T>);
impl<T> AtomicPtr<T> {
    pub(crate) const fn new(v: *mut T) -> Self {
        Self(std::sync::atomic::AtomicPtr::new(v))
    }
    fn a(&self) -> usize {
        self as *const Self as usize
    }
    pub(crate) fn raw(&self) -> *mut T {
        self.0.load(Ordering::SeqCst)
    }
    #[track_caller]
    pub(crate) fn load(&self, o: Ordering) -> *mut T {
        at_point(self.a(), "load", Location::caller());
        let v = self.0.load(o);
        note(|| super::unit_name(v as usize));
        v
    }
    #[track_caller]
    pub(crate) fn store(&self, v: *mut T, o: Ordering) {
        at_point(self.a(), "store", Location::caller());
        self.0.store(v, o);
        note(|| super::unit_name(v.addr()));
    }
    #[track_caller]
    pub(crate) fn swap(&self, v: *mut T, o: Ordering) -> *mut T {
        at_point(self.a(), "swap", Location::caller());
        let r = self.0.swap(v, o);
        note(|| {
            format!(
                "{} => {}",
                super::unit_name(r as usize),
                super::unit_name(v as usize)
            )
        });
        r
    }
    #[track_caller]
    pub(crate) fn compare_exchange(
        &self,
        c: *mut T,
        n: *mut T,
        s: Ordering,
        f: Ordering,
    ) -> Result<*mut T, *mut T> {
        at_point(self.a(), "compare_exchange", Location::caller());
        self.0.compare_exchange(c, n, s, f)
    }
}

/// A mutex whose lock is a scheduling point and whose contention blocks
/// the thread in the checker, so a descheduled holder never spins the
/// scheduler.
pub(crate) struct Mutex<T> {
    inner: std::sync::Mutex<T>,
}
pub(crate) struct MutexGuard<'a, T> {
    g: std::mem::ManuallyDrop<std::sync::MutexGuard<'a, T>>,
    addr: usize,
}
#[derive(Debug)]
pub(crate) struct WouldBlock;
impl<T> Mutex<T> {
    pub(crate) const fn new(v: T) -> Self {
        Self {
            inner: std::sync::Mutex::new(v),
        }
    }
    fn a(&self) -> usize {
        self as *const Self as usize
    }
    fn guard(&self) -> Option<MutexGuard<'_, T>> {
        let g = match self.inner.try_lock() {
            Ok(g) => g,
            Err(std::sync::TryLockError::Poisoned(e)) => e.into_inner(),
            Err(std::sync::TryLockError::WouldBlock) => return None,
        };
        Some(MutexGuard {
            g: std::mem::ManuallyDrop::new(g),
            addr: self.a(),
        })
    }
    /// The contents for a state dump, if nobody holds the lock.
    pub(crate) fn peek<R>(&self, f: impl FnOnce(&T) -> R) -> Option<R> {
        self.inner.try_lock().ok().map(|g| f(&g))
    }
    #[track_caller]
    pub(crate) fn lock(&self) -> Result<MutexGuard<'_, T>, WouldBlock> {
        let at = Location::caller();
        let Some(m) = me() else {
            let g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            return Ok(MutexGuard {
                g: std::mem::ManuallyDrop::new(g),
                addr: self.a(),
            });
        };
        // A quiet lock is no scheduling point, but a held one still waits.
        if !quiet(self.a()) {
            point(m, Wait::Run, |st| format!("lock {}", st.name(self.a())), at);
        }
        loop {
            if let Some(g) = self.guard() {
                return Ok(g);
            }
            point(
                m,
                Wait::Lock(self.a()),
                |st| format!("lock {} (held: waits)", st.name(self.a())),
                at,
            );
        }
    }
    #[track_caller]
    pub(crate) fn try_lock(&self) -> Result<MutexGuard<'_, T>, WouldBlock> {
        at_point(self.a(), "try_lock", Location::caller());
        let r = self.guard().ok_or(WouldBlock);
        note(|| {
            if r.is_ok() {
                "taken".into()
            } else {
                "held".into()
            }
        });
        r
    }
}
impl<T> Drop for MutexGuard<'_, T> {
    fn drop(&mut self) {
        // SAFETY: dropped once, here.
        unsafe { std::mem::ManuallyDrop::drop(&mut self.g) };
        if SCHED_ON.load(Ordering::Relaxed) {
            let mut st = lock();
            for t in &mut st.threads {
                if t.wait == Wait::Lock(self.addr) {
                    t.wait = Wait::Run;
                }
            }
        }
    }
}
#[track_caller]
pub(crate) fn read_arrival<T>(node: *const T) {
    at_point(node.addr(), "read arrival", Location::caller());
}

pub(super) static SCHED_ON: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);
impl<T> std::ops::Deref for MutexGuard<'_, T> {
    type Target = T;
    fn deref(&self) -> &T {
        &self.g
    }
}
impl<T> std::ops::DerefMut for MutexGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        &mut self.g
    }
}

pub(crate) mod thread {
    use super::super::{Wait, lock, me, point};
    use std::panic::Location;
    /// A worker's handle: the checker's thread index inside a run, the
    /// standard handle outside one.
    #[derive(Clone)]
    pub(crate) enum Thread {
        Checked(usize),
        Os(std::thread::Thread),
    }
    impl Thread {
        #[track_caller]
        pub(crate) fn unpark(&self) {
            let at = Location::caller();
            let (Some(m), Thread::Checked(t)) = (me(), self) else {
                if let Thread::Os(t) = self {
                    t.unpark();
                }
                return;
            };
            let t = *t;
            point(
                m,
                Wait::Run,
                |st| format!("unpark {}", st.threads[t].name),
                at,
            );
            let mut st = lock();
            st.threads[t].token = true;
            if matches!(st.threads[t].wait, Wait::Park | Wait::ParkUntil(_)) {
                st.threads[t].wait = Wait::Run;
            }
        }
    }
    pub(crate) fn current() -> Thread {
        match me() {
            Some(m) => Thread::Checked(m),
            None => Thread::Os(std::thread::current()),
        }
    }
    #[track_caller]
    pub(crate) fn park() {
        let at = Location::caller();
        let Some(m) = me() else {
            return std::thread::park();
        };
        point(m, Wait::Run, |_| "park".into(), at);
        let mut st = lock();
        if std::mem::take(&mut st.threads[m].token) {
            st.log(m, "park returns at once (token)".into());
            return;
        }
        drop(st);
        point(m, Wait::Park, |_| "park: sleeps on its handle".into(), at);
        lock().threads[m].token = false;
    }

    #[track_caller]
    pub(crate) fn park_timeout(duration: std::time::Duration) {
        let at = Location::caller();
        let Some(m) = me() else {
            return std::thread::park_timeout(duration);
        };
        point(m, Wait::Run, |_| "park with timeout".into(), at);
        let mut st = lock();
        if std::mem::take(&mut st.threads[m].token) {
            return;
        }
        let at_ns = st.time + duration.as_nanos() as u64;
        drop(st);
        point(
            m,
            Wait::ParkUntil(at_ns),
            |_| "park until timeout".into(),
            at,
        );
        lock().threads[m].token = false;
    }
}

#[track_caller]
pub(crate) fn monotonic_ns() -> u64 {
    if SCHED.get().is_none() {
        return crate::os::monotonic_ns();
    }
    if let Some(m) = me() {
        point(m, Wait::Run, |_| "clock".into(), Location::caller());
    }
    lock().time
}
use super::SCHED;

/// # Safety
/// `evs` has room for `max` events.
#[track_caller]
pub(crate) unsafe fn epoll_wait(
    epfd: i32,
    evs: *mut libc::epoll_event,
    max: i32,
    timeout: i32,
) -> i32 {
    let at = Location::caller();
    let Some(m) = me() else {
        // SAFETY: the caller's contract.
        return unsafe { libc::epoll_wait(epfd, evs, max, timeout) };
    };
    point(
        m,
        Wait::Run,
        |_| format!("epoll_wait timeout {timeout}ms"),
        at,
    );
    let mut deadline = None;
    let k = lock()
        .kernel
        .slots
        .iter()
        .position(|s| s.epfd == epfd)
        .expect("epoll_wait on an unmodelled descriptor");
    loop {
        let mut st = lock();
        let mut n = 0usize;
        let mut got = Vec::new();
        if std::mem::take(&mut st.kernel.slots[k].wake) && n < max as usize {
            // SAFETY: n < max.
            unsafe {
                *evs.add(n) = libc::epoll_event {
                    events: libc::EPOLLIN as u32,
                    u64: super::super::WAKE_TAG,
                }
            };
            n += 1;
            got.push("wake eventfd".to_string());
        }
        while n < max as usize
            && let Some((tag, ev)) = st.kernel.slots[k].edges.pop_front()
        {
            // SAFETY: n < max.
            unsafe {
                *evs.add(n) = libc::epoll_event {
                    events: ev,
                    u64: tag,
                }
            };
            n += 1;
            got.push(format!(
                "edge unit {} key {}",
                tag >> hirsel_core_net::SHARED_UNIT_SHIFT,
                tag & 0xffff_ffff
            ));
        }
        if n > 0 {
            st.log(m, format!("epoll_wait returns {n}: {}", got.join(", ")));
            return n as i32;
        }
        if timeout == 0 {
            st.log(m, "epoll_wait returns 0".into());
            return 0;
        }
        if deadline.is_none() && timeout > 0 {
            deadline = Some(st.time + timeout as u64 * 1_000_000);
        }
        if let Some(d) = deadline
            && st.time >= d
        {
            st.log(m, "epoll_wait times out".into());
            return 0;
        }
        drop(st);
        point(
            m,
            Wait::Epoll(k, deadline),
            |_| {
                format!(
                    "epoll_wait blocks (deadline {:?})",
                    deadline.map(|d| (d - super::t0()) / 1000)
                )
            },
            at,
        );
    }
}
/// # Safety
/// `buf` has eight readable bytes.
#[track_caller]
pub(crate) unsafe fn write(fd: i32, buf: *const libc::c_void, n: usize) -> isize {
    let at = Location::caller();
    let Some(m) = me() else {
        // SAFETY: the caller's contract.
        return unsafe { libc::write(fd, buf, n) };
    };
    point(m, Wait::Run, |_| "write wake eventfd".into(), at);
    let mut st = lock();
    let k = st
        .kernel
        .slots
        .iter()
        .position(|s| s.wakefd == fd)
        .filter(|_| n == 8)
        .expect("write on an unmodelled descriptor");
    // SAFETY: eight readable bytes.
    assert_ne!(unsafe { *buf.cast::<u64>() }, 0);
    st.kernel.slots[k].wake = true;
    st.wake_epoll(k);
    8
}
