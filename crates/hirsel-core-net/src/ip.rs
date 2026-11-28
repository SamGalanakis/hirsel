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

// tools/decoder-fuzz builds this crate's test configuration with the libFuzzer
// callback for nightly discovery. No other build compiles it.
#[cfg(all(test, fuzzing))]
#[path = "../../../tools/decoder-fuzz/host_target.rs"]
mod host_target;
#[cfg(test)]
#[path = "../../hirsel-core-driver/src/property_corpus/reviewed.rs"]
mod reviewed;

/// Discovery's entry to the owner predicates. The first byte selects the
/// surface, so an edited input keeps its parent's. `expect` is a declared
/// status that the observation must also meet. Returns the observation.
#[cfg(all(test, fuzzing))]
fn discovered(input: &[u8], _schedule: usize, expect: Option<&str>) -> String {
    let surface = input
        .split_first()
        .and_then(|(&index, bytes)| Some((*laws::SURFACES.get(usize::from(index))?, bytes)));
    let observation = match surface {
        Some((surface, bytes)) => match laws::observe(surface, bytes) {
            Ok(projection) => format!("accepted {surface} {projection}"),
            Err(reason) => format!("refused {surface} {reason}"),
        },
        None => "refused surface unknown".into(),
    };
    if let Some(expect) = expect {
        let status = observation.split(' ').next().unwrap();
        assert_eq!(status, expect, "discovered: declared status");
    }
    observation
}

/// The socket-free decoders of this crate under their owner predicates: each
/// returns an accepted input's projection or a refusal's reason.
#[cfg(test)]
mod laws {
    use super::*;

    pub(super) const SURFACES: [&str; 4] = ["address-records", "tls-name", "tls-policy", "scram"];

    /// Resolved address records. Accepted bytes are exactly the records of
    /// the addresses read from them.
    fn records(bytes: &[u8]) -> Result<String, String> {
        let Some(addresses) = ip_records(bytes) else {
            return Err("records".into());
        };
        let again: Vec<u8> = addresses.iter().copied().flat_map(ip_record).collect();
        assert_eq!(again, bytes, "accepted records are not their addresses'");
        let text: Vec<String> = addresses.iter().map(ToString::to_string).collect();
        Ok(text.join(","))
    }

    pub(super) fn observe(surface: &str, bytes: &[u8]) -> Result<String, String> {
        match surface {
            "address-records" => records(bytes),
            "tls-name" => crate::tls::laws::name(bytes),
            "tls-policy" => crate::tls_rule::laws::policy(bytes),
            "scram" => crate::scram::laws::exchange(bytes),
            other => panic!("no surface {other}"),
        }
    }

    #[test]
    fn reviewed_net() {
        #[derive(serde::Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Limits {
            bytes: usize,
        }
        #[derive(serde::Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Setup {
            surface: String,
            password: Option<String>,
        }
        #[derive(Debug, PartialEq, serde::Deserialize)]
        #[serde(rename_all = "snake_case")]
        enum Expected {
            Accepted(String),
            Refused(String),
        }
        #[derive(serde::Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Schedule {
            kind: String,
        }
        type Case = reviewed::Case<Limits, Setup, Expected, Schedule>;
        let cases: Vec<(Case, Vec<u8>)> = reviewed::read(
            include_bytes!("../../../core/tests/decoder-corpus/net.json"),
            "net",
            "reviewed_net",
            "socket-free-input",
            "net-published-vectors",
        )
        .unwrap();
        // Accepted and refused cases of each surface.
        let mut seen = [[0; 2]; SURFACES.len()];
        for (case, bytes) in cases {
            let id = &case.id;
            let surface = case.setup.surface.as_str();
            let index = SURFACES.iter().position(|name| *name == surface).unwrap();
            assert!(bytes.len() <= case.limits.bytes, "{id}");
            assert!(
                matches!(case.schedules.as_slice(), [one] if one.kind == "contiguous"),
                "{id}"
            );
            let password = (surface == "scram").then_some(crate::scram::laws::PASSWORD);
            assert_eq!(
                case.setup.password.as_deref().map(str::as_bytes),
                password,
                "{id}"
            );
            // Two replays, each over its own copy of the bytes.
            let first = observe(surface, &bytes.clone());
            assert_eq!(observe(surface, &bytes.clone()), first, "{id}: replay");
            let observed = match first {
                Ok(projection) => Expected::Accepted(projection),
                Err(reason) => Expected::Refused(reason),
            };
            assert_eq!(observed, case.expected, "{id}");
            seen[index][usize::from(matches!(observed, Expected::Refused(_)))] += 1;
            eprintln!("{id}: {surface} {observed:?} twice");
        }
        assert!(seen.iter().flatten().all(|&count| count > 0), "{seen:?}");
    }
}
