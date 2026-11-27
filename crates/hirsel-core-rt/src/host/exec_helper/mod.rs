//! Exec custody. A standalone run supervises its children in its own process.
//! An embedded run never changes its host's process state, so it supervises
//! them in a helper process, the shipped `hirsel-exec-helper` executable
//! (EMB-3 "Exec in an embedded launch"): the same Supervisor, reached over a
//! socket.
mod client;
mod serve;
mod spawn;
mod wire;

use crate::host::exec::{Process, Signal, SpawnError, SpawnInput, Supervisor, Termination};
pub(crate) use client::Helper;
pub use serve::main;
use std::io;
use std::os::fd::OwnedFd;
use std::sync::Arc;

/// Who owns an Exec grant's children.
#[derive(Clone)]
pub(crate) enum Custody {
    /// The run's own Supervisor (standalone).
    Local(Arc<Supervisor>),
    /// The embedder's helper process.
    Helper(Arc<Helper>),
}

pub(crate) struct Spawned {
    pub(crate) child: Child,
    pub(crate) stdin: Option<OwnedFd>,
    pub(crate) stdout: Option<OwnedFd>,
    pub(crate) stderr: Option<OwnedFd>,
}

impl Custody {
    pub(crate) fn spawn(&self, input: SpawnInput) -> Result<Spawned, SpawnError> {
        match self {
            Self::Local(supervisor) => {
                let started = supervisor.spawn(input)?;
                Ok(Spawned {
                    child: Child::Local(started.process),
                    stdin: started.stdin,
                    stdout: started.stdout,
                    stderr: started.stderr,
                })
            }
            Self::Helper(helper) => {
                let ([stdin, stdout, stderr], child) = helper.spawn(input)?;
                Ok(Spawned {
                    child: Child::Helper(child),
                    stdin,
                    stdout,
                    stderr,
                })
            }
        }
    }
}

/// One direct child of either custody. Drop releases it: the child is killed
/// and reaped by its supervisor.
pub(crate) enum Child {
    Local(Process),
    Helper(client::Proc),
}

impl Child {
    pub(crate) fn wait_or_park(&mut self, token: Option<u64>) -> io::Result<Option<Termination>> {
        match self {
            Self::Local(process) => process.wait_or_park(token),
            Self::Helper(proc) => proc.wait_or_park(token),
        }
    }

    pub(crate) fn withdraw_wait(&self) {
        match self {
            Self::Local(process) => process.withdraw_wait(),
            Self::Helper(proc) => proc.withdraw_wait(),
        }
    }

    pub(crate) fn signal(&mut self, signal: Signal) -> io::Result<()> {
        match self {
            Self::Local(process) => process.signal(signal),
            Self::Helper(proc) => proc.signal(signal),
        }
    }
}

fn signal_code(signal: Signal) -> u8 {
    match signal {
        Signal::Int => 0,
        Signal::Term => 1,
        Signal::Kill => 2,
    }
}

fn signal_of(code: u8) -> Option<Signal> {
    match code {
        0 => Some(Signal::Int),
        1 => Some(Signal::Term),
        2 => Some(Signal::Kill),
        _ => None,
    }
}

/// The launch's Exec grants. Their children live in one helper process,
/// started now from the `exec_helper` image the embedder named, so the
/// helper's startup refusal is a start refusal, and the host's own process
/// state is never changed (EMB-3).
pub(crate) fn embed_grants(
    requests: &[hirsel_core_net::exec::Request],
    exec_helper: &[u8],
    start_timeout_ms: u32,
    generation: &Arc<crate::run_stop::Execution>,
) -> Result<Box<[crate::exec::Grant]>, String> {
    if requests.is_empty() {
        return Ok(Box::default());
    }
    if exec_helper.is_empty() {
        return Err("ExecHelper(not supplied)".into());
    }
    let image = match std::ffi::CString::new(exec_helper) {
        Ok(image) if exec_helper.starts_with(b"/") => image,
        _ => {
            return Err(format!(
                "ExecHelper(not an absolute path: {})",
                String::from_utf8_lossy(exec_helper)
            ));
        }
    };
    let policies = requests
        .iter()
        .map(|request| request.load())
        .collect::<Result<Vec<_>, _>>()?;
    let helper = Helper::start(
        &image,
        std::time::Duration::from_millis(start_timeout_ms.into()),
        generation.clone(),
    )?;
    policies
        .into_iter()
        .map(|policy| {
            crate::exec::Grant::prepare(policy, Custody::Helper(helper.clone()))
                .map(|(grant, _)| grant)
        })
        .collect()
}
