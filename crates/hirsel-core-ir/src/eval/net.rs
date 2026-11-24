use super::*;

impl Machine<'_> {
    pub(super) fn network_host(&mut self, t: &Task, op: HostOp, a: &[u64]) -> X<Vec<u64>> {
        match op {
            HostOp::Connect | HostOp::Listen => {
                if let Some(rules) = self.world.network.grants.get(&a[0]) {
                    let denial = hirsel_core_net::address_denial(
                        rules.iter().filter_map(|rule| match rule {
                            hirsel_core_net::GrantRule::Address(rule) => Some(*rule),
                            _ => None,
                        }),
                        u8::from(op == HostOp::Listen),
                        a[1] as u8,
                        a[2],
                        a[3],
                        a[4] as u16,
                    );
                    if let Some(tag) = denial {
                        let mut row = vec![0, u64::from(tag), 0, 0];
                        if op == HostOp::Listen {
                            row.extend([0; 4]);
                        }
                        return Ok(row);
                    }
                    if let Some(&c) = self.world.net_ports.get(&a[0]) {
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
                            self.world.cells[c],
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
}
