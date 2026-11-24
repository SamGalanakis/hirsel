//! NET-18's address-class table, behind both the `Public` reach preflight and
//! `net.class`, and shared with TURN-7's peer policy.
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

#[cfg(test)]
mod tests;

/// The class of an address, in `core.net.AddressClass` case order.
#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AddressClass {
    Public,
    Unspecified,
    ThisNetwork,
    Private,
    SharedCgnat,
    Loopback,
    LinkLocal,
    Protocol,
    Documentation,
    Relay6to4,
    Benchmarking,
    Multicast,
    Reserved,
    Broadcast,
    UniqueLocal,
    Nat64Local,
    NotGlobal6,
}

use AddressClass::*;

/// IPv4 rows as (prefix, length, class). A row nested in another comes first.
const V4: [(u32, u8, AddressClass); 17] = [
    (0x0000_0000, 32, Unspecified),
    (0x0000_0000, 8, ThisNetwork),
    (0x0a00_0000, 8, Private),
    (0xac10_0000, 12, Private),
    (0xc0a8_0000, 16, Private),
    (0x6440_0000, 10, SharedCgnat),
    (0x7f00_0000, 8, Loopback),
    (0xa9fe_0000, 16, LinkLocal),
    (0xc000_0000, 24, Protocol),
    (0xc000_0200, 24, Documentation),
    (0xc633_6400, 24, Documentation),
    (0xcb00_7100, 24, Documentation),
    (0xc058_6300, 24, Relay6to4),
    (0xc612_0000, 15, Benchmarking),
    (0xe000_0000, 4, Multicast),
    (0xffff_ffff, 32, Broadcast),
    (0xf000_0000, 4, Reserved),
];

/// IPv6 rows outside the embedding forms, as (prefix, length, class).
const V6: [(u128, u8, AddressClass); 9] = [
    (0, 128, Unspecified),
    (1, 128, Loopback),
    (0x0064_ff9b_0001 << 80, 48, Nat64Local),
    (0x2001_0db8 << 96, 32, Documentation),
    (0x3fff << 112, 20, Documentation),
    (0x2001 << 112, 23, Protocol),
    (0xfc << 120, 7, UniqueLocal),
    (0xfe80 << 112, 10, LinkLocal),
    (0xff << 120, 8, Multicast),
];

const MAPPED: (u128, u8) = (0xffff << 32, 96);
const NAT64: (u128, u8) = (0x0064_ff9b << 96, 96);
const SIX_TO_FOUR: (u128, u8) = (0x2002 << 112, 16);
const TEREDO: (u128, u8) = (0x2001 << 112, 32);
const GLOBAL: (u128, u8) = (0x2 << 124, 3);

fn mask(width: u8, bits: u8) -> u128 {
    if bits == 0 {
        0
    } else {
        (u128::MAX << (128 - bits)) >> (128 - width)
    }
}

fn within(ip: u128, (prefix, bits): (u128, u8)) -> bool {
    ip & mask(128, bits) == prefix
}

fn v4_class(ip: u32) -> AddressClass {
    V4.iter()
        .find(|(prefix, bits, _)| (u128::from(ip) & mask(32, *bits)) == u128::from(*prefix))
        .map_or(Public, |row| row.2)
}

fn v6_class(ip: u128) -> AddressClass {
    if within(ip, MAPPED) || within(ip, NAT64) {
        return v4_class(ip as u32);
    }
    if within(ip, SIX_TO_FOUR) {
        return v4_class((ip >> 80) as u32);
    }
    if within(ip, TEREDO) {
        // The server decides when it is not public, then the client.
        let server = v4_class((ip >> 64) as u32);
        return if server != Public {
            server
        } else {
            v4_class(!(ip as u32))
        };
    }
    if let Some(row) = V6
        .iter()
        .find(|(prefix, bits, _)| within(ip, (*prefix, *bits)))
    {
        return row.2;
    }
    if within(ip, GLOBAL) {
        Public
    } else {
        NotGlobal6
    }
}

