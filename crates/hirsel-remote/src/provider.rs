//! A run's `remote.tls@1` runners (conformance/old-spec/runners.md, Providers), for any engine. Each
//! runner's provider ([`Client`]) runs on its own thread with the TLS connection it opens,
//! so the run never blocks on the network: the run hands it each sent call and each
//! forwarded cancellation, and takes back each completion at its next scheduling point.
//! Each completion calls the run's `notify`, so an idle run wakes for it.
//!
//! The TLS material, the endpoint and the settings are read once, when the run opens its
//! providers; a runner whose material does not load refuses the launch.
use crate::client::{Client, ClientAction, Settings};
use crate::frame::Call;
use crate::transport::{ClientTls, Command, Commands, Event};
use crate::wire::Wire;
use hirsel_run_schema::{Endpoint, Roots, RunnerCompletion, RunnerEntry};
use std::collections::BTreeMap;
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// How a run learns that a completion arrived.
pub type Notify = Arc<dyn Fn() + Send + Sync>;

enum Input {
    Submit(u64, Box<Call>, [u8; 32]),
    Cancel(u64, u32),
    /// A frame or the end of connection `generation`.
    Connection(u64, Event),
    Close,
}

struct Runner {
    inputs: Sender<Input>,
    thread: Option<JoinHandle<()>>,
}

/// The run's remote runners, by name.
pub struct Providers {
    runners: BTreeMap<String, Runner>,
    completions: Arc<Mutex<Vec<(u64, RunnerCompletion)>>>,
}

fn read(path: &str, what: &str) -> Result<Vec<u8>, String> {
    std::fs::read(path).map_err(|e| format!("io.runners: {what} {path}: {e}"))
}

impl Providers {
    /// Open a provider for every `remote.tls@1` runner of `entries`; None when there is none.
    pub fn open(
        entries: &[RunnerEntry],
        instance: [u8; 16],
        wire: impl FnOnce() -> Result<Wire, String>,
        notify: Notify,
    ) -> Result<Option<Self>, String> {
        let remote: Vec<&RunnerEntry> = entries
            .iter()
            .filter(|e| e.provider == "remote.tls@1")
            .collect();
        if remote.is_empty() {
            return Ok(None);
        }
        let wire = Arc::new(wire()?);
        let completions = Arc::new(Mutex::new(vec![]));
        let mut runners = BTreeMap::new();
        for entry in remote {
            let roots = match entry.roots.as_ref() {
                Some(Roots::Pem { pem }) => Some(read(pem, "roots")?),
                _ => None,
            };
            let identity = match &entry.identity {
                Some(files) => Some((
                    read(&files.chain, "identity")?,
                    read(&files.key, "identity")?,
                )),
                None => None,
            };
            let tls = ClientTls::new(
                entry.server_name.as_deref().unwrap_or_default(),
                roots.as_deref(),
                identity.as_ref().map(|(c, k)| (c.as_slice(), k.as_slice())),
            )
            .map_err(|e| format!("io.runners.{}: {e}", entry.name))?;
            let endpoint = entry
                .endpoint
                .clone()
                .ok_or_else(|| format!("io.runners.{}: no endpoint", entry.name))?;
            let stream = hirsel_run_schema::hash_parts(
                "hirsel.stream.v1",
                [entry.name.as_bytes(), &instance[..]],
            );
            let client = Client::new(
                stream,
                instance,
                Settings {
                    timeout_ns: entry.timeout_ns,
                    reconnect: entry.reconnects(),
                    retry: entry.attempts(),
                    reconcile: entry.reconcile,
                },
            );
            let (inputs, received) = channel();
            let worker = Worker {
                tls,
                endpoint,
                wire: wire.clone(),
                client,
                inputs: inputs.clone(),
                received,
                connection: None,
                generation: 0,
                completions: completions.clone(),
                notify: notify.clone(),
                start: Instant::now(),
            };
            let thread = std::thread::Builder::new()
                .name(format!("runner {}", entry.name))
                .spawn(move || worker.run())
                .map_err(|e| format!("io.runners: {e}"))?;
            runners.insert(
                entry.name.clone(),
                Runner {
                    inputs,
                    thread: Some(thread),
                },
            );
        }
        Ok(Some(Self {
            runners,
            completions,
        }))
    }
    pub fn serves(&self, runner: &str) -> bool {
        self.runners.contains_key(runner)
    }
    pub fn submit(&self, runner: &str, token: u64, call: Call, digest: [u8; 32]) {
        if let Some(r) = self.runners.get(runner) {
            let _ = r.inputs.send(Input::Submit(token, Box::new(call), digest));
        }
    }
    pub fn cancel(&self, runner: &str, token: u64, cause: u32) {
        if let Some(r) = self.runners.get(runner) {
            let _ = r.inputs.send(Input::Cancel(token, cause));
        }
    }
    /// The completions that arrived since the last take.
    pub fn take(&self) -> Vec<(u64, RunnerCompletion)> {
        std::mem::take(&mut *self.completions.lock().unwrap())
    }
}
impl Drop for Providers {
    /// The run ended: each stream's session closes, acknowledging what the run holds.
    fn drop(&mut self) {
        for runner in self.runners.values_mut() {
            let _ = runner.inputs.send(Input::Close);
            if let Some(thread) = runner.thread.take() {
                let _ = thread.join();
            }
        }
    }
}

