//! Native selection of the same network model used by the evaluator.
use crate::{
    net::{Address, NetworkRequest},
    provider::*,
    tls::{HandshakeState, TlsRequest},
};
use hirsel_core_net::model::{Model, Operation, Reply};
use std::sync::Arc;

/// Pin this adapter and its fallback through the run, like OsProvider. Select
/// the returned Provider for both Net and Clock, and as the run's provider for
/// operations through owned socket handles. Other host operations use fallback.
pub struct ModelProvider {
    model: Arc<Model>,
    fallback: Provider,
}
impl ModelProvider {
    pub fn new(model: Model, fallback: Provider) -> Self {
        Self {
            model: Arc::new(model),
            fallback,
        }
    }
    pub fn provider(&mut self) -> Provider {
        Provider {
            state: (self as *mut Self).cast(),
            wait,
            issue,
            resume,
            abort,
            deliver,
        }
    }
}
impl crate::tasks::ModelClock for Model {
    fn now(&self) -> u64 {
        Model::now(self)
    }
    fn poll(&self, deadline: Option<u64>, idle: bool) -> Vec<u64> {
        Model::poll(self, deadline, idle)
    }
}
fn modeled(op: u8) -> bool {
    matches!(
        op,
        CONNECT
            | LISTEN
            | ACCEPT
            | RECV_BOUNDED
            | SEND_PAIR
            | TLS_CONNECT
            | SEND_SECRET
            | MONO_NS
            | WALL_MS
            | RESOLVE
            | SLEEP_UNTIL
    )
}
/// # Safety
/// Pinned ModelProvider and a live borrowed request with the provider ABI layout.
unsafe extern "C" fn wait(state: *mut u8, request: *mut Request, token: u64) -> Wait {
    unsafe {
        let p = &*state.cast::<ModelProvider>();
        let r = &*request;
        if !modeled(r.operation) {
            return (p.fallback.wait)(p.fallback.state, request, token);
        }
        crate::tasks::attach_model(p.model.clone());
        if r.operation == SLEEP_UNTIL && r.buffer.len >= 8 {
            let at = r.buffer.data.cast::<u64>().read_unaligned();
            if at > p.model.now() {
                return Wait {
                    kind: WAIT_UNTIL,
                    deadline_ns: at,
                };
            }
        }
        Wait::default()
    }
}
fn outcome(reply: &Reply) -> Outcome {
    Outcome {
        value: reply.value,
        error: IoError {
            tag: reply.error,
            code: reply.code,
        },
        ..Outcome::default()
    }
}
/// # Safety
/// As wait; network requests pin NetworkRequest and their caller-owned buffer.
unsafe extern "C" fn issue(state: *mut u8, request: *mut Request, token: u64) -> Step {
    unsafe {
        let p = &*state.cast::<ModelProvider>();
        let r = &mut *request;
        if !modeled(r.operation) {
            return (p.fallback.issue)(p.fallback.state, request, token);
        }
        crate::tasks::attach_model(p.model.clone());
        if matches!(r.operation, MONO_NS | WALL_MS | SLEEP_UNTIL) {
            return Step {
                outcome: Outcome {
                    value: if r.operation == SLEEP_UNTIL {
                        0
                    } else {
                        p.model.clock_read()
                    },
                    ..Outcome::default()
                },
                ..Step::default()
            };
        }
        if r.operation == RESOLVE {
            let resolve = &*r.path.cast::<crate::resolve::ResolveRequest>();
            let outcome = if resolve
                .network
                .until
                .is_some_and(|(_, _, at)| at <= p.model.now())
            {
                crate::os_net::signal(6)
            } else {
                let addresses = p.model.resolve(
                    resolve.name.as_ref().unwrap(),
                    crate::resolve::spare(&r.buffer),
                );
                crate::resolve::append(&mut r.buffer, &addresses)
            };
            return Step {
                outcome,
                ..Step::default()
            };
        }
        if r.operation == TLS_CONNECT {
            return tls_step(&p.model, r, token);
        }
        if r.operation == SEND_SECRET {
            return secret_step(&p.model, r, token);
        }
        let n = &mut *r.path.cast_mut().cast::<NetworkRequest>();
        let address = hirsel_core_net::model::Address {
            family: n.address.family,
            hi: n.address.hi,
            lo: n.address.lo,
            port: n.address.port,
        };
        let operation = match r.operation {
            CONNECT => Operation::Connect(address),
            LISTEN => Operation::Listen(address, n.backlog),
            ACCEPT => Operation::Accept(r.handle),
            RECV_BOUNDED => Operation::Recv(r.handle),
            SEND_PAIR => Operation::Send(r.handle),
            _ => unreachable!(),
        };
        let input = if r.operation == SEND_PAIR {
            // SAFETY: the send adapter pins both initialized prefixes.
            [&r.buffer, &*n.send_tail].map(|part| {
                if part.len == 0 {
                    &[][..]
                } else {
                    std::slice::from_raw_parts(part.data, part.len)
                }
            })
        } else {
            [&[][..]; 2]
        };
        if let Some(reply) = p.model.attempt_parts(
            operation,
            input,
            r.buffer.capacity.saturating_sub(r.buffer.len),
            token,
        ) {
            if reply.error == 0 {
                if matches!(r.operation, CONNECT | LISTEN | ACCEPT) {
                    (*n.sockets).adopt_model(&p.model, reply.value);
                }
                n.address = Address {
                    family: reply.address.family,
                    hi: reply.address.hi,
                    lo: reply.address.lo,
                    port: reply.address.port,
                };
                if r.operation == RECV_BOUNDED && !reply.bytes.is_empty() {
                    std::ptr::copy_nonoverlapping(
                        reply.bytes.as_ptr(),
                        r.buffer.data.add(r.buffer.len),
                        reply.bytes.len(),
                    );
                    r.buffer.len += reply.bytes.len();
                }
            }
            return Step {
                outcome: outcome(&reply),
                ..Step::default()
            };
        }
        let deadline = n.until.map(|(_, _, at)| at);
        if deadline.is_some_and(|at| at <= p.model.now()) {
            return Step {
                outcome: outcome(&p.model.abort(token, false)),
                ..Step::default()
            };
        }
        pending(deadline)
    }
}

