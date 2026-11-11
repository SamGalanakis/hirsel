//! The callee service has one owner: the launcher on its admitting thread.
//! Worker-visible Slot holds only transport state and an opaque home location.
use super::place::{Owed, Place};
use super::*;
use crate::io::Adapter;
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicPtr, Ordering};

pub(crate) struct Slot {
    pub(super) transport: Arc<Transport>,
    home: AtomicPtr<Home>,
    poll: unsafe fn(*mut Home, *mut RunShared) -> Stopped<bool>,
}
impl Slot {
    pub(crate) fn new(io: Arc<crate::io_real::ConcurrentIo>) -> Result<Arc<Self>, AdapterError> {
        Ok(Arc::new(Self {
            transport: Transport::new(io)?,
            home: AtomicPtr::new(std::ptr::null_mut()),
            poll: poll_home,
        }))
    }
}

unsafe fn poll_home(home: *mut Home, run: *mut RunShared) -> Stopped<bool> {
    // SAFETY: only the admission owner calls the slot's poll hook.
    unsafe { (*home).poll(run) }
}

/// A sent call that starts a callee run.
struct Called {
    request: RunnerRequest,
    caller: [u8; 16],
    grant: RunnerGrant,
    ordinal: u64,
}

struct Child {
    run: CalleeRun,
    key: [u8; 32],
    lose_reply: bool,
    crashing: bool,
    io: Rc<RefCell<RealIo>>,
    recording: bool,
}

/// A `db@1` place home serves, by place key (its runner's node, else its name).
struct Head {
    place: Arc<Place>,
    /// The head's run, until it ends.
    run: Option<CalleeRun>,
    _io: Option<Rc<RefCell<RealIo>>>,
}

/// The place this run's root heads for a runner host in another process (`--hirsel-place`):
/// the host's admitted calls arrive on the link, and each end goes back on it. The host
/// owns the streams, so an arrival's key is its link id.
struct Link {
    place: Arc<Place>,
    /// Each arrival the reader took off the link, then `None` once the link closed.
    arrivals: std::sync::mpsc::Receiver<Option<(u64, place::ServedCall)>>,
    out: std::fs::File,
}
impl Link {
    fn key(id: u64) -> [u8; 32] {
        let mut key = [0; 32];
        key[..8].copy_from_slice(&id.to_le_bytes());
        key
    }
    /// Give the host every end the head made; a host that is gone takes no more, and its
    /// closed link must not end the head's program by SIGPIPE.
    fn send_ends(&mut self) -> bool {
        let ends = self.place.ends();
        if ends.is_empty() {
            return false;
        }
        crate::process::without_sigpipe(|| {
            for (key, outcome) in &ends {
                let id = u64::from_le_bytes(key[..8].try_into().unwrap());
                let end = hirsel_run_schema::place::PlaceMessage::End(id, outcome.clone());
                if end.write(&mut self.out).is_err() {
                    break;
                }
            }
        });
        true
    }
}
impl Drop for Link {
    /// The head's last ends reach the host before the process exits.
    fn drop(&mut self) {
        self.send_ends();
    }
}

/// A caller's transact stream at one place: the last position with a definite end, the
/// request in flight, and the transact calls waiting for it to end.
#[derive(Default)]
struct Stream {
    applied: u64,
    in_flight: Option<[u8; 32]>,
    waiting: VecDeque<(String, Owed)>,
}

pub(crate) struct Home {
    slot: Arc<Slot>,
    config: Config,
    children: Vec<Child>,
    heads: BTreeMap<String, Head>,
    /// Each caller's transact stream, by (caller instance, runner name).
    streams: BTreeMap<([u8; 16], String), Stream>,
    /// The stream of each transact call routed to a place and not yet ended.
    served: BTreeMap<[u8; 32], ([u8; 16], String)>,
    tag: u64,
    driver_installed: bool,
    link: Option<Link>,
}
impl Home {
    /// # Safety
    /// Sole admission owner before root publication. The returned stable box
    /// stays home and outlives every call to service on this run.
    pub(crate) unsafe fn new(slot: Arc<Slot>, config: Config) -> Box<Self> {
        let mut home = Box::new(Self {
            slot,
            config,
            children: Vec::new(),
            heads: BTreeMap::new(),
            streams: BTreeMap::new(),
            served: BTreeMap::new(),
            tag: 0,
            driver_installed: false,
            link: None,
        });
        let pointer = &raw mut *home;
        assert!(home.slot.home.swap(pointer, Ordering::AcqRel).is_null());
        home
    }

