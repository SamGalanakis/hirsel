//! Canonical UDP operands and checked packet deliveries for the tape provider.
use super::*;
use hirsel_tape_format::{Args, Corrupt, Cursor, OK, Out, put_varint};

pub(crate) const RECORD_BYTES: usize = 36;
pub(crate) const CLOSE_MODE: u8 = 0x50;
pub(crate) const CLOSE_ARGS: &[u8] = &[3];

struct Close {
    files: *mut Files,
    key: u64,
}

pub(super) unsafe fn close(
    context: *mut Context,
    files: *mut Files,
    key: u64,
    provider: Provider,
    tape_id: u64,
) {
    let mut call = Close { files, key };
    let mut request = Request {
        operation: CLOSE,
        mode: CLOSE_MODE,
        handle: tape_id,
        path: (&raw mut call).cast(),
        path_len: 0,
        buffer: crate::net::empty_buffer(),
        site: 0,
    };
    // SAFETY: the destructor uniquely owns the live UDP resource.
    unsafe { hirsel_core_host(context, &provider, &mut request) };
    // SAFETY: a replay, or a fault before source issue, leaves the local resource here.
    if unsafe { (*files).udp_sockets.contains_key(&key) } {
        unsafe { drop_native(files, key) };
    }
}

pub(crate) unsafe fn close_source(request: &Request) {
    // SAFETY: the UDP CLOSE pins its sole-owned local key until issue.
    let call = unsafe { &*request.path.cast::<Close>() };
    unsafe { drop_native(call.files, call.key) };
}

pub(crate) fn creates(operation: u8) -> bool {
    matches!(operation, UDP_OPEN | UDP_BIND | UDP_BIND_GROUP)
}

fn put_address(output: &mut Vec<u8>, address: Address) {
    output.push(address.family);
    output.extend_from_slice(&address.hi.to_le_bytes());
    output.extend_from_slice(&address.lo.to_le_bytes());
    output.extend_from_slice(&address.port.to_le_bytes());
}

fn address(input: &mut Cursor<'_>) -> Result<Address, Corrupt> {
    let address = Address {
        family: input.u8()?,
        hi: u64::from_le_bytes(input.fixed()?),
        lo: u64::from_le_bytes(input.fixed()?),
        port: u16::from_le_bytes(input.fixed()?),
    };
    match address.family {
        0 if address.hi == 0 && address.lo == u64::from(address.lo as u32) => Ok(address),
        1 => Ok(address),
        _ => Err(Corrupt),
    }
}

pub(crate) unsafe fn arguments(request: &Request) -> Args {
    // SAFETY: a UDP request pins its typed operands.
    unsafe {
        let call = &*request.path.cast::<Call>();
        let input = served::inputs(request);
        let mut header = Vec::new();
        if creates(request.operation) {
            put_address(&mut header, input.endpoint);
            for n in [input.slots, input.slot_bytes, input.members, input.steer] {
                put_varint(&mut header, n);
            }
        }
        if matches!(request.operation, UDP_RECV | UDP_SEND) {
            if let Some((_, clock, at)) = call.until {
                put_varint(&mut header, u64::from(clock as u32));
                header.extend_from_slice(&at.to_le_bytes());
            } else {
                header.push(0);
            }
        }
        match request.operation {
            UDP_RECV => Args::of(&header).buffered([input.room, input.entries]),
            UDP_SEND => {
                put_varint(&mut header, input.room);
                put_varint(&mut header, input.entries);
                Args::of_parts(&[
                    &header,
                    served::payload(request, 2),
                    served::payload(request, 1),
                ])
            }
            _ => Args::of(&header),
        }
    }
}

pub(crate) unsafe fn created(request: &Request, value: u64) -> Vec<u64> {
    if request.operation != UDP_BIND_GROUP {
        return vec![value];
    }
    // SAFETY: the successful group creation initialized exactly members handles.
    unsafe {
        let call = &*request.path.cast::<Call>();
        let packets = &*call.packets;
        std::slice::from_raw_parts(
            packets.data.cast::<u64>().add(call.tape_start.1),
            packets.len - call.tape_start.1,
        )
        .to_vec()
    }
}

