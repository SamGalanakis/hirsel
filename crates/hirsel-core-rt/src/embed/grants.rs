//! EMB-3 & EMB-7 embedded grant parsing, directory base resolution, secret
//! source validation, and descriptor-derived `StoreOverlapsDir` enforcement.
//!
//! Reads zero environment variables, zero current working directory, and zero
//! inherited file descriptors.
use crate::entry::{
    ARGS, CLOCK, CRED, DIR, ENV, EXEC, FOREIGN, NET, PROCESS, RANDOM, Requirement, SECRET, STDERR,
    STDIN, STDOUT, TASKS,
};
use hirsel_core_ids::launch::{XDG_FORMS, valid_app};
use hirsel_core_layout::{Os, Target};
use std::{
    collections::BTreeMap,
    ffi::CString,
    io,
    os::{
        fd::{AsRawFd, FromRawFd, OwnedFd},
        unix::ffi::OsStrExt,
    },
    path::{Path, PathBuf},
};

/// Parsed `dir_bases` lines (`config=`, `cache=`, `data=`, `state=`).
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct DirBases {
    pub(crate) config: Option<PathBuf>,
    pub(crate) cache: Option<PathBuf>,
    pub(crate) data: Option<PathBuf>,
    pub(crate) state: Option<PathBuf>,
}

impl DirBases {
    pub(crate) fn parse(raw: &[u8]) -> Result<Self, String> {
        let text =
            std::str::from_utf8(raw).map_err(|_| "DirBases: dir_bases is not UTF-8".to_owned())?;
        let mut out = Self::default();
        for line in text.lines() {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            let Some((key, val)) = trimmed.split_once('=') else {
                return Err(format!("DirBases: invalid line {trimmed:?}"));
            };
            let path = PathBuf::from(val);
            if !path.is_absolute() || val.as_bytes().contains(&0) {
                return Err(format!("DirBases: base {key} must be an absolute path"));
            }
            let slot = match key {
                "config" => &mut out.config,
                "cache" => &mut out.cache,
                "data" => &mut out.data,
                "state" => &mut out.state,
                _ => return Err(format!("DirBases: unknown key {key:?}")),
            };
            if slot.is_some() {
                return Err(format!("DirBases: duplicate base {key}"));
            }
            *slot = Some(path);
        }
        Ok(out)
    }

    fn for_form(&self, form: usize) -> Option<&Path> {
        match form {
            0 => self.config.as_deref(),
            1 => self.cache.as_deref(),
            2 => self.data.as_deref(),
            3 => self.state.as_deref(),
            _ => None,
        }
    }
}

/// Validated directory grant, opened and pinned via an owned directory fd.
pub(crate) struct OpenedDir {
    pub(crate) spec: String,
    pub(crate) path: PathBuf,
    pub(crate) fd: OwnedFd,
}

/// Validated embedded secret grant (`Secret:<param>=file:<abs> <uses>`).
#[derive(Clone, Debug)]
pub(crate) struct ParsedSecret {
    pub(crate) param: String,
    pub(crate) path: PathBuf,
    pub(crate) policy: String,
}

/// Validated embedded launch grants ready for [`super::launch`].
pub(crate) struct ResolvedGrants {
    pub(crate) grant_mask: u64,
    pub(crate) store_dir: OpenedDir,
    pub(crate) extra_dirs: Vec<OpenedDir>,
    pub(crate) net_rules: Vec<String>,
    pub(crate) secrets: Vec<ParsedSecret>,
    pub(crate) has_foreign: bool,
}

