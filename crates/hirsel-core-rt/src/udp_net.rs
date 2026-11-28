//! Native UDP carriers and provider requests.

mod abi;
mod native;
#[cfg(feature = "record")]
pub(crate) mod record;
pub(crate) mod served;

use crate::{
    Context,
    entry::{hirsel_core_capability_handle, hirsel_core_capability_provider},
    host_file::{Files, RunGrants, RunPart},
    limit::Payer,
    net::{Address, AddressOutcome, DENIED, Rules},
    os_net::{Due, deadline, error, signal},
    provider::*,
    secret::{Charge, ChargeCell, HandedCharge, HandedOver},
};
use hirsel_core_net::UdpMember;
use hirsel_core_net::udp::{self, Options, ReceivePool, Socket, Steer, TxPacket, UdpRules, Window};
use native::Received;
use std::{
    collections::BTreeMap,
    io,
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
    num::{NonZeroU16, NonZeroU64},
    ptr,
    sync::{
        Arc, Mutex, Weak,
        atomic::{AtomicU64, Ordering},
    },
};

const HANDLE: u64 = 1 << 60;
const OVERSIZE: u8 = 12;
const EXHAUSTED: u8 = 13;

pub(crate) use served::valid_creation;

pub(crate) fn is_socket(key: u64) -> bool {
    key & HANDLE != 0 && key < HANDLE * 2
}

pub(crate) struct Allowance {
    left: AtomicU64,
    per_socket: u64,
    next: AtomicU64,
    charges: Mutex<BTreeMap<u64, Weak<ChargeCell>>>,
}

impl Default for Allowance {
    fn default() -> Self {
        let stop_allowance =
            || crate::run_stop::raise(crate::run_stop::Stop::from_static(b"UdpAllowance"));
        let bytes = if crate::run_stop::is_contained() {
            64 * 1024 * 1024
        } else {
            match std::env::var("HIRSEL_UDP_KERNEL_BYTES") {
                Ok(value) => value.parse().unwrap_or_else(|_| stop_allowance()),
                Err(std::env::VarError::NotPresent) => 64 * 1024 * 1024,
                Err(std::env::VarError::NotUnicode(_)) => stop_allowance(),
            }
        };
        Self {
            left: AtomicU64::new(bytes),
            per_socket: crate::host::sys::udp_queue_bytes().unwrap_or(u64::MAX),
            next: AtomicU64::new(1),
            charges: Mutex::default(),
        }
    }
}

/// A run's UDP host state (NET-UDP host allowance), built by its first socket.
#[derive(Default)]
pub(crate) struct RunState {
    rules: Mutex<BTreeMap<u64, Arc<UdpRules>>>,
    pub(crate) allowance: Arc<Allowance>,
    pub(crate) waiters: Arc<Waiters>,
}

impl RunPart for RunState {
    fn hand_over(&self, payer: Payer) -> HandedOver {
        let (handed, refunds) = self.allowance.hand_over(payer);
        drop(refunds);
        handed
    }
}

pub(crate) fn run_state(grants: &RunGrants) -> &RunState {
    crate::host_file::run_part(&grants.udp)
}

impl Allowance {
    fn hand_over(&self, payer: Payer) -> (HandedOver, Vec<Charge>) {
        let charges = self.charges.lock().unwrap();
        let mut handed = Vec::new();
        let mut refunds = Vec::new();
        for weak in charges.values() {
            if let Some(cell) = weak.upgrade()
                && let Some(charge) = cell.take_charge(payer)
            {
                handed.push(HandedCharge::new(&cell, charge.bytes()));
                refunds.push(charge);
            }
        }
        (handed, refunds)
    }

    fn reserve(self: &Arc<Self>) -> Option<KernelCharge> {
        self.left
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |left| {
                left.checked_sub(self.per_socket)
            })
            .ok()?;
        Some(KernelCharge(self.clone()))
    }

    fn handles(&self, count: u64) -> Option<u64> {
        let index = self
            .next
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |index| {
                (index + count <= HANDLE).then_some(index + count)
            })
            .ok()?;
        Some(HANDLE | index)
    }
}

struct KernelCharge(Arc<Allowance>);

impl Drop for KernelCharge {
    fn drop(&mut self) {
        self.0.left.fetch_add(self.0.per_socket, Ordering::Relaxed);
    }
}

