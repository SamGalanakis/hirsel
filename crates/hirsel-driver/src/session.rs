//! Filesystem and event delivery over the shared immutable compiler graph.
//! Sessions retain observations, policy and work metadata. Semantic retention
//! and invalidation belong to engine-selected queries for fresh and resident use.
use hirsel_engine::{Results, Verify};
use std::{
    cell::RefCell,
    path::{Path, PathBuf},
};

/// One changed file: its new bytes, or None when it no longer exists.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FileChange {
    pub path: PathBuf,
    pub bytes: Option<Vec<u8>>,
}

/// What a session did, for tests and `--timings`.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct SessionStats {
    /// The source revision: one more for each `apply` that changed something.
    pub revision: u64,
    /// Input-footprint discoveries (the first, then after loader changes).
    pub loads: u64,
    /// Source observations replaced by an explicit edit or disk refresh.
    pub replaced: u64,
    /// Distinct query memos read or written since the first apply through the last check (O3).
    pub touched: u64,
    /// Every raw memo operation in that interval, including repeated operations on one memo.
    pub memo_operations: u64,
}

/// Input work at the resident boundary, independent of query counters.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct SessionInputWork {
    pub polled_files: u64,
    pub polled_bytes: u64,
    pub applied_sources: u64,
    pub applied_bytes: u64,
}

/// The ordinary host facade over the engine-owned compiler request selector.
pub type Session = crate::resident::ObservedSession<crate::requests::CompilerRequests>;

/// The per-command choices a path entry point compiles under: V, the result
/// store the CLI selects for the command, and whether that store's hits are
/// recomputed (`--verify-cache`). Sessions are kept by policy, so a session
/// opened under one policy is never reused under another.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Policy {
    pub verify: Verify,
    pub results: Results,
    pub verify_store: hirsel_engine::store::VerifyStore,
}
impl Policy {
    /// The library default (no result store) with V `verify`.
    pub fn verify(verify: Verify) -> Self {
        Self {
            verify,
            ..Self::default()
        }
    }
    /// The driver's compiler under this policy.
    pub fn compiler(self) -> hirsel_engine::Compiler {
        crate::COMPILER
            .with_verify(self.verify)
            .with_results(self.results)
            .with_verify_store(self.verify_store)
    }
}

impl Session {
    /// Capture host policy now; observe source inputs lazily on first request.
    pub fn open(entry: &Path, policy: Policy) -> Self {
        Self::open_with_compiler(
            entry,
            policy
                .compiler()
                .with_retention(hirsel_engine::memo::Retention::Resident),
        )
    }
    /// Explicit stores, lowering, jobs and zero-byte policies remain exact.
    pub fn open_configured(entry: &Path, config: hirsel_engine::CompileConfig) -> Self {
        Self::open_with_compiler(entry, crate::COMPILER.with_config(config))
    }
    /// An explicit compiler factory and policy, captured before observation.
    pub fn open_with_compiler(entry: &Path, compiler: hirsel_engine::Compiler) -> Self {
        let config = compiler.configuration();
        let compiler = compiler.with_config(config.clone()).with_owned_engines();
        let engine = crate::requests::CompilerRequests::for_host(compiler.clone(), config.clone());
        let limits = engine.input_limits();
        let attempts = engine.capture_attempts();
        Self::new(entry.to_owned(), compiler, config, engine, limits, attempts)
    }
    /// Replace the compiler policy without replacing explicitly delivered inputs.
    pub fn configure(&mut self, compiler: hirsel_engine::Compiler) {
        let config = compiler.configuration();
        let engine = crate::requests::CompilerRequests::for_host(compiler.clone(), config.clone());
        let limits = engine.input_limits();
        let attempts = engine.capture_attempts();
        self.replace_engine(compiler, config, engine, limits, attempts);
    }
}

/// The sessions this thread keeps, unique by key, most recently used last.
struct Registry {
    sessions: Vec<(Key, Session)>,
    limit: usize,
    opened: u64,
}
thread_local! {
    static RESIDENT: RefCell<Registry> = const {
        RefCell::new(Registry {
            sessions: Vec::new(),
            limit: 1,
            opened: 0,
        })
    };
}

