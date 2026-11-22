//! Language adapters for Unix Net. Path and handle-state checks precede issue.
use crate::{
    Context,
    entry::{hirsel_core_capability_handle, hirsel_core_capability_provider},
    net::{self, DENIED, Rules},
    provider::*,
};
use hirsel_core_net::{GrantRule, Sockets, UnixDirection, UnixRule, valid_relative_path};
use std::{os::fd::OwnedFd, ptr};

// This is a typed extension, not a raw path byte range.
pub(crate) struct UnixRequest<'a> {
    pub os: &'static crate::os_unix::Operations,
    pub root: &'a str,
    pub path: &'a str,
    pub backlog: u16,
    pub authorized: bool,
    pub until: Option<(Provider, u64, u64)>,
    pub sockets: *mut Sockets,
    pub connecting: Option<u64>,
    pub target: Option<OwnedFd>,
    pub retry: Option<u64>,
}

#[derive(Clone, Copy)]
pub(crate) struct Datagram {
    rules: u64,
    provider: Provider,
    bound: bool,
    connected: bool,
}

fn direction(op: u8) -> Option<UnixDirection> {
    match op {
        UNIX_CONNECT => Some(UnixDirection::Connect),
        UNIX_LISTEN => Some(UnixDirection::Listen),
        DATAGRAM_BIND => Some(UnixDirection::Bind),
        DATAGRAM_CONNECT | DATAGRAM_SEND_TO => Some(UnixDirection::Send),
        _ => None,
    }
}

unsafe fn str_operand<'a>(carrier: *const Buffer) -> Option<&'a str> {
    if carrier.is_null() {
        return Some("");
    }
    // SAFETY: emitter pins the initialized contents of the carrier.
    std::str::from_utf8(unsafe { crate::tls::bytes(&*carrier) }).ok()
}

pub(crate) unsafe fn permits(rules: u64, direction: UnixDirection, root: &str, path: &str) -> bool {
    // SAFETY: the run pins the resolved Net grant and all its row text.
    unsafe {
        (*(rules as *const Rules)).permits(|row| {
            row.address.dir == 4
                && matches!(row.decode(), Some(GrantRule::Unix(rule)) if rule.permits(direction, root, path))
        })
    }
}

#[allow(clippy::too_many_arguments)]
unsafe fn call(
    context: *mut Context,
    operation: u8,
    handle: u64,
    root: *const Buffer,
    path: *const Buffer,
    operand: *mut Buffer,
    backlog: u16,
    until_kind: u8,
    clock: u64,
    at: u64,
    site: u64,
) -> Outcome {
    // SAFETY: the entry pins all typed carriers and their extents through delivery.
    unsafe {
        crate::process::deny_describe_capability(context);
        let files = (*context).host_files;
        let (Some(root), Some(path)) = (str_operand(root), str_operand(path)) else {
            return DENIED;
        };
        let new_datagram = matches!(operation, DATAGRAM_OPEN | DATAGRAM_BIND);
        let new_handle =
            new_datagram || matches!(operation, UNIX_CONNECT | UNIX_LISTEN | UNIX_ACCEPT);
        let (rules, provider, key, state_ok) = match operation {
            UNIX_CONNECT | UNIX_LISTEN | DATAGRAM_OPEN | DATAGRAM_BIND => {
                let provider = *hirsel_core_capability_provider(context, handle);
                let key = hirsel_core_capability_handle(context, handle);
                #[cfg(feature = "record")]
                let rules = if crate::tape::owns(&provider) {
                    crate::tape::net_rules(&provider, key)
                } else {
                    key
                };
                #[cfg(not(feature = "record"))]
                let rules = key;
                (rules, provider, key, true)
            }
            UNIX_ACCEPT => (0, net::socket_provider(context, handle), handle, true),
            _ => {
                let _commit = crate::sequencer::enter();
                let state = (&(*files).datagrams)[&handle];
                let state_ok = (operation != DATAGRAM_RECV || state.bound)
                    && (operation != DATAGRAM_SEND || state.connected);
                (state.rules, state.provider, handle, state_ok)
            }
        };
        let served = crate::provide::owns(&provider);
        let authorized = !served
            && state_ok
            && direction(operation).is_none_or(|dir| {
                !root.is_empty()
                    && path.len() <= crate::os_unix::max_path()
                    && valid_relative_path(path.as_bytes())
                    && permits(rules, dir, root, path)
            });
        // Until the Unix issue and delivery codecs are installed, refuse
        // before the tape can append or replay an incomplete operation.
        #[cfg(feature = "record")]
        if authorized && (crate::tape::owns(&provider) || (!served && (*files).net_tape.is_some()))
        {
            crate::process::stop(b"UnixRecording");
        }
        let mut unix = UnixRequest {
            os: &crate::os_unix::OPERATIONS,
            root,
            path,
            backlog,
            authorized,
            until: net::deadline(context, until_kind, clock, at),
            sockets: &raw mut (*files).sockets,
            connecting: None,
            target: None,
            retry: None,
        };
        let mut request = Request {
            operation,
            mode: 0,
            handle: key,
            path: (&raw mut unix).cast(),
            path_len: 0,
            buffer: if operand.is_null() {
                net::empty_buffer()
            } else {
                *operand
            },
            site,
        };
        #[cfg(feature = "record")]
        let provider = if let Some(tape) = (*files).net_tape.filter(|_| !served)
            && !matches!(
                operation,
                UNIX_CONNECT | UNIX_LISTEN | DATAGRAM_OPEN | DATAGRAM_BIND
            ) {
            request.handle = tape.handle(key);
            tape.provider
        } else {
            provider
        };
        let mut outcome = hirsel_core_host(context, &provider, &mut request);
        if outcome.status == 0 && outcome.error == IoError::OK {
            #[cfg(feature = "record")]
            if new_handle && let Some(tape) = (*files).net_tape.filter(|_| !served) {
                outcome = tape.carrier(outcome);
            }
            let _commit = crate::sequencer::enter();
            if new_datagram {
                (*files).datagrams.insert(
                    outcome.value,
                    Datagram {
                        rules,
                        provider,
                        bound: operation == DATAGRAM_BIND,
                        connected: false,
                    },
                );
            } else if operation == DATAGRAM_CONNECT {
                (*files).datagrams.get_mut(&handle).unwrap().connected = true;
            } else if operation == DATAGRAM_RECV {
                (*operand).len = request.buffer.len;
                outcome.written = u64::from(outcome.kind);
            }
            if new_handle && served {
                (*files).unix_served.insert(outcome.value, provider);
            }
        }
        outcome
    }
}