pub(crate) struct SocketCharge {
    allowance: Arc<Allowance>,
    key: u64,
    _cell: Arc<ChargeCell>,
}

impl SocketCharge {
    #[cfg(feature = "record")]
    pub(crate) fn record_retirement(&self, retirement: crate::tape::Retirement) {
        self._cell.record_retirement(retirement);
    }

    #[cfg(feature = "record")]
    pub(crate) fn refund_replayed(&self, payer: Payer) -> bool {
        self._cell.refund_replayed(payer)
    }

    fn new(allowance: Arc<Allowance>, key: u64, charge: Charge) -> Self {
        let cell = ChargeCell::new(charge);
        allowance
            .charges
            .lock()
            .unwrap()
            .insert(key, Arc::downgrade(&cell));
        Self {
            allowance,
            key,
            _cell: cell,
        }
    }
}

impl Drop for SocketCharge {
    fn drop(&mut self) {
        #[cfg(feature = "record")]
        self._cell.retire();
        self.allowance.charges.lock().unwrap().remove(&self.key);
    }
}

struct Standalone {
    socket: Socket,
    pool: ReceivePool,
    readiness: u64,
    _kernel: KernelCharge,
    _charge: SocketCharge,
}

enum Carrier {
    Standalone(Standalone),
    Member(UdpMember),
    Served(served::Socket),
    #[cfg(feature = "record")]
    Replayed {
        _pool: Arc<SocketCharge>,
        _intake: Option<Arc<SocketCharge>>,
    },
}

pub(crate) struct Resource {
    carrier: Carrier,
    provider: Provider,
    rules: Arc<UdpRules>,
    options: Options,
    v6: bool,
    bound: bool,
    receiving: bool,
    #[cfg(feature = "record")]
    tape_id: Option<u64>,
}

impl Resource {
    fn socket(&self) -> &Socket {
        match &self.carrier {
            Carrier::Standalone(socket) => &socket.socket,
            Carrier::Member(member) => member.socket(),
            Carrier::Served(_) => unreachable!("served operations do not use kernel sockets"),
            #[cfg(feature = "record")]
            Carrier::Replayed { .. } => {
                unreachable!("replayed operations do not use kernel sockets")
            }
        }
    }

    fn cancel_waiter(&mut self, sending: bool, token: u64, waiters: &Waiters) {
        if let Carrier::Member(member) = &mut self.carrier {
            if sending {
                member.cancel_writer();
            } else {
                member.reader().cancel_waiter();
            }
            drop(waiters.take(token));
        }
    }

    fn send(
        &self,
        bytes: &[u8],
        window: &[TxPacket],
        waiters: &Waiters,
        token: u64,
    ) -> io::Result<Window> {
        let result = self.socket().send_window(bytes, window);
        let Carrier::Member(member) = &self.carrier else {
            return result;
        };
        let blocked = |error: &io::Error| error.kind() == io::ErrorKind::WouldBlock;
        if result.as_ref().is_err_and(blocked) {
            waiters.publish(token);
            member.publish_writer(NonZeroU64::new(token).unwrap());
            let retry = self.socket().send_window(bytes, window);
            if !retry.as_ref().is_err_and(blocked) {
                member.cancel_writer();
                drop(waiters.take(token));
            }
            retry
        } else {
            result
        }
    }
}

pub(crate) unsafe fn drop_socket(context: *mut Context, key: u64) {
    // SAFETY: the destructor owns this key and its context.
    let files = unsafe { (*context).host_files };
    #[cfg(feature = "record")]
    {
        let resource = unsafe { (*files).udp_sockets.get(&key) }.unwrap();
        if let Some(tape_id) = resource.tape_id {
            unsafe { record::close(context, files, key, resource.provider, tape_id) };
            return;
        }
    }
    unsafe { drop_native(files, key) };
}

unsafe fn drop_native(files: *mut Files, key: u64) {
    // SAFETY: transfer or drop exclusively owns this resource and unit registry.
    let resource = unsafe { (*files).udp_sockets.remove(&key) }.unwrap();
    if let Carrier::Standalone(socket) = &resource.carrier {
        // SAFETY: the resource's ordinary readiness registration lives in this registry.
        unsafe { (*files).sockets.release(socket.readiness) };
    }
    drop(resource);
}