pub(crate) unsafe fn record(request: &Request, output: &mut Out, handles: Vec<u64>) {
    // SAFETY: the source completed the same pinned operands.
    unsafe {
        let call = &mut *request.path.cast_mut().cast::<Call>();
        match request.operation {
            UDP_OPEN | UDP_BIND | UDP_BIND_GROUP | UDP_LOCAL_ADDR => {
                put_address(&mut output.payload, call.endpoint);
                call.served.ids = handles;
            }
            UDP_RECV => {
                let bytes = &*call.bytes;
                let packets = &*call.packets;
                output.written = (bytes.len - call.tape_start.0) as u64;
                let count = packets.len - call.tape_start.1;
                let records = std::slice::from_raw_parts(
                    packets.data.cast::<PacketCarrier>().add(call.tape_start.1),
                    count,
                );
                for (index, packet) in records.iter().enumerate() {
                    let end = records.get(index + 1).map_or(bytes.len as u64, |p| p.at);
                    put_address(&mut output.payload, SocketAddr::from(packet.peer).into());
                    output.payload.extend_from_slice(&packet.len.to_le_bytes());
                    output
                        .payload
                        .extend_from_slice(&(end - packet.at).to_le_bytes());
                    output.payload.push(packet.ecn);
                }
                if output.written != 0 {
                    output.payload.extend_from_slice(std::slice::from_raw_parts(
                        bytes.data.add(call.tape_start.0),
                        output.written as usize,
                    ));
                }
            }
            _ => {}
        }
    }
}

pub(crate) unsafe fn replay(
    request: &Request,
    out: &Out,
    next_ordinal: u64,
    replay_capacity: Option<[u64; 2]>,
) -> Result<usize, Corrupt> {
    if out.status != OK {
        return if out.value == 0 && out.written == 0 && out.payload.is_empty() {
            Ok(0)
        } else {
            Err(Corrupt)
        };
    }
    // SAFETY: the live request pins its operands; delivery follows validation.
    unsafe {
        let call = &mut *request.path.cast_mut().cast::<Call>();
        match request.operation {
            UDP_OPEN | UDP_BIND | UDP_BIND_GROUP | UDP_LOCAL_ADDR => {
                let mut payload = Cursor::new(&out.payload);
                let endpoint = address(&mut payload)?;
                if !payload.done() || endpoint.family != call.endpoint.family {
                    return Err(Corrupt);
                }
                if request.operation == UDP_LOCAL_ADDR {
                    if out.value != 0 || endpoint.port == 0 {
                        return Err(Corrupt);
                    }
                    call.endpoint = endpoint;
                    return Ok(0);
                }
                let count = call.group.map_or(1, |(n, _)| usize::from(n.get()));
                if !valid_creation(
                    call.endpoint,
                    endpoint,
                    request.operation != UDP_OPEN,
                    out.value,
                    count as u64,
                    count as u64,
                    |ordinals| {
                        *ordinals.start() == next_ordinal && *ordinals.end() <= u64::from(u32::MAX)
                    },
                ) {
                    return Err(Corrupt);
                }
                call.endpoint = endpoint;
                Ok(count)
            }
            UDP_SEND => {
                if out.value > (*call.packets).len as u64 {
                    return Err(Corrupt);
                }
                Ok(0)
            }
            UDP_RECV => {
                let [room, entries] = replay_capacity.ok_or(Corrupt)?;
                let count = usize::try_from(out.value).map_err(|_| Corrupt)?;
                let total = usize::try_from(out.written).map_err(|_| Corrupt)?;
                let metadata = count.checked_mul(RECORD_BYTES).ok_or(Corrupt)?;
                let (records, bytes) = out.payload.split_at_checked(metadata).ok_or(Corrupt)?;
                if bytes.len() != total {
                    return Err(Corrupt);
                }
                let mut packets = Vec::new();
                packets.try_reserve_exact(count).map_err(|_| Corrupt)?;
                let mut records = Cursor::new(records);
                for _ in 0..count {
                    packets.push(udp::Packet {
                        peer: address(&mut records)?.into(),
                        len: u64::from_le_bytes(records.fixed()?),
                        appended: u64::from_le_bytes(records.fixed()?),
                        ecn: records.u8()?,
                    });
                }
                if !udp::valid_receive(
                    call.endpoint.family == 1,
                    call.served.ingress.then_some(call.rules.as_ref()),
                    call.options.payload_room(),
                    room,
                    entries,
                    &packets,
                    total,
                ) {
                    return Err(Corrupt);
                }
                served::deliver_receive(request, &packets, bytes);
                Ok(0)
            }
            _ => Err(Corrupt),
        }
    }
}

