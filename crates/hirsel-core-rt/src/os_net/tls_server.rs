//! TLS-9's two server steps on the OS provider: `TLS_HELLO` reads the
//! ClientHello through rustls's `Acceptor`, and `TLS_ACCEPT` completes the
//! handshake the program resumed with its chain and key.
use super::*;
use crate::{
    os_tls::server,
    tls_server::{AcceptState, HelloRequest, HelloState, TlsAcceptRequest},
};

pub(crate) static HELLO_OPERATIONS: Operations = Operations {
    step: hello_step,
    abort: hello_abort,
    adopt_listener,
};

pub(crate) static ACCEPT_OPERATIONS: Operations = Operations {
    step: accept_step,
    abort: accept_abort,
    adopt_listener,
};

fn done(outcome: Outcome) -> Step {
    Step {
        outcome,
        ..Step::default()
    }
}

/// Wait for the socket in one direction, or time out.
unsafe fn wait(n: &mut NetworkRequest, key: u64, write: bool, token: u64) -> Result<Step, Outcome> {
    unsafe {
        let due = deadline(n.until, token);
        if due == Due::Passed {
            return Err(signal(6));
        }
        let sockets = &mut *n.sockets;
        if let Err(e) = sockets.arm(key, write, token) {
            return Err(error(e.raw_os_error().unwrap_or(libc::EIO)));
        }
        crate::tasks::attach_net(n.sockets, sockets.poller().unwrap());
        Ok(Step {
            kind: STEP_PENDING,
            deadline_ns: due.host().unwrap_or(0),
            ..Step::default()
        })
    }
}

unsafe fn hello_step(request: *mut Request, token: u64, first: bool) -> Step {
    unsafe {
        let request = &*request;
        let hello = &mut *request.path.cast_mut().cast::<HelloRequest>();
        let key = request.handle;
        let state = &mut *hello.state.cast::<HelloState>();
        let sockets = &mut *hello.network.sockets;
        if first {
            state.acceptor = Some(rustls::server::Acceptor::default());
        } else {
            sockets.disarm(key, false);
        }
        let acceptor = state.acceptor.as_mut().unwrap();
        match server::read_hello(acceptor, sockets.fd(key)) {
            Ok(Some(accepted)) => {
                state.acceptor = None;
                let out = &mut *hello.name_out;
                let mut name = [0; 253];
                let Some(len) = server::server_name(&accepted, &mut name) else {
                    server::send_alert(sockets.fd(key), &server::INTERNAL_ERROR);
                    return hello_finish(hello, key, crate::net::tls_failure(4, 0));
                };
                ptr::copy_nonoverlapping(name.as_ptr(), out.data, len.min(out.capacity));
                out.len = len.min(out.capacity);
                state.accepted = Some(Box::new(accepted));
                // The Hello keeps the socket's key; no host value is recorded.
                done(Outcome::default())
            }
            Ok(None) => match wait(&mut hello.network, key, false, token) {
                Ok(step) => step,
                Err(outcome) => hello_finish(hello, key, outcome),
            },
            Err(outcome) => hello_finish(hello, key, outcome),
        }
    }
}

unsafe fn hello_finish(hello: &mut HelloRequest, key: u64, outcome: Outcome) -> Step {
    unsafe {
        let state = &mut *hello.state.cast::<HelloState>();
        state.acceptor = None;
        (*hello.network.sockets).release(key);
        state.consumed = true;
    }
    done(outcome)
}

unsafe fn hello_abort(request: *mut Request, _: u64, reason: u8) -> Outcome {
    unsafe {
        let r = &*request;
        let hello = &mut *r.path.cast_mut().cast::<HelloRequest>();
        let reason = signal(if reason == ABORT_CANCEL { 7 } else { 6 });
        hello_finish(hello, r.handle, reason).outcome
    }
}

unsafe fn accept_step(request: *mut Request, token: u64, first: bool) -> Step {
    unsafe {
        let request = &*request;
        let tls = &mut *request.path.cast_mut().cast::<TlsAcceptRequest>();
        let key = request.handle;
        let state = &mut *tls.state.cast::<AcceptState>();
        let sockets = &mut *tls.network.sockets;
        let fd = sockets.fd(key);
        if first {
            // A Hello this provider did not read has no handshake to resume.
            let Some(accepted) = state.accepted.take() else {
                return accept_finish(tls, key, crate::net::tls_failure(4, 0));
            };
            let Some(document) = state.key.bytes.clone() else {
                server::send_alert(fd, &server::INTERNAL_ERROR);
                return accept_finish(tls, key, crate::net::tls_failure(8, 0));
            };
            let chain = std::mem::take(&mut state.chain);
            let alpn = std::mem::take(&mut state.alpn);
            let started = crate::units::off_slot(move || {
                server::start(fd, *accepted, chain, &document[..], alpn)
            });
            match started {
                Ok(handshake) => state.handshake = Some(handshake),
                Err(outcome) => return accept_finish(tls, key, outcome),
            }
        } else {
            sockets.disarm(key, state.pending_write);
        }
        let stream = &mut state.handshake.as_mut().unwrap().stream;
        match stream.handshake(fd) {
            Ok(Ok(())) => {
                let stream = state.handshake.take().unwrap().finish();
                *tls.protocol_out = stream
                    .connection
                    .alpn_protocol()
                    .and_then(|chosen| tls.index_of(chosen))
                    .unwrap_or(u64::MAX);
                (*tls.network.tls).insert(key, Box::new(stream));
                sockets.split(key);
                done(Outcome {
                    value: key,
                    ..Outcome::default()
                })
            }
            Ok(Err(write)) => {
                state.pending_write = write;
                match wait(&mut tls.network, key, write, token) {
                    Ok(step) => step,
                    Err(outcome) => accept_finish(tls, key, outcome),
                }
            }
            Err(outcome) => accept_finish(tls, key, outcome),
        }
    }
}

unsafe fn accept_finish(tls: &mut TlsAcceptRequest, key: u64, outcome: Outcome) -> Step {
    unsafe {
        let state = &mut *tls.state.cast::<AcceptState>();
        drop(state.handshake.take());
        drop(state.accepted.take());
        (*tls.network.sockets).release(key);
        state.consumed = true;
    }
    done(outcome)
}

unsafe fn accept_abort(request: *mut Request, _: u64, reason: u8) -> Outcome {
    unsafe {
        let r = &*request;
        let tls = &mut *r.path.cast_mut().cast::<TlsAcceptRequest>();
        let reason = signal(if reason == ABORT_CANCEL { 7 } else { 6 });
        accept_finish(tls, r.handle, reason).outcome
    }
}