pub(crate) struct Transfer {
    key: u64,
    watch: Option<hirsel_core_net::SocketTransfer>,
    resource: Resource,
}

// SAFETY: the transferred carrier has no waiter or concurrent access.
unsafe impl Send for Transfer {}

impl Transfer {
    pub(crate) fn detach(files: &mut Files, key: u64) -> Self {
        let resource = files.udp_sockets.remove(&key).unwrap();
        let watch = if let Carrier::Standalone(socket) = &resource.carrier {
            Some(files.sockets.detach(socket.readiness).unwrap_or_else(|_| {
                crate::run_stop::raise(crate::run_stop::Stop::from_static(b"HostPoll"))
            }))
        } else {
            None
        };
        Self {
            key,
            resource,
            watch,
        }
    }

    pub(crate) fn attach(mut self, files: &mut Files) -> u64 {
        if let Carrier::Standalone(socket) = &mut self.resource.carrier {
            socket.readiness = files
                .sockets
                .attach(self.watch.take().unwrap())
                .unwrap_or_else(|_| {
                    crate::run_stop::raise(crate::run_stop::Stop::from_static(b"HostPoll"))
                });
        }
        files.udp_sockets.insert(self.key, self.resource);
        self.key
    }
}

impl From<Address> for SocketAddr {
    fn from(address: Address) -> Self {
        let ip = if address.family == 0 {
            IpAddr::V4(Ipv4Addr::from(address.lo as u32))
        } else {
            IpAddr::V6(Ipv6Addr::from(
                u128::from(address.hi) << 64 | u128::from(address.lo),
            ))
        };
        SocketAddr::new(ip, address.port)
    }
}

impl From<SocketAddr> for Address {
    fn from(address: SocketAddr) -> Self {
        let (family, hi, lo) = match address.ip() {
            IpAddr::V4(ip) => (0, 0, u64::from(u32::from(ip))),
            IpAddr::V6(ip) => {
                let n = u128::from(ip);
                (1, (n >> 64) as u64, n as u64)
            }
        };
        Self {
            family,
            hi,
            lo,
            port: address.port(),
        }
    }
}

// The flattened Address used by host calls stores V4 in lo, while Core's Ip
// case stores its u32 at the first payload offset. Read only that case's bytes.
#[repr(C)]
#[derive(Clone, Copy)]
union IpPayload {
    v4: u32,
    v6: [u64; 2],
}

#[repr(C)]
#[derive(Clone, Copy)]
struct CoreAddr {
    family: u8,
    ip: IpPayload,
    port: u16,
}

impl From<CoreAddr> for SocketAddr {
    fn from(address: CoreAddr) -> Self {
        // SAFETY: a typed Addr initializes the union member indicated by its case.
        let ip = unsafe {
            match address.family {
                0 => IpAddr::V4(Ipv4Addr::from(address.ip.v4)),
                1 => {
                    let [hi, lo] = address.ip.v6;
                    IpAddr::V6(Ipv6Addr::from(u128::from(hi) << 64 | u128::from(lo)))
                }
                _ => unreachable!("invalid Core Ip case"),
            }
        };
        SocketAddr::new(ip, address.port)
    }
}

impl From<SocketAddr> for CoreAddr {
    fn from(address: SocketAddr) -> Self {
        let (family, ip) = match address.ip() {
            IpAddr::V4(ip) => (0, IpPayload { v4: u32::from(ip) }),
            IpAddr::V6(ip) => {
                let n = u128::from(ip);
                (
                    1,
                    IpPayload {
                        v6: [(n >> 64) as u64, n as u64],
                    },
                )
            }
        };
        Self {
            family,
            ip,
            port: address.port(),
        }
    }
}

// The packet types are single-case Core variants with aligned payloads.
#[repr(C)]
#[derive(Clone, Copy)]
struct TxCarrier {
    case: u8,
    to: CoreAddr,
    at: u64,
    len: u64,
    ecn: u8,
}

impl From<TxCarrier> for TxPacket {
    fn from(packet: TxCarrier) -> Self {
        Self {
            to: packet.to.into(),
            at: packet.at,
            len: packet.len,
            ecn: packet.ecn,
        }
    }
}

