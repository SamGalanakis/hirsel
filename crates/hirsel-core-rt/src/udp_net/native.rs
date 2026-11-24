use super::*;

impl Call {
    pub(super) unsafe fn create(&mut self, bind: bool) -> Outcome {
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
                            #[cfg(feature = "record")]
                            tape_id: None,
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
                    #[cfg(feature = "record")]
                    tape_id: None,
                },
            );
            Outcome {
                value: key,
                ..Outcome::default()
            }
        }
    }

    pub(super) unsafe fn receive(
        &self,
        resource: &mut Resource,
        sockets: &mut hirsel_core_net::Sockets,
    ) -> io::Result<Received> {
        // SAFETY: the current request exclusively owns both typed output carriers.
        let (bytes, packets) = unsafe { (&mut *self.bytes, &mut *self.packets) };
        let poller = sockets.udp_poller()?;
        crate::tasks::attach_net(sockets, poller.clone());
        let mut budget = poller.udp_turn_budget();
        let mut n = 0;
        while packets.len < packets.capacity {
            let captured = match &mut resource.carrier {
                Carrier::Standalone(socket) => socket.pool.front(),
                Carrier::Member(member) => member.reader().front(),
                Carrier::Served(_) => unreachable!(),
                #[cfg(feature = "record")]
                Carrier::Replayed { .. } => unreachable!(),
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
                    captured_at,
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
                        Carrier::Member(member) => {
                            member.reader().advance();
                            member.measure_delivery(&poller, captured_at);
                        }
                        Carrier::Served(_) => unreachable!(),
                        #[cfg(feature = "record")]
                        Carrier::Replayed { .. } => unreachable!(),
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
