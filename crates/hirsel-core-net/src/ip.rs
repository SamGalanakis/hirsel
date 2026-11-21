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