#[repr(C)]
struct PacketCarrier {
    case: u8,
    peer: CoreAddr,
    at: u64,
    len: u64,
    truncated: bool,
    ecn: u8,
}

pub(crate) struct Operations {
    pub(crate) step: unsafe fn(*mut Request, u64) -> Step,
    pub(crate) abort: unsafe fn(*mut Request, u64, u8) -> Outcome,
}

pub(crate) static OPERATIONS: Operations = Operations { step, abort };

struct Call {
    operations: &'static Operations,
    files: *mut Files,
    provider: Provider,
    until: Option<(Provider, u64, u64)>,
    endpoint: Address,
    options: Options,
    group: Option<(NonZeroU16, Steer)>,
    rules: Arc<UdpRules>,
    key: u64,
    charge: Option<Charge>,
    bytes: *mut Buffer,
    packets: *mut Buffer,
    progress: usize,
    receive_deferred: bool,
    served: served::Delivery,
    #[cfg(feature = "record")]
    tape_start: (usize, usize),
}

#[derive(Default)]
pub(crate) struct Waiters(Mutex<BTreeMap<u64, crate::tasks::Outside>>);

impl Waiters {
    fn publish(&self, token: u64) {
        self.0
            .lock()
            .unwrap()
            .entry(token)
            .or_insert_with(crate::tasks::Outside::begin);
    }

    fn take(&self, token: u64) -> Option<crate::tasks::Outside> {
        self.0.lock().unwrap().remove(&token)
    }

    fn ready(&self, token: u64) {
        if let Some(waiter) = self.take(token) {
            waiter.complete(token);
        }
    }
}

fn complete(outcome: Outcome) -> Step {
    Step {
        outcome,
        ..Step::default()
    }
}

fn count(value: usize) -> Outcome {
    Outcome {
        value: value as u64,
        ..Outcome::default()
    }
}

fn oversize(max: u32) -> Outcome {
    Outcome {
        error: IoError {
            tag: OVERSIZE,
            code: max,
        },
        ..Outcome::default()
    }
}

fn transport(problem: io::Error) -> Outcome {
    match problem.raw_os_error().unwrap_or(libc::EIO) {
        libc::EMSGSIZE => oversize(0),
        libc::EADDRINUSE => signal(4),
        libc::ENFILE | libc::EMFILE | libc::ENOMEM | libc::ENOBUFS => signal(EXHAUSTED),
        code => error(code),
    }
}

unsafe fn operands<'a>(carrier: *const Buffer) -> &'a [TxCarrier] {
    // SAFETY: the caller pins a typed List[UdpTx] through issue and delivery.
    unsafe {
        if (*carrier).len == 0 {
            &[]
        } else {
            std::slice::from_raw_parts((*carrier).data.cast(), (*carrier).len)
        }
    }
}

pub(crate) unsafe fn operations(request: *const Request) -> &'static Operations {
    // SAFETY: UDP operations carry a pinned Call.
    unsafe { (*(*request).path.cast::<Call>()).operations }
}

