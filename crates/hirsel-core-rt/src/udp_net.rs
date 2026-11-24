//! Native UDP carriers and provider requests.

mod abi;

use crate::{
    Context,
    entry::{hirsel_core_capability_handle, hirsel_core_capability_provider},
    host_file::Files,
    limit::Payer,
    net::{Address, AddressOutcome, DENIED, Rules},
    os_net::{Due, deadline, error, signal},
    provider::*,
    secret::{Charge, ChargeCell, HandedOver},
};
use hirsel_core_net::UdpMember;
use hirsel_core_net::udp::{
    self, Captured, Options, ReceivePool, Socket, Steer, TurnBudget, TxPacket, UdpRules, Window,
};
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
        let bytes = std::env::var("HIRSEL_UDP_KERNEL_BYTES")
            .ok()
            .map_or(Some(64 * 1024 * 1024), |bytes| bytes.parse().ok());
        let sysctl = |name| {
            std::fs::read_to_string(format!("/proc/sys/net/core/{name}"))
                .ok()
                .and_then(|n| n.trim().parse::<u64>().ok())
        };
        Self {
            left: AtomicU64::new(bytes.unwrap_or(0)),
            per_socket: sysctl("rmem_default")
                .zip(sysctl("wmem_default"))
                .and_then(|(r, w)| r.checked_add(w))
                .unwrap_or(u64::MAX),
            next: AtomicU64::new(1),
            charges: Mutex::default(),
        }
    }
}