fn pending(deadline: Option<u64>) -> Step {
    Step {
        kind: STEP_PENDING,
        deadline_ns: deadline.unwrap_or(0),
        ..Step::default()
    }
}

unsafe fn secret_step(model: &Model, request: &Request, token: u64) -> Step {
    // SAFETY: SEND_SECRET pins the three operands and progress through delivery.
    unsafe {
        let send = &mut *request.path.cast_mut().cast::<crate::secret::SendRequest>();
        let parts = [&request.buffer, &send.secret, &*send.network.send_tail]
            .map(|part| crate::tls::bytes(part));
        let deadline = send.network.until.map(|(_, _, at)| at);
        let reply = if let Some(reply) =
            model.attempt_secret(request.handle, parts, &mut send.committed, token)
        {
            reply
        } else if deadline.is_some_and(|at| at <= model.now()) {
            model.abort(token, false)
        } else {
            return pending(deadline);
        };
        let mut out = outcome(&reply);
        if reply.error != 0 {
            out.written = send.committed as u64;
        }
        Step {
            outcome: out,
            ..Step::default()
        }
    }
}

unsafe fn tls_step(model: &Model, request: &Request, token: u64) -> Step {
    // SAFETY: both adapters keep the extension, offer and output buffers live.
    unsafe {
        let tls = &mut *request.path.cast_mut().cast::<TlsRequest>();
        let state = &mut *tls.state.cast::<HandshakeState>();
        let key = request.handle;
        let deadline = tls.network.until.map(|(_, _, at)| at);
        let reply = if let Some(reply) = model.attempt_tls(key, &state.alpn, token) {
            reply
        } else if deadline.is_some_and(|at| at <= model.now()) {
            model.abort(token, false)
        } else {
            return pending(deadline);
        };
        if reply.error == 0 {
            let protocol = &mut *tls.protocol_out;
            if !reply.bytes.is_empty() {
                std::ptr::copy_nonoverlapping(
                    reply.bytes.as_ptr(),
                    protocol.data,
                    reply.bytes.len(),
                );
            }
            protocol.len = reply.bytes.len();
            *tls.peer_out = reply.peer;
            (*tls.network.sockets).split(key);
        } else {
            tls_release(tls, key);
        }
        Step {
            outcome: outcome(&reply),
            ..Step::default()
        }
    }
}

unsafe fn tls_release(tls: &mut TlsRequest, key: u64) {
    // SAFETY: the consuming request still owns the unsplit socket.
    unsafe {
        (*tls.network.sockets).release(key);
        (*tls.state.cast::<HandshakeState>()).consumed = true;
    }
}

unsafe extern "C" fn resume(state: *mut u8, request: *mut Request, token: u64) -> Step {
    unsafe {
        let p = &*state.cast::<ModelProvider>();
        if modeled((*request).operation) {
            issue(state, request, token)
        } else {
            (p.fallback.resume)(p.fallback.state, request, token)
        }
    }
}
/// # Safety
/// As issue, for an operation that previously answered Pending.
unsafe extern "C" fn abort(
    state: *mut u8,
    request: *mut Request,
    token: u64,
    reason: u8,
) -> Outcome {
    unsafe {
        let p = &*state.cast::<ModelProvider>();
        if !modeled((*request).operation) {
            return (p.fallback.abort)(p.fallback.state, request, token, reason);
        }
        if reason == ABORT_DEADLINE {
            let step = issue(state, request, token);
            if step.kind == STEP_DONE {
                return step.outcome;
            }
        }
        let reply = p.model.abort(token, reason == ABORT_CANCEL);
        let r = &*request;
        if r.operation == TLS_CONNECT {
            tls_release(&mut *r.path.cast_mut().cast::<TlsRequest>(), r.handle);
        }
        let mut out = outcome(&reply);
        if r.operation == SEND_SECRET {
            out.written = (*r.path.cast::<crate::secret::SendRequest>()).committed as u64;
        }
        out
    }
}
/// # Safety
/// As issue; outcome is the result committed by this provider's attempt.
unsafe extern "C" fn deliver(state: *mut u8, request: *mut Request, result: Outcome) -> Outcome {
    unsafe {
        let p = &*state.cast::<ModelProvider>();
        if modeled((*request).operation) {
            result
        } else {
            (p.fallback.deliver)(p.fallback.state, request, result)
        }
    }
}

#[cfg(test)]
mod tests;
