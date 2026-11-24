//! NET-6 authority and NET-7 ownership, shared by both execution engines.
#[cfg(test)]
#[path = "../../../tools/test_support/loopback.rs"]
#[allow(dead_code)]
mod loopback;
#[cfg(test)]
hirsel_guard::install!();

use std::net::IpAddr;

pub mod agreement_registry;
mod env;
pub mod exec;
pub mod udp;
pub mod unix;
pub use env::{EnvPatterns, EnvSnapshot};

/// The public grant spelling, also used in configuration diagnostics.
pub const RULE_SYNTAX: &str = "Net=<Connect|Listen|UdpBind|UdpSend> <prefix>/<len> <lo>-<hi>[;<rule>] or Net=Tls <names> <lo>-<hi> <System|Roots=sha256:<hex>> or Net=Resolve <names> or Net=UnixRoot <label> <absolute-path>;<UnixConnect|UnixListen|UnixBind|UnixSend> <label> <path>";

/// Address rule tags in grant rows and tape headers. Tags 2 through 5
/// identify other kinds of Net rules and are separate from Core variants.
#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AddressDirection {
    Connect = 0,
    Listen = 1,
    UdpBind = 6,
    UdpSend = 7,
}

impl AddressDirection {
    pub const ALL: [Self; 4] = [Self::Connect, Self::Listen, Self::UdpBind, Self::UdpSend];