pub(crate) unsafe fn step(request: *mut Request, token: u64) -> Step {
    // SAFETY: UDP operations carry a pinned Call and the unit owns its socket table.
    unsafe {
        let request = &mut *request;
        let call = &mut *request.path.cast_mut().cast::<Call>();
        match request.operation {
            UDP_OPEN | UDP_BIND | UDP_BIND_GROUP => {
                return complete(call.create(request.operation != UDP_OPEN));
            }
            _ => {}
        }
        let files = &mut *call.files;
        let resource = files.udp_sockets.get_mut(&call.key).unwrap();
        if request.operation == UDP_LOCAL_ADDR {
            return complete(match resource.socket().local_addr() {
                Ok(value) => {
                    call.endpoint = value.into();
                    Outcome::default()
                }
                Err(problem) => transport(problem),
            });
        }
        let sending = request.operation == UDP_SEND;
        let waiters = &run_state(files.registry.run_grants()).waiters;
        resource.cancel_waiter(sending, token, waiters);
        let due = deadline(call.until, token);
        let mut runnable = false;
        if sending {
            let packets = operands(call.packets);
            let bytes = crate::tls::bytes(&*call.bytes);
            while call.progress < packets.len() {
                if deadline(call.until, token) == Due::Passed {
                    return complete(if call.progress == 0 {
                        signal(6)
                    } else {
                        count(call.progress)
                    });
                }
                let ends = (call.progress + 64).min(packets.len());
                let from = &packets[call.progress..ends];
                let mut window = [TxPacket::from(from[0]); 64];
                for (to, from) in window.iter_mut().zip(from) {
                    *to = TxPacket::from(*from);
                }
                match resource.send(bytes, &window[..from.len()], waiters, token) {
                    Ok(Window::Sent(n)) => {
                        call.progress += n;
                        resource.receiving |= n != 0;
                        if n == 0 {
                            break;
                        }
                    }
                    Ok(Window::RetryAsSingles) => continue,
                    Err(problem) if problem.kind() == io::ErrorKind::WouldBlock => break,
                    Err(problem) if problem.kind() == io::ErrorKind::Interrupted => continue,
                    Err(problem) => {
                        return complete(if call.progress == 0 {
                            transport(problem)
                        } else {
                            count(call.progress)
                        });
                    }
                }
            }
            if call.progress == packets.len() {
                return complete(count(call.progress));
            }
        } else {
            match call.receive(resource, &mut files.sockets) {
                Ok(Received::Batch(n)) => return complete(count(n)),
                Ok(Received::Empty) => {}
                Ok(Received::BudgetExhausted) => {
                    // An expired receive still gets a nonblocking attempt. If the turn's
                    // budget blocked that attempt, let the poller give it one fresh turn.
                    runnable = due != Due::Passed || !call.receive_deferred;
                    call.receive_deferred = true;
                }
                Err(problem) if problem.kind() == io::ErrorKind::Interrupted => {
                    runnable = due != Due::Passed;
                }
                Err(problem) if problem.kind() == io::ErrorKind::WouldBlock => {}
                Err(problem) => return complete(transport(problem)),
            }
        }
        if due == Due::Passed && !runnable {
            resource.cancel_waiter(sending, token, waiters);
            return complete(if call.progress != 0 {
                count(call.progress)
            } else {
                signal(6)
            });
        }
        if runnable {
            let outside = crate::tasks::Outside::begin();
            files
                .sockets
                .udp_poller()
                .unwrap()
                .defer_udp_reader(move || outside.complete(token));
        } else {
            match &mut resource.carrier {
                Carrier::Standalone(socket) => {
                    if let Err(problem) = files.sockets.arm(socket.readiness, sending, token) {
                        return complete(transport(problem));
                    }
                    crate::tasks::attach_net(&mut files.sockets, files.sockets.poller().unwrap());
                }
                Carrier::Member(member) if !sending => {
                    waiters.publish(token);
                    member
                        .reader()
                        .publish_waiter(NonZeroU64::new(token).unwrap());
                    if !member.reader().recheck_waiter() {
                        drop(waiters.take(token));
                        if let Ok(Received::Batch(n)) = call.receive(resource, &mut files.sockets) {
                            return complete(count(n));
                        }
                        unreachable!("only this reader removes its queued datagrams");
                    }
                }
                Carrier::Member(_) => {}
                Carrier::Served(_) => unreachable!(),
                #[cfg(feature = "record")]
                Carrier::Replayed { .. } => unreachable!(),
            }
        }
        Step {
            kind: STEP_PENDING,
            deadline_ns: due.host().unwrap_or(0),
            ..Step::default()
        }
    }
}

pub(crate) unsafe fn abort(request: *mut Request, token: u64, reason: u8) -> Outcome {
    // SAFETY: an outstanding UDP request pins its Call and owned socket.
    unsafe {
        let request = &*request;
        let call = &mut *request.path.cast_mut().cast::<Call>();
        let files = &mut *call.files;
        if let Some(resource) = files.udp_sockets.get_mut(&call.key) {
            let sending = request.operation == UDP_SEND;
            resource.cancel_waiter(
                sending,
                token,
                &run_state(files.registry.run_grants()).waiters,
            );
            if let Carrier::Standalone(socket) = &resource.carrier {
                files.sockets.disarm(socket.readiness, sending);
            }
        }
        if call.progress != 0 {
            count(call.progress)
        } else if reason == ABORT_DEADLINE {
            signal(6)
        } else {
            signal(7)
        }
    }
}