/// Keep up to `limit` sessions on this thread from now on. Every thread
/// keeps one, so repeated requests for one program reuse its session;
/// `hirsel serve` keeps more for its several clients.
pub fn keep_sessions(limit: usize) {
    RESIDENT.with(|r| {
        let mut registry = r.borrow_mut();
        registry.limit = limit.max(1);
        let over = registry.sessions.len().saturating_sub(registry.limit);
        registry.sessions.drain(..over);
    });
}
/// Sessions the path entry points opened on this thread so far.
pub fn opened() -> u64 {
    RESIDENT.with(|r| r.borrow().opened)
}
/// Current logical input paths, including absent manifests and sidecars.
/// Event hosts register these before comparing bytes; event metadata alone
/// never establishes source identity.
pub fn resident_watched() -> Vec<PathBuf> {
    RESIDENT.with(|r| {
        r.borrow()
            .sessions
            .iter()
            .flat_map(|(_, session)| session.watched().map(Path::to_path_buf))
            .collect()
    })
}
/// Whether a file of any kept session changed since it was last used.
pub fn resident_changed() -> bool {
    RESIDENT.with(|r| {
        r.borrow()
            .sessions
            .iter()
            .any(|(_, s)| !s.poll().is_empty())
    })
}

/// Fresh and resident callers use the same immutable request selector. The
/// registry changes only the lifetime of the input cursor and shared host.
pub(crate) fn with_session<R>(path: &Path, policy: Policy, f: impl FnOnce(&mut Session) -> R) -> R {
    // Without a working directory no key identifies the program: it runs unkept.
    let Ok(directory) = std::env::current_dir() else {
        return f(&mut Session::open(path, policy));
    };
    let key = Key {
        directory,
        entry: path.to_path_buf(),
        policy,
    };
    let mut session = take(&key);
    let result = f(&mut session);
    put(key, session);
    result
}

pub(crate) fn request(
    path: &Path,
    policy: Policy,
    operation: crate::requests::CompileOperation,
) -> Result<std::sync::Arc<crate::requests::CompileOutput>, hirsel_engine::ToolError> {
    with_session(path, policy, |session| session.request(operation))
}

/// A kept session's identity: the working directory, the entry as given (a
/// standalone source's diagnostics print the path as given) and the policy.
#[derive(Eq, PartialEq)]
struct Key {
    directory: PathBuf,
    entry: PathBuf,
    policy: Policy,
}

/// `key`'s session out of the registry, or a new one. A session is taken
/// while in use so a nested lease of the same program loads its own.
fn take(key: &Key) -> Session {
    let kept = RESIDENT.with(|r| {
        let mut registry = r.borrow_mut();
        let index = registry.sessions.iter().position(|(k, _)| k == key);
        match index {
            Some(i) => Some(registry.sessions.remove(i).1),
            None => {
                registry.opened += 1;
                None
            }
        }
    });
    kept.unwrap_or_else(|| Session::open(&key.entry, key.policy))
}
/// Keep `session` under `key`, replacing a nested lease's session of the same key.
fn put(key: Key, session: Session) {
    RESIDENT.with(|r| {
        let mut registry = r.borrow_mut();
        registry.sessions.retain(|(k, _)| *k != key);
        registry.sessions.push((key, session));
        let over = registry.sessions.len().saturating_sub(registry.limit);
        registry.sessions.drain(..over);
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The CLI chooses a store per command; one process may serve commands
    /// with different choices. A kept session carries its policy's store, so
    /// a lease under another policy must open its own session, and a lease
    /// under the first policy again must get the first session back.
    #[test]
    fn leases_under_different_store_choices_never_share_a_session() {
        let directory = tempfile::tempdir().unwrap();
        let entry = directory.path().join("main.hir");
        std::fs::write(&entry, "pub fn main() {}\n").unwrap();
        keep_sessions(2);
        let off = Policy::default();
        let host = Policy {
            results: Results::Host,
            ..off
        };
        let store = |policy| {
            with_session(&entry, policy, |session| {
                std::sync::Arc::as_ptr(&session.configuration().store).cast::<()>()
            })
        };
        let before = opened();
        let first = store(off);
        let other = store(host);
        let again = store(off);
        assert_ne!(first, other, "a Host lease reused the Off session");
        assert_eq!(first, again, "the Off session was not kept");
        assert_eq!(opened() - before, 2);
    }
}