/// Validate `target`, `store_dir`, `dir_bases`, `grants`, and `parameters`
/// per EMB-3 and EMB-7.
pub(crate) fn resolve_embedded_grants(
    target_triple: &str,
    store_dir_raw: &[u8],
    dir_bases_raw: &[u8],
    grants_raw: &[u8],
    parameters: &[Requirement],
    parameter_names: &[&str],
) -> Result<ResolvedGrants, String> {
    let target = Target::from_triple(target_triple)
        .ok_or_else(|| format!("TargetUnknown({target_triple})"))?;
    let store_dir_str = std::str::from_utf8(store_dir_raw)
        .map_err(|_| "DirSource(store_dir is not UTF-8)".to_owned())?;
    let store_path = Path::new(store_dir_str);
    if store_dir_str.is_empty() || !store_path.is_absolute() || store_dir_raw.contains(&0) {
        return Err(format!("DirSource(store_dir={store_dir_str})"));
    }
    let bases = DirBases::parse(dir_bases_raw)?;
    let grants_text = std::str::from_utf8(grants_raw)
        .map_err(|_| "GrantSyntax(grants is not UTF-8)".to_owned())?;

    let mut grant_mask: u64 = 0;
    let mut dir_specs: Vec<String> = Vec::new();
    let mut net_rules: Vec<String> = Vec::new();
    let mut secrets: Vec<ParsedSecret> = Vec::new();
    let mut has_foreign = false;

    for raw_line in grants_text.lines() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        // Support either one grant per line or space-separated tokens, while
        // keeping `Secret:<param>=<src> <uses>` on its own line cleanly.
        if let Some(rest) = line.strip_prefix("Secret:") {
            let secret = parse_secret_grant(line, rest)?;
            secrets.push(secret);
            grant_mask |= 1 << SECRET;
            continue;
        }
        if let Some(spec) = line.strip_prefix("Dir=") {
            dir_specs.push(spec.to_owned());
            continue;
        }
        if let Some(rule) = line.strip_prefix("Net=") {
            check_target_net_rule(target, rule)?;
            net_rules.push(rule.to_owned());
            grant_mask |= 1 << NET;
            continue;
        }
        if line.starts_with("Exec") {
            check_target_exec(target, line)?;
            return Err(format!("GrantUnsupported({line})"));
        }
        if line.starts_with("Env") {
            return Err(format!("EnvSource({line})"));
        }
        for token in line.split_whitespace() {
            if let Some(spec) = token.strip_prefix("Dir=") {
                dir_specs.push(spec.to_owned());
            } else if let Some(rule) = token.strip_prefix("Net=") {
                check_target_net_rule(target, rule)?;
                net_rules.push(rule.to_owned());
                grant_mask |= 1 << NET;
            } else if token.starts_with("Exec") {
                check_target_exec(target, token)?;
                return Err(format!("GrantUnsupported({token})"));
            } else if token.starts_with("Env") {
                return Err(format!("EnvSource({token})"));
            } else if let Some(rest) = token.strip_prefix("Secret:") {
                let secret = parse_secret_grant(token, rest)?;
                secrets.push(secret);
                grant_mask |= 1 << SECRET;
            } else {
                match token {
                    "Clock" => grant_mask |= 1 << CLOCK,
                    "Random" => grant_mask |= 1 << RANDOM,
                    "Tasks" => grant_mask |= 1 << TASKS,
                    "Process" => grant_mask |= 1 << PROCESS,
                    "Stdout" => grant_mask |= 1 << STDOUT,
                    "Stderr" => grant_mask |= 1 << STDERR,
                    "Stdin" => grant_mask |= 1 << STDIN,
                    "Foreign" => {
                        grant_mask |= 1 << FOREIGN;
                        has_foreign = true;
                    }
                    "Net" => grant_mask |= 1 << NET,
                    _ => return Err(format!("GrantUnknown({token})")),
                }
            }
        }
    }

    // EMB-7: embedded runs always grant Process (in-process stop hook) when
    // the program requests Process, plus Clock/Random/Tasks if granted or required?
    // EMB-3: "an ungranted want is refused". Only explicit grants in `grants`
    // (plus `store_dir` as Dir 0 and `Process` via embed stop provider if declared)
    // satisfy wants.
    let dir_params = parameters.iter().filter(|r| r.kind == DIR).count();
    let total_dirs = 1 + dir_specs.len();
    if dir_params > total_dirs {
        return Err(format!(
            "GrantMissing(Dir: needs {dir_params}, granted {total_dirs})"
        ));
    }

    // Check every required parameter has a matching grant BEFORE creating xdg dirs.
    let mut secret_by_param: BTreeMap<&str, usize> = BTreeMap::new();
    for (i, s) in secrets.iter().enumerate() {
        secret_by_param.insert(s.param.as_str(), i);
    }
    let mut ordered_secrets: Vec<ParsedSecret> = Vec::new();
    for (idx, req) in parameters.iter().enumerate() {
        let name = parameter_names.get(idx).copied().unwrap_or("");
        match req.kind {
            DIR | ARGS => {}
            PROCESS => {
                // Embedded stop is always delivered through the in-process Process capability.
                grant_mask |= 1 << PROCESS;
            }
            EXEC => {
                check_target_exec(target, "Exec")?;
                return Err("GrantMissing(Exec)".into());
            }
            ENV => return Err(format!("GrantMissing(Env:{name})")),
            CRED => return Err(format!("GrantMissing(Cred:{name})")),
            SECRET => {
                let matched = secret_by_param
                    .get(name)
                    .copied()
                    .or_else(|| (req.index < secrets.len()).then_some(req.index));
                let Some(sidx) = matched else {
                    return Err(format!("GrantMissing(Secret:{name})"));
                };
                ordered_secrets.push(secrets[sidx].clone());
            }
            NET => {
                if grant_mask & (1 << NET) == 0 {
                    return Err(format!("GrantMissing(Net:{name})"));
                }
            }
            CLOCK | RANDOM | TASKS | STDOUT | STDERR | STDIN | FOREIGN => {
                if grant_mask & (1 << req.kind) == 0 {
                    return Err(format!("GrantMissing({name})"));
                }
            }
            other => return Err(format!("GrantUnsupported(kind={other})")),
        }
    }

    // Validate & resolve directory grant paths before creating xdg-* directories.
    let mut pending_dirs: Vec<(String, PathBuf, bool)> = Vec::with_capacity(dir_specs.len());
    for spec in &dir_specs {
        let (path, create_if_missing) = resolve_dir_spec(spec, &bases)?;
        pending_dirs.push((spec.clone(), path, create_if_missing));
    }

    // Open store_dir first (must exist and be a directory, or create? EMB-3: store_dir
    // is a directory the host provides; open with O_DIRECTORY).
    let store_fd = open_directory(store_path)
        .map_err(|e| format!("DirUnavailable({}, {})", store_path.display(), errno_of(&e)))?;
    let store_ancestors = ancestor_identities(store_fd.as_raw_fd())
        .map_err(|e| format!("DirUnavailable({}, {})", store_path.display(), errno_of(&e)))?;
    let store_id = *store_ancestors
        .first()
        .ok_or_else(|| format!("DirUnavailable({}, 5)", store_path.display()))?;

    // Open each extra Dir grant and check descriptor-derived StoreOverlapsDir.
    let mut extra_dirs = Vec::with_capacity(pending_dirs.len());
    for (spec, path, create_if_missing) in pending_dirs {
        if create_if_missing {
            ensure_xdg_dir(&path)
                .map_err(|e| format!("DirUnavailable({}, {})", path.display(), errno_of(&e)))?;
        }
        let fd = open_directory(&path)
            .map_err(|e| format!("DirUnavailable({}, {})", path.display(), errno_of(&e)))?;
        let dir_ancestors = ancestor_identities(fd.as_raw_fd())
            .map_err(|e| format!("DirUnavailable({}, {})", path.display(), errno_of(&e)))?;
        let dir_id = *dir_ancestors
            .first()
            .ok_or_else(|| format!("DirUnavailable({}, 5)", path.display()))?;
        if dir_ancestors.contains(&store_id) || store_ancestors.contains(&dir_id) {
            return Err(format!("StoreOverlapsDir({spec})"));
        }
        extra_dirs.push(OpenedDir { spec, path, fd });
    }

    // Verify all Secret files are readable and nonempty.
    for secret in &ordered_secrets {
        let bytes = std::fs::read(&secret.path).map_err(|_| {
            format!(
                "SecretSource(Secret:{}=file:{})",
                secret.param,
                secret.path.display()
            )
        })?;
        if bytes.is_empty() {
            return Err(format!(
                "SecretSource(Secret:{}=file:{})",
                secret.param,
                secret.path.display()
            ));
        }
    }

    Ok(ResolvedGrants {
        grant_mask,
        store_dir: OpenedDir {
            spec: store_dir_str.to_owned(),
            path: store_path.to_owned(),
            fd: store_fd,
        },
        extra_dirs,
        net_rules,
        secrets: ordered_secrets,
        has_foreign,
    })
}