impl Call {
    unsafe fn run(
        &mut self,
        context: *mut Context,
        operation: u8,
        handle: u64,
        site: u64,
    ) -> Outcome {
        let mut request = Request {
            operation,
            mode: 0,
            handle,
            path: std::ptr::from_mut(self).cast(),
            path_len: 0,
            buffer: Buffer {
                data: ptr::null_mut(),
                len: 0,
                capacity: 0,
            },
            site,
        };
        let provider = self.provider;
        // SAFETY: the typed request pins every operand and the resolved provider through the wait.
        unsafe { hirsel_core_host(context, &provider, &mut request) }
    }
}

struct Creation {
    endpoint: Address,
    slots: u32,
    slot_bytes: u32,
    group: Option<(NonZeroU16, Steer)>,
    output: *mut Buffer,
}

unsafe fn create(
    context: *mut Context,
    net: u64,
    create: Creation,
    operation: u8,
    site: u64,
) -> AddressOutcome {
    let Creation {
        endpoint,
        slots,
        slot_bytes,
        group,
        output,
    } = create;
    let fail = |outcome| AddressOutcome::new(outcome, Address::default());
    let Some(options) = Options::new(slots, slot_bytes) else {
        return fail(error(libc::EINVAL));
    };
    if endpoint.family > 1 {
        return fail(DENIED);
    }
    // SAFETY: types pin the Net, its rule rows and the caller's exclusive context.
    unsafe {
        crate::process::deny_describe_capability(context);
        let files = (*context).host_files;
        let provider = *hirsel_core_capability_provider(context, net);
        let handle = hirsel_core_capability_handle(context, net);
        if !crate::provide::grant_live(&provider, handle) {
            return fail(crate::provide::withdrawn(context, site));
        }
        let served = crate::provide::owns(&provider);
        #[cfg(feature = "record")]
        let taped = crate::tape::owns(&provider);
        #[cfg(not(feature = "record"))]
        let taped = false;
        if !served && !taped && !crate::os::owns(&provider) {
            return fail(DENIED);
        }
        #[cfg(feature = "record")]
        let rules_id = if taped {
            crate::tape::net_rules(&provider, handle)
        } else {
            handle
        };
        #[cfg(not(feature = "record"))]
        let rules_id = handle;
        if rules_id == 0 {
            return fail(DENIED);
        }
        let rules_id = std::ptr::from_ref(Rules::from_handle(rules_id)) as u64;
        let rules = {
            let mut cache = run_state((*files).registry.run_grants())
                .rules
                .lock()
                .unwrap();
            cache
                .entry(rules_id)
                .or_insert_with(|| {
                    let rows = Rules::from_handle(rules_id).as_slice();
                    Arc::new(UdpRules::new(
                        rows.iter()
                            .filter_map(hirsel_core_net::GrantRule::as_address),
                    ))
                })
                .clone()
        };
        if operation != UDP_OPEN
            && let Some(tag) = rules.bind_denial(endpoint.into())
        {
            return fail(signal(tag));
        }
        let members = group.map(|(members, _)| members);
        let bytes = options.charge(members) + 8 * members.map_or(1, |n| u64::from(n.get()));
        let payer = (*context).payer();
        if let Err(capacity) = payer.admit(Some(bytes)) {
            crate::abi::hirsel_core_raise_capacity(
                context,
                site,
                capacity.requested,
                capacity.available,
            );
            return fail(Outcome {
                status: 1,
                ..Outcome::default()
            });
        }
        let charge = Charge::new(payer, bytes);
        let mut call = Call {
            operations: &OPERATIONS,
            files,
            provider,
            until: None,
            endpoint,
            options,
            group,
            rules,
            key: if served {
                let handles = members.map_or(1, |n| 1 + u64::from(n.get()));
                let Some(key) = run_state((*files).registry.run_grants())
                    .allowance
                    .handles(handles)
                else {
                    return fail(signal(EXHAUSTED));
                };
                key
            } else {
                0
            },
            charge: Some(charge),
            bytes: ptr::null_mut(),
            packets: output,
            progress: 0,
            receive_deferred: false,
            served: served::Delivery::default(),
            #[cfg(feature = "record")]
            tape_start: (0, if output.is_null() { 0 } else { (*output).len }),
        };
        let mut outcome = call.run(context, operation, handle, site);
        if served && outcome.status == 0 && outcome.error == IoError::OK {
            outcome = call.install_served(operation);
        }
        #[cfg(feature = "record")]
        if taped && outcome.status == 0 && outcome.error == IoError::OK {
            call.key = outcome.value;
            outcome = call.install_recorded(operation);
        }
        AddressOutcome::new(outcome, call.endpoint)
    }
}