    pub fn from_tag(tag: u8) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|direction| *direction as u8 == tag)
    }

    pub fn from_variant(variant: u8) -> Option<Self> {
        Self::ALL.get(usize::from(variant)).copied()
    }

    pub fn word(self) -> &'static str {
        match self {
            Self::Connect => "Connect",
            Self::Listen => "Listen",
            Self::UdpBind => "UdpBind",
            Self::UdpSend => "UdpSend",
        }
    }

    fn is_bind(self) -> bool {
        matches!(self, Self::Listen | Self::UdpBind)
    }
}

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
        AddressDirection::from_tag(self.dir).is_some()
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
        self.covers_address(dir, family, hi, lo)
            && if AddressDirection::from_tag(dir).is_some_and(AddressDirection::is_bind)
                && port == 0
            {
                self.first <= 1 && self.last == u16::MAX
            } else {
                (self.first..=self.last).contains(&port)
            }
    }

    fn covers_address(self, dir: u8, family: u8, hi: u64, lo: u64) -> bool {
        if family > 1 || self.dir != dir || self.family != family {
            return false;
        }
        let ip = (u128::from(hi) << 64) | u128::from(lo);
        if family == 1 && ((hi == 0 && lo >> 32 == 0xffff) || hi >> 54 == 0x3fa) {
            return false;
        }
        let Some(direction) = AddressDirection::from_tag(dir) else {
            return false;
        };
        if ip == 0 && (!direction.is_bind() || self.bits != 0) {
            return false;
        }
        if direction == AddressDirection::UdpSend && family == 0 && ip == u128::from(u32::MAX) {
            return false;
        }
        ip & self.mask() == self.prefix()
    }
    pub fn parse(text: &str) -> Result<Self, String> {
        let error = || format!("expected --grant {RULE_SYNTAX}: {text}");
        let parts: Vec<_> = text.split_whitespace().collect();
        if parts.len() != 3 {
            return Err(error());
        }
        let dir = AddressDirection::ALL
            .into_iter()
            .find(|direction| direction.word() == parts[0])
            .ok_or_else(error)? as u8;
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

/// Refusal specific to an ephemeral listen whose matching rules leave a port uncovered.
pub const EPHEMERAL_OUTSIDE_NET_GRANT: u8 = 11;

/// A kernel-selected port can be any nonzero port, including when the host's
/// ephemeral range is changed concurrently. Matching rules together must
/// cover that interval before a port-zero bind can be issued.
pub fn address_denial(
    mut rules: impl Iterator<Item = Rule> + Clone,
    dir: u8,
    family: u8,
    hi: u64,
    lo: u64,
    port: u16,
) -> Option<u8> {
    if !AddressDirection::from_tag(dir).is_some_and(AddressDirection::is_bind) || port != 0 {
        return (!rules.any(|rule| rule.permits(dir, family, hi, lo, port))).then_some(1);
    }
    let matching = rules.filter(|rule| rule.covers_address(dir, family, hi, lo));
    if matching.clone().next().is_none() {
        return Some(1);
    }
    let mut next = 1;
    loop {
        let Some(last) = matching
            .clone()
            .filter(|rule| rule.first <= next && rule.last >= next)
            .map(|rule| rule.last)
            .max()
        else {
            return Some(EPHEMERAL_OUTSIDE_NET_GRANT);
        };
        if last == u16::MAX {
            return None;
        }
        next = last + 1;
    }
}

mod ip;
pub mod marks;
pub use ip::{IP_RECORD_LEN, Ip, ip_record, ip_records};
mod grant;
pub use grant::{
    GrantRule, NetGrant, ResolveRule, RuleView, UnixDirection, UnixRule, valid_relative_path,
};
mod reactor;
pub use reactor::{
    Here, Poller, REACTOR_ENV, Reactor, SEND_ZC_ENV, SHARED_UNIT_SHIFT, SocketTransfer, Sockets,
    TxSlot, TxState, UdpMember, reactor, slot_epoll_pollers, slot_pollers,
};
/// mc-units-untied: kernel waits (epoll_wait or io_uring_enter) made by run reactors.
pub static EPOLL_WAITS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
mod signature;
pub use signature::{
    SignatureAlgorithm, verify_ed25519, verify_p256_asn1, verify_p256_fixed, verify_p384_asn1,
    verify_p384_fixed, verify_rsa_pkcs1_sha256, verify_rsa_pss_sha256, verify_signature,
};
mod split_mix;
pub use split_mix::SplitMix64;
pub mod argon2id;
pub mod scram;
pub mod tls;
mod tls_rule;
pub use tls_rule::{Name, Pattern, TlsRule, TlsTrust};
mod cred_grant;
pub mod key_agreement;
pub mod key_wrap;
pub use cred_grant::{CredGrant, CredSource, StsSource};
pub mod p256_key;
pub mod packet_cipher;
pub mod random_stream;
pub mod secret_aead;
mod secret_grant;
pub mod secret_kdf;
pub mod srtp_cipher;
pub mod tls_kdf;
pub use secret_grant::{
    HmacSha2, SecretGrant, SecretPolicy, SecretSource, SecretUse, derive_hmac_sha256,
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn udp_grants_bind_and_send_with_distinct_authority() {
        let bind = Rule::parse("UdpBind 0.0.0.0/0 1-1023").unwrap();
        let remaining = Rule::parse("UdpBind 0.0.0.0/0 1024-65535").unwrap();
        let send = Rule::parse("UdpSend 0.0.0.0/0 1-65535").unwrap();
        let tcp = Rule::parse("Listen 0.0.0.0/0 1-65535").unwrap();
        let loopback = Rule::parse("UdpBind 127.0.0.0/8 1-65535").unwrap();
        assert!(bind.intersect(tcp).is_none());
        assert!(bind.intersect(send).is_none());
        assert_eq!(bind.intersect(loopback).unwrap().last, 1023);
        assert_eq!(
            address_denial([bind, send, tcp].into_iter(), bind.dir, 0, 0, 0, 0),
            Some(EPHEMERAL_OUTSIDE_NET_GRANT),
        );
        assert_eq!(
            address_denial([bind, remaining].into_iter(), bind.dir, 0, 0, 0, 0),
            None,
        );
        assert_eq!(
            address_denial([loopback].into_iter(), bind.dir, 0, 0, 0, 0),
            Some(1)
        );
        assert!(send.permits(send.dir, 0, 0, 0x7f000001, 53));
        assert!(!send.permits(send.dir, 0, 0, 0, 53));
        assert!(!send.permits(send.dir, 0, 0, u64::from(u32::MAX), 53));
        let v6 = Rule::parse("UdpSend ::/0 1-65535").unwrap();
        assert!(!v6.permits(v6.dir, 1, 0, 0xffff_7f00_0001, 53));
        assert!(!v6.permits(v6.dir, 1, 0xfe80_0000_0000_0000, 1, 53));
        assert_eq!(GrantRule::Address(send).logical_size(), 37);
        // SAFETY: an address row contains no borrowed text.
        assert_eq!(
            unsafe { RuleView::address(send).decode() },
            Some(GrantRule::Address(send))
        );
    }

    #[test]
    fn secret_grant_names_destinations_only_inside_tls_send() {
        // TLS-5: `to <patterns>` is CLI sugar for tls_send(<patterns>), so
        // `to mac` sends to the host `mac` and grants no MAC use; a bare name
        // is no policy; the policy is the longest suffix that parses.
        let policy = |spec: &str| SecretGrant::parse(spec).map(|g| (g.source, g.policy));
        let pair = |source: &str, policy: &str| Some((source.to_owned(), policy.to_owned()));
        assert_eq!(
            policy("Secret:k=env:V to mac"),
            pair("env:V", "tls_send(mac)")
        );
        assert_eq!(
            policy("Secret:k=env:V to ALPHA.TEST.,10.0.0.7 mac"),
            pair("env:V", "tls_send(alpha.test,10.0.0.7) mac")
        );
        assert_eq!(
            policy("Secret:k=file:/run/my mac/db wrap sign(ed25519)"),
            pair("file:/run/my mac/db", "sign(ed25519) wrap")
        );
        assert_eq!(policy("Secret:k=env:V alpha.test"), None);
        assert_eq!(
            SecretPolicy::parse("tls_send(a.test) tls_send(b.test)"),
            None
        );
    }
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
    #[test]
    fn exec_narrowing_matches_both_grants_and_charges_the_result() {
        use super::exec::{ArgToken, ExecRule, ExecRules, Form, Names, ProfileRules};
        use ArgToken::{Any, Lit, Rest};
        use std::collections::BTreeMap;

        // Scan the original pattern, independently of prefix lengths and intersection.
        fn admits(tokens: &[ArgToken], args: &[&[u8]]) -> bool {
            match tokens.split_first() {
                None => args.is_empty(),
                Some((Rest, _)) => true,
                Some((token, rest)) => args.split_first().is_some_and(|(arg, tail)| {
                    (*token == Any || matches!(token, Lit(bytes) if bytes == arg))
                        && admits(rest, tail)
                }),
            }
        }

        let a = Lit(b"a".to_vec());
        let b = Lit(b"b".to_vec());
        let c = Lit(b"c".to_vec());
        let examples = [
            (
                vec![a.clone(), Rest],
                vec![a.clone(), b.clone(), c.clone()],
                Some(vec![a.clone(), b.clone(), c]),
            ),
            (vec![a.clone()], vec![a.clone(), b.clone(), Rest], None),
            (vec![Any], vec![Any], Some(vec![Any])),
            (vec![], vec![Rest], Some(vec![])),
        ];
        for (left, right, expected) in examples {
            assert_eq!(
                Form::new(left)
                    .unwrap()
                    .intersect(&Form::new(right).unwrap()),
                expected.map(|tokens| Form::new(tokens).unwrap()),
            );
        }

        let mut inputs = vec![vec![]];
        for len in 1..=4 {
            for word in 0..1 << len {
                inputs.push(
                    (0..len)
                        .map(|i| {
                            if word & (1 << i) == 0 {
                                b"a".as_slice()
                            } else {
                                b"b".as_slice()
                            }
                        })
                        .collect(),
                );
            }
        }
        let mut forms = vec![vec![], vec![Rest]];
        for x in [a.clone(), b.clone(), Any] {
            forms.push(vec![x.clone()]);
            forms.push(vec![x.clone(), Rest]);
            for y in [a.clone(), b.clone(), Any] {
                forms.push(vec![x.clone(), y.clone()]);
                forms.push(vec![x.clone(), y, Rest]);
            }
        }
        let program = || "git".to_owned();
        let names = |s| Names::parse(s).unwrap();
        let host = |tokens: &[ArgToken]| {
            ExecRules::host(BTreeMap::from([(
                program(),
                ProfileRules::host(
                    vec![Form::new(tokens.to_vec()).unwrap()],
                    ["CALLER".into()].into(),
                    ["work".into()].into(),
                    ["FIXED".into()].into(),
                ),
            )]))
        };

        for left in &forms {
            let parent = host(left);
            for right in &forms {
                let narrowed = parent
                    .narrow(&ExecRule::Form(
                        program(),
                        Form::new(right.clone()).unwrap(),
                    ))
                    .unwrap();
                for args in &inputs {
                    assert_eq!(
                        narrowed.permits("git", args, &["CALLER"], "work"),
                        admits(left, args) && admits(right, args),
                        "{left:?} and {right:?} for {args:?}",
                    );
                }
            }
        }

        let parent = ExecRules::universal();
        let deny = parent
            .narrow(&ExecRule::Dirs(program(), names("")))
            .unwrap();
        for name in ["git", "other"] {
            for args in &inputs {
                for label in ["work", "other"] {
                    assert_eq!(deny.permits(name, args, &["CALLER"], label), name != "git");
                }
            }
        }
        assert!(
            !deny
                .narrow(&ExecRule::Programs(names("git")))
                .unwrap()
                .permits("git", &[], &[], "work")
        );
        assert_eq!(deny.charge(), Some(56)); // 32 + 8 remainder + 8 + 3, rounded.
        let narrow = parent
            .narrow(&ExecRule::Form(
                program(),
                Form::new(vec![Lit(b"status".to_vec())]).unwrap(),
            ))
            .unwrap();
        assert_eq!(narrow.charge(), Some(96)); // EXEC-2's worked example.
        let fixed = host(&[Rest])
            .narrow(&ExecRule::EnvKeys(program(), names("")))
            .unwrap();
        assert!(fixed.permits("git", &[], &[], "work"));
        assert!(!fixed.permits("git", &[], &["FIXED"], "work"));
        assert!(!fixed.permits("git", &[], &["CALLER"], "work"));
        assert_eq!(fixed.charge(), Some(80)); // 32 + 11 + 16 + 8 + 12, rounded.
    }

    #[test]
    fn unix_rules_intersect_only_the_same_direction_root_and_path() {
        let exact = GrantRule::parse("UnixConnect pg .s.PGSQL.5432").unwrap();
        assert_eq!(exact.logical_size(), 24); // 8 + 2 + 13, rounded to 8.
        for other in [
            "UnixListen pg .s.PGSQL.5432",
            "UnixConnect other .s.PGSQL.5432",
            "UnixConnect pg .s.PGSQL.1234",
            "Connect 0.0.0.0/0 0-65535",
            "Tls pg.test 0-65535 System",
            "Resolve pg.test",
        ] {
            let other = GrantRule::parse(other).unwrap();
            assert_eq!(exact.intersect(&other), None);
            assert_eq!(other.intersect(&exact), None);
        }
        assert_eq!(exact.intersect(&exact), Some(exact));
        let parsed = NetGrant::parse(["UnixConnect pg x", "UnixRoot pg /run/pg"]).unwrap();
        assert_eq!(parsed.roots["pg"], "/run/pg");
        assert_eq!(parsed.rules.len(), 1);
        for rows in [
            vec!["UnixConnect pg x"],
            vec!["UnixRoot pg /run/pg", "UnixRoot pg /elsewhere"],
            vec!["UnixRoot pg run/pg"],
            vec!["UnixRoot pg /run/pg", "UnixConnect pg ../x"],
        ] {
            assert!(NetGrant::parse(rows).is_err());
        }
        assert!(
            NetGrant::parse(["UnixRoot pg /run/pg"])
                .unwrap()
                .rules
                .is_empty()
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
    fn resolve_narrow_only_keeps_the_shared_names() {
        let parent = GrantRule::parse("Resolve *.a.test,127.0.0.1").unwrap();
        let requested = GrantRule::parse("Resolve *.test,127.0.0.2,127.0.0.1").unwrap();
        for (a, b) in [(&parent, &requested), (&requested, &parent)] {
            let GrantRule::Resolve(result) = a.intersect(b).unwrap() else {
                panic!("NET-11 intersection must be a Resolve rule");
            };
            for (name, shared) in [
                ("x.a.test", true),
                ("X.A.TEST.", true),
                ("deep.x.a.test", true),
                ("a.test", false),
                ("xa.test", false),
                ("x.test", false),
                ("127.0.0.1", true),
                ("127.0.0.2", false),
            ] {
                assert_eq!(
                    result.permits(&Name::parse(name).unwrap()),
                    shared,
                    "resolve_narrow_label_and_literal_authority: {name}"
                );
            }
        }
        let outside = GrantRule::parse("Resolve xa.test").unwrap();
        assert!(parent.intersect(&outside).is_none());
        let tls = GrantRule::parse("Tls *.a.test 0-65535 System").unwrap();
        assert!(parent.intersect(&tls).is_none());
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