    /// Serve the place this run's root heads over `link` to its runner host: the root's
    /// call state gets the place, and a reader thread hands home each arrival.
    ///
    /// # Safety
    /// Sole admission owner of `root`, before any worker reads its call state.
    pub(crate) unsafe fn serve_link(
        &mut self,
        root: *mut TaskCtx,
        link: std::fs::File,
    ) -> Result<(), String> {
        let place = Place::new(self.slot.transport.signal.clone());
        let run = unsafe { (*root).run() };
        let call = run
            .runner_call
            .take()
            .ok_or("a place link without a call run")?;
        let mut call = Arc::try_unwrap(call).map_err(|_| "the root's call state is shared")?;
        call.place = Some(place.clone());
        run.runner_call = Some(Arc::new(call));
        let mut input = std::io::BufReader::new(link.try_clone().map_err(|e| e.to_string())?);
        let (arrivals, received) = std::sync::mpsc::channel();
        let signal = self.slot.transport.signal.clone();
        self.slot.transport.linked.store(true, Ordering::Release);
        std::thread::Builder::new()
            .name("place link".into())
            .spawn(move || {
                use hirsel_run_schema::place::PlaceMessage;
                loop {
                    let arrival = match PlaceMessage::read(&mut input) {
                        Ok(Some(PlaceMessage::Arrive(id, call))) => Some((id, call)),
                        // A clean close, a broken link or a message only the head sends.
                        _ => None,
                    };
                    let closed = arrival.is_none();
                    let _ = arrivals.send(arrival);
                    signal.notify();
                    if closed {
                        break;
                    }
                }
            })
            .map_err(|e| e.to_string())?;
        self.link = Some(Link {
            place,
            arrivals: received,
            out: link,
        });
        Ok(())
    }

    /// Hand the head what arrived on its link, and the host the ends the head made.
    fn poll_link(&mut self, waiters: &mut Vec<(u32, u32)>) -> bool {
        let Some(link) = &mut self.link else {
            return false;
        };
        let mut changed = false;
        for arrival in link.arrivals.try_iter() {
            changed = true;
            let woken = match arrival {
                Some((id, call)) => link.place.arrive(Owed {
                    key: Link::key(id),
                    call,
                }),
                None => {
                    self.slot.transport.linked.store(false, Ordering::Release);
                    link.place.disconnect()
                }
            };
            waiters.extend(woken);
        }
        changed | link.send_ends()
    }