#[allow(clippy::too_many_arguments)]
unsafe fn existing(
    context: *mut Context,
    key: u64,
    operation: u8,
    bytes: *mut Buffer,
    packets: *mut Buffer,
    until_kind: u8,
    clock: u64,
    at: u64,
    site: u64,
) -> AddressOutcome {
    let fail = |outcome| AddressOutcome::new(outcome, Address::default());
    // SAFETY: the running task owns its socket and pins any carrier through delivery.
    unsafe {
        let files = (*context).host_files;
        let resource = (*files).udp_sockets.get(&key).unwrap();
        if let Carrier::Served(socket) = &resource.carrier
            && !crate::provide::socket_live(&resource.provider, socket.key())
        {
            return fail(crate::provide::withdrawn(context, site));
        }
        if operation != UDP_SEND && !resource.receiving {
            return fail(DENIED);
        }
        let charge = |steps| crate::steps::take_many(context, steps, site);
        let out_of_time = || {
            fail(Outcome {
                status: 1,
                ..Outcome::default()
            })
        };
        if operation == UDP_RECV {
            let entries = (*packets).capacity - (*packets).len;
            if entries == 0 {
                return fail(Outcome::default());
            }
            let room = (*bytes).capacity - (*bytes).len;
            let (entry_steps, byte_steps) =
                resource.options.receive_steps(room as u64, entries as u64);
            if !charge(entry_steps) || !charge(byte_steps) {
                return out_of_time();
            }
        }
        if operation == UDP_SEND {
            let packets = operands(packets);
            if !charge(udp::send_entry_steps(packets.len() as u64)) {
                return out_of_time();
            }
            let mut byte_steps = match udp::send_byte_steps(
                packets.iter().copied().map(TxPacket::from),
                (*bytes).len as u64,
                resource.v6,
                &resource.rules,
            ) {
                Ok(steps) => steps,
                Err(udp::SendRefusal::Denied) => return fail(DENIED),
                Err(udp::SendRefusal::Oversize(max)) => return fail(oversize(max)),
            };
            while byte_steps != 0 {
                let chunk = byte_steps.min(u128::from(u64::MAX)) as u64;
                if !charge(chunk) {
                    return out_of_time();
                }
                byte_steps -= u128::from(chunk);
            }
        }
        let until = (until_kind != 0).then(|| {
            (
                *hirsel_core_capability_provider(context, clock),
                hirsel_core_capability_handle(context, clock),
                at,
            )
        });
        let mut call = Call {
            operations: &OPERATIONS,
            files,
            provider: resource.provider,
            until,
            endpoint: Address {
                family: u8::from(resource.v6),
                ..Address::default()
            },
            options: resource.options,
            group: None,
            rules: resource.rules.clone(),
            key,
            charge: None,
            bytes,
            packets,
            progress: 0,
            receive_deferred: false,
            served: served::Delivery::default(),
            #[cfg(feature = "record")]
            tape_start: (
                if bytes.is_null() { 0 } else { (*bytes).len },
                if packets.is_null() { 0 } else { (*packets).len },
            ),
        };
        let port = match &resource.carrier {
            Carrier::Served(socket) => socket.key(),
            _ => key,
        };
        #[cfg(feature = "record")]
        let port = resource.tape_id.unwrap_or(port);
        call.served.ingress = !resource.bound;
        if port != key && operation == UDP_SEND {
            call.served.encode(operands(packets));
        }
        let outcome = call.run(context, operation, port, site);
        if port != key && operation == UDP_SEND && outcome.value != 0 {
            (*files).udp_sockets.get_mut(&key).unwrap().receiving = true;
        }
        AddressOutcome::new(outcome, call.endpoint)
    }
}