fn check_target_exec(target: Target, grant: &str) -> Result<(), String> {
    match target.os() {
        Os::Ios => Err(format!("TargetCapabilityUnsupported(ios, {grant})")),
        Os::Linux | Os::Macos => Ok(()),
    }
}

fn check_target_net_rule(target: Target, rule: &str) -> Result<(), String> {
    let is_unix = rule.starts_with("unix:")
        || rule.starts_with("unix-listen:")
        || rule.starts_with("unix-connect:")
        || rule.contains("unix");
    match target.os() {
        Os::Ios if is_unix => Err(format!("TargetCapabilityUnsupported(ios, Net={rule})")),
        Os::Ios | Os::Linux | Os::Macos => Ok(()),
    }
}

fn parse_secret_grant(full: &str, rest: &str) -> Result<ParsedSecret, String> {
    let Some((param, rhs)) = rest.split_once('=') else {
        return Err(format!("SecretSource({full})"));
    };
    if param.is_empty() {
        return Err(format!("SecretSource({full})"));
    }
    let rhs = rhs.trim();
    let (source, policy) = match rhs.split_once(char::is_whitespace) {
        Some((s, p)) => (s, p.trim()),
        None => return Err(format!("SecretSource({full})")),
    };
    if policy.is_empty() || hirsel_core_net::SecretPolicy::parse(policy).is_none() {
        return Err(format!("SecretSource({full})"));
    }
    // EMB-3: only `file:<abs>` is admitted; `env:<VAR>` and relative paths refuse
    // `SecretSource` without touching the environment.
    let Some(path_str) = source.strip_prefix("file:") else {
        return Err(format!("SecretSource({full})"));
    };
    let path = Path::new(path_str);
    if path_str.is_empty() || !path.is_absolute() || path_str.as_bytes().contains(&0) {
        return Err(format!("SecretSource({full})"));
    }
    Ok(ParsedSecret {
        param: param.to_owned(),
        path: path.to_owned(),
        policy: policy.to_owned(),
    })
}