    fn start_callee(
        &mut self,
        key: [u8; 32],
        spec: Called,
        start: Start,
        lose_reply: bool,
        crash_after: Option<u32>,
    ) -> Stopped<()> {
        let seed = |name: &str| hirsel_run_schema::callee_seed(&key, name);
        let grants = spec.grant.grants;
        self.tag += 1;
        let stdout = self
            .config
            .stdout
            .clone()
            .unwrap_or_else(|| crate::host::Sink::new(crate::launch::RawStdout));
        // Follow the source host's host_callee admission: the same checked
        // callee facts, narrowed grants, cause and derived seeds bind its tape.
        let facts = self
            .config
            .callees
            .get(&spec.request.path)
            .ok_or_else(|| Stop::Refused("missing checked callee facts".into()))?;
        let cause = hirsel_run_schema::Cause {
            instance: spec.caller,
            ordinal: spec.ordinal,
            digest: spec.request.digest,
        };
        let mut record = hirsel_run_config::RunConfig::empty(
            facts.program,
            spec.request.path.clone(),
            hirsel_run_schema::LaunchKind::Call,
        );
        record.call = Some(hirsel_run_schema::CallRecord::new(
            spec.request.form,
            &cause,
            &spec.request.argument,
        ));
        record.inputs.hash_seed = seed("hash");
        record.inputs.random_seed = seed("random");
        record.inputs.test_seed = seed("test");
        record.io = (spec.request.form == CallForm::Call).then_some(grants);
        record.admission =
            spec.grant
                .entry
                .admission
                .as_ref()
                .map(|a| hirsel_run_schema::Admission {
                    not_before: None,
                    expires: None,
                    revocation: None,
                    code: a.code.clone(),
                });
        let refused = |error: hirsel_run_schema::RunRefusal| {
            Stop::Refused(format!("callee admission: {error}"))
        };
        let bound = record
            .validated()
            .map_err(refused)?
            .bind(&hirsel_run_config::ProgramFacts {
                program: facts.program,
                entry: spec.request.path.clone(),
                launch: hirsel_run_schema::LaunchKind::Call,
                roots: if spec.request.form == CallForm::Call {
                    vec![hirsel_run_config::Root::Io]
                } else {
                    vec![]
                },
                code: facts.reach.clone(),
            })
            .map_err(refused)?;
        let mut host = crate::host_launch::HostGrants::sharing(
            stdout,
            self.slot.transport.driver.tagged(self.tag),
        );
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|e| Stop::Refused(e.to_string()))?
            .as_nanos();
        let now = i64::try_from(now).map_err(|e| Stop::Refused(e.to_string()))?;
        let admitted = bound.admit(&mut host, now).map_err(refused)?;
        let mut io = host.into_io();
        let tape = self
            .config
            .tape
            .as_ref()
            .filter(|_| spec.grant.entry.recording.as_deref() != Some("none"))
            .map(|tape| {
                let mut name = tape.clone().into_os_string();
                name.push(format!(".call-{}", spec.ordinal));
                std::path::PathBuf::from(name)
            });
        crate::host_launch::journal(&mut io, &admitted, tape.as_deref(), &[]).map_err(|e| {
            Stop::Environmental(hirsel_run_schema::Environmental::Recording(format!(
                "callee Io tape: {e}"
            )))
        })?;
        io.discard_history();
        let io = Rc::new(RefCell::new(io));
        let binding = match crash_after {
            Some(point) => Binding::new(super::callee_io::CalleeIo::new(io.clone(), point)),
            None => Binding::new(crate::launch::Shared(io.clone())),
        };
        let binding = Box::new(binding);
        let mut runtime = self.config.runtime;
        runtime.hash_seed = seed("hash");
        let run = unsafe {
            CalleeRun::start(
                runtime,
                self.config.failure_table,
                &self.config,
                start,
                binding,
                self.slot.transport.driver.clone(),
                None,
            )
        };
        unsafe { run.notify_on(self.slot.transport.signal.clone()) };
        self.children.push(Child {
            run,
            key,
            lose_reply,
            crashing: crash_after.is_some(),
            io,
            recording: tape.is_some(),
        });
        Ok(())
    }

    /// Route the served call `key` to its place on its caller's stream: a transact call goes
    /// at the position after the last definite end, one in flight per stream; a query at 0.
    fn serve(
        &mut self,
        key: [u8; 32],
        caller: [u8; 16],
        grant: &RunnerGrant,
        call: place::ServedCall,
    ) -> Stopped<()> {
        let entry = &grant.entry;
        let place = entry.node.clone().unwrap_or_else(|| entry.name.clone());
        self.head(&place, grant)?;
        let stream_key = (caller, entry.name.clone());
        let stream = self.streams.entry(stream_key.clone()).or_default();
        let mut owed = Owed { key, call };
        owed.call.acknowledged = stream.applied;
        if owed.call.form == CallForm::Transact {
            self.served.insert(key, stream_key);
            if stream.in_flight.is_some() {
                stream.waiting.push_back((place, owed));
                return Ok(());
            }
            owed.call.position = stream.applied + 1;
            stream.in_flight = Some(key);
        } else {
            owed.call.position = 0;
        }
        self.arrive(&place, owed);
        Ok(())
    }

    /// `owed` arrives at `place`, whose parked take wakes.
    fn arrive(&mut self, place: &str, owed: Owed) {
        let head = &self.heads[place];
        if let Some(waiter) = head.place.arrive(owed)
            && let Some(run) = &head.run
        {
            unsafe { crate::tasks::free::home::runner_wakes((*run.root).run_ptr(), &[waiter]) };
        }
    }

    /// Start `place`'s head at its first call: a run of `$native.runner_main[head]` under the
    /// runner's grants, its dirs in memory (the simulated world of `sim@1`), its store
    /// authorities exactly the granted ones, with claims and seeds from the place instance.
    fn head(&mut self, place: &str, grant: &RunnerGrant) -> Stopped<()> {
        if self.heads.contains_key(place) {
            return Ok(());
        }
        let refused = |what: &str, e: hirsel_run_schema::RunRefusal| {
            Stop::refusal(AdapterError::Unsupported(format!(
                "a place's {what}: {e:?}"
            )))
        };
        let path = grant
            .entry
            .head
            .clone()
            .ok_or_else(|| Stop::Refused("a db@1 place without a head".into()))?;
        let lookup = |path: &str| {
            self.config
                .entries
                .iter()
                .find(|(_, p)| p == path)
                .map(|(code, _)| *code)
                .ok_or_else(|| Stop::Refused(format!("place head {path} is not built")))
        };
        let environment = empty_environment();
        let start = Start {
            main: (
                lookup(&hirsel_run_schema::place_main_path(&path))?,
                environment,
            ),
            entry: (lookup(&path)?, environment),
            argument: super::super::places::UNIT_ARGUMENT.to_vec(),
            form: CallForm::Call,
        };
        let digest = hirsel_run_schema::hash_parts("hirsel.place.v1", [place.as_bytes()]);
        let mut instance = [0; 16];
        instance.copy_from_slice(&digest[..16]);
        let seed = |part: &str| {
            let h = hirsel_run_schema::hash_parts(
                "hirsel.place.v1",
                [place.as_bytes(), part.as_bytes()],
            );
            u64::from_le_bytes(h[..8].try_into().unwrap())
        };
        let grants = &grant.grants;
        let stdout = self
            .config
            .stdout
            .clone()
            .unwrap_or_else(|| crate::host::Sink::new(crate::launch::RawStdout));
        let mounts = grants
            .dirs
            .iter()
            .map(|dir| (dir.path.clone(), dir.access))
            .collect();
        let mut io = RealIo::fixture(BTreeMap::new(), mounts, vec![], stdout)
            .map_err(|e| refused("dirs", e))?;
        io.set_authorities(&grants.authorities, Some(instance))
            .map_err(|e| refused("authorities", e))?;
        io.set_projections(super::super::projections(grants));
        self.tag += 1;
        io.set_cooperative(self.slot.transport.driver.tagged(self.tag));
        io.clock_grants(
            grants.monotonic_clock.is_some(),
            grants.wall_clock.is_some(),
        );
        io.set_random(
            hirsel_run_schema::ProviderContract::RandomSplitMix64,
            seed("random"),
        );
        io.discard_history();
        let io = Rc::new(RefCell::new(io));
        let binding = Box::new(Binding::new(crate::launch::Shared(io.clone())));
        let mut runtime = self.config.runtime;
        runtime.hash_seed = seed("hash");
        let place_rows = Place::new(self.slot.transport.signal.clone());
        let run = unsafe {
            CalleeRun::start(
                runtime,
                self.config.failure_table,
                &self.config,
                start,
                binding,
                self.slot.transport.driver.clone(),
                Some(place_rows.clone()),
            )
        };
        unsafe { run.notify_on(self.slot.transport.signal.clone()) };
        self.heads.insert(
            place.to_owned(),
            Head {
                place: place_rows,
                run: Some(run),
                _io: Some(io),
            },
        );
        Ok(())
    }

    /// Run the heads and give their ends to the callers. An ended head closes its place.
    fn poll_heads(&mut self, waiters: &mut Vec<(u32, u32)>) -> Stopped<bool> {
        let mut changed = false;
        let mut ends = Vec::new();
        for head in self.heads.values_mut() {
            if let Some(run) = &mut head.run {
                changed |= run.service();
                if run.poll().is_some() {
                    // However the head ended, its place serves nothing more.
                    head.run = None;
                    head.place.head_ended();
                    changed = true;
                }
            }
            ends.extend(head.place.ends());
        }
        for (key, outcome) in ends {
            changed = true;
            self.end(key, outcome, waiters);
        }
        Ok(changed)
    }

    /// The served call `key` ended: its caller's call completes, and its stream's next
    /// waiting transact call goes at the position after it.
    fn end(&mut self, key: [u8; 32], outcome: CallOutcome, waiters: &mut Vec<(u32, u32)>) {
        let advanced = matches!(
            outcome,
            CallOutcome::Ok(_) | CallOutcome::Failed(_) | CallOutcome::InDoubt(_)
        );
        if let Some(waiter) = self.slot.transport.complete(
            key,
            RunnerCompletion {
                outcome,
                callee: None,
            },
        ) {
            waiters.push(waiter);
        }
        let Some(stream_key) = self.served.remove(&key) else {
            return;
        };
        let stream = self.streams.get_mut(&stream_key).unwrap();
        if advanced {
            stream.applied += 1;
        }
        stream.in_flight = None;
        if let Some((place, mut owed)) = stream.waiting.pop_front() {
            owed.call.position = stream.applied + 1;
            owed.call.acknowledged = stream.applied;
            stream.in_flight = Some(owed.key);
            self.arrive(&place, owed);
        }
    }

    fn poll(&mut self, root: *mut RunShared) -> Stopped<bool> {
        if !self.driver_installed {
            unsafe { crate::pool::home_driver(root, self.slot.transport.driver.clone()) };
            self.driver_installed = true;
        }
        self.slot.transport.signal.drain();
        // Consume the work signal's readiness with the signal itself; a stale one would end
        // every idle wait at once and spin home.
        self.slot.transport.work.ready().map_err(Stop::refusal)?;
        let mut changed = false;
        for (_token, key, spec) in self.slot.transport.take() {
            let Spec {
                request,
                caller,
                grant,
                ordinal,
                work,
            } = spec;
            match work {
                Work::Callee {
                    start,
                    lose_reply,
                    crash_after,
                } => {
                    let called = Called {
                        request,
                        caller,
                        grant,
                        ordinal,
                    };
                    self.start_callee(key, called, start, lose_reply, crash_after)?
                }
                Work::Served(call) => self.serve(key, caller, &grant, call)?,
            }
            changed = true;
        }
        for key in self.slot.transport.cancels() {
            if let Some(child) = self.children.iter().find(|child| child.key == key) {
                unsafe { crate::tasks::free::home::runner_cancel((*child.run.root).run_ptr()) };
                changed = true;
            }
        }
        let mut waiters = Vec::new();
        let mut index = 0;
        while index < self.children.len() {
            changed |= self.children[index].run.service();
            let Some(reply) = self.children[index].run.poll() else {
                index += 1;
                continue;
            };
            if matches!(
                &reply,
                Err(Stop::Environmental(
                    hirsel_run_schema::Environmental::Stopped
                ))
            ) && self.children[index].crashing
            {
                let child = self.children.swap_remove(index);
                if child.recording {
                    child
                        .io
                        .borrow_mut()
                        .finish_recording(&hirsel_run_schema::RunOutcome::Environmental(
                            hirsel_run_schema::Environmental::Stopped,
                        ))
                        .map_err(|e| {
                            Stop::Environmental(hirsel_run_schema::Environmental::Recording(
                                format!("callee Io trailer: {e}"),
                            ))
                        })?;
                }
                if let Some(waiter) = self.slot.transport.complete(
                    child.key,
                    RunnerCompletion {
                        outcome: CallOutcome::InDoubt(ABANDONED),
                        callee: Some(hirsel_run_schema::callee_instance(&child.key)),
                    },
                ) {
                    waiters.push(waiter);
                }
                changed = true;
                continue;
            }
            let reply = reply?;
            let child = self.children.swap_remove(index);
            let returned = match reply.tag {
                REPLY_OK => CalleeOutcome::Ok(reply.bytes.clone()),
                REPLY_ERROR => CalleeOutcome::Err(reply.bytes.clone()),
                _ => CalleeOutcome::Failed(reply.bytes.clone()),
            };
            if !matches!(
                child.io.borrow_mut().call(Request::CallReturn(returned)),
                Ok(Completion::Ready)
            ) {
                return Err(Stop::refusal(AdapterError::InvalidCompletion));
            }
            if child.recording {
                use hirsel_run_schema::{Returned, RunOutcome};
                let outcome = match reply.tag {
                    REPLY_OK => RunOutcome::Returned(Returned::Ok),
                    REPLY_ERROR => RunOutcome::Returned(Returned::Error),
                    _ => {
                        let table = self.config.failure_table.expect("callee failure table");
                        let mut failures =
                            reply.chain.iter().map(|(record, _)| table.failure(record));
                        RunOutcome::Failed {
                            primary: failures.next().expect("callee primary failure"),
                            causes: failures.collect(),
                        }
                    }
                };
                child
                    .io
                    .borrow_mut()
                    .finish_recording(&outcome)
                    .map_err(|e| {
                        Stop::Environmental(hirsel_run_schema::Environmental::Recording(format!(
                            "callee Io trailer: {e}"
                        )))
                    })?;
            }
            let outcome = if child.crashing {
                // A callee that ends before its selected crash point ran to
                // completion, but the simulated place still loses its outcome.
                CallOutcome::InDoubt(ABANDONED)
            } else if child.lose_reply {
                CallOutcome::InDoubt(LOST)
            } else {
                match reply.tag {
                    REPLY_OK => CallOutcome::Ok(reply.bytes),
                    REPLY_ERROR => CallOutcome::Failed(reply.bytes),
                    _ => CallOutcome::Faulted(reply.bytes),
                }
            };
            if let Some(waiter) = self.slot.transport.complete(
                child.key,
                RunnerCompletion {
                    outcome,
                    callee: Some(hirsel_run_schema::callee_instance(&child.key)),
                },
            ) {
                waiters.push(waiter);
            }
            changed = true;
        }
        changed |= self.poll_heads(&mut waiters)?;
        changed |= self.poll_link(&mut waiters);
        unsafe { crate::tasks::free::home::runner_wakes(root, &waiters) };
        if !changed && self.slot.transport.pending() && !unsafe { crate::pool::home_posts(root) } {
            crate::readiness::Readiness::wait_many(&[
                &self.slot.transport.driver,
                &self.slot.transport.work,
            ])
            .map_err(Stop::refusal)?;
            return Ok(true);
        }
        Ok(changed)
    }
}
impl Drop for Home {
    fn drop(&mut self) {
        self.slot
            .home
            .store(std::ptr::null_mut(), Ordering::Release);
        // Children are destroyed before config/Sink on this home thread;
        // CalleeRun closes its bridge before its Rc adapter is dropped.
        self.children.clear();
        self.heads.clear();
    }
}

/// # Safety
/// Admission's home loop, never from program code or a worker. It serializes
/// all access to Home with native runner metadata entries served on home.
pub(crate) unsafe fn service(run: *mut RunShared) -> bool {
    let Some(slot) = (unsafe { (*run).runner_home.as_ref() }) else {
        return false;
    };
    let home = slot.home.load(Ordering::Acquire);
    if home.is_null() {
        return false;
    }
    match unsafe { (slot.poll)(home, run) } {
        Ok(changed) => changed,
        Err(stop) => {
            unsafe { (*run).control.request(stop) };
            true
        }
    }
}
