//! Network operand adapters. Authority checks precede issue; buffers stay caller-owned.
use crate::{
    Context,
    entry::{hirsel_core_capability_handle, hirsel_core_capability_provider},
    provider::*,
};
use hirsel_core_net::{GrantRule, ResolveRule, Rule, RuleView, TlsRule, TlsTrust};
use std::ptr;

#[repr(C)]
pub struct Rules {
    pub data: *const RuleView,
    pub len: usize,
}
impl Rules {
    /// # Safety
    /// The run pins this grant's immutable rows.
    pub(crate) unsafe fn permits(&self, test: impl FnMut(&RuleView) -> bool) -> bool {
        unsafe { self.as_slice() }.iter().any(test)
    }

    /// # Safety
    /// The run pins the configured or narrowed immutable rule list.
    pub unsafe fn as_slice(&self) -> &[RuleView] {
        if self.len == 0 {
            &[]
        } else {
            unsafe { std::slice::from_raw_parts(self.data, self.len) }
        }
    }
}
pub(crate) struct RulesStorage {
    _rows: Box<[RuleView]>,
    _patterns: Vec<String>,
    view: Box<Rules>,
}

impl RulesStorage {
    pub(crate) fn empty() -> Self {
        Self {
            _rows: Box::default(),
            _patterns: Vec::new(),
            view: Box::new(Rules {
                data: ptr::null(),
                len: 0,
            }),
        }
    }

    /// # Safety
    /// Each TLS or Resolve row pins its pattern bytes for this call.
    pub(crate) unsafe fn from_views(rows: &[RuleView]) -> Option<Self> {
        let rules = rows
            .iter()
            .filter(|row| !row.is_unix_root())
            .map(|row| {
                // SAFETY: the launcher borrows the pattern bytes through startup.
                unsafe { row.decode() }.filter(|rule| !rule.is_served())
            })
            .collect::<Option<Vec<_>>>()?;
        Some(Self::new(rules))
    }