impl Allowance {
    pub(crate) fn hand_over(&self, payer: Payer) -> (HandedOver, Vec<Charge>) {
        let charges = self.charges.lock().unwrap();
        let mut handed = Vec::new();
        let mut refunds = Vec::new();
        for weak in charges.values() {
            if let Some(cell) = weak.upgrade()
                && let Some(charge) = cell.take_charge(payer)
            {
                handed.push((weak.clone(), charge.bytes()));
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

struct SocketCharge {
    allowance: Arc<Allowance>,
    key: u64,
    _cell: Arc<ChargeCell>,
}

impl SocketCharge {
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
}

pub(crate) struct Resource {
    carrier: Carrier,
    provider: Provider,
    rules: Arc<UdpRules>,
    options: Options,
    v6: bool,
    bound: bool,
    receiving: bool,
}

impl Resource {
    fn socket(&self) -> &Socket {
        match &self.carrier {
            Carrier::Standalone(socket) => &socket.socket,
            Carrier::Member(member) => member.socket(),
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

pub(crate) unsafe fn drop_socket(files: *mut Files, key: u64) {
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
            Some(
                files
                    .sockets
                    .detach(socket.readiness)
                    .unwrap_or_else(|_| crate::process::stop(b"HostPoll")),
            )
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
                .unwrap_or_else(|_| crate::process::stop(b"HostPoll"));
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

struct Call {
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

enum Received {
    Batch(usize),
    Empty,
    Runnable,
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

impl Call {
    unsafe fn create(&mut self, bind: bool) -> Outcome {
        // SAFETY: the request is executing on the owning unit and pins Files.
        unsafe {
            let files = &mut *self.files;
            let allowance = files.registry.run_grants().udp_allowance.clone();
            let keys = self
                .group
                .map_or(1, |(members, _)| 1 + u64::from(members.get()));
            let (Some(kernel), Some(key)) = (allowance.reserve(), allowance.handles(keys)) else {
                return signal(EXHAUSTED);
            };
            let v6 = self.endpoint.family == 1;
            let socket = match Socket::open(v6, self.options) {
                Ok(socket) => socket,
                Err(problem) => return transport(problem),
            };
            if bind {
                match socket.bind(self.endpoint.into()) {
                    Ok(endpoint) => self.endpoint = endpoint.into(),
                    Err(problem) => return transport(problem),
                }
            }
            if let Some((members, steer)) = self.group {
                let poller = match files.sockets.udp_poller() {
                    Ok(poller) => poller,
                    Err(problem) => return transport(problem),
                };
                let n = usize::from(members.get());
                let mut charge = self.charge.take().unwrap();
                let per_member = self.options.charge(None) + 8;
                let mut charges = Vec::new();
                if charges.try_reserve_exact(n).is_err() {
                    return signal(EXHAUSTED);
                }
                for i in 0..n {
                    charges.push(Some(SocketCharge::new(
                        allowance.clone(),
                        key + i as u64,
                        charge.partition(per_member),
                    )));
                }
                let charge = SocketCharge::new(allowance, key + n as u64, charge);
                let waiters = files.registry.run_grants().udp_waiters.clone();
                let opened = poller.add_udp_group(
                    socket,
                    self.options,
                    members,
                    steer,
                    move |token| waiters.ready(token),
                    move |index| {
                        let _keep_intake = (&charge, &kernel);
                        drop(charges[index].take().unwrap());
                    },
                );
                let opened = match opened {
                    Ok(opened) => opened,
                    Err(problem) => return transport(problem),
                };
                let output = &mut *self.packets;
                for (index, member) in opened.into_iter().enumerate() {
                    let handle = key + index as u64;
                    files.udp_sockets.insert(
                        handle,
                        Resource {
                            carrier: Carrier::Member(member),
                            provider: self.provider,
                            rules: self.rules.clone(),
                            options: self.options,
                            v6,
                            bound: true,
                            receiving: true,
                        },
                    );
                    output.data.cast::<u64>().add(output.len).write(handle);
                    output.len += 1;
                }
                crate::tasks::attach_net(&mut files.sockets, poller);
                return Outcome::default();
            }
            let pool = match ReceivePool::new(self.options) {
                Ok(pool) => pool,
                Err(problem) => return transport(problem),
            };
            let Some(readiness) = files.sockets.insert_shared(socket.descriptor()) else {
                return signal(EXHAUSTED);
            };
            files.udp_sockets.insert(
                key,
                Resource {
                    carrier: Carrier::Standalone(Standalone {
                        socket,
                        pool,
                        readiness,
                        _kernel: kernel,
                        _charge: SocketCharge::new(allowance, key, self.charge.take().unwrap()),
                    }),
                    provider: self.provider,
                    rules: self.rules.clone(),
                    options: self.options,
                    v6,
                    bound: bind,
                    receiving: bind,
                },
            );
            Outcome {
                value: key,
                ..Outcome::default()
            }
        }
    }

    unsafe fn receive(&self, resource: &mut Resource) -> io::Result<Received> {
        // SAFETY: the current request exclusively owns both typed output carriers.
        let (bytes, packets) = unsafe { (&mut *self.bytes, &mut *self.packets) };
        let mut budget = TurnBudget::default();
        let mut n = 0;
        while packets.len < packets.capacity {
            let captured = match &mut resource.carrier {
                Carrier::Standalone(socket) => socket.pool.front(),
                Carrier::Member(member) => member.reader().front(),
            };
            match captured {
                Some(Captured::Discarded) if budget.dispatch(0) => {
                    let Carrier::Standalone(socket) = &mut resource.carrier else {
                        unreachable!()
                    };
                    socket.pool.advance();
                }
                Some(Captured::Discarded) if n == 0 => return Ok(Received::Runnable),
                Some(Captured::Datagram {
                    peer,
                    ecn,
                    length,
                    payload,
                }) => {
                    let appended = (bytes.capacity - bytes.len).min(payload.len());
                    if n != 0 && appended != length {
                        break;
                    }
                    let packet = PacketCarrier {
                        case: 0,
                        peer: peer.into(),
                        at: bytes.len as u64,
                        len: length as u64,
                        truncated: appended != length,
                        ecn,
                    };
                    // SAFETY: both appends are within spare capacity and the pool is separate storage.
                    unsafe {
                        if appended != 0 {
                            ptr::copy_nonoverlapping(
                                payload.as_ptr(),
                                bytes.data.add(bytes.len),
                                appended,
                            );
                        }
                        packets
                            .data
                            .cast::<PacketCarrier>()
                            .add(packets.len)
                            .write(packet);
                    }
                    bytes.len += appended;
                    packets.len += 1;
                    n += 1;
                    match &mut resource.carrier {
                        Carrier::Standalone(socket) => socket.pool.advance(),
                        Carrier::Member(member) => member.reader().advance(),
                    }
                }
                None if n == 0 => {
                    let Carrier::Standalone(socket) = &mut resource.carrier else {
                        break;
                    };
                    let ingress = (!resource.bound).then_some(resource.rules.as_ref());
                    if socket.pool.read(&socket.socket, ingress, &mut budget)? == 0 {
                        return Ok(Received::Runnable);
                    }
                }
                _ => break,
            }
        }
        Ok(if n == 0 {
            Received::Empty
        } else {
            Received::Batch(n)
        })
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
        let waiters = &files.registry.run_grants().udp_waiters;
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
            match call.receive(resource) {
                Ok(Received::Batch(n)) => return complete(count(n)),
                Ok(Received::Empty) => {}
                Ok(Received::Runnable) => runnable = true,
                Err(problem) if problem.kind() == io::ErrorKind::Interrupted => runnable = true,
                Err(problem) if problem.kind() == io::ErrorKind::WouldBlock => {}
                Err(problem) => return complete(transport(problem)),
            }
        }
        if due == Due::Passed {
            resource.cancel_waiter(sending, token, waiters);
            return complete(if call.progress != 0 {
                count(call.progress)
            } else {
                signal(6)
            });
        }
        if runnable {
            crate::tasks::Outside::begin().complete(token);
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
                        if let Ok(Received::Batch(n)) = call.receive(resource) {
                            return complete(count(n));
                        }
                        unreachable!("only this reader removes its queued datagrams");
                    }
                }
                Carrier::Member(_) => {}
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
            resource.cancel_waiter(sending, token, &files.registry.run_grants().udp_waiters);
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
        return fail(DENIED);
    };
    if endpoint.family > 1 {
        return fail(DENIED);
    }
    // SAFETY: types pin the Net, its rule rows and the caller's exclusive context.
    unsafe {
        crate::process::deny_describe_capability(context);
        let files = (*context).host_files;
        let provider = *hirsel_core_capability_provider(context, net);
        if !crate::os::owns(&provider) {
            return fail(DENIED);
        }
        let handle = hirsel_core_capability_handle(context, net);
        let rules = {
            let mut cache = (*files).registry.run_grants().udp_rules.lock().unwrap();
            cache
                .entry(handle)
                .or_insert_with(|| {
                    let rows = (*(handle as *const Rules)).as_slice();
                    Arc::new(UdpRules::new(rows.iter().map(|row| row.address)))
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
        let mut call = Call {
            files,
            provider,
            until: None,
            endpoint,
            options,
            group,
            rules,
            key: 0,
            charge: Some(Charge::new(payer, bytes)),
            bytes: ptr::null_mut(),
            packets: output,
            progress: 0,
        };
        let outcome = call.run(context, operation, handle, site);
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
        let resource = (&(*files).udp_sockets).get(&key).unwrap();
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
            files,
            provider: resource.provider,
            until,
            endpoint: Address::default(),
            options: resource.options,
            group: None,
            rules: resource.rules.clone(),
            key,
            charge: None,
            bytes,
            packets,
            progress: 0,
        };
        let outcome = call.run(context, operation, key, site);
        AddressOutcome::new(outcome, call.endpoint)
    }
}