/// `net.class`: the class of `ip` under NET-18's table.
pub fn class(ip: IpAddr) -> AddressClass {
    match ip {
        IpAddr::V4(ip) => v4_class(u32::from(ip)),
        IpAddr::V6(ip) => v6_class(u128::from(ip)),
    }
}

/// The class of a rule-shaped address: family 0 is V4 in `lo`, 1 is V6.
pub fn class_of(family: u8, hi: u64, lo: u64) -> AddressClass {
    if family == 0 {
        class(Ipv4Addr::from(lo as u32).into())
    } else {
        class(Ipv6Addr::from((u128::from(hi) << 64) | u128::from(lo)).into())
    }
}

/// Whether some point of `[lo, hi]` lies outside every hole. Holes are inclusive.
fn uncovered(lo: u128, hi: u128, holes: impl Iterator<Item = (u128, u128)>) -> bool {
    let mut holes: Vec<_> = holes.filter(|&(a, b)| b >= lo && a <= hi).collect();
    holes.sort_unstable();
    let mut next = lo;
    for (a, b) in holes {
        if a > next {
            return true;
        }
        if b >= hi {
            return false;
        }
        next = next.max(b + 1);
    }
    true
}

fn range(width: u8, prefix: u128, bits: u8) -> (u128, u128) {
    (prefix, prefix | (!mask(width, bits) & mask(width, width)))
}

/// Whether the IPv4 prefix holds a public address.
fn v4_public(prefix: u32, bits: u8) -> bool {
    let (lo, hi) = range(32, u128::from(prefix), bits);
    uncovered(
        lo,
        hi,
        V4.iter().map(|(p, b, _)| range(32, u128::from(*p), *b)),
    )
}

/// The IPv4 prefix (address, fixed length) embedded at bit `at`, counted from
/// the top, of an IPv6 prefix of length `bits`.
fn embedded(prefix: u128, bits: u8, at: u8) -> (u32, u8) {
    let fixed = bits.saturating_sub(at).min(32);
    let v4 = (prefix >> (96 - at)) as u32;
    (v4 & (mask(32, fixed) as u32), fixed)
}

/// Whether an IPv6 prefix and an embedding form share an address, and the
/// longer of the two when they do.
fn meet(prefix: u128, bits: u8, (form, form_bits): (u128, u8)) -> Option<(u128, u8)> {
    let shorter = bits.min(form_bits);
    (prefix & mask(128, shorter) == form & mask(128, shorter)).then(|| {
        if bits >= form_bits {
            (prefix, bits)
        } else {
            (form, form_bits)
        }
    })
}

/// Whether the prefix holds an address the table calls public, computed over
/// the table's intervals. IPv4-mapped addresses never count: NET-6 denies every
/// connect to one before the table is read.
pub fn covers_public(family: u8, prefix: u128, bits: u8) -> bool {
    if family == 0 {
        return v4_public(prefix as u32, bits);
    }
    let (lo, hi) = range(128, prefix, bits);
    let global = range(128, GLOBAL.0, GLOBAL.1);
    if lo <= global.1 && hi >= global.0 {
        let holes = V6
            .iter()
            .map(|(p, b, _)| (*p, *b))
            .chain([SIX_TO_FOUR, TEREDO])
            .map(|(p, b)| range(128, p, b));
        if uncovered(lo.max(global.0), hi.min(global.1), holes) {
            return true;
        }
    }
    if let Some((p, b)) = meet(prefix, bits, NAT64) {
        let (v4, fixed) = embedded(p, b, 96);
        if v4_public(v4, fixed) {
            return true;
        }
    }
    if let Some((p, b)) = meet(prefix, bits, SIX_TO_FOUR) {
        let (v4, fixed) = embedded(p, b, 16);
        if v4_public(v4, fixed) {
            return true;
        }
    }
    if let Some((p, b)) = meet(prefix, bits, TEREDO) {
        let (server, server_bits) = embedded(p, b, 32);
        let (client, client_bits) = embedded(!p, b, 96);
        if v4_public(server, server_bits) && v4_public(client, client_bits) {
            return true;
        }
    }
    false
}