struct Worker {
    tls: ClientTls,
    endpoint: Endpoint,
    wire: Arc<Wire>,
    client: Client,
    inputs: Sender<Input>,
    received: Receiver<Input>,
    connection: Option<Commands>,
    generation: u64,
    completions: Arc<Mutex<Vec<(u64, RunnerCompletion)>>>,
    notify: Notify,
    start: Instant,
}
impl Worker {
    fn now(&self) -> u64 {
        u64::try_from(self.start.elapsed().as_nanos()).unwrap_or(u64::MAX)
    }
    fn run(mut self) {
        loop {
            let wait = self
                .client
                .deadline()
                .map(|at| Duration::from_nanos(at.saturating_sub(self.now())))
                .unwrap_or(Duration::from_secs(3600));
            let input = match self.received.recv_timeout(wait) {
                Ok(input) => Some(input),
                Err(RecvTimeoutError::Timeout) => None,
                Err(RecvTimeoutError::Disconnected) => return,
            };
            let now = self.now();
            let mut actions = self.client.tick(now);
            match input {
                None => {}
                Some(Input::Submit(token, call, digest)) => {
                    actions.extend(self.client.submit(token, *call, digest))
                }
                Some(Input::Cancel(token, cause)) => {
                    actions.extend(self.client.cancel(token, cause))
                }
                Some(Input::Connection(generation, event)) if generation == self.generation => {
                    actions.extend(match event {
                        Event::Frame(frame) => self.client.frame(frame),
                        Event::Closed => {
                            self.connection = None;
                            self.client.lost()
                        }
                    })
                }
                Some(Input::Connection(..)) => {}
                Some(Input::Close) => {
                    let actions = self.client.close();
                    self.apply(actions);
                    if let Some(connection) = self.connection.take() {
                        let _ = connection.send(Command::Close);
                    }
                    return;
                }
            }
            self.apply(actions);
        }
    }
    fn apply(&mut self, actions: Vec<ClientAction>) {
        let mut queue = std::collections::VecDeque::from(actions);
        while let Some(action) = queue.pop_front() {
            match action {
                ClientAction::Connect => match self.tls.connect(&self.endpoint) {
                    Ok(tls) => {
                        self.generation += 1;
                        let generation = self.generation;
                        let (commands, connection) = crate::transport::queue();
                        let inputs = self.inputs.clone();
                        let wire = self.wire.clone();
                        let spawned = std::thread::Builder::new()
                            .name("runner connection".into())
                            .spawn(move || {
                                crate::transport::serve(tls, wire, connection, |event| {
                                    inputs.send(Input::Connection(generation, event)).is_ok()
                                })
                            });
                        if spawned.is_ok() {
                            self.connection = Some(commands);
                            queue.extend(self.client.connected());
                        } else {
                            queue.extend(self.client.lost());
                        }
                    }
                    Err(_) => queue.extend(self.client.lost()),
                },
                ClientAction::Send(frame) => {
                    if let Some(connection) = &self.connection {
                        let _ = connection.send(Command::Send(frame));
                    }
                }
                ClientAction::Disconnect => {
                    if let Some(connection) = self.connection.take() {
                        let _ = connection.send(Command::Close);
                    }
                }
                ClientAction::Complete(token, completion) => {
                    self.completions.lock().unwrap().push((token, completion));
                    (self.notify)();
                }
            }
        }
    }
}
