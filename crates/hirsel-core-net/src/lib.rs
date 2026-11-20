//! NET-6 authority and NET-7 ownership, shared by both execution engines.
#[cfg(test)]
hirsel_guard::install!();

use std::net::IpAddr;

/// The public grant spelling, also used in configuration diagnostics.
pub const RULE_SYNTAX: &str = "Net=<Connect|Listen> <prefix>/<len> <lo>-<hi>[;<rule>] or Net=Tls <names> <lo>-<hi> <System|Roots=sha256:<hex>>";

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
pub struct Rule {
    pub dir: u8,
    pub family: u8,
    pub hi: u64,
    pub lo: u64,
    pub bits: u8,
    pub first: u16,
    pub last: u16,
}
impl Rule {
    fn prefix(self) -> u128 {
        (u128::from(self.hi) << 64) | u128::from(self.lo)
    }
    fn width(self) -> u8 {
        if self.family == 0 { 32 } else { 128 }
    }
    fn mask(self) -> u128 {
        if self.bits == 0 {
            0
        } else {
            u128::MAX << (self.width() - self.bits)
        }
    }
    pub fn valid(self) -> bool {
        self.dir <= 1
            && self.family <= 1
            && self.bits <= self.width()
            && (self.family != 0 || (self.hi == 0 && self.lo <= u64::from(u32::MAX)))
            && self.prefix() & !self.mask() == 0
            && self.first <= self.last
            && !(self.family == 1 && self.hi == 0 && self.lo >> 32 == 0xffff)
    }
    pub fn intersect(self, other: Self) -> Option<Self> {
        if self.dir != other.dir || self.family != other.family {
            return None;
        }
        let (short, long) = if self.bits <= other.bits {
            (self, other)
        } else {
            (other, self)
        };
        if long.prefix() & short.mask() != short.prefix() {
            return None;
        }
        let first = self.first.max(other.first);
        let last = self.last.min(other.last);
        (first <= last).then_some(Self {
            first,
            last,
            ..long
        })
    }
    pub fn permits(self, dir: u8, family: u8, hi: u64, lo: u64, port: u16) -> bool {
        if family > 1 || self.dir != dir || self.family != family {
            return false;
        }
        let ip = (u128::from(hi) << 64) | u128::from(lo);
        if family == 1 && ((hi == 0 && lo >> 32 == 0xffff) || hi >> 54 == 0x3fa) {
            return false;
        }
        if ip == 0 && (dir == 0 || self.bits != 0) {
            return false;
        }
        if dir == 1 && port == 0 && (self.first != 0 || self.last != u16::MAX) {
            return false;
        }
        ip & self.mask() == self.prefix() && (self.first..=self.last).contains(&port)
    }
    pub fn parse(text: &str) -> Result<Self, String> {
        let error = || format!("expected --grant {RULE_SYNTAX}: {text}");
        let parts: Vec<_> = text.split_whitespace().collect();
        if parts.len() != 3 {
            return Err(error());
        }
        let dir = match parts[0] {
            "Connect" => 0,
            "Listen" => 1,
            _ => return Err(error()),
        };
        let (prefix, bits) = parts[1].split_once('/').ok_or_else(error)?;
        let ip: IpAddr = prefix.parse().map_err(|_| error())?;
        let (family, hi, lo) = match ip {
            IpAddr::V4(ip) => (0, 0, u64::from(u32::from(ip))),
            IpAddr::V6(ip) => {
                let n = u128::from(ip);
                (1, (n >> 64) as u64, n as u64)
            }
        };
        let (first, last) = parts[2].split_once('-').ok_or_else(error)?;
        let rule = Self {
            dir,
            family,
            hi,
            lo,
            bits: bits.parse().map_err(|_| error())?,
            first: first.parse().map_err(|_| error())?,
            last: last.parse().map_err(|_| error())?,
        };
        if !rule.valid() {
            return Err(error());
        }
        Ok(rule)
    }
}

