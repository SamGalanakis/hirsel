use super::{Grants, cfg};
use cfg::{Directory as D, Grant as G, NetRule as N, NonDirGrant as O};
use hirsel_core_net::{self as net, AddressDirection as A, GrantRule, Reach, UnixDirection as U};

pub(super) fn directory(dir: D) -> Result<String, String> {
    match dir {
        D::Dir(path) if !path.contains('\0') => Ok(path),
        D::Dir(_) => Err("directory path contains NUL".into()),
        D::ScopedDir { kind, app } => {
            let form = match kind {
                cfg::DirectoryKind::ConfigDir => 0,
                cfg::DirectoryKind::CacheDir => 1,
                cfg::DirectoryKind::DataDir => 2,
                cfg::DirectoryKind::StateDir => 3,
            };
            if !hirsel_core_ids::launch::valid_app(&app) {
                return Err("invalid application directory name".into());
            }
            Ok(hirsel_core_ids::launch::Xdg { form, app: &app }.slot())
        }
    }
}

pub(crate) fn non_dir(g: O) -> G {
    match g {
        O::Clock => G::Clock,
        O::Random => G::Random,
        O::Tasks => G::Tasks,
        O::Stdin => G::Stdin,
        O::Stdout => G::Stdout,
        O::Stderr => G::Stderr,
        O::Process => G::Process,
        O::Compiler => G::Compiler,
        O::Net(n) => G::Net(n),
        O::Env(n) => G::Env(n),
        O::Foreign(n) => G::Foreign(n),
        O::Secret {
            parameter,
            source,
            policy,
        } => G::Secret {
            parameter,
            source,
            policy,
        },
        O::Cred {
            parameter,
            source,
            policy,
        } => G::Cred {
            parameter,
            source,
            policy,
        },
        O::Exec { policy, profiles } => G::Exec { policy, profiles },
    }
}

fn network(net: &mut net::NetGrant, rule: N) -> Result<(), String> {
    let address = |dir, cidr: &str, first, last, reach| {
        net::Rule::from_cidr(dir, cidr, first, last, reach).map(GrantRule::Address)
    };
    let unix = |dir, label: &str, path: &str| {
        (!label.contains('\0'))
            .then(|| net::UnixRule::new(dir, label, path))
            .flatten()
            .map(GrantRule::Unix)
    };
    let rule = match rule {
        N::Listen {
            cidr,
            first_port,
            last_port,
        } => address(A::Listen, &cidr, first_port, last_port, Reach::Any),
        N::Connect {
            cidr,
            first_port,
            last_port,
            public_only,
        } => address(
            A::Connect,
            &cidr,
            first_port,
            last_port,
            if public_only.unwrap_or(false) {
                Reach::Public
            } else {
                Reach::Any
            },
        ),
        N::UdpBind {
            cidr,
            first_port,
            last_port,
        } => address(A::UdpBind, &cidr, first_port, last_port, Reach::Any),
        N::UdpSend {
            cidr,
            first_port,
            last_port,
        } => address(A::UdpSend, &cidr, first_port, last_port, Reach::Any),
        N::UnixRoot { label, path } => return net.add_root(&label, &path).map_err(str::to_owned),
        N::UnixListen { label, path } => unix(U::Listen, &label, &path),
        N::UnixConnect { label, path } => unix(U::Connect, &label, &path),
        N::UnixBind { label, path } => unix(U::Bind, &label, &path),
        N::UnixSend { label, path } => unix(U::Send, &label, &path),
        N::Resolve(patterns) => net::ResolveRule::new(&patterns).map(GrantRule::Resolve),
        N::Tls(policy) => net::TlsRule::parse(&format!("Tls {policy}")).map(GrantRule::Tls),
    }
    .ok_or("invalid network rule")?;
    net.rules.push(rule);
    Ok(())
}

pub(super) fn decode(values: impl IntoIterator<Item = G>) -> Result<Grants, String> {
    use hirsel_core_ir::release_recipe::GrantKind as Kind;
    let mut out = Grants::default();
    let mut net = net::NetGrant::default();
    for value in values {
        match value {
            G::Clock => out.mask |= 1 << Kind::Clock as u8,
            G::Random => out.mask |= 1 << Kind::Random as u8,
            G::Tasks => out.mask |= 1 << Kind::Tasks as u8,
            G::Stdin => out.mask |= 1 << Kind::Stdin as u8,
            G::Stdout => out.mask |= 1 << Kind::Stdout as u8,
            G::Stderr => out.mask |= 1 << Kind::Stderr as u8,
            G::Process => out.mask |= 1 << Kind::Process as u8,
            G::Compiler => out.mask |= 1 << Kind::Compiler as u8,
            G::Dir(p) => out.directories.push(directory(D::Dir(p))?),
            G::ScopedDir { kind, app } => {
                out.directories.push(directory(D::ScopedDir { kind, app })?)
            }
            G::Net(rule) => {
                network(&mut net, rule)?;
                out.mask |= 1 << Kind::Net as u8;
            }
            G::Env(names) => {
                if names.is_empty() {
                    return Err("empty environment grant".into());
                }
                let patterns = net::EnvPatterns::from_patterns(names.iter().map(String::as_str))
                    .ok_or(net::EnvPatterns::SYNTAX)?;
                out.env
                    .get_or_insert_with(Default::default)
                    .extend(patterns);
                out.mask |= 1 << Kind::Env as u8;
            }
            G::Secret {
                parameter,
                source,
                policy,
            } => {
                let source = match source {
                    cfg::Binding::Environment(v) => format!("env:{v}"),
                    cfg::Binding::File(v) => format!("file:{v}"),
                };
                if out.secrets.iter().any(|g| g.parameter == parameter) {
                    return Err("a Secret parameter can be granted only once".into());
                }
                let policy = net::SecretPolicy::parse(&policy).ok_or(net::SecretGrant::SYNTAX)?;
                out.secrets.push(
                    net::SecretGrant::new(&parameter, &source, &policy.text())
                        .ok_or(net::SecretGrant::SYNTAX)?,
                );
            }
            G::Cred {
                parameter,
                source,
                policy,
            } => {
                if out.creds.iter().any(|g| g.parameter == parameter) {
                    return Err("a Cred parameter can be granted only once".into());
                }
                let policy = net::SecretPolicy::parse(&policy).ok_or(net::CredGrant::SYNTAX)?;
                out.creds
                    .push(net::CredGrant::new(&parameter, &source, &policy.text())?);
            }
            G::Foreign(name) => {
                hirsel_core_ids::DefPath::from_full(&name)
                    .ok_or("expected a qualified Foreign interface")?;
                if !out.foreign.insert(name) {
                    return Err("a Foreign interface can be granted only once".into());
                }
            }
            G::Exec { policy, profiles } => out.exec.push(crate::exec_launch::Binding {
                request: net::exec::Request::new(policy, profiles)?,
                policy: None,
                workdir: None,
            }),
        }
    }
    net.check_roots()?;
    out.net_rules = net.rules;
    out.unix_roots = net.roots;
    Ok(out)
}
