//! Language adapters for Unix Net. Path and handle-state checks precede issue.
use crate::{
    Context,
    entry::{hirsel_core_capability_handle, hirsel_core_capability_provider},
    net::{self, DENIED, Rules},
    provider::*,
};
use hirsel_core_net::{GrantRule, Sockets, UnixDirection, UnixRule, valid_relative_path};
use std::ptr;

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
    pub target: Option<crate::host::UnixEndpoint>,
    pub retry: Option<u64>,
}

pub(crate) const SERVED_UNIX_LISTEN: u64 = 10;
pub(crate) const SERVED_DATAGRAM_RECV: u64 = 17;

#[derive(Clone, Copy)]
pub(crate) enum UnixField {
    Listener,
    Datagram,
    Root(UnixDirection),
    Path,
    Backlog,
    Bytes,
    Room,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum UnixHandle {
    Stream,
    Listener,
    Datagram,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum UnixReply {
    Created(UnixHandle),
    Done,
    Packet,
}

#[derive(Clone, Copy)]
pub(crate) struct UnixOperation {
    pub served: u64,
    pub fields: &'static [UnixField],
    pub reply: UnixReply,
}

impl UnixOperation {
    pub(crate) fn describe(operation: u8) -> Option<Self> {
        use UnixDirection::{Bind, Connect, Listen, Send};
        use UnixField::*;
        use UnixReply::{Created, Done, Packet};
        // Served NetOp tags and fields follow their declaration in core.md.
        let (served, fields, reply): (_, &[_], _) = match operation {
            UNIX_CONNECT => (9, &[Root(Connect), Path], Created(UnixHandle::Stream)),
            UNIX_LISTEN => (
                SERVED_UNIX_LISTEN,
                &[Root(Listen), Path, Backlog],
                Created(UnixHandle::Listener),
            ),
            UNIX_ACCEPT => (11, &[Listener], Created(UnixHandle::Stream)),
            DATAGRAM_OPEN => (12, &[], Created(UnixHandle::Datagram)),
            DATAGRAM_BIND => (13, &[Root(Bind), Path], Created(UnixHandle::Datagram)),
            DATAGRAM_CONNECT => (14, &[Datagram, Root(Send), Path], Done),
            DATAGRAM_SEND => (15, &[Datagram, Bytes], Done),
            DATAGRAM_SEND_TO => (16, &[Datagram, Root(Send), Path, Bytes], Done),
            DATAGRAM_RECV => (SERVED_DATAGRAM_RECV, &[Datagram, Room], Packet),
            _ => return None,
        };
        Some(Self {
            served,
            fields,
            reply,
        })
    }

    fn direction(self) -> Option<UnixDirection> {
        self.fields.iter().find_map(|field| match *field {
            UnixField::Root(direction) => Some(direction),
            _ => None,
        })
    }
}

#[derive(Clone, Copy)]
pub(crate) struct Datagram {
    rules: u64,
    provider: Provider,
    bound: bool,
    connected: bool,
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
        Rules::from_handle(rules).permits(
            |row| matches!(row, GrantRule::Unix(rule) if rule.permits(direction, root, path)),
        )
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
        let description = UnixOperation::describe(operation).unwrap();
        let target = description.fields.first();
        let (rules, provider, key, state_ok) = match target {
            Some(UnixField::Listener) => (0, net::socket_provider(context, handle), handle, true),
            Some(UnixField::Datagram) => {
                let state = (&(*files).datagrams)[&handle];
                let state_ok = (operation != DATAGRAM_RECV || state.bound)
                    && (operation != DATAGRAM_SEND || state.connected);
                (state.rules, state.provider, handle, state_ok)
            }
            _ => {
                let provider = *hirsel_core_capability_provider(context, handle);
                let key = hirsel_core_capability_handle(context, handle);
                if !crate::provide::grant_live(&provider, key) {
                    return crate::provide::withdrawn(context, site);
                }
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
        };
        let served = crate::provide::owns(&provider);
        if matches!(target, Some(UnixField::Listener | UnixField::Datagram))
            && !crate::provide::socket_live(&provider, key)
        {
            return crate::provide::withdrawn(context, site);
        }
        let authorized = state_ok
            && description.direction().is_none_or(|dir| {
                !root.is_empty()
                    && path.len() <= hirsel_core_net::unix::MAX_PATH_BYTES
                    && valid_relative_path(path.as_bytes())
                    && permits(rules, dir, root, path)
            });
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
            && matches!(target, Some(UnixField::Listener | UnixField::Datagram))
        {
            request.handle = tape.handle(key);
            tape.provider
        } else {
            provider
        };
        let mut outcome = hirsel_core_host(context, &provider, &mut request);
        if outcome.status == 0 && outcome.error == IoError::OK {
            let new_handle = matches!(description.reply, UnixReply::Created(_));
            #[cfg(feature = "record")]
            if new_handle && let Some(tape) = (*files).net_tape.filter(|_| !served) {
                outcome = tape.carrier(outcome);
            }
            if description.reply == UnixReply::Created(UnixHandle::Datagram) {
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
            } else if description.reply == UnixReply::Packet {
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
/// Live list narrow `builder` from `hirsel_core_net_narrow_begin`, with
/// borrowed UTF-8 operands.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_net_narrow_unix(
    _context: *mut Context,
    builder: u64,
    direction: u8,
    root: *const Buffer,
    path: *const Buffer,
    _site: u64,
) -> Outcome {
    // SAFETY: caller pins both carriers through this call.
    unsafe {
        let rule = UnixDirection::from_tag(direction)
            .and_then(|direction| UnixRule::new(direction, str_operand(root)?, str_operand(path)?));
        net::narrow_add(builder, rule.map(GrantRule::Unix))
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
