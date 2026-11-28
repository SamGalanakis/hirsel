use super::*;
use hirsel_core_net::{AddressDirection, GrantRule};

impl Machine<'_> {
    pub(super) fn network_host(&mut self, t: &Task, op: HostOp, a: &[u64]) -> X<Vec<u64>> {
        match op {
            HostOp::Connect | HostOp::Listen => {
                if let Some(rules) = self.host.world.network.grants.get(&a[0]) {
                    let denial = hirsel_core_net::address_denial(
                        rules.iter().filter_map(GrantRule::as_address),
                        if op == HostOp::Listen {
                            AddressDirection::Listen
                        } else {
                            AddressDirection::Connect
                        },
                        a[1] as u8,
                        a[2],
                        a[3],
                        a[4] as u16,
                    );
                    if let Some(tag) = denial {
                        let rules = rules.iter().filter_map(GrantRule::as_address);
                        if op == HostOp::Connect
                            && hirsel_core_net::not_public(
                                rules,
                                AddressDirection::Connect,
                                a[1] as u8,
                                a[2],
                                a[3],
                                a[4] as u16,
                            )
                        {
                            let provider = self.host.world.net_ports.get(&a[0]).copied();
                            *self.host.world.not_public.entry(provider).or_default() += 1;
                        }
                        let mut row = vec![0, u64::from(tag), 0, 0];
                        if op == HostOp::Listen {
                            row.extend([0; 4]);
                        }
                        return Ok(row);
                    }
                    if let Some(&c) = self.host.world.net_ports.get(&a[0]) {
                        return self.net_call(t, op, c, 0, true, a);
                    }
                }
            }
            HostOp::LocalAddr | HostOp::PeerAddr => {
                let socket = self.port_socket(a[0], PORT_SOCKET);
                let addressable =
                    if op == HostOp::LocalAddr && socket.is_some_and(|(_, _, live)| !live) {
                        self.port_socket(a[0], 4)
                    } else {
                        socket
                    };
                if let Some((c, id, live)) = addressable {
                    return self.net_call(t, op, c, id, live, a);
                }
            }
            HostOp::Accept | HostOp::RecvBounded | HostOp::RecvReady | HostOp::SendPair => {
                let stream = op != HostOp::Accept;
                let kind = if stream { PORT_SOCKET } else { 4 };
                let original = self.port_socket(a[0], kind);
                let socket = if stream && original.is_some_and(|(_, _, live)| !live) {
                    self.port_socket(a[0], PORT_UNIX_SOCKET)
                } else {
                    original
                };
                if let Some((c, id, mut live)) = socket {
                    if stream
                        && matches!(
                            self.host.world.cells[c],
                            VirtualCell::Port {
                                vocab: Vocab::Exec,
                                ..
                            }
                        )
                    {
                        let kind = if op == HostOp::SendPair {
                            PORT_PIPE_WRITE
                        } else {
                            PORT_PIPE_READ
                        };
                        (_, _, live) = self.port_socket(a[0], kind).unwrap();
                        if op == HostOp::RecvReady {
                            // ExecOp has no readiness call: a served pipe is
                            // ready at once and its receive waits (NET-1).
                            self.check_cancel(t)?;
                            return Ok(vec![0, 0, 0, 0]);
                        }
                    }
                    return self.net_call(t, op, c, id, live, a);
                }
            }
            _ => unreachable!(),
        }
        let mut denied = vec![0, 1, 0, 0];
        if matches!(
            op,
            HostOp::Listen | HostOp::Accept | HostOp::LocalAddr | HostOp::PeerAddr
        ) {
            denied.extend([0; 4]);
        }
        Ok(denied)
    }

    fn tls_offer(&self, a: &[u64], port: u16) -> Result<Vec<Vec<u8>>, (u64, u64)> {
        use hirsel_core_net::{GrantRule, tls};
        let name = tls::verification_name(self.carrier_bytes(a[2])).ok_or((9, 1))?;
        let len = self.load(a[5] + 8, 8);
        if len > 16 {
            return Err((9, 2));
        }
        let data = self.load(a[5], 8);
        let protocols: Vec<_> = (0..len)
            .map(|i| self.carrier_bytes(data + i * 24))
            .collect();
        if !tls::valid_protocols(&protocols) {
            return Err((9, 2));
        }
        let pem = if a[3] == 1 {
            self.carrier_bytes(a[4])
        } else {
            &[]
        };
        let (trust, _) = tls::trust(a[3] as u8, pem).ok_or((9, 3))?;
        let allowed = self
            .host
            .world
            .network
            .grants
            .get(&a[0])
            .is_some_and(|rules| {
                rules.iter().any(
                    |rule| matches!(rule, GrantRule::Tls(rule) if rule.permits(&name, port, trust)),
                )
            });
        if !allowed {
            return Err((1, 0));
        }
        Ok(protocols.into_iter().map(<[u8]>::to_vec).collect())
    }

    pub(super) fn tls_connect(&mut self, t: &Task, a: &[u64]) -> X<Vec<u64>> {
        let key = a[1];
        let served = self.host.world.net_ports.get(&a[0]).copied();
        if let Some(&(c, _, peer)) = self.host.world.port_keys.get(&(key & !3)) {
            return self.port_tls(t, a, c, peer, served);
        }
        self.host.world.network.sockets.release(key);
        Ok(vec![0, 1, 0, 0, 0, 0, 0, 0])
    }

    /// `tls.hello` (TLS-9) on a socket a served Net's listener accepted, the
    /// only accepted TCP socket the evaluator has: its server reads the
    /// ClientHello and stages the server name. The Socket is consumed on
    /// every return, as natively.
    pub(super) fn tls_hello(&mut self, t: &Task, a: &[u64]) -> X<Vec<u64>> {
        let key = a[0];
        let Some((c, id, true)) = self.port_socket(key, PORT_SOCKET).filter(|&(c, id, _)| {
            let VirtualCell::Port { handles, .. } = &self.host.world.cells[c] else {
                unreachable!()
            };
            handles.get(&id).is_some_and(|h| h.accepted_socket)
        }) else {
            self.socket_drop(key);
            return Ok(vec![0, 1, 0, 0]);
        };
        let mut w = [0; 9];
        w[..3].copy_from_slice(&[NET_HELLO, id, 253]);
        let row = match self.check_cancel(t).and_then(|()| {
            let deadline = self.until(a, 1);
            self.port_call_with(t, c, w, vec![], None, true, &mut None, deadline)
        }) {
            Ok(row) => row,
            Err(stop) => {
                self.socket_drop(key);
                return Err(stop);
            }
        };
        let name = self.host.world.hello_names.remove(&t.k);
        if row[1] != 0 {
            self.socket_drop(key);
            return Ok(row);
        }
        self.host
            .world
            .tls_hellos
            .insert(key, name.unwrap_or_default());
        Ok(vec![key, 0, 0, 0])
    }

    /// `tls.accept` (TLS-9) on a served `Hello`: the key's grant, the chain
    /// and the ALPN list are checked before issue, as natively; the server
    /// receives the chain and the list, never the key, and answers the
    /// handshake. The Hello's socket is consumed on every return.
    pub(super) fn tls_accept(&mut self, t: &Task, a: &[u64]) -> X<Vec<u64>> {
        use hirsel_core_net::{SecretUse, tls};
        let key = a[0];
        let failed = |s: &mut Self, tag: u64, code: u64| {
            s.socket_drop(key);
            Ok(vec![0, tag, code, 0])
        };
        let hello = self.host.world.tls_hellos.remove(&key).is_some();
        let Some((c, id, true)) = self.port_socket(key, PORT_SOCKET).filter(|_| hello) else {
            return failed(self, 1, 0);
        };
        if !self
            .secret(a[2])
            .is_some_and(|key| key.policy.uses().permits(SecretUse::TLS_SERVE))
        {
            return failed(self, 1, 0);
        }
        let list = |s: &Self, carrier: u64| -> Vec<Vec<u8>> {
            let (data, len) = (s.load(carrier, 8), s.load(carrier + 8, 8));
            (0..len)
                .map(|i| s.carrier_bytes(data + i * 24).to_vec())
                .collect()
        };
        let entries = list(self, a[1]);
        if !tls::valid_chain(&entries) {
            return failed(self, 9, 7);
        }
        let protocols = list(self, a[3]);
        if !tls::valid_protocols(&protocols) {
            return failed(self, 9, 2);
        }
        // The chain as natively packed: each entry's u32 length, then its DER.
        let packed: Vec<u8> = entries
            .iter()
            .flat_map(|der| {
                (der.len() as u32)
                    .to_le_bytes()
                    .into_iter()
                    .chain(der.iter().copied())
            })
            .collect();
        let wire: Vec<u8> = protocols
            .iter()
            .flat_map(|p| std::iter::once(p.len() as u8).chain(p.iter().copied()))
            .collect();
        let mut w = [0; 9];
        w[..4].copy_from_slice(&[NET_SERVE, id, packed.len() as u64, wire.len() as u64]);
        let row = match self.check_cancel(t).and_then(|()| {
            let deadline = self.until(a, 4);
            let fields = vec![vec![], packed, wire];
            self.port_call_with(t, c, w, fields, None, true, &mut None, deadline)
        }) {
            Ok(row) => row,
            Err(stop) => {
                self.socket_drop(key);
                return Err(stop);
            }
        };
        if row[1] != 0 {
            return failed(self, row[1], row[2]);
        }
        self.port_socket_part(key, true);
        // A server half has no verified name: it authorizes no secret.
        self.host
            .world
            .tls_writes
            .insert(key & !3, (None, false, Default::default()));
        Ok(vec![key, 0, 0, row[3]])
    }

    /// `tls.connect` on a served Net's socket (native `tls::connect` through
    /// the port): the handshake is a call its server answers with Secured,
    /// and only through that Net (NOTES 32). The Socket is consumed on every
    /// return: a failure releases it whole.
    fn port_tls(
        &mut self,
        t: &Task,
        a: &[u64],
        c: usize,
        peer: u16,
        served: Option<usize>,
    ) -> X<Vec<u64>> {
        let key = a[1];
        let failed = |s: &mut Self, tag: u64, code: u64| {
            s.port_socket_part(key, false);
            Ok(vec![0, tag, code, 0, 0, 0, 0, 0])
        };
        let Some((_, id, true)) = self.port_socket(key, PORT_SOCKET) else {
            return failed(self, 1, 0);
        };
        let VirtualCell::Port { handles, .. } = &self.host.world.cells[c] else {
            unreachable!()
        };
        if served != Some(c) || !handles[&id].client_socket {
            return failed(self, 1, 0);
        }
        let offered = match self.tls_offer(a, peer) {
            Ok(offered) => offered,
            Err((tag, code)) => return failed(self, tag, code),
        };
        let name = hirsel_core_net::tls::verification_name(self.carrier_bytes(a[2])).unwrap();
        let canonical = match &name {
            hirsel_core_net::Name::Dns(name) => name.as_bytes().to_vec(),
            hirsel_core_net::Name::Ip(ip) => ip.to_string().into_bytes(),
        };
        let wire: Vec<u8> = offered
            .iter()
            .flat_map(|p| std::iter::once(p.len() as u8).chain(p.iter().copied()))
            .collect();
        let mut w = [0; 9];
        w[..5].copy_from_slice(&[
            6,
            id,
            canonical.len() as u64,
            wire.len() as u64,
            u64::from(a[3] == 0),
        ]);
        let row = match self.check_cancel(t).and_then(|()| {
            let deadline = self.until(a, 6);
            let fields = vec![vec![], canonical, wire];
            self.port_call_with(t, c, w, fields, None, true, &mut None, deadline)
        }) {
            Ok(row) => row,
            Err(stop) => {
                self.port_socket_part(key, false);
                return Err(stop);
            }
        };
        if row[1] != 0 {
            return failed(self, row[1], row[2]);
        }
        self.port_socket_part(key, true);
        self.host
            .world
            .tls_writes
            .insert(key & !3, (Some(name), false, Default::default()));
        Ok([&[key, 0, 0], &row[3..8]].concat())
    }
}
