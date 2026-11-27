//! Marshalling for the private core.compiler host row. The ordinary Hirsel
//! wrapper owns the request bytes and constructs the public typed result.
use crate::{
    AuthorityVerdict, CheckLibraryCaps, CheckLibraryRequest, CheckedBinding, CheckedLibrary,
    HistoryRow, LibraryEntry, Limit, Refusal, Service, parse_digest,
};
use hirsel_core_ids::ContentHash;

use std::fs::Permissions;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, atomic::AtomicBool};

struct Bound {
    service: Service,
    _scratch: tempfile::TempDir,
}

/// One host designation shares its supervisor seats and private directory
/// between every call. An unused designation creates no files.
pub struct Designation {
    worker: PathBuf,
    bound: Mutex<Option<Arc<Bound>>>,
}

impl Designation {
    pub fn new(worker: PathBuf) -> Self {
        Self {
            worker,
            bound: Default::default(),
        }
    }

    fn service(&self) -> std::io::Result<Arc<Bound>> {
        if !self.worker.is_absolute() {
            return Err(std::io::Error::other(
                "compiler worker requires an absolute path",
            ));
        }
        let mut bound = self.bound.lock().unwrap();
        if let Some(bound) = bound.as_ref() {
            return Ok(bound.clone());
        }
        let scratch = tempfile::Builder::new()
            .prefix("hirsel-compiler-")
            .permissions(Permissions::from_mode(0o700))
            .tempdir()?;
        let service = Service::new(
            self.worker.clone(),
            scratch.path().into(),
            crate::Caps::HOST,
            2,
            64,
        );
        let service = Arc::new(Bound {
            service,
            _scratch: scratch,
        });
        *bound = Some(service.clone());
        Ok(service)
    }

    /// Dispatch the private, length-delimited core.compiler row.
    pub fn check(&self, input: &[u8]) -> (u64, Vec<u8>) {
        self.check_cancellable(input, &AtomicBool::new(false))
    }

    pub fn check_cancellable(&self, input: &[u8], cancelled: &AtomicBool) -> (u64, Vec<u8>) {
        match self.service() {
            Ok(bound) => check(&bound.service, input, cancelled),
            Err(error) => (9, error.to_string().into_bytes()),
        }
    }
}

struct Input<'a>(&'a [u8]);

impl<'a> Input<'a> {
    fn count(&mut self) -> Option<usize> {
        let (bytes, rest) = self.0.split_first_chunk::<8>()?;
        self.0 = rest;
        usize::try_from(u64::from_le_bytes(*bytes)).ok()
    }

    fn bytes(&mut self) -> Option<&'a [u8]> {
        let len = self.count()?;
        let (bytes, rest) = self.0.split_at_checked(len)?;
        self.0 = rest;
        Some(bytes)
    }

    fn text(&mut self) -> Option<&'a str> {
        std::str::from_utf8(self.bytes()?).ok()
    }

    fn digest(&mut self) -> Option<ContentHash> {
        parse_digest(self.text()?)
    }

    fn entry(&mut self) -> Option<LibraryEntry<'a>> {
        Some(LibraryEntry {
            root: self.digest()?,
            content: self.bytes()?,
        })
    }

    fn history(&mut self) -> Option<HistoryRow> {
        Some(HistoryRow {
            origin: self.text()?.into(),
            identity: self.text()?.into(),
            version: self.text()?.into(),
            root: self.digest()?,
        })
    }
}

fn run(
    service: &Service,
    mut input: Input<'_>,
    cancelled: &AtomicBool,
) -> Option<Result<CheckedLibrary, Refusal>> {
    let head = input.entry()?;
    let count = input.count()?;
    if count >= CheckLibraryCaps::REGISTRY.max_packages {
        return Some(Err(Refusal::Limit(Limit::Input)));
    }
    let closure: Vec<_> = (0..count).map(|_| input.entry()).collect::<Option<_>>()?;
    let rows = input.count()?;
    if rows as u64 > CheckLibraryCaps::REGISTRY.max_history_metadata_bytes / 44 {
        return Some(Err(Refusal::Limit(Limit::Input)));
    }
    let history: Vec<_> = (0..rows).map(|_| input.history()).collect::<Option<_>>()?;
    if !input.0.is_empty() {
        return None;
    }
    Some(crate::check_library::run_cancellable(
        service,
        &CheckLibraryRequest {
            root: head.root,
            content: head.content,
            closure: &closure,
            history: &history,
        },
        CheckLibraryCaps::REGISTRY,
        cancelled,
    ))
}

