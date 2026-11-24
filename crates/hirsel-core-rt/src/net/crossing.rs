use super::TcpRoute;

/// Owns a socket or listener while it is in transit.
pub(crate) struct CrossedSocket {
    carrier: Option<hirsel_core_net::SocketTransfer>,
    udp: Option<crate::udp_net::Transfer>,
    route: Option<TcpRoute>,
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
            crate::process::stop(b"UnitSocket");
        }
    }

    pub(crate) fn detach(files: &mut crate::host_file::Files, key: u64) -> Self {
        if crate::udp_net::is_socket(key) {
            return Self {
                carrier: None,
                udp: Some(crate::udp_net::Transfer::detach(files, key)),
                route: None,
            };
        }
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
            udp: None,
            route: files.tcp_routes.remove(&key),
        }
    }

    pub(crate) fn attach(mut self, files: &mut crate::host_file::Files) -> u64 {
        if let Some(udp) = self.udp.take() {
            return udp.attach(files);
        }
        let key = files
            .sockets
            .attach(self.carrier.take().unwrap())
            .unwrap_or_else(|_| crate::process::stop(b"HostPoll"));
        if let Some(route) = self.route.take() {
            files.tcp_routes.insert(key, route);
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
