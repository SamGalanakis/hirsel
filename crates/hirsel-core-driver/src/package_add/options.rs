// `hirsel add`'s command line: the SOURCE forms and the options (packages.md section 11).

use super::{Failure, git};
use crate::cli::{AddOption as Opt, Request};
use crate::package_content::{Id, Root};
use crate::package_store::PackageStore;
use std::path::PathBuf;

pub(crate) enum Source {
    Registry(crate::package_add::registry_select::Query),
    Archive(String),
    Url(String),
    Git(git::Request),
    Store(Root),
    Toolchain(Id),
    Member(String),
}

impl Source {
    pub(super) fn parse(text: String) -> Result<Self, Failure> {
        if text.starts_with("https://") {
            Ok(Self::Url(text))
        } else if text.starts_with("git+") {
            Ok(Self::Git(git::Request::parse_git(&text)?))
        } else if text.starts_with("github:") {
            Ok(Self::Git(git::Request::parse_github(&text)?))
        } else if text.starts_with("sha256:") {
            Ok(Self::Store(Root::parse(&text).ok_or(
                "expected sha256: followed by 64 lowercase hex digits",
            )?))
        } else if text.contains("://") {
            Err("add SOURCE requires an https:// URL, a git+https:// or github: source, a .hpkg path, a sha256: root, NAME@MAJOR, or a member directory".into())
        } else if text.ends_with(".hpkg") {
            Ok(Self::Archive(text))
        } else if let Some(query) = crate::package_add::registry_select::Query::parse(&text)? {
            Ok(Self::Registry(query))
        } else if let Some((name, major)) = text.split_once('@')
            && name.starts_with(|c: char| c.is_ascii_lowercase())
            && name
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
            && !major.is_empty()
            && major.bytes().all(|b| b.is_ascii_digit())
        {
            let major = major
                .parse()
                .map_err(|_| format!("{text}: the major does not fit in 32 bits"))?;
            Ok(Self::Toolchain(Id {
                name: name.to_owned(),
                major,
            }))
        } else {
            Ok(Self::Member(text))
        }
    }
}

pub(super) struct Options {
    pub(super) source: Source,
    pub(super) expected: Option<Root>,
    pub(super) store: PackageStore,
    pub(super) alias: Option<String>,
    pub(super) workspace: bool,
    pub(super) dry_run: bool,
    pub(super) json: bool,
}

impl Options {
    pub(super) fn read(invocation: Request<Opt>) -> Result<Self, Failure> {
        let mut expected = None;
        let mut store = None;
        let mut alias = None;
        let mut workspace = false;
        let mut dry_run = false;
        let mut json = false;
        for option in invocation.options {
            match option {
                Opt::Locked | Opt::Frozen => {
                    let flag = if option == Opt::Locked {
                        "--locked"
                    } else {
                        crate::package_lock::FROZEN_FLAG
                    };
                    return Err(
                        format!("hirsel.hcfg needs to be updated but {flag} was passed").into(),
                    );
                }
                Opt::AddRoot(value) => {
                    expected = Some(
                        Root::parse(&value)
                            .ok_or("--root requires sha256: followed by 64 lowercase hex digits")?,
                    );
                }
                Opt::AddAs(value) => {
                    if !crate::package_manifest::name::alias(&value) {
                        return Err(format!("{value:?} is not a dependency alias").into());
                    }
                    alias = Some(value);
                }
                Opt::AddWorkspace => workspace = true,
                Opt::AddDryRun => dry_run = true,
                Opt::Json => json = true,
                Opt::PackageStore(value) => store = Some(PathBuf::from(value)),
            }
        }
        let source = Source::parse(invocation.positional.ok_or("add requires SOURCE")?)?;
        if expected.is_some()
            && matches!(
                source,
                Source::Store(_) | Source::Toolchain(_) | Source::Member(_) | Source::Registry(_)
            )
        {
            return Err("--root applies to a fetched source".into());
        }
        Ok(Self {
            source,
            expected,
            store: PackageStore::with_transport(
                store,
                crate::package_lock::Mode::Update.transport(),
            ),
            alias,
            workspace,
            dry_run,
            json,
        })
    }
}
