use super::*;

pub(crate) struct Retirement {
    _member: SocketCharge,
    _intake: Option<Arc<SocketCharge>>,
}

pub(super) struct Socket {
    provider: Provider,
    key: u64,
    retirement: Option<Retirement>,
}

impl Socket {
    pub(super) fn key(&self) -> u64 {
        self.key
    }
}

impl Drop for Socket {
    fn drop(&mut self) {
        crate::provide::release_udp(&self.provider, self.key, self.retirement.take().unwrap());
    }
}

#[derive(Default)]
pub(super) struct Delivery {
    pub(super) ids: Vec<u64>,
    pub(super) ingress: bool,
    send_packets: Vec<u8>,
}

impl Delivery {
    pub(super) fn encode(&mut self, packets: &[TxCarrier]) {
        self.send_packets.reserve_exact(36 * packets.len());
        for &packet in packets {
            let tx = TxPacket::from(packet);
            let addr = Address::from(tx.to);
            self.send_packets.push(addr.family);
            self.send_packets.extend_from_slice(&addr.hi.to_le_bytes());
            self.send_packets.extend_from_slice(&addr.lo.to_le_bytes());
            self.send_packets
                .extend_from_slice(&addr.port.to_le_bytes());
            self.send_packets.extend_from_slice(&tx.at.to_le_bytes());
            self.send_packets.extend_from_slice(&tx.len.to_le_bytes());
            self.send_packets.push(tx.ecn);
        }
    }
}

impl Call {
    pub(super) unsafe fn install_served(&mut self, operation: u8) -> Outcome {
        // SAFETY: the caller still holds the returned virtual handles and the output list.
        unsafe {
            let files = &mut *self.files;
            let allowance = files.registry.run_grants().udp_allowance.clone();
            let mut charge = self.charge.take().unwrap();
            let per_member = self.options.charge(None) + 8;
            let group = if self.group.is_some() {
                let bytes = self.options.charge(None);
                Some(Arc::new(SocketCharge::new(
                    allowance.clone(),
                    self.key + self.served.ids.len() as u64,
                    charge.partition(bytes),
                )))
            } else {
                None
            };
            for (index, port_key) in self.served.ids.drain(..).enumerate() {
                let key = self.key + index as u64;
                let retirement = Retirement {
                    _member: SocketCharge::new(
                        allowance.clone(),
                        key,
                        charge.partition(per_member),
                    ),
                    _intake: group.clone(),
                };
                files.udp_sockets.insert(
                    key,
                    Resource {
                        carrier: Carrier::Served(Socket {
                            provider: self.provider,
                            key: port_key,
                            retirement: Some(retirement),
                        }),
                        provider: self.provider,
                        rules: self.rules.clone(),
                        options: self.options,
                        v6: self.endpoint.family == 1,
                        bound: operation != UDP_OPEN,
                        receiving: operation != UDP_OPEN,
                    },
                );
                if operation == UDP_BIND_GROUP {
                    let output = &mut *self.packets;
                    output.data.cast::<u64>().add(output.len).write(key);
                    output.len += 1;
                }
            }
            count(self.key as usize)
        }
    }
}

pub(crate) struct Input {
    pub(crate) endpoint: Address,
    pub(crate) slots: u64,
    pub(crate) slot_bytes: u64,
    pub(crate) members: u64,
    pub(crate) steer: u64,
    pub(crate) room: u64,
    pub(crate) entries: u64,
}

/// # Safety
/// The unsettled UDP request pins its operands through delivery.
pub(crate) unsafe fn inputs(r: &Request) -> Input {
    // SAFETY: as above.
    unsafe {
        let call = &*r.path.cast::<Call>();
        let (members, steer) = call.group.map_or((1, 0), |(n, steer)| {
            (u64::from(n.get()), u64::from(steer == Steer::Quic))
        });
        let (room, entries) = match r.operation {
            UDP_RECV => (
                (*call.bytes).capacity - (*call.bytes).len,
                (*call.packets).capacity - (*call.packets).len,
            ),
            UDP_SEND => ((*call.bytes).len, (*call.packets).len),
            _ => (0, 0),
        };
        Input {
            endpoint: call.endpoint,
            slots: call.options.slots() as u64,
            slot_bytes: call.options.slot_bytes() as u64,
            members,
            steer,
            room: room as u64,
            entries: entries as u64,
        }
    }
}

pub(crate) unsafe fn until(r: &Request) -> Option<(Provider, u64, u64)> {
    // SAFETY: the provider lends the pinned UDP extension.
    unsafe { (*r.path.cast::<Call>()).until }
}

pub(crate) unsafe fn payload(r: &Request, field: u64) -> &[u8] {
    // SAFETY: an unsettled UDP send pins its operands.
    let call = unsafe { &*r.path.cast::<Call>() };
    match field {
        1 => unsafe { crate::tls::bytes(&*call.bytes) },
        2 => &call.served.send_packets,
        _ => &[],
    }
}

#[repr(C)]
struct Staged {
    case: u8,
    peer: CoreAddr,
    len: u64,
    appended: u64,
    ecn: u8,
}

pub(crate) unsafe fn stage(records: &Buffer, count: usize, out: &mut Vec<udp::Packet>) {
    if count != 0 {
        // SAFETY: the server owns a pinned List of at least count initialized records.
        let records = unsafe { std::slice::from_raw_parts(records.data.cast::<Staged>(), count) };
        out.extend(records.iter().map(|record| udp::Packet {
            peer: record.peer.into(),
            len: record.len,
            appended: record.appended,
            ecn: record.ecn,
        }));
    }
}

pub(crate) unsafe fn valid(r: &Request, records: &[udp::Packet], bytes: usize) -> bool {
    // SAFETY: the unsettled call's operands are pinned for validation.
    unsafe {
        let call = &*r.path.cast::<Call>();
        let input = inputs(r);
        udp::valid_receive(
            call.endpoint.family == 1,
            call.served.ingress.then_some(call.rules.as_ref()),
            call.options.payload_room(),
            input.room,
            input.entries,
            records,
            bytes,
        )
    }
}

pub(crate) unsafe fn deliver_receive(r: &Request, records: &[udp::Packet], staged: &[u8]) {
    // SAFETY: successful validation established room for all the appends.
    unsafe {
        let call = &mut *r.path.cast_mut().cast::<Call>();
        let bytes = &mut *call.bytes;
        let packets = &mut *call.packets;
        if !staged.is_empty() {
            ptr::copy_nonoverlapping(staged.as_ptr(), bytes.data.add(bytes.len), staged.len());
        }
        let mut at = bytes.len as u64;
        for packet in records {
            let value = PacketCarrier {
                case: 0,
                peer: packet.peer.into(),
                at,
                len: packet.len,
                truncated: packet.len > packet.appended,
                ecn: packet.ecn,
            };
            packets
                .data
                .cast::<PacketCarrier>()
                .add(packets.len)
                .write(value);
            packets.len += 1;
            at += packet.appended;
        }
        bytes.len += staged.len();
    }
}

pub(crate) unsafe fn deliver_address(r: &Request, endpoint: Address, keys: Vec<u64>) {
    // SAFETY: the caller pins this operation's result structure.
    let call = unsafe { &mut *r.path.cast_mut().cast::<Call>() };
    call.endpoint = endpoint;
    call.served.ids = keys;
}