/// The tag is the private row's result. Text refusals carry UTF-8 in the
/// reply, and success carries the length-delimited fields of CheckedLibrary.
fn check(service: &Service, input: &[u8], cancelled: &AtomicBool) -> (u64, Vec<u8>) {
    match run(service, Input(input), cancelled).unwrap_or_else(|| {
        Err(Refusal::Rejected(
            "invalid library request or digest".into(),
        ))
    }) {
        Ok(checked) => {
            let mut reply = Reply(Vec::new());
            match reply.library(&checked) {
                Ok(()) => (0, reply.0),
                Err(message) => (9, message.as_bytes().to_vec()),
            }
        }
        Err(Refusal::Rejected(s)) => (1, s.into_bytes()),
        Err(Refusal::Unsupported(s)) => (2, s.into_bytes()),
        Err(Refusal::Limit(kind)) => (
            match kind {
                Limit::Input => 3,
                Limit::Cpu => 4,
                Limit::Wall => 5,
                Limit::Memory => 6,
                Limit::Output => 7,
            },
            vec![],
        ),
        Err(Refusal::Busy) => (8, vec![]),
        Err(Refusal::Unavailable(s)) => (9, s.into_bytes()),
    }
}

struct Reply(Vec<u8>);

impl Reply {
    // The supervisor bounded the entire JSON reply to 64 MiB.
    fn count(&mut self, n: usize) {
        self.0.extend_from_slice(&(n as u32).to_le_bytes());
    }
    fn text(&mut self, s: &str) {
        self.count(s.len());
        self.0.extend_from_slice(s.as_bytes());
    }
    fn optional(&mut self, s: Option<&str>) {
        self.0.push(u8::from(s.is_some()));
        if let Some(s) = s {
            self.text(s);
        }
    }
    fn strings<'a>(&mut self, strings: impl ExactSizeIterator<Item = &'a String>) {
        self.count(strings.len());
        for s in strings {
            self.text(s);
        }
    }

    fn library(&mut self, checked: &CheckedLibrary) -> Result<(), &'static str> {
        self.text(&checked.root);
        self.text(&checked.header.identity);
        self.text(&checked.header.version);
        self.text(&checked.header.charge);
        self.count(checked.bindings.len());
        for binding in &checked.bindings {
            match binding {
                CheckedBinding::Toolchain { alias, id } => {
                    self.0.push(0);
                    self.text(alias);
                    self.text(id);
                }
                CheckedBinding::Exact {
                    alias,
                    id,
                    tested_root,
                } => {
                    self.0.push(1);
                    self.text(alias);
                    self.text(id);
                    self.text(tested_root);
                }
                CheckedBinding::Floor {
                    alias,
                    id,
                    tested_root,
                    bound_version,
                } => {
                    self.0.push(2);
                    self.text(alias);
                    self.text(id);
                    self.text(tested_root);
                    self.text(bound_version);
                }
            }
        }
        let api = &checked.interface;
        self.0.extend_from_slice(&api.version.to_le_bytes());
        self.text(&api.package.name);
        self.text(&api.package.version);
        self.count(api.modules.len());
        for module in &api.modules {
            self.strings(module.path.iter());
            self.0.push(u8::from(module.extends_core));
            self.count(module.items.len());
            for item in &module.items {
                self.text(&item.definition);
                self.text(&item.name);
                self.text(&item.kind);
                self.text(&item.signature);
                self.optional(item.doc.as_deref());
            }
        }
        let card = &checked.authority;
        self.count(card.carried.len());
        for (name, row) in &card.carried {
            self.text(name);
            self.strings(row.takes.iter());
            self.strings(row.returns.iter());
        }
        self.count(card.reached.len());
        for (name, row) in &card.reached {
            self.text(name);
            self.strings(row.kinds.iter());
            self.strings(row.provided.iter());
            self.strings(row.contained.iter());
            self.optional(row.unresolved.as_deref());
        }
        match &card.verdict {
            AuthorityVerdict::Status(s) => self.0.push(match s.as_str() {
                "pure" => 0,
                "conditional" => 1,
                "unknown" => 2,
                _ => return Err("invalid compiler authority verdict"),
            }),
            AuthorityVerdict::Kinds(kinds) => {
                self.0.push(3);
                self.strings(kinds.iter());
            }
        }
        self.text(&checked.facts_digest);
        self.count(checked.facts_bytes.len());
        self.0.extend_from_slice(&checked.facts_bytes);
        self.text(&checked.compiler_id);
        Ok(())
    }
}
