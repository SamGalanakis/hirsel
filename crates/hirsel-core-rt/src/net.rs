//! Network operand adapters. Authority checks precede issue; buffers stay caller-owned.
use crate::{
    Context,
    entry::{hirsel_core_capability_handle, hirsel_core_capability_provider},
    provider::*,
};
use hirsel_core_net::Rule;
use std::ptr;

#[repr(C)]
pub struct Rules {
    pub data: *const Rule,
    pub len: usize,
}
impl Rules {
    /// # Safety
    /// The run pins the configured or narrowed immutable rule list.
    pub unsafe fn as_slice(&self) -> &[Rule] {
        if self.len == 0 {
            &[]
        } else {
            unsafe { std::slice::from_raw_parts(self.data, self.len) }
        }
    }
}
pub(crate) struct RulesStorage {
    _storage: Box<[Rule]>,
    view: Box<Rules>,
}

impl RulesStorage {
    pub(crate) fn new(storage: Box<[Rule]>) -> Self {
        let view = Box::new(Rules {
            data: storage.as_ptr(),
            len: storage.len(),
        });
        Self {
            _storage: storage,
            view,
        }
    }

    pub(crate) fn handle(&self) -> u64 {
        (&*self.view as *const Rules) as u64
    }
}

pub(crate) struct Grant {
    _rules: RulesStorage,
    payer: crate::limit::Payer,
    charge: u64,
}
impl Drop for Grant {
    fn drop(&mut self) {
        self.payer.refund(self.charge);
    }
}
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct Address {
    pub family: u8,
    pub hi: u64,
    pub lo: u64,
    pub port: u16,
}
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct AddressOutcome {
    pub value: u64,
    pub error: IoError,
    pub written: u64,
    pub status: u8,
    pub family: u8,
    pub hi: u64,
    pub lo: u64,
    pub port: u16,
}
impl AddressOutcome {
    fn new(o: Outcome, a: Address) -> Self {
        Self {
            value: o.value,
            error: o.error,
            written: o.written,
            status: o.status,
            family: a.family,
            hi: a.hi,
            lo: a.lo,
            port: a.port,
        }
    }
    fn outcome(self) -> Outcome {
        Outcome {
            value: self.value,
            error: self.error,
            written: self.written,
            status: self.status,
        }
    }
}
/// Borrowed through delivery. Only the leading send-tail pointer is visible
/// to external providers, and only for SEND_PAIR.
#[repr(C)]
pub(crate) struct NetworkRequest {
    pub send_tail: *const Buffer,
    pub address: Address,
    pub sockets: *mut hirsel_core_net::Sockets,
    pub until: Option<(Provider, u64, u64)>,
    pub backlog: u16,
    pub connecting: Option<u64>,
    pub denied: bool,
    pub direction: u8,
    pub os: &'static crate::os_net::Operations,
}
/// A CLI run substitutes all Net grants on one tape. A socket carrier
/// keeps the tape's object ordinal and the usual two direction bits.
#[derive(Clone, Copy)]
pub(crate) struct SocketTape {
    provider: Provider,
    generation: u64,
}

impl SocketTape {
    pub(crate) fn new(provider: Provider, handle: u64) -> Self {
        Self {
            provider,
            generation: handle & !u64::from(u32::MAX),
        }
    }

    fn handle(self, key: u64) -> u64 {
        self.generation | (key >> 2)
    }

    fn carrier(self, mut outcome: Outcome) -> Outcome {
        if outcome.status == 0 && outcome.error == IoError::OK {
            outcome.value = u64::from(outcome.value as u32) << 2;
        }
        outcome
    }

    fn source(self, key: u64) -> Option<u64> {
        crate::tape::source(&self.provider, self.handle(key)).map(|(_, handle)| handle | (key & 3))
    }
}

unsafe fn deadline(
    context: *mut Context,
    kind: u8,
    clock: u64,
    at: u64,
) -> Option<(Provider, u64, u64)> {
    (kind != 0).then(|| unsafe {
        (
            *hirsel_core_capability_provider(context, clock),
            hirsel_core_capability_handle(context, clock),
            at,
        )
    })
}
pub(crate) const DENIED: Outcome = Outcome {
    value: 0,
    error: IoError { tag: 1, code: 0 },
    written: 0,
    status: 0,
};

