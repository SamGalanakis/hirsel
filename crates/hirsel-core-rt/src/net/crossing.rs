use super::TcpRoute;

/// Owns a socket or listener while it is in transit.
pub(crate) struct CrossedSocket {
    carrier: Option<hirsel_core_net::SocketTransfer>,
    udp: Option<crate::udp_net::Transfer>,
    route: Option<TcpRoute>,
    /// A `Hello`'s record moves with its socket (TLS-9: `Hello` is `send`).
    hello: Option<Box<crate::tls_server::Hello>>,
    /// A recording unit's socket: its object there and its snapshot
    /// (docs/recording.md, "Sockets that cross").
    #[cfg(feature = "record")]
    taped: Option<Box<crate::tape::units::crossing::Departed>>,
}

// SAFETY: the provider descriptor moves as opaque data and is used only
// under its existing calling contract.
unsafe impl Send for CrossedSocket {}

impl CrossedSocket {
    pub(crate) fn check(files: &crate::host_file::Files, key: u64) {
        if crate::udp_net::is_socket(key) {
            return;
        }
        if key & 3 != 0
            || files.unix_served.contains_key(&key)
            || files.tls_sessions.contains_key(&key)
            || files.tls_writes.contains_key(&key)
            || files
                .tcp_routes
                .get(&key)
                .is_some_and(|route| crate::provide::owns(&route.provider))
        {
            // A port owns its served keys. TLS halves own separate stream state.
            crate::run_stop::raise(crate::run_stop::Stop::from_static(b"UnitSocket"));
        }
    }

    pub(crate) fn detach(files: &mut crate::host_file::Files, key: u64) -> Self {
        if crate::udp_net::is_socket(key) {
            return Self {
                carrier: None,
                udp: Some(crate::udp_net::Transfer::detach(files, key)),
                route: None,
                hello: None,
                #[cfg(feature = "record")]
                taped: None,
            };
        }
        // A recording unit's carrier names its object; the reactor key is
        // that object's source. A replaying unit holds no reactor socket.
        #[cfg(feature = "record")]
        let (carrier, taped) = match files.net_tape {
            Some(tape) => {
                let source = tape.source(key);
                let taped =
                    crate::tape::units::crossing::depart(&tape.provider, key, tape.handle(key));
                match source {
                    Some(source) => (
                        Some(files.sockets.detach(source).unwrap_or_else(|_| {
                            crate::run_stop::raise(crate::run_stop::Stop::from_static(b"HostPoll"))
                        })),
                        taped,
                    ),
                    None if taped.is_some() => (None, taped),
                    None => crate::run_stop::raise(crate::run_stop::Stop::from_static(b"HostPoll")),
                }
            }
            None => (
                Some(files.sockets.detach(key).unwrap_or_else(|_| {
                    crate::run_stop::raise(crate::run_stop::Stop::from_static(b"HostPoll"))
                })),
                None,
            ),
        };
        #[cfg(not(feature = "record"))]
        let carrier = Some(files.sockets.detach(key).unwrap_or_else(|_| {
            crate::run_stop::raise(crate::run_stop::Stop::from_static(b"HostPoll"))
        }));
        Self {
            carrier,
            udp: None,
            route: files.tcp_routes.remove(&key),
            hello: files.tls_hellos.remove(&key),
            #[cfg(feature = "record")]
            taped,
        }
    }

    pub(crate) fn attach(mut self, files: &mut crate::host_file::Files) -> u64 {
        if let Some(udp) = self.udp.take() {
            return udp.attach(files);
        }
        #[cfg(feature = "record")]
        let key = match self.carrier.take() {
            Some(carrier) => {
                let reactor = files.sockets.attach(carrier).unwrap_or_else(|_| {
                    crate::run_stop::raise(crate::run_stop::Stop::from_static(b"HostPoll"))
                });
                self.carrier_in(files, reactor)
            }
            None => self
                .taped
                .as_ref()
                .expect("a replaying socket keeps its departed record")
                .key(),
        };
        #[cfg(not(feature = "record"))]
        let key = files
            .sockets
            .attach(self.carrier.take().unwrap())
            .unwrap_or_else(|_| {
                crate::run_stop::raise(crate::run_stop::Stop::from_static(b"HostPoll"))
            });
        if let Some(route) = self.route.take() {
            files.tcp_routes.insert(key, route);
        }
        if let Some(hello) = self.hello.take() {
            files.tls_hellos.insert(key, hello);
        }
        key
    }
}

#[cfg(feature = "record")]
impl CrossedSocket {
    /// The carrier of reactor key `reactor` in the attaching unit: in a
    /// recording unit, the object its take named or its own on its way back,
    /// whose route then goes through the unit's tape; elsewhere the reactor
    /// key, routed to the socket's source.
    fn carrier_in(&mut self, files: &crate::host_file::Files, reactor: u64) -> u64 {
        let source = self
            .taped
            .as_ref()
            .and_then(|d| d.source())
            .or(self.route.map(|r| r.provider));
        let Some(tape) = files.net_tape else {
            if let (Some(route), Some(source)) = (self.route.as_mut(), source) {
                route.provider = source;
            }
            return reactor;
        };
        // A listener a unit without a recording sends has no source to name.
        let source = source.unwrap_or_else(|| {
            crate::run_stop::raise(crate::run_stop::Stop::from_static(b"UnitRecording"))
        });
        match crate::tape::units::crossing::settle(
            &tape.provider,
            tape.handle(0),
            self.taped.as_deref(),
            source,
            reactor,
        ) {
            Some(carrier) => {
                if let Some(route) = self.route.as_mut() {
                    route.provider = tape.provider;
                }
                carrier
            }
            None => reactor,
        }
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

    /// The recording's record of the socket in transit whose sender's
    /// carrier is `key`.
    #[cfg(feature = "record")]
    pub(crate) fn departed(
        &mut self,
        key: u64,
    ) -> Option<&mut crate::tape::units::crossing::Departed> {
        let Self::InTransit(sockets) = self else {
            return None;
        };
        sockets
            .iter_mut()
            .filter_map(|s| s.taped.as_deref_mut())
            .find(|d| d.key() == key)
    }

    pub(crate) fn discard(&mut self) -> Vec<u64> {
        match std::mem::replace(self, Self::Taken(Vec::new())) {
            Self::InTransit(_) => Vec::new(),
            Self::Taken(keys) => keys,
        }
    }
}