    pub(crate) fn new(storage: impl IntoIterator<Item = GrantRule>) -> Self {
        let mut patterns = Vec::new();
        let rows: Box<[_]> = storage
            .into_iter()
            .map(|rule| match rule {
                GrantRule::Address(address) => RuleView::address(address),
                GrantRule::Tls(rule) => {
                    patterns.push(rule.pattern_text());
                    RuleView::tls(&rule, patterns.last().unwrap())
                }
                GrantRule::Resolve(rule) => {
                    patterns.push(rule.pattern_text());
                    RuleView::resolve(patterns.last().unwrap())
                }
                GrantRule::Unix(rule) => {
                    let text = rule
                        .endpoint()
                        .map_or_else(String::new, |(label, path)| [label, path].concat());
                    patterns.push(text);
                    RuleView::unix(&rule, patterns.last().unwrap())
                }
            })
            .collect();
        let view = Box::new(Rules {
            data: rows.as_ptr(),
            len: rows.len(),
        });
        Self {
            _rows: rows,
            _patterns: patterns,
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
    pub(crate) fn outcome(self) -> Outcome {
        Outcome {
            value: self.value,
            error: self.error,
            written: self.written,
            status: self.status,
            kind: 0,
        }
    }
}
/// Only a ready TLS session enters the socket registry. Keep ordinary Net
/// callbacks independent of a concrete TLS backend.
pub(crate) trait SecureStream: Send {
    fn receive(&mut self, fd: i32, plaintext: &mut [u8]) -> Option<Outcome>;
    fn send(&mut self, fd: i32, prefix: &[u8], tail: &[u8]) -> Option<Outcome>;
    fn shutdown(&mut self, fd: i32);
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
    pub tls: *mut crate::os_tls::Streams,
    pub pending_write: bool,
    /// RECV_BOUNDED's bound, which a served Net's call reports.
    pub max_receive: u64,
}
/// A CLI run substitutes all Net grants on one tape. A socket carrier
/// keeps the tape's object ordinal and the usual two direction bits.
#[cfg(feature = "record")]
#[derive(Clone, Copy)]
pub(crate) struct SocketTape {
    pub(crate) provider: Provider,
    generation: u64,
}

#[cfg(feature = "record")]
impl SocketTape {
    pub(crate) fn new(provider: Provider, handle: u64) -> Self {
        Self {
            provider,
            generation: handle & !u64::from(u32::MAX),
        }
    }

    pub(crate) fn handle(self, key: u64) -> u64 {
        self.generation | (key >> 2)
    }

    pub(crate) fn carrier(self, mut outcome: Outcome) -> Outcome {
        if outcome.status == 0 && outcome.error == IoError::OK {
            outcome.value = u64::from(outcome.value as u32) << 2;
        }
        outcome
    }

    /// Release a taped Proc pipe or Unix object at its recorded position.
    /// Other socket types keep their existing cleanup path.
    unsafe fn release(self, context: *mut Context, key: u64) -> Option<Outcome> {
        let handle = self.handle(key);
        if !crate::tape::typed_socket(&self.provider, handle) {
            return None;
        }
        // SAFETY: cleanup owns this half/whole object and pins Files through CLOSE.
        unsafe {
            let sockets = &raw mut (*(*context).host_files).sockets;
            let mut request = Request {
                operation: CLOSE,
                mode: SOCKET_CLOSE | (key as u8 & 3),
                handle,
                path: sockets.cast(),
                path_len: 0,
                buffer: empty_buffer(),
                site: 0,
            };
            (*context).cancel_shield += 1;
            let answer = drive(context, &self.provider, &mut request, None);
            (*context).cancel_shield -= 1;
            Some(answer)
        }
    }

    fn source(self, key: u64) -> Option<u64> {
        crate::tape::source(&self.provider, self.handle(key)).map(|(_, handle)| handle | (key & 3))
    }
}

#[cfg(feature = "record")]
pub(crate) const SOCKET_CLOSE: u8 = 0x40;

#[cfg(feature = "record")]
pub(crate) unsafe fn close_socket_source(request: &Request) -> bool {
    // SAFETY: SocketTape::release pins its local source registry.
    unsafe {
        (&mut *request.path.cast_mut().cast::<hirsel_core_net::Sockets>()).release(request.handle)
    }
}

pub(crate) unsafe fn deadline(
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
    kind: 0,
};

/// Check an address before issue. A tape records the denial for its inner grant
/// and delivers it at its ordinary provider boundary.
pub(crate) unsafe fn authorize(provider: &Provider, request: &mut Request) -> bool {
    if unix_operation(request.operation) {
        // SAFETY: the adapter pins this extension through the provider call.
        return unsafe { (*request.path.cast::<crate::unix_net::UnixRequest>()).authorized };
    }
    if request.operation == RESOLVE {
        // SAFETY: the resolve adapter pins its request and the Net grant.
        return unsafe { crate::resolve::authorize(provider, request) };
    }
    if !matches!(request.operation, CONNECT | LISTEN) {
        return true;
    }
    // SAFETY: CONNECT and LISTEN carry the Net adapter's pinned extension.
    unsafe {
        #[cfg(feature = "record")]
        if crate::tape::owns(provider) {
            if let Some((_, handle)) = crate::tape::source(provider, request.handle) {
                let denied = !permitted(request, handle);
                (*request.path.cast_mut().cast::<NetworkRequest>()).denied = denied;
            }
            return true;
        }
        #[cfg(not(feature = "record"))]
        let _ = provider;
        permitted(request, request.handle)
    }
}

unsafe fn permitted(request: &Request, handle: u64) -> bool {
    // SAFETY: network adapters pin the scalar address and the grant's Rules.
    unsafe {
        let address = &(*request.path.cast::<NetworkRequest>()).address;
        permits_address(handle, address, u8::from(request.operation == LISTEN))
    }
}

/// SAFETY: handle is the live Net grant's immutable rule slice.
pub(crate) unsafe fn permits_address(handle: u64, address: &Address, dir: u8) -> bool {
    unsafe {
        let rules = &*(handle as *const Rules);
        rules.permits(|r| {
            r.address
                .permits(dir, address.family, address.hi, address.lo, address.port)
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
        let files = (*context).host_files;
        let mut network = NetworkRequest {
            send_tail: ptr::null(),
            address,
            sockets: &raw mut (*files).sockets,
            until: deadline(context, until_kind, clock, deadline_ns),
            backlog,
            connecting: None,
            denied: false,
            direction: 0,
            os: &crate::os_net::OPERATIONS,
            tls: &raw mut (*files).tls,
            pending_write: false,
            max_receive: 0,
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
        #[cfg_attr(not(feature = "record"), allow(unused_mut))]
        let mut outcome = hirsel_core_host(context, provider, &mut request);
        let _commit = crate::sequencer::enter();
        // A served Net's keys are its own, never a tape's carriers.
        let served = crate::provide::owns(&*provider);
        #[cfg(feature = "record")]
        if let Some(tape) = (*files).net_tape
            && !served
        {
            outcome = tape.carrier(outcome);
        }
        let ok = outcome.status == 0 && outcome.error == IoError::OK;
        if ok && (op == CONNECT || served) {
            // A served Net's listeners route to it too.
            (*files)
                .connected
                .insert(outcome.value, (address, *provider));
        }
        AddressOutcome::new(outcome, network.address)
    }
}
/// An owned network operation's state: the request and everything it
/// borrows through delivery (the network request, the paired-send tail and
/// the provider value), initialized in place so its internal pointers hold.
/// The stackful rows keep it on the task's stack; the returnable rows keep it
/// in the task's frame record (`tasks::FrameHeader`), where a parked task
/// leaves it when it gives its stack back. Tape keys follow the request's
/// address, which is the same for the operation's whole life in either place.
#[repr(C)]
pub(crate) struct OwnedNet {
    network: NetworkRequest,
    request: Request,
    tail: Buffer,
    provider: Provider,
}
/// How an owned operation was routed, which only an accept's delivery reads:
/// a served listener's port, or the CLI's net tape.
#[derive(Clone, Copy, Default)]
pub(crate) struct Routing {
    served: Option<Provider>,
    #[cfg(feature = "record")]
    taped: bool,
}
impl OwnedNet {
    /// Build the state at `this`. Err is an outcome that ends the operation
    /// before entry (a spent write half).
    /// # Safety
    /// `this` is writable and stays put until delivery; operands as the rows.
    #[allow(clippy::too_many_arguments)]
    #[inline(always)]
    unsafe fn init(
        this: *mut Self,
        context: *mut Context,
        op: u8,
        key: u64,
        operand: *const Buffer,
        send_tail: *const Buffer,
        max_receive: u64,
        until_kind: u8,
        clock: u64,
        deadline_ns: u64,
        site: u64,
    ) -> Result<Routing, AddressOutcome> {
        unsafe {
            let files = (*context).host_files;
            if op == SEND_PAIR && write_spent(files, key) {
                return Err(AddressOutcome::new(RESET, Address::default()));
            }
            let tail = &raw mut (*this).tail;
            tail.write(if send_tail.is_null() {
                empty_buffer()
            } else {
                *send_tail
            });
            (&raw mut (*this).network).write(NetworkRequest {
                send_tail: if send_tail.is_null() {
                    ptr::null()
                } else {
                    tail
                },
                address: Address::default(),
                sockets: &raw mut (*files).sockets,
                until: deadline(context, until_kind, clock, deadline_ns),
                backlog: 0,
                connecting: None,
                denied: false,
                direction: (key & 3) as u8,
                os: &crate::os_net::OPERATIONS,
                tls: &raw mut (*files).tls,
                pending_write: false,
                max_receive,
            });
            let mut buffer = if operand.is_null() {
                empty_buffer()
            } else {
                *operand
            };
            let served = served_socket(context, key);
            if op == RECV_BOUNDED && !served.as_ref().is_some_and(crate::provide::exec_port) {
                let max = usize::try_from(max_receive).unwrap_or(usize::MAX);
                buffer.capacity = buffer.capacity.min(buffer.len.saturating_add(max));
            }
            #[cfg(feature = "record")]
            let (provider, handle, taped) = match (*files).net_tape.filter(|_| served.is_none()) {
                Some(tape) => (tape.provider, tape.handle(key), true),
                None => (socket_provider(context, key), key, false),
            };
            #[cfg(not(feature = "record"))]
            let (provider, handle) = (socket_provider(context, key), key);
            (&raw mut (*this).provider).write(provider);
            (&raw mut (*this).request).write(Request {
                operation: op,
                mode: 0,
                handle,
                path: (&raw mut (*this).network).cast(),
                path_len: size_of::<NetworkRequest>(),
                buffer,
                site,
            });
            Ok(Routing {
                served,
                #[cfg(feature = "record")]
                taped,
            })
        }
    }
    /// The delivered outcome, with the receive length written back to the
    /// language carrier and a served listener's accepted socket routed to its
    /// port.
    /// # Safety
    /// `operand` is the carrier the operation was built from, pinned and
    /// writable for a receive.
    #[inline(always)]
    unsafe fn finish(
        this: *mut Self,
        context: *mut Context,
        operand: *mut Buffer,
        routing: Routing,
        outcome: Outcome,
    ) -> AddressOutcome {
        unsafe {
            let op = (*this).request.operation;
            let files = (*context).host_files;
            #[cfg(feature = "record")]
            let outcome = match (*files).net_tape {
                Some(tape) if op == ACCEPT && routing.taped => tape.carrier(outcome),
                _ => outcome,
            };
            if op == RECV_BOUNDED {
                (*operand).len = (*this).request.buffer.len;
            }
            if let Some(provider) = routing.served
                && op == ACCEPT
                && outcome.status == 0
                && outcome.error == IoError::OK
            {
                let _commit = crate::sequencer::enter();
                (*files)
                    .connected
                    .insert(outcome.value, ((*this).network.address, provider));
            }
            AddressOutcome::new(outcome, (*this).network.address)
        }
    }
}
#[allow(clippy::too_many_arguments)]
pub(crate) unsafe fn owned_op(
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
        let mut state = std::mem::MaybeUninit::<OwnedNet>::uninit();
        let this = state.as_mut_ptr();
        let routing = match OwnedNet::init(
            this,
            context,
            op,
            key,
            operand,
            send_tail,
            max_receive,
            until_kind,
            clock,
            deadline_ns,
            site,
        ) {
            Ok(routing) => routing,
            Err(outcome) => return outcome,
        };
        let outcome = hirsel_core_host(context, &(*this).provider, &raw mut (*this).request);
        OwnedNet::finish(this, context, operand, routing, outcome)
    }
}
/// The returnable form of `owned_op`, for a wait the compiler proved is an
/// owned boundary: every frame up to the task's root is represented in its
/// frame record, and `word` is the calling frame's resume word there. A
/// fresh call (`*word == 0`) issues as `owned_op` does; if the operation
/// parks it sets the word, and the caller returns through its represented
/// frames with the dummy outcome. Called again with the word set, after the
/// scheduler made the task runnable, it continues the same operation:
/// nothing is moved, admitted, polled or debited twice.
/// # Safety
/// As `owned_op`; `word` lives in the current task's frame record, and a
/// resume passes the operands the parked call passed.
#[allow(clippy::too_many_arguments)]
unsafe fn returnable_op(
    context: *mut Context,
    word: *mut u64,
    op: u8,
    key: u64,
    operand: *mut Buffer,
    send_tail: *const Buffer,
    max_receive: u64,
    until_kind: u8,
    clock: u64,
    deadline_ns: u64,
    site: u64,
) -> Outcome {
    unsafe {
        let Some(header) = crate::tasks::frame_header() else {
            crate::process::stop(b"FrameRecord");
        };
        let this: *mut OwnedNet = (&raw mut (*header).net).cast();
        let outcome = if *word == 0 {
            if let Err(outcome) = OwnedNet::init(
                this,
                context,
                op,
                key,
                operand,
                send_tail,
                max_receive,
                until_kind,
                clock,
                deadline_ns,
                site,
            ) {
                return outcome.outcome();
            }
            crate::provider::returnable(context, &(*this).provider, &raw mut (*this).request)
        } else {
            crate::tasks::resume_returnable(context)
        };
        match outcome {
            Some(outcome) => {
                *word = 0;
                // Only an accept reads the routing, and it is never returnable.
                OwnedNet::finish(this, context, operand, Routing::default(), outcome).outcome()
            }
            None => {
                *word = 1;
                Outcome::default()
            }
        }
    }
}
/// # Safety
/// As `hirsel_core_host_recv_bounded`; `word` as `returnable_op`.
#[allow(clippy::too_many_arguments)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_host_recv_bounded_rec(
    context: *mut Context,
    word: *mut u64,
    key: u64,
    operand: *mut Buffer,
    max_receive: u64,
    until_kind: u8,
    clock: u64,
    deadline_ns: u64,
    site: u64,
) -> Outcome {
    unsafe {
        returnable_op(
            context,
            word,
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
    }
}
/// # Safety
/// As `hirsel_core_host_send_pair`; `word` as `returnable_op`.
#[allow(clippy::too_many_arguments)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_host_send_pair_rec(
    context: *mut Context,
    word: *mut u64,
    key: u64,
    prefix: *const Buffer,
    tail: *const Buffer,
    until_kind: u8,
    clock: u64,
    deadline_ns: u64,
    site: u64,
) -> Outcome {
    unsafe {
        returnable_op(
            context,
            word,
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
pub(crate) const RESET: Outcome = Outcome {
    error: IoError { tag: 5, code: 0 },
    value: 0,
    written: 0,
    status: 0,
    kind: 0,
};

unsafe fn write_spent(files: *mut crate::host_file::Files, key: u64) -> bool {
    let _commit = crate::sequencer::enter();
    unsafe {
        (*files)
            .tls_writes
            .get(&(key & !3))
            .is_some_and(|write| write.spent)
    }
}

/// Whether `key` is part of a TLS stream.
unsafe fn tls_stream(context: *mut Context, key: u64) -> bool {
    let _commit = crate::sequencer::enter();
    unsafe {
        (*(*context).host_files)
            .tls_writes
            .contains_key(&(key & !3))
    }
}

/// A served Net's socket, half or listener: its port's provider.
pub(crate) unsafe fn served_socket(context: *mut Context, key: u64) -> Option<Provider> {
    let _commit = crate::sequencer::enter();
    unsafe {
        (*(*context).host_files)
            .unix_served
            .get(&(key & !3))
            .copied()
            .or_else(|| {
                (*(*context).host_files)
                    .connected
                    .get(&(key & !3))
                    .map(|record| record.1)
            })
            .filter(crate::provide::owns)
    }
}

pub(crate) unsafe fn socket_provider(context: *mut Context, key: u64) -> Provider {
    let _commit = crate::sequencer::enter();
    unsafe {
        (*(*context).host_files)
            .connected
            .get(&(key & !3))
            .map(|record| record.1)
            .or_else(|| {
                (*(*context).host_files)
                    .unix_served
                    .get(&(key & !3))
                    .copied()
            })
            .unwrap_or_else(|| *(*(*context).run).provider)
    }
}

/// # Safety
/// The write half is exclusively borrowed. The two language carriers and
/// their initialized prefixes are pinned through delivery.
#[allow(clippy::too_many_arguments)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_host_send_secret(
    context: *mut Context,
    key: u64,
    before: *const Buffer,
    s: u64,
    after: *const Buffer,
    until_kind: u8,
    clock: u64,
    deadline_ns: u64,
    site: u64,
) -> Outcome {
    unsafe {
        crate::process::deny_describe_capability(context);
        let files = (*context).host_files;
        // A served Net's stream was verified by its server, so it never
        // satisfies a granted Secret (TLS-5, NOTES 6).
        let served = served_socket(context, key).is_some();
        let secret = crate::secret::resolve(context, s);
        if served && matches!(secret.origin, crate::secret::Origin::Granted(_)) {
            return DENIED;
        }
        let narrowed = {
            let _commit = crate::sequencer::enter();
            let Some(write) = (*files).tls_writes.get(&(key & !3)) else {
                return DENIED;
            };
            if key & 3 != 2 {
                return DENIED;
            }
            if write.spent {
                return RESET;
            }
            if !secret
                .policy
                .patterns
                .iter()
                .any(|pattern| pattern.covers(&write.name))
            {
                return DENIED;
            }
            // Its server receives the Secret narrowed to the stream's name,
            // never its bytes (`provide.secret`).
            served.then(|| crate::secret::Secret {
                origin: secret.origin,
                policy: hirsel_core_net::SecretPolicy::exactly(&write.name),
                bytes: secret.bytes.clone(),
            })
        };
        let bytes = match (&narrowed, &secret.bytes) {
            (None, Some(bytes)) => bytes.as_slice(),
            _ => &[][..],
        };
        let mut send = crate::secret::SendRequest {
            network: NetworkRequest {
                send_tail: after,
                address: Address::default(),
                sockets: &raw mut (*files).sockets,
                until: deadline(context, until_kind, clock, deadline_ns),
                backlog: 0,
                connecting: None,
                denied: false,
                direction: 2,
                os: &crate::os_net::OPERATIONS,
                tls: &raw mut (*files).tls,
                pending_write: false,
                max_receive: 0,
            },
            secret: Buffer {
                data: bytes.as_ptr().cast_mut(),
                len: bytes.len(),
                capacity: 0,
            },
            origin: secret.origin,
            patterns: secret.policy.patterns,
            committed: 0,
            narrowed,
        };
        let request = Request {
            operation: SEND_SECRET,
            mode: 0,
            handle: key,
            path: (&raw mut send).cast(),
            path_len: size_of::<crate::secret::SendRequest>(),
            buffer: *before,
            site,
        };
        send_sink(context, request)
    }
}

/// SEND_SECRET and SCRAM borrow this extension's shared prefix.
pub(crate) unsafe fn send_sink(context: *mut Context, mut request: Request) -> Outcome {
    unsafe {
        let files = (*context).host_files;
        let key = request.handle;
        let provider = socket_provider(context, key);
        // A served stream's results are the run's own: never taped (CAP-8).
        let served = crate::provide::owns(&provider);
        #[cfg(feature = "record")]
        let provider = if let Some(tape) = (*files).net_tape.filter(|_| !served) {
            request.handle = tape.handle(key);
            tape.provider
        } else {
            provider
        };
        let outcome = hirsel_core_host(context, &provider, &mut request);
        let send = &*request.path.cast::<crate::secret::SendRequest>();
        // A server reports no committed count: any failure after issue may
        // follow a partial forward. A queued call has taken its narrowed Secret.
        if (outcome.status != 0 || outcome.error != IoError::OK)
            && (served && send.narrowed.is_none() || outcome.written != 0 || send.committed != 0)
        {
            spend_write(files, key);
        }
        outcome
    }
}

pub(crate) unsafe fn spend_write(files: *mut crate::host_file::Files, key: u64) {
    let _commit = crate::sequencer::enter();
    unsafe { (*files).tls_writes.get_mut(&(key & !3)).unwrap().spent = true };
}

/// # Safety
/// Consumes the sole Socket carrier. No allocation, admission or replay point.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_net_split(
    context: *mut Context,
    key: u64,
    _site: u64,
) -> Outcome {
    unsafe {
        if let Some(provider) = served_socket(context, key) {
            crate::provide::split_socket(&provider, key);
            return Outcome {
                value: key | 1,
                written: key | 2,
                ..Outcome::default()
            };
        }
        let _commit = crate::sequencer::enter();
        let files = (*context).host_files;
        #[cfg(feature = "record")]
        let (read, write) = if let Some(tape) = (*files).net_tape {
            crate::tape::socket_parts(&tape.provider, tape.handle(key), 0b110);
            if let Some(source) = tape.source(key) {
                (*files).sockets.split(source);
            }
            (key | 1, key | 2)
        } else {
            (*files).sockets.split(key)
        };
        #[cfg(not(feature = "record"))]
        let (read, write) = (*files).sockets.split(key);
        Outcome {
            value: read,
            written: write,
            ..Outcome::default()
        }
    }
}
/// Owns a socket or listener while it is in transit.
pub(crate) struct CrossedSocket {
    carrier: Option<hirsel_core_net::SocketTransfer>,
    connected: Option<(Address, Provider)>,
}

// SAFETY: the provider descriptor moves as opaque data and is used only
// under its existing calling contract.
unsafe impl Send for CrossedSocket {}

impl CrossedSocket {
    pub(crate) fn check(files: &crate::host_file::Files, key: u64) {
        if key & 3 != 0
            || files.unix_served.contains_key(&key)
            || files.tls.sessions.contains_key(&key)
            || files.tls_writes.contains_key(&key)
            || files
                .connected
                .get(&key)
                .is_some_and(|(_, provider)| crate::provide::owns(provider))
        {
            // A port owns its served keys. TLS halves own separate stream state.
            crate::process::stop(b"UnitSocket");
        }
    }

    pub(crate) fn detach(files: &mut crate::host_file::Files, key: u64) -> Self {
        #[cfg(feature = "record")]
        if files.net_tape.is_some() {
            crate::process::stop(b"UnitRecording");
        }
        let carrier = files
            .sockets
            .detach(key)
            .unwrap_or_else(|_| crate::process::stop(b"HostPoll"));
        Self {
            carrier: Some(carrier),
            connected: files.connected.remove(&key),
        }
    }

    pub(crate) fn attach(mut self, files: &mut crate::host_file::Files) -> u64 {
        let key = files
            .sockets
            .attach(self.carrier.take().unwrap())
            .unwrap_or_else(|_| crate::process::stop(b"HostPoll"));
        if let Some(connected) = self.connected.take() {
            files.connected.insert(key, connected);
        }
        key
    }
}

/// Sockets taken from a waiting send are held jointly by its item and the
/// issuing call, which needs them back if the send is refused or withdrawn.
pub(crate) type CrossingSockets = std::sync::Arc<std::sync::Mutex<SocketCrossing>>;

pub(crate) enum SocketCrossing {
    InTransit(Vec<CrossedSocket>),
    Taken(Vec<u64>),
}

impl SocketCrossing {
    pub(crate) fn take(&mut self, files: &mut crate::host_file::Files) {
        if let Self::InTransit(sockets) = self {
            let keys = sockets
                .drain(..)
                .map(|socket| socket.attach(files))
                .collect();
            *self = Self::Taken(keys);
        }
    }

    pub(crate) fn keys(&mut self, files: &mut crate::host_file::Files) -> Vec<u64> {
        self.take(files);
        let Self::Taken(keys) = self else {
            unreachable!()
        };
        std::mem::take(keys)
    }

    pub(crate) fn discard(&mut self) -> Vec<u64> {
        match std::mem::replace(self, Self::Taken(Vec::new())) {
            Self::InTransit(_) => Vec::new(),
            Self::Taken(keys) => keys,
        }
    }
}

/// Reunite the halves of one plain client socket in the run's registry.
/// # Safety
/// Both keys are live, uniquely owned halves. Failure returns both unchanged.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_net_join(
    context: *mut Context,
    read: u64,
    write: u64,
    _site: u64,
) -> Outcome {
    let refused = || Outcome {
        value: read,
        written: write,
        error: IoError { tag: 1, code: 0 },
        ..Outcome::default()
    };
    unsafe {
        // A served socket's halves rejoin at its port; a TLS stream's never.
        if let Some(provider) = served_socket(context, read) {
            let unix = {
                let _commit = crate::sequencer::enter();
                (*(*context).host_files)
                    .unix_served
                    .contains_key(&(read & !3))
            };
            return if read & !3 == write & !3
                && !unix
                && !tls_stream(context, read)
                && crate::provide::join_socket(&provider, read)
            {
                Outcome {
                    value: read & !3,
                    ..Outcome::default()
                }
            } else {
                refused()
            };
        }
    }
    let _commit = crate::sequencer::enter();
    unsafe {
        let files = (*context).host_files;
        let key = read & !3;
        if key != write & !3
            || (*files).tls_writes.contains_key(&key)
            || !(*files).connected.contains_key(&key)
        {
            return refused();
        }
        #[cfg(feature = "record")]
        let source = match (*files).net_tape {
            Some(tape) => {
                crate::tape::socket_parts(&tape.provider, tape.handle(key), 1);
                tape.source(key)
            }
            None => Some(key),
        };
        #[cfg(not(feature = "record"))]
        let source = Some(key);
        if let Some(source) = source {
            (*files).sockets.rejoin_client(source);
        }
        Outcome {
            value: key,
            ..Outcome::default()
        }
    }
}

/// # Safety
/// Consumes one live socket, directional half or listener carrier exactly once.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_socket_drop(context: *mut Context, key: u64) {
    unsafe {
        if crate::exec_runtime::is_proc(key) {
            crate::exec_runtime::drop_proc(context, key);
            return;
        }
        let files = (*context).host_files;
        let served = served_socket(context, key);
        #[cfg(feature = "record")]
        if let Some(tape) = (*files).net_tape.filter(|_| served.is_none())
            && let Some(answer) = tape.release(context, key)
        {
            if answer.status == 0 && answer.error == IoError::OK {
                let charge = {
                    let _commit = crate::sequencer::enter();
                    if answer.value != 0 {
                        (*files).datagrams.remove(&key);
                        (*files).connected.remove(&(key & !3));
                    }
                    (*files).exec_pipes.remove(&key)
                };
                drop(charge);
            }
            return;
        }
        let charge = {
            let _commit = crate::sequencer::enter();
            (*files).exec_pipes.remove(&key)
        };
        drop(charge);
        if let Some(provider) = served {
            // The port queues the Released; its server is readied outside
            // the run's commit section.
            let gone = crate::provide::release_socket(&provider, key);
            let _commit = crate::sequencer::enter();
            if key & 3 == 2 {
                (*files).tls_writes.remove(&(key & !3));
            }
            if gone {
                (*files).datagrams.remove(&key);
                (*files).unix_served.remove(&(key & !3));
                (*files).connected.remove(&(key & !3));
            }
            return;
        }
        let _commit = crate::sequencer::enter();
        (*files).datagrams.remove(&key);
        if key & 3 == 2 {
            (*files).tls_writes.remove(&(key & !3));
        }
        #[cfg(feature = "record")]
        let source = match (*files).net_tape {
            Some(tape) => tape.source(key),
            None => Some(key),
        };
        #[cfg(not(feature = "record"))]
        let source = Some(key);
        if let Some(source) = source {
            if source & 3 == 2
                && let Some(stream) = (*files).tls.sessions.get_mut(&(source & !3))
            {
                stream.shutdown((*files).sockets.fd(source));
            }
            if (*files).sockets.release(source) {
                (*files).connected.remove(&(key & !3));
                (*files).tls.sessions.remove(&(source & !3));
            }
        } else {
            (*files).connected.remove(&(key & !3));
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
        return DENIED;
    }
    unsafe { narrow(context, n, GrantRule::Address(rule)) }
}

/// # Safety
/// Live run, typed Net grant and pinned pattern and digest carriers.
#[allow(clippy::too_many_arguments)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_net_narrow_tls(
    context: *mut Context,
    n: u64,
    patterns: *const Buffer,
    first: u16,
    last: u16,
    trust: u8,
    digest: *const Buffer,
    _site: u64,
) -> Outcome {
    unsafe {
        let trust = match trust {
            0 => TlsTrust::System,
            1 => match crate::tls::bytes(&*digest).try_into() {
                Ok(digest) => TlsTrust::Roots(digest),
                Err(_) => return DENIED,
            },
            _ => return DENIED,
        };
        let Ok(patterns) = std::str::from_utf8(crate::tls::bytes(&*patterns)) else {
            return DENIED;
        };
        match TlsRule::new(patterns, first, last, trust) {
            Some(rule) => narrow(context, n, GrantRule::Tls(rule)),
            None => DENIED,
        }
    }
}
/// # Safety
/// Live run, typed Net grant and its pinned pattern carrier.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_net_narrow_resolve(
    context: *mut Context,
    net: u64,
    patterns: *const Buffer,
    _site: u64,
) -> Outcome {
    // SAFETY: the source pins its String's initialized bytes.
    let patterns = unsafe { crate::tls::bytes(&*patterns) };
    match std::str::from_utf8(patterns)
        .ok()
        .and_then(ResolveRule::new)
    {
        Some(rule) => unsafe { narrow(context, net, GrantRule::Resolve(rule)) },
        None => DENIED,
    }
}

pub(crate) unsafe fn narrow(context: *mut Context, n: u64, rule: GrantRule) -> Outcome {
    unsafe {
        crate::process::deny_describe_capability(context);
        let provider = *hirsel_core_capability_provider(context, n);
        let parent = hirsel_core_capability_handle(context, n);
        #[cfg(feature = "record")]
        let taped = crate::tape::owns(&provider);
        #[cfg(feature = "record")]
        let rules_handle = if taped {
            crate::tape::net_rules(&provider, parent)
        } else {
            parent
        };
        #[cfg(not(feature = "record"))]
        let rules_handle = parent;
        let rules = (*(rules_handle as *const Rules)).as_slice();
        let intersections: Vec<_> = rules
            .iter()
            .filter_map(|r| r.decode()?.intersect(&rule))
            .collect();
        let charge = intersections
            .iter()
            .try_fold(0u64, |total, r| total.checked_add(r.logical_size()))
            .and_then(|n| n.checked_add(7))
            .map(|n| n & !7);
        let payer = (*context).payer();
        if let Err(capacity) = payer.admit_run_owned(charge) {
            return Outcome {
                value: capacity.requested,
                error: IoError { tag: 2, code: 0 },
                written: capacity.available,
                status: 0,
                kind: 0,
            };
        }
        let rules = RulesStorage::new(intersections);
        let handle = rules.handle();
        let files = (*context).host_files;
        (*files)
            .registry
            .run_grants()
            .net_grants
            .lock()
            .unwrap()
            .push(Grant {
                _rules: rules,
                payer,
                charge: charge.unwrap(),
            });
        #[cfg(feature = "record")]
        let value = if taped {
            let (handle, _group) = crate::tape::narrow_net(&provider, parent, handle);
            (*files).registry.grant(provider, handle)
        } else {
            (*files).registry.grant(provider, handle)
        };
        #[cfg(not(feature = "record"))]
        let value = (*files).registry.grant(provider, handle);
        Outcome {
            value,
            ..Outcome::default()
        }
    }
}

pub(crate) fn tls_failure(case: u8, detail: u8) -> Outcome {
    Outcome {
        error: IoError {
            tag: 9,
            code: u32::from(case) | (u32::from(detail) << 8),
        },
        ..Outcome::default()
    }
}