pub(crate) unsafe fn permitted(request: &Request, handle: u64) -> bool {
    // SAFETY: network adapters pin the scalar address and the grant's Rules.
    unsafe {
        let address = &(*request.path.cast::<NetworkRequest>()).address;
        let rules = &*(handle as *const Rules);
        rules.as_slice().iter().any(|r| {
            r.permits(
                u8::from(request.operation == LISTEN),
                address.family,
                address.hi,
                address.lo,
                address.port,
            )
        })
    }
}
pub(crate) fn empty_buffer() -> Buffer {
    Buffer {
        data: ptr::null_mut(),
        len: 0,
        capacity: 0,
    }
}
/// # Safety
/// Live run, typed Net grant, optional Clock designation and source site.
#[allow(clippy::too_many_arguments)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_host_connect(
    context: *mut Context,
    n: u64,
    family: u8,
    hi: u64,
    lo: u64,
    port: u16,
    until_kind: u8,
    clock: u64,
    deadline_ns: u64,
    site: u64,
) -> Outcome {
    unsafe {
        address_op(
            context,
            CONNECT,
            n,
            family,
            hi,
            lo,
            port,
            until_kind,
            clock,
            deadline_ns,
            0,
            site,
        )
        .outcome()
    }
}
/// # Safety
/// Live run and typed Net grant.
#[allow(clippy::too_many_arguments)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_host_listen(
    context: *mut Context,
    n: u64,
    family: u8,
    hi: u64,
    lo: u64,
    port: u16,
    backlog: u16,
    site: u64,
) -> AddressOutcome {
    unsafe {
        address_op(
            context, LISTEN, n, family, hi, lo, port, 0, 0, 0, backlog, site,
        )
    }
}
#[allow(clippy::too_many_arguments)]
unsafe fn address_op(
    context: *mut Context,
    op: u8,
    n: u64,
    family: u8,
    hi: u64,
    lo: u64,
    port: u16,
    until_kind: u8,
    clock: u64,
    deadline_ns: u64,
    backlog: u16,
    site: u64,
) -> AddressOutcome {
    unsafe {
        let address = Address {
            family,
            hi,
            lo,
            port,
        };
        let mut network = NetworkRequest {
            send_tail: ptr::null(),
            address,
            sockets: &raw mut (*(*context).host_files).sockets,
            until: deadline(context, until_kind, clock, deadline_ns),
            backlog,
            connecting: None,
            denied: false,
            direction: 0,
            os: &crate::os_net::OPERATIONS,
        };
        let mut request = Request {
            operation: op,
            mode: 0,
            handle: hirsel_core_capability_handle(context, n),
            path: (&mut network as *mut NetworkRequest).cast(),
            path_len: size_of::<NetworkRequest>(),
            buffer: empty_buffer(),
            site,
        };
        let provider = hirsel_core_capability_provider(context, n);
        let mut outcome = hirsel_core_host(context, provider, &mut request);
        if let Some(tape) = (*(*context).host_files).net_tape {
            outcome = tape.carrier(outcome);
        }
        AddressOutcome::new(outcome, network.address)
    }
}
#[allow(clippy::too_many_arguments)]
unsafe fn owned_op(
    context: *mut Context,
    op: u8,
    key: u64,
    operand: *mut Buffer,
    send_tail: *const Buffer,
    max_receive: u64,
    until_kind: u8,
    clock: u64,
    deadline_ns: u64,
    site: u64,
) -> AddressOutcome {
    unsafe {
        let files = (*context).host_files;
        let tape = (*files).net_tape;
        let mut network = NetworkRequest {
            send_tail,
            address: Address::default(),
            sockets: &raw mut (*files).sockets,
            until: deadline(context, until_kind, clock, deadline_ns),
            backlog: 0,
            connecting: None,
            denied: false,
            direction: (key & 3) as u8,
            os: &crate::os_net::OPERATIONS,
        };
        let mut buffer = if operand.is_null() {
            empty_buffer()
        } else {
            *operand
        };
        if op == RECV_BOUNDED {
            let max = usize::try_from(max_receive).unwrap_or(usize::MAX);
            buffer.capacity = buffer.capacity.min(buffer.len.saturating_add(max));
        }
        let mut request = Request {
            operation: op,
            mode: 0,
            handle: key,
            path: (&mut network as *mut NetworkRequest).cast(),
            path_len: size_of::<NetworkRequest>(),
            buffer,
            site,
        };
        let outcome = if let Some(tape) = tape {
            request.handle = tape.handle(key);
            let outcome = hirsel_core_host(context, &tape.provider, &mut request);
            if op == ACCEPT {
                tape.carrier(outcome)
            } else {
                outcome
            }
        } else {
            hirsel_core_host(context, (*(*context).run).provider, &mut request)
        };
        if op == RECV_BOUNDED {
            (*operand).len = request.buffer.len;
        }
        AddressOutcome::new(outcome, network.address)
    }
}
/// # Safety
/// Live listener and optional Clock designation, pinned through delivery.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_host_accept(
    context: *mut Context,
    key: u64,
    until_kind: u8,
    clock: u64,
    deadline_ns: u64,
    site: u64,
) -> AddressOutcome {
    unsafe {
        owned_op(
            context,
            ACCEPT,
            key,
            ptr::null_mut(),
            ptr::null(),
            0,
            until_kind,
            clock,
            deadline_ns,
            site,
        )
    }
}
/// # Safety
/// A live read half and optional Clock designation from this run, with an
/// exclusively borrowed carrier pinned and writable through delivery.
#[allow(clippy::too_many_arguments)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_host_recv_bounded(
    context: *mut Context,
    key: u64,
    operand: *mut Buffer,
    max_receive: u64,
    until_kind: u8,
    clock: u64,
    deadline_ns: u64,
    site: u64,
) -> Outcome {
    unsafe {
        owned_op(
            context,
            RECV_BOUNDED,
            key,
            operand,
            ptr::null(),
            max_receive,
            until_kind,
            clock,
            deadline_ns,
            site,
        )
        .outcome()
    }
}
/// # Safety
/// A live write half and optional Clock designation from this run, with
/// both readable byte carriers (and their initialized prefixes) pinned
/// and immutable through delivery.
#[allow(clippy::too_many_arguments)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_host_send_pair(
    context: *mut Context,
    key: u64,
    prefix: *const Buffer,
    tail: *const Buffer,
    until_kind: u8,
    clock: u64,
    deadline_ns: u64,
    site: u64,
) -> Outcome {
    // SAFETY: the caller pins both carriers and the write half until delivery.
    // owned_op writes back the first carrier only for receive.
    unsafe {
        owned_op(
            context,
            SEND_PAIR,
            key,
            prefix.cast_mut(),
            tail,
            0,
            until_kind,
            clock,
            deadline_ns,
            site,
        )
        .outcome()
    }
}
/// # Safety
/// Consumes the sole Socket carrier. No allocation, admission or replay point.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_net_split(
    context: *mut Context,
    key: u64,
    _site: u64,
) -> Outcome {
    let _commit = crate::sequencer::enter();
    unsafe {
        let files = (*context).host_files;
        let (read, write) = if let Some(tape) = (*files).net_tape {
            if let Some(source) = tape.source(key) {
                (*files).sockets.split(source);
            }
            (key | 1, key | 2)
        } else {
            (*files).sockets.split(key)
        };
        Outcome {
            value: read,
            written: write,
            ..Outcome::default()
        }
    }
}
/// # Safety
/// Consumes one live socket, directional half or listener carrier exactly once.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_socket_drop(context: *mut Context, key: u64) {
    let _commit = crate::sequencer::enter();
    unsafe {
        let files = (*context).host_files;
        let source = match (*files).net_tape {
            Some(tape) => tape.source(key),
            None => Some(key),
        };
        if let Some(source) = source {
            (*files).sockets.release(source);
        }
    }
}
/// # Safety
/// Live run and typed Net grant. Rule scalars come from the caller's Rule.
#[allow(clippy::too_many_arguments)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_net_narrow(
    context: *mut Context,
    n: u64,
    dir: u8,
    family: u8,
    hi: u64,
    lo: u64,
    bits: u8,
    first: u16,
    last: u16,
    _site: u64,
) -> Outcome {
    let rule = Rule {
        dir,
        family,
        hi,
        lo,
        bits,
        first,
        last,
    };
    if !rule.valid() {
        return Outcome {
            error: IoError { tag: 1, code: 0 },
            ..Outcome::default()
        };
    }
    unsafe {
        crate::process::deny_describe_capability(context);
        let provider = *hirsel_core_capability_provider(context, n);
        let parent = hirsel_core_capability_handle(context, n);
        let taped = crate::tape::owns(&provider);
        let rules_handle = if taped {
            crate::tape::net_rules(&provider, parent)
        } else {
            parent
        };
        let rules = (*(rules_handle as *const Rules)).as_slice();
        let count = rules.iter().filter_map(|r| r.intersect(rule)).count();
        let charge = (count as u64)
            .checked_mul(37)
            .and_then(|n| n.checked_add(7))
            .map(|n| n & !7);
        let payer = (*context).payer();
        if let Err(capacity) = payer.admit(charge) {
            return Outcome {
                value: capacity.requested,
                error: IoError { tag: 2, code: 0 },
                written: capacity.available,
                status: 0,
            };
        }
        let rules = RulesStorage::new(rules.iter().filter_map(|r| r.intersect(rule)).collect());
        let handle = rules.handle();
        let files = (*context).host_files;
        (*files).net_grants.lock().unwrap().push(Grant {
            _rules: rules,
            payer,
            charge: charge.unwrap(),
        });
        let value = if taped {
            let (handle, group) = crate::tape::narrow_net(&provider, parent, handle);
            (*files).registry.taped_grant(provider, handle, group)
        } else {
            (*files).registry.grant(provider, handle)
        };
        Outcome {
            value,
            ..Outcome::default()
        }
    }
}