fn resolve_dir_spec(spec: &str, bases: &DirBases) -> Result<(PathBuf, bool), String> {
    if spec.is_empty() || spec.as_bytes().contains(&0) {
        return Err(format!("DirSource({spec})"));
    }
    if spec.starts_with("env:") || spec.starts_with("fd:") {
        return Err(format!("DirSource({spec})"));
    }
    for (form, (prefix, ..)) in XDG_FORMS.iter().enumerate() {
        if let Some(app) = spec
            .strip_prefix(prefix)
            .and_then(|rest| rest.strip_prefix(':'))
        {
            if !valid_app(app) {
                return Err(format!("DirSource({spec})"));
            }
            let Some(base) = bases.for_form(form) else {
                return Err(format!("DirBase({prefix})"));
            };
            return Ok((base.join(app), true));
        }
    }
    let path = Path::new(spec);
    if !path.is_absolute() {
        return Err(format!("DirSource({spec})"));
    }
    Ok((path.to_owned(), false))
}

fn ensure_xdg_dir(path: &Path) -> io::Result<()> {
    // Reject dangling symlinks or non-directory existing components.
    for prefix in path.ancestors().collect::<Vec<_>>().into_iter().rev() {
        if prefix.as_os_str().is_empty() {
            continue;
        }
        match std::fs::symlink_metadata(prefix) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => break,
            Err(e) => return Err(e),
            Ok(_) => {
                if !std::fs::metadata(prefix)?.is_dir() {
                    return Err(io::Error::from_raw_os_error(libc::ENOTDIR));
                }
            }
        }
    }
    for prefix in path.ancestors().collect::<Vec<_>>().into_iter().rev() {
        if prefix.as_os_str().is_empty() || prefix == Path::new("/") {
            continue;
        }
        let name = CString::new(prefix.as_os_str().as_bytes())
            .map_err(|_| io::Error::from_raw_os_error(libc::EINVAL))?;
        // SAFETY: `name` is NUL-terminated.
        if unsafe { libc::mkdirat(libc::AT_FDCWD, name.as_ptr(), 0o700) } != 0 {
            let err = io::Error::last_os_error();
            if err.raw_os_error() != Some(libc::EEXIST) {
                return Err(err);
            }
        }
    }
    Ok(())
}

