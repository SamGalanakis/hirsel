//! Host configuration projected from the toolchain-owned data schemas.
use super::{Error, finite::Failure};
use crate::operator::{Forward, Front, Record, Registration};
use hirsel_config_schema::{
    Config,
    host_v1::{self as h, Grant as G, NetRule as N},
};
use std::io::Read;
use std::path::{Path, PathBuf};

pub(super) fn decode<T: Config>(reader: impl Read) -> Result<T, Failure> {
    let limits = hirsel_config_schema::Limits::default();
    let mut bytes = Vec::new();
    // The schema reader enforces its input limit before lexing. Bound the read too.
    let max = limits.input_bytes;
    reader
        .take(max as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(Failure::io)?;
    T::read(&bytes, limits).map_err(|e| Failure::input("document", format!("{e:?}")))
}

fn grant(g: G) -> String {
    match g {
        G::Dir(p) => format!("Dir={p}"),
        G::Compiler => "Compiler".into(),
        G::ScopedDir { kind, app } => format!(
            "Dir=xdg-{}:{app}",
            match kind {
                h::DirectoryKind::ConfigDir => "config",
                h::DirectoryKind::DataDir => "data",
                h::DirectoryKind::StateDir => "state",
                h::DirectoryKind::CacheDir => "cache",
            }
        ),
        G::Clock => "Clock".into(),
        G::Random => "Random".into(),
        G::Tasks => "Tasks".into(),
        G::Stdin => "Stdin".into(),
        G::Stdout => "Stdout".into(),
        G::Stderr => "Stderr".into(),
        G::Process => "Process".into(),
        G::Foreign(n) => format!("Foreign={n}"),
        G::Exec { policy, profiles } => format!("Exec={policy} {}", profiles.join(",")),
        G::Env(names) => format!("Env={}", names.join(",")),
        G::Secret {
            parameter,
            source,
            policy,
        } => {
            let source = match source {
                h::Binding::Environment(p) => format!("env:{p}"),
                h::Binding::File(p) => format!("file:{p}"),
            };
            format!("Secret:{parameter}={source} {policy}")
        }
        G::Cred {
            parameter,
            source,
            policy,
        } => format!("Cred:{parameter}={source} {policy}"),
        G::Net(rule) => format!(
            "Net={}",
            match rule {
                N::Listen {
                    cidr,
                    first_port,
                    last_port,
                } => format!("Listen {cidr} {first_port}-{last_port}"),
                N::Connect {
                    cidr,
                    first_port,
                    last_port,
                    public_only,
                } => format!(
                    "Connect {cidr} {first_port}-{last_port}{}",
                    if public_only.unwrap_or(false) {
                        " public"
                    } else {
                        ""
                    }
                ),
                N::UdpBind {
                    cidr,
                    first_port,
                    last_port,
                } => format!("UdpBind {cidr} {first_port}-{last_port}"),
                N::UdpSend {
                    cidr,
                    first_port,
                    last_port,
                } => format!("UdpSend {cidr} {first_port}-{last_port}"),
                N::UnixBind { label, path } => format!("UnixBind {label} {path}"),
                N::UnixSend { label, path } => format!("UnixSend {label} {path}"),
                N::UnixRoot { label, path } => format!("UnixRoot {label} {path}"),
                N::UnixListen { label, path } => format!("UnixListen {label} {path}"),
                N::UnixConnect { label, path } => format!("UnixConnect {label} {path}"),
                N::Resolve(names) => format!("Resolve {names}"),
                N::Tls(policy) => format!("Tls {policy}"),
            }
        ),
    }
}

pub(super) fn group(path: &Path) -> Result<(Registration, Vec<String>), Failure> {
    let g: h::Group = decode(std::fs::File::open(path).map_err(Failure::io)?)?;
    let parent = path.parent().unwrap_or(Path::new("."));
    let absolute = |s: String| -> Result<String, Error> {
        let p: PathBuf = std::path::absolute(parent.join(s))?;
        Ok(p.into_os_string()
            .into_string()
            .map_err(|_| "path must be UTF-8")?)
    };
    let make = || -> Result<_, Error> {
        let record = g
            .recording
            .map(|r| -> Result<_, Error> {
                Ok(Record {
                    dir: absolute(r.dir)?,
                    retention_s: r
                        .retention_days
                        .checked_mul(86400)
                        .filter(|s| *s > 0)
                        .ok_or("invalid recording retention")?,
                })
            })
            .transpose()?;
        let front = g.front.map(|f| match f {
            h::Front::Http(names) => Front {
                names,
                forward: Forward::Http,
            },
            h::Front::Stream(names) => Front {
                names,
                forward: Forward::Stream,
            },
        });
        let packages = g
            .package_roots
            .iter()
            .map(|v| {
                crate::package_content::Root::parse(v)
                    .map(|r| r.0)
                    .ok_or("invalid package root")
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok((
            Registration {
                name: g.name,
                tenant: g.tenant,
                workdir: absolute(g.workdir)?,
                trusted: g.trusted,
                listen: g.listen,
                store: absolute(g.store)?,
                memory: g.memory_bytes,
                args: g.args,
                record,
                front,
                packages,
            },
            {
                let (network, mut grants): (Vec<_>, Vec<_>) = g
                    .grants
                    .into_iter()
                    .map(grant)
                    .partition(|line| line.starts_with("Net="));
                if !network.is_empty() {
                    let rules = network
                        .iter()
                        .map(|s| &s[4..])
                        .collect::<Vec<_>>()
                        .join(";");
                    grants.push(format!("Net={rules}"));
                }
                grants
            },
        ))
    };
    make().map_err(|e| Failure::input("create", e))
}
