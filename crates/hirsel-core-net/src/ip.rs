//! Native layout of the core Ip element: one tag followed by its aligned payload.
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

#[repr(C)]
#[derive(Clone, Copy)]
pub struct Ip {
    tag: u8,
    hi: u64,
    lo: u64,
}

impl From<IpAddr> for Ip {
    fn from(address: IpAddr) -> Self {
        match address {
            IpAddr::V4(address) => Self {
                tag: 0,
                hi: u64::from(u32::from(address)),
                lo: 0,
            },
            IpAddr::V6(address) => {
                let number = u128::from(address);
                Self {
                    tag: 1,
                    hi: (number >> 64) as u64,
                    lo: number as u64,
                }
            }
        }
    }
}

impl From<Ip> for IpAddr {
    fn from(ip: Ip) -> Self {
        match ip.tag {
            0 => Ipv4Addr::from(ip.hi as u32).into(),
            1 => Ipv6Addr::from((u128::from(ip.hi) << 64) | u128::from(ip.lo)).into(),
            _ => unreachable!("invalid core Ip tag"),
        }
    }
}

impl Ip {
    pub const SIZE: usize = size_of::<Self>();

    /// Initialized bytes at the same offsets as the native element.
    pub fn layout_bytes(self) -> [u8; Self::SIZE] {
        let mut bytes = [0; Self::SIZE];
        bytes[0] = self.tag;
        bytes[8..16].copy_from_slice(&self.hi.to_le_bytes());
        bytes[16..24].copy_from_slice(&self.lo.to_le_bytes());
        bytes
    }
}

/// The byte length of one resolved address record: a family byte (0 for V4,
/// 1 for V6) and 16 octets, a V4 address in the last four after twelve zeros.
/// A resolve's tape delivery and a served Net's staged `Resolve` bytes are
/// such records.
pub const IP_RECORD_LEN: usize = 17;

/// One address as a resolved address record.
pub fn ip_record(address: IpAddr) -> [u8; IP_RECORD_LEN] {
    let mut record = [0; IP_RECORD_LEN];
    match address {
        IpAddr::V4(ip) => record[13..].copy_from_slice(&ip.octets()),
        IpAddr::V6(ip) => {
            record[0] = 1;
            record[1..].copy_from_slice(&ip.octets());
        }
    }
    record
}

/// The addresses of whole, well-formed and distinct records, in order.
pub fn ip_records(bytes: &[u8]) -> Option<Vec<IpAddr>> {
    if bytes.len() % IP_RECORD_LEN != 0 {
        return None;
    }
    let mut addresses = Vec::with_capacity(bytes.len() / IP_RECORD_LEN);
    let mut seen = std::collections::HashSet::with_capacity(addresses.capacity());
    for record in bytes.chunks_exact(IP_RECORD_LEN) {
        let address = match record[0] {
            0 if record[1..13] == [0; 12] => {
                IpAddr::from(<[u8; 4]>::try_from(&record[13..]).unwrap())
            }
            1 => IpAddr::from(<[u8; 16]>::try_from(&record[1..]).unwrap()),
            _ => return None,
        };
        if !seen.insert(address) {
            return None;
        }
        addresses.push(address);
    }
    Some(addresses)
}