fn open_directory(path: &Path) -> io::Result<OwnedFd> {
    let cpath = CString::new(path.as_os_str().as_bytes())
        .map_err(|_| io::Error::from_raw_os_error(libc::EINVAL))?;
    // SAFETY: `cpath` is a live NUL-terminated path.
    let fd = unsafe {
        libc::open(
            cpath.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `open` returned a fresh owned descriptor.
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

/// Walk `(st_dev, st_ino)` parent chain from an opened directory fd to filesystem root.
/// Uses only `fstat` and `openat(fd, "..", O_RDONLY | O_DIRECTORY | O_CLOEXEC)` so
/// symlinks, bind mounts, and relative hops resolve strictly through kernel descriptors.
fn ancestor_identities(dir_fd: i32) -> io::Result<Vec<(u64, u64)>> {
    let mut chain = Vec::new();
    let first = fstat_dev_ino(dir_fd)?;
    chain.push(first);
    let mut current_fd = dir_fd;
    let mut owned: Option<OwnedFd> = None;
    let dotdot = c"..";
    for _ in 0..4096 {
        // SAFETY: `current_fd` is an open directory fd and `dotdot` is NUL-terminated.
        let parent = unsafe {
            libc::openat(
                current_fd,
                dotdot.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC,
            )
        };
        if parent < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: `openat` returned a fresh descriptor.
        let parent_fd = unsafe { OwnedFd::from_raw_fd(parent) };
        let id = fstat_dev_ino(parent_fd.as_raw_fd())?;
        if Some(&id) == chain.last() {
            break;
        }
        chain.push(id);
        current_fd = parent_fd.as_raw_fd();
        owned = Some(parent_fd);
    }
    drop(owned);
    Ok(chain)
}

fn fstat_dev_ino(fd: i32) -> io::Result<(u64, u64)> {
    let mut st = std::mem::MaybeUninit::<libc::stat>::zeroed();
    // SAFETY: `fd` is an open descriptor and `st` is writable.
    if unsafe { libc::fstat(fd, st.as_mut_ptr()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `fstat` succeeded and initialized `st`.
    let st = unsafe { st.assume_init() };
    Ok((st.st_dev as u64, st.st_ino as u64))
}

fn errno_of(err: &io::Error) -> i32 {
    err.raw_os_error().unwrap_or(libc::EIO)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    #[test]
    fn dir_bases_and_xdg_resolution() {
        let tmp = tempfile::tempdir().unwrap();
        let store = tmp.path().join("store");
        let data_base = tmp.path().join("data_base");
        std::fs::create_dir_all(&store).unwrap();
        std::fs::create_dir_all(&data_base).unwrap();

        let bases = format!("data={}\n", data_base.display());
        let resolved = resolve_embedded_grants(
            "x86_64-unknown-linux-gnu",
            store.as_os_str().as_bytes(),
            bases.as_bytes(),
            b"Dir=xdg-data:myapp\nClock\nRandom\nTasks\n",
            &[
                Requirement {
                    kind: DIR,
                    index: 0,
                },
                Requirement {
                    kind: DIR,
                    index: 1,
                },
                Requirement {
                    kind: CLOCK,
                    index: 0,
                },
            ],
            &["store", "data", "clock"],
        )
        .unwrap();
        assert_eq!(resolved.extra_dirs.len(), 1);
        assert_eq!(resolved.extra_dirs[0].path, data_base.join("myapp"));
        assert!(data_base.join("myapp").is_dir());

        // Missing base -> DirBase
        let err = resolve_embedded_grants(
            "x86_64-unknown-linux-gnu",
            store.as_os_str().as_bytes(),
            b"",
            b"Dir=xdg-data:myapp\n",
            &[Requirement {
                kind: DIR,
                index: 0,
            }],
            &["store"],
        )
        .err()
        .unwrap();
        assert!(err.starts_with("DirBase(xdg-data)"), "got {err}");

        // Relative path -> DirSource
        let err = resolve_embedded_grants(
            "x86_64-unknown-linux-gnu",
            store.as_os_str().as_bytes(),
            b"",
            b"Dir=relative/dir\n",
            &[Requirement {
                kind: DIR,
                index: 0,
            }],
            &["store"],
        )
        .err()
        .unwrap();
        assert!(err.starts_with("DirSource("), "got {err}");
    }

    #[test]
    fn store_overlaps_dir_covers_nesting_and_symlinks() {
        let tmp = tempfile::tempdir().unwrap();
        let parent = tmp.path().join("root");
        let store = parent.join("store");
        let nested = store.join("sub");
        let sibling = parent.join("sibling");
        let link_to_store = tmp.path().join("link_store");
        let link_to_parent = tmp.path().join("link_parent");
        let link_to_sibling = tmp.path().join("link_sibling");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::create_dir_all(&sibling).unwrap();
        symlink(&store, &link_to_store).unwrap();
        symlink(&parent, &link_to_parent).unwrap();
        symlink(&sibling, &link_to_sibling).unwrap();

        let check = |dir: &Path| {
            let grants = format!("Dir={}\n", dir.display());
            resolve_embedded_grants(
                "x86_64-unknown-linux-gnu",
                store.as_os_str().as_bytes(),
                b"",
                grants.as_bytes(),
                &[
                    Requirement {
                        kind: DIR,
                        index: 0,
                    },
                    Requirement {
                        kind: DIR,
                        index: 1,
                    },
                ],
                &["store", "extra"],
            )
        };

        assert!(
            check(&store)
                .err()
                .unwrap()
                .starts_with("StoreOverlapsDir(")
        );
        assert!(
            check(&nested)
                .err()
                .unwrap()
                .starts_with("StoreOverlapsDir(")
        );
        assert!(
            check(&parent)
                .err()
                .unwrap()
                .starts_with("StoreOverlapsDir(")
        );
        assert!(
            check(&link_to_store)
                .err()
                .unwrap()
                .starts_with("StoreOverlapsDir(")
        );
        assert!(
            check(&link_to_parent)
                .err()
                .unwrap()
                .starts_with("StoreOverlapsDir(")
        );
        assert!(check(&sibling).is_ok());
        assert!(check(&link_to_sibling).is_ok());
    }

    #[test]
    fn secret_source_and_ios_target_refusals() {
        let tmp = tempfile::tempdir().unwrap();
        let store = tmp.path().join("store");
        let sec = tmp.path().join("token.txt");
        std::fs::create_dir_all(&store).unwrap();
        std::fs::write(&sec, b"s3cr3t").unwrap();

        let err = resolve_embedded_grants(
            "x86_64-unknown-linux-gnu",
            store.as_os_str().as_bytes(),
            b"",
            b"Secret:k=env:DO_NOT_READ verify\n",
            &[
                Requirement {
                    kind: DIR,
                    index: 0,
                },
                Requirement {
                    kind: SECRET,
                    index: 0,
                },
            ],
            &["store", "k"],
        )
        .err()
        .unwrap();
        assert!(err.starts_with("SecretSource("), "got {err}");

        let ok = resolve_embedded_grants(
            "x86_64-unknown-linux-gnu",
            store.as_os_str().as_bytes(),
            b"",
            format!("Secret:k=file:{} verify\n", sec.display()).as_bytes(),
            &[
                Requirement {
                    kind: DIR,
                    index: 0,
                },
                Requirement {
                    kind: SECRET,
                    index: 0,
                },
            ],
            &["store", "k"],
        );
        assert!(ok.is_ok());

        // EMB-7: iOS target refuses Exec and unix Net rules.
        let ios_exec = resolve_embedded_grants(
            "aarch64-apple-ios",
            store.as_os_str().as_bytes(),
            b"",
            b"Exec=foo\n",
            &[Requirement {
                kind: DIR,
                index: 0,
            }],
            &["store"],
        )
        .err()
        .unwrap();
        assert!(
            ios_exec.contains("TargetCapabilityUnsupported"),
            "got {ios_exec}"
        );

        let ios_unix = resolve_embedded_grants(
            "aarch64-apple-ios",
            store.as_os_str().as_bytes(),
            b"",
            b"Net=unix:/tmp/a.sock\n",
            &[Requirement {
                kind: DIR,
                index: 0,
            }],
            &["store"],
        )
        .err()
        .unwrap();
        assert!(
            ios_unix.contains("TargetCapabilityUnsupported"),
            "got {ios_unix}"
        );
    }
}
