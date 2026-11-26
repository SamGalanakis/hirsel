use crate::{AddressDirection, EPHEMERAL_OUTSIDE_NET_GRANT, Rule};
use std::{
    collections::{BTreeMap, HashMap},
    net::{IpAddr, Ipv4Addr, SocketAddr},
    sync::atomic::{AtomicU64, Ordering::Relaxed},
};

type Ports = Vec<(u16, u16)>;
type Family = BTreeMap<u8, HashMap<u128, Ports>>;

/// The UDP address rules of one Net. Each indexed prefix owns its merged port intervals.
#[derive(Default)]
pub struct UdpRules {
    bind: [Family; 2],
    send: [Family; 2],
    ingress_discards: AtomicU64,
}

impl UdpRules {
    pub fn new(rules: impl IntoIterator<Item = Rule>) -> Self {
        let mut index = Self::default();
        for rule in rules {
            let target = match rule.dir {
                AddressDirection::UdpBind => &mut index.bind,
                AddressDirection::UdpSend => &mut index.send,
                _ => continue,
            };
            target[rule.family as usize]
                .entry(rule.bits)
                .or_default()
                .entry(rule.prefix())
                .or_default()
                .push((rule.first, rule.last));
        }
        for family in index.bind.iter_mut().chain(&mut index.send) {
            for ports in family.values_mut().flat_map(HashMap::values_mut) {
                ports.sort_unstable();
                let mut merged: usize = 0;
                for read in 0..ports.len() {
                    let (start, end) = ports[read];
                    if merged != 0 && start <= ports[merged - 1].1.saturating_add(1) {
                        let previous = &mut ports[merged - 1].1;
                        *previous = (*previous).max(end);
                    } else {
                        ports[merged] = (start, end);
                        merged += 1;
                    }
                }
                ports.truncate(merged);
            }
        }
        index
    }

    pub fn permits_send(&self, addr: SocketAddr) -> bool {
        if addr.ip().is_unspecified()
            || addr.ip() == IpAddr::V4(Ipv4Addr::BROADCAST)
            || invalid_v6(addr)
        {
            return false;
        }
        matching(&self.send, addr, false).any(|ports| interval_end(ports, addr.port()).is_some())
    }

    pub fn permits_ingress(&self, addr: SocketAddr, datagrams: u64) -> bool {
        if self.permits_send(addr) {
            return true;
        }
        self.ingress_discards.fetch_add(datagrams, Relaxed);
        false
    }

    pub fn bind_denial(&self, addr: SocketAddr) -> Option<u8> {
        if invalid_v6(addr) {
            return Some(1);
        }
        let wildcard = addr.ip().is_unspecified();
        let spans = matching(&self.bind, addr, wildcard);
        if addr.port() != 0 {
            return (!spans
                .into_iter()
                .any(|ports| interval_end(ports, addr.port()).is_some()))
            .then_some(1);
        }
        let spans: Vec<_> = spans.collect();
        if spans.is_empty() {
            return Some(1);
        }
        let mut next = 1;
        loop {
            let Some(end) = spans
                .iter()
                .filter_map(|ports| interval_end(ports, next))
                .max()
            else {
                return Some(EPHEMERAL_OUTSIDE_NET_GRANT);
            };
            if end == u16::MAX {
                return None;
            }
            next = end + 1;
        }
    }
}

impl Drop for UdpRules {
    fn drop(&mut self) {
        let count = *self.ingress_discards.get_mut();
        if count != 0
            && std::env::var_os("HIRSEL_UDP_MEASURE_INGRESS").as_deref() == Some("1".as_ref())
        {
            eprintln!("UDP_INGRESS_DISCARD {count}");
        }
    }
}

fn matching(index: &[Family; 2], addr: SocketAddr, wildcard: bool) -> impl Iterator<Item = &Ports> {
    let (family, ip, width) = match addr.ip() {
        IpAddr::V4(ip) => (0, u128::from(u32::from(ip)), 32),
        IpAddr::V6(ip) => (1, u128::from(ip), 128),
    };
    index[family].iter().filter_map(move |(&bits, prefixes)| {
        let prefix = if bits == 0 {
            0
        } else if wildcard {
            return None;
        } else {
            ip & (u128::MAX << (width - bits))
        };
        prefixes.get(&prefix)
    })
}

fn interval_end(ports: &[(u16, u16)], port: u16) -> Option<u16> {
    let i = ports
        .partition_point(|&(lo, _)| lo <= port)
        .checked_sub(1)?;
    let hi = ports[i].1;
    (port <= hi).then_some(hi)
}

fn invalid_v6(addr: SocketAddr) -> bool {
    matches!(addr.ip(), IpAddr::V6(ip) if ip.to_ipv4_mapped().is_some() || ip.is_unicast_link_local())
}