/// # Safety
/// Live run and typed Net with borrowed UTF-8 operands.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_net_narrow_unix(
    context: *mut Context,
    net: u64,
    direction: u8,
    root: *const Buffer,
    path: *const Buffer,
    _site: u64,
) -> Outcome {
    // SAFETY: caller pins both carriers through this call.
    unsafe {
        let Some(rule) = UnixDirection::from_tag(direction)
            .and_then(|direction| UnixRule::new(direction, str_operand(root)?, str_operand(path)?))
        else {
            return DENIED;
        };
        net::narrow(context, net, GrantRule::Unix(rule))
    }
}

// The emitter ABI gives each operation only its own operands.
macro_rules! adapter {
    ($name:ident ($($arg:ident : $ty:ty),* $(,)?) => $op:expr, $root:expr, $path:expr, $bytes:expr, $backlog:expr, [$kind:expr, $clock:expr, $at:expr]) => {
        /// # Safety
        /// Live run, typed Net or owned handle, and pinned carrier operands.
        #[unsafe(no_mangle)]
        #[allow(clippy::too_many_arguments)]
        pub unsafe extern "C" fn $name(
            context: *mut Context, handle: u64, $($arg: $ty,)* site: u64
        ) -> Outcome {
            // SAFETY: forwarded from the typed emitter ABI.
            unsafe { call(context, $op, handle, $root, $path, $bytes, $backlog, $kind, $clock, $at, site) }
        }
    };
}

adapter!(hirsel_core_host_connect_unix(root: *const Buffer, path: *const Buffer, kind: u8, clock: u64, at: u64)
    => UNIX_CONNECT, root, path, ptr::null_mut(), 0, [kind, clock, at]);
adapter!(hirsel_core_host_listen_unix(root: *const Buffer, path: *const Buffer, backlog: u16)
    => UNIX_LISTEN, root, path, ptr::null_mut(), backlog, [0, 0, 0]);
adapter!(hirsel_core_host_accept_unix(kind: u8, clock: u64, at: u64)
    => UNIX_ACCEPT, ptr::null(), ptr::null(), ptr::null_mut(), 0, [kind, clock, at]);
adapter!(hirsel_core_host_open_datagram()
    => DATAGRAM_OPEN, ptr::null(), ptr::null(), ptr::null_mut(), 0, [0, 0, 0]);
adapter!(hirsel_core_host_bind_datagram(root: *const Buffer, path: *const Buffer)
    => DATAGRAM_BIND, root, path, ptr::null_mut(), 0, [0, 0, 0]);
adapter!(hirsel_core_host_connect_datagram(root: *const Buffer, path: *const Buffer)
    => DATAGRAM_CONNECT, root, path, ptr::null_mut(), 0, [0, 0, 0]);
adapter!(hirsel_core_host_send_datagram(bytes: *const Buffer, kind: u8, clock: u64, at: u64)
    => DATAGRAM_SEND, ptr::null(), ptr::null(), bytes.cast_mut(), 0, [kind, clock, at]);
adapter!(hirsel_core_host_send_to(root: *const Buffer, path: *const Buffer, bytes: *const Buffer, kind: u8, clock: u64, at: u64)
    => DATAGRAM_SEND_TO, root, path, bytes.cast_mut(), 0, [kind, clock, at]);
adapter!(hirsel_core_host_recv_datagram(bytes: *mut Buffer, kind: u8, clock: u64, at: u64)
    => DATAGRAM_RECV, ptr::null(), ptr::null(), bytes, 0, [kind, clock, at]);