pub(crate) unsafe fn replay_handles(request: &Request, handles: Vec<u64>) {
    // SAFETY: the delivery pins the UDP creation extension.
    unsafe { (*request.path.cast_mut().cast::<Call>()).served.ids = handles };
}

impl Call {
    pub(super) unsafe fn install_recorded(&mut self, operation: u8) -> Outcome {
        // SAFETY: the successful creation pinned this unit and completed its output.
        unsafe {
            let files = &mut *self.files;
            let allowance = super::run_state(files.registry.run_grants())
                .allowance
                .clone();
            if let Some(mut charge) = self.charge.take() {
                let count = self.served.ids.len() as u64;
                let Some(first) = allowance.handles(count + u64::from(self.group.is_some())) else {
                    return signal(EXHAUSTED);
                };
                self.key = first;
                let intake = self.group.is_some().then(|| {
                    Arc::new(SocketCharge::new(
                        allowance.clone(),
                        first + count,
                        charge.partition(self.options.charge(None)),
                    ))
                });
                let mut pools = Vec::with_capacity(count as usize);
                for (index, tape_id) in self.served.ids.iter().copied().enumerate() {
                    let key = first + index as u64;
                    let pool = Arc::new(SocketCharge::new(
                        allowance.clone(),
                        key,
                        charge.partition(self.options.charge(None) + 8),
                    ));
                    pools.push(pool.clone());
                    files.udp_sockets.insert(
                        key,
                        Resource {
                            carrier: Carrier::Replayed {
                                _pool: pool,
                                _intake: intake.clone(),
                            },
                            provider: self.provider,
                            rules: self.rules.clone(),
                            options: self.options,
                            v6: self.endpoint.family == 1,
                            bound: operation != UDP_OPEN,
                            receiving: operation != UDP_OPEN,
                            tape_id: Some(tape_id),
                        },
                    );
                    if operation == UDP_BIND_GROUP {
                        let output = &mut *self.packets;
                        output.data.cast::<u64>().add(output.len).write(key);
                        output.len += 1;
                    }
                }
                crate::tape::register_udp_retirements(
                    self.provider,
                    self.served.ids[0],
                    &pools,
                    intake,
                );
                return count_value(first);
            }
            // Each pool's name reads its unit's cell, so it is taken before
            // the allowance's lock.
            let queue = self.group.is_some();
            let retirements = (self.served.ids.iter())
                .map(|&id| crate::tape::Retirement::new(self.provider, id, false, queue))
                .collect::<Vec<_>>();
            let intake = queue.then(|| {
                crate::tape::Retirement::new(self.provider, self.served.ids[0], true, true)
            });
            let cells = allowance.charges.lock().unwrap();
            let mut first = 0;
            for (index, tape_id) in self.served.ids.iter().copied().enumerate() {
                let key = if operation == UDP_BIND_GROUP {
                    let output = &*self.packets;
                    *output.data.cast::<u64>().add(self.tape_start.1 + index)
                } else {
                    self.key
                };
                if index == 0 {
                    first = key;
                }
                files.udp_sockets.get_mut(&key).unwrap().tape_id = Some(tape_id);
                cells[&key]
                    .upgrade()
                    .unwrap()
                    .record_retirement(retirements[index]);
            }
            if let Some(intake) = intake {
                let key = first + self.served.ids.len() as u64;
                cells[&key].upgrade().unwrap().record_retirement(intake);
            }
            count_value(self.key)
        }
    }
}

fn count_value(key: u64) -> Outcome {
    Outcome {
        value: key,
        ..Outcome::default()
    }
}