mod grant;
pub use grant::{GrantRule, RuleView};
mod reactor;
pub use reactor::{Poller, Sockets};
pub mod model;
pub mod tls;
mod tls_rule;
pub use tls_rule::{Name, Pattern, TlsRule, TlsTrust};

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rules_intersect_directions_prefixes_ports_and_validate_boundaries() {
        let parent = Rule::parse("Connect 127.0.0.0/8 20-90").unwrap();
        let child = Rule::parse("Connect 127.0.0.0/16 80-100").unwrap();
        let narrowed = parent.intersect(child).unwrap();
        assert_eq!((narrowed.bits, narrowed.first, narrowed.last), (16, 80, 90));
        assert!(narrowed.permits(0, 0, 0, 0x7f000001, 80));
        for r in [
            "Connect 127.0.0.1/8 0-80",
            "Connect 0.0.0.0/33 0-80",
            "Listen ::/129 0-80",
            "Connect ::ffff:0:0/96 0-80",
            "Connect ::/0 81-80",
        ] {
            assert!(Rule::parse(r).is_err(), "{r}");
        }
        assert!(
            parent
                .intersect(Rule::parse("Listen 127.0.0.0/8 20-90").unwrap())
                .is_none()
        );
        let v6 = Rule::parse("Connect ::/0 0-65535").unwrap();
        assert!(!v6.permits(0, 1, 0, 0xffff7f000001, 80));
        assert!(!v6.permits(0, 1, 0xfe80000000000000, 1, 80));
        assert!(!v6.permits(0, 1, 0, 0, 80));
        let listen = Rule::parse("Listen 0.0.0.0/0 0-65535").unwrap();
        assert!(listen.permits(1, 0, 0, 0, 0));
        assert!(!Rule { last: 80, ..listen }.permits(1, 0, 0, 0x7f000001, 0));
        assert!(
            !Rule::parse("Listen 127.0.0.0/8 0-65535")
                .unwrap()
                .permits(1, 0, 0, 0, 80)
        );
    }
    // TL4. The truth table follows TLS-3's label-suffix rule.
    #[test]
    fn tls_rule_intersections_match_the_name_and_port_sets() {
        let names = ["a.test", "x.a.test", "y.x.a.test", "X.A.TEST.", "xa.test"]
            .map(|text| Name::parse(text).unwrap());
        let rules = [
            (
                "Tls *.a.test 440-450 System",
                [false, true, true, true, false],
                440..=450,
            ),
            (
                "Tls x.a.test 443-455 System",
                [false, true, false, true, false],
                443..=455,
            ),
            (
                "Tls *.x.a.test 440-450 System",
                [false, false, true, false, false],
                440..=450,
            ),
        ]
        .map(|(text, verdicts, ports)| (TlsRule::parse(text).unwrap(), verdicts, ports));
        let root = TlsRule::parse("Tls a.test,*.test 0-65535 System").unwrap();
        for (a, a_names, a_ports) in &rules {
            for (b, b_names, b_ports) in &rules {
                for (first, second) in [(a, b), (b, a)] {
                    let result = root.intersect(first).and_then(|r| r.intersect(second));
                    for (i, name) in names.iter().enumerate() {
                        for port in 439..=456 {
                            let expected = a_names[i]
                                && b_names[i]
                                && a_ports.contains(&port)
                                && b_ports.contains(&port);
                            assert_eq!(
                                result
                                    .as_ref()
                                    .is_some_and(|r| { r.permits(name, port, TlsTrust::System) }),
                                expected,
                                "{first:?} then {second:?}, {name:?}:{port}"
                            );
                        }
                    }
                }
            }
        }
        let roots = TlsRule::parse(&format!(
            "Tls *.a.test 440-450 Roots=sha256:{}",
            "00".repeat(32)
        ))
        .unwrap();
        assert!(root.intersect(&roots).is_none());
        let outside = TlsRule::parse("Tls *.a.test 1-1 System").unwrap();
        assert!(rules[0].0.intersect(&outside).is_none());
    }

    #[test]
    fn write_half_shutdown_does_not_close_read_owner() {
        use std::{io::Read, os::unix::net::UnixStream};
        let (socket, mut peer) = UnixStream::pair().unwrap();
        let mut sockets = Sockets::default();
        let key = sockets.insert(socket.into()).unwrap();
        let (r, w) = sockets.split(key);
        sockets.release(w);
        assert_eq!(peer.read(&mut [0]).unwrap(), 0);
        assert_eq!(sockets.len(), 1);
        sockets.release(r);
        assert!(sockets.is_empty());
    }
}
