//! The request record (STEP-C 2.1, S3): one `hirsel` request's answer, stored
//! under one key, so that a request whose inputs are all unchanged is answered
//! without opening a check product.
//!
//! The key is the request's snapshot (compiler identity, sources by module,
//! signatures flag, digest and manifest-relative name, image roots, entry)
//! with this request's tuple: its command and check variant, every native
//! option that changes the answer, and the toolchain it observed (link
//! inputs, runtime, recipe and keyed environment). The value holds what the
//! request answers: check root, checked definition count, check and build acceptance kept apart,
//! diagnostics, incomplete and resource rows, the reach census, and the
//! executable's key. Paths are never stored: a site names its file by its
//! position in the normalised source order (R1) and a byte span, and a
//! request renders the name from its own manifest (D6). Work counters are
//! not answers and a hit reports none (D2).
//!
//! A value records the memory limits it succeeded under (D5): the soft data
//! limit and the host budget's cap. Exhausting either ends the process
//! (exit 71) and publishes nothing, so the lowest limits a request succeeded
//! under bound its peak; a request under lower limits re-runs. A refusal, an
//! interrupt or an I/O failure is never published; an executable the output
//! could not be written to is (D3), since the build itself succeeded.
use super::*;
use hirsel_core_resolve::CausalRole;
use hirsel_core_syntax::Span;

/// What one request adds to its snapshot to make the record's key.
pub(crate) struct Request {
    tuple: Vec<u8>,
    /// Where the request's executables are stored; a build's value is a hit
    /// only while its executable there passes its check.
    native_store: Option<PathBuf>,
}
/// The memory limits a request runs under.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq)]
pub(crate) struct Limits {
    data: u64,
    host: u64,
}
impl Limits {
    pub(crate) fn current() -> Self {
        let mut data = libc::rlimit {
            rlim_cur: 0,
            rlim_max: 0,
        };
        // SAFETY: getrlimit writes the struct it is given.
        unsafe { libc::getrlimit(libc::RLIMIT_DATA, &mut data) };
        Self {
            data: data.rlim_cur,
            host: hirsel_guard::admission::host_budget().1,
        }
    }
    /// A value recorded under `recorded` answers a request under these limits.
    fn cover(self, recorded: Self) -> bool {
        self.data >= recorded.data && self.host >= recorded.host
    }
}
impl Request {
    /// `command` with its check variant; `native`, the options of a build,
    /// whose toolchain this observes once for the whole request (the build
    /// links with what the key names).
    pub(crate) fn new(
        command: &str,
        variant: (bool, bool, bool),
        native: Option<&mut NativeOptions>,
    ) -> io::Result<Self> {
        let native = match native {
            Some(options) => {
                let observed = crate::tool_inputs::toolchain(
                    &options.link,
                    &options.target,
                    &options.runtime,
                )?;
                options.observed = Some(Arc::new(observed));
                Some(native_tuple(options)?)
            }
            None => None,
        };
        let native_store = native.as_ref().map(|(store, _)| store.clone());
        Ok(Self {
            tuple: encode(&(command, variant, native.map(|(_, tuple)| tuple)))?,
            native_store,
        })
    }
    pub(crate) fn key(&self, snapshot: ContentHash) -> ContentHash {
        ContentHash::of_parts("hirsel.request-record.v4", [&snapshot.0[..], &self.tuple])
    }
}
/// Every native option, by name: those that change the executable enter the
/// key, and those that do not say why.
fn native_tuple(options: &NativeOptions) -> io::Result<(PathBuf, Vec<u8>)> {
    let NativeOptions {
        recording,
        replay,
        backend,
        optimize,
        debug_lines,
        target,
        // The runtime archive and the link enter by the toolchain's
        // identities, which name their bytes.
        runtime: _,
        link: _,
        // The store holds the record; the output is rendered per request.
        store,
        output: _,
        // A part's object is a function of its key alone, so where the
        // parts are kept does not change the executable.
        parts: _,
        root_limit,
        instance_limit,
        directories,
        grant_mask,
        net_rules,
        unix_roots,
        secret_grants,
        cred_grants,
        foreign_grants,
        exec_grants,
        env_patterns,
        // An observation build writes a report besides its answer; it is
        // never recorded (`Request` is not made for one).
        logical_charge_report: _,
        observed,
    } = options;
    // The toolchain image holds the link recipe's identity and the runtime
    // archive's bytes, so it alone names what the link reads.
    let (toolchain, _) = observed.as_deref().expect("observed above");
    Ok((
        store.clone(),
        encode(&(
            backend,
            optimize,
            debug_lines,
            target,
            root_limit,
            instance_limit,
            directories,
            grant_mask,
            (net_rules, unix_roots),
            (secret_grants, cred_grants),
            (foreign_grants, exec_grants),
            env_patterns,
            recording,
            replay,
            toolchain.image,
            toolchain.environment,
        ))?,
    ))
}

/// A source file a site is in: a request file by its position in the
/// normalised order, or a name fixed by the key (an image's file, or a
/// module named without a manifest).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub(crate) enum File {
    Request(u32),
    Named(String),
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub(crate) struct StoredSite {
    file: File,
    span: Span,
    line: Option<usize>,
    column: Option<usize>,
    line_text: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
struct StoredCause {
    site: StoredSite,
    role: CausalRole,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
struct StoredEdit {
    file: File,
    span: Span,
    text: String,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
struct StoredFix {
    applicability: Applicability,
    edits: Vec<StoredEdit>,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
struct Row {
    rule: String,
    primary: StoredSite,
    causal: Vec<StoredCause>,
    declared: Option<String>,
    required: Option<String>,
    message: String,
    fixes: Vec<StoredFix>,
    missing_patterns: Vec<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
struct IncompleteRow {
    primary: StoredSite,
    causal: Vec<StoredCause>,
    message: String,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
struct ResourceRow {
    primary: StoredSite,
    message: String,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub(crate) struct Stop {
    definition: String,
    rule: String,
    construct: String,
    primary: StoredSite,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub(crate) struct Reach {
    accepted: bool,
    bodies: usize,
    declarations: usize,
    census: BTreeMap<String, usize>,
    first_stops: Vec<Stop>,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub(crate) struct Native {
    private_variants: usize,
    identity: ContentHash,
    address: ContentHash,
    observation_mode: bool,
    debug_lines: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub(crate) struct Value {
    limits: Limits,
    root: ContentHash,
    /// The check's acceptance; a build's is the reach's.
    check_accepted: bool,
    files: usize,
    checked_definitions: usize,
    source_bytes: usize,
    diagnostics: Vec<Row>,
    incomplete: Vec<IncompleteRow>,
    resources: Vec<ResourceRow>,
    reach: Option<Reach>,
    native: Option<Native>,
}
/// A request the record answers: its value, the current names of the
/// request's files, and a build's executable bytes, already checked.
pub(crate) struct Hit {
    pub(crate) value: Value,
    names: Vec<String>,
    pub(crate) executable: Option<crate::native_debug::NativeImage>,
}

/// The value stored under `key`, when the current limits cover its own and a
/// build's executable is stored whole; anything else is a miss.
/// Why a request's record did not answer it, as the meter stage it ends.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Miss {
    /// No record, or one this compiler cannot read.
    Absent,
    /// Recorded under a higher memory limit than this request's.
    Limits,
    /// Its executable is no longer stored intact.
    Executable,
}
impl Miss {
    pub(crate) fn stage(self) -> &'static str {
        match self {
            Self::Absent => "request record/miss",
            Self::Limits => "request record/miss: limits",
            Self::Executable => "request record/miss: executable",
        }
    }
}
pub(crate) fn lookup(
    store: &Store,
    key: ContentHash,
    request: &Request,
    names: impl FnOnce() -> Vec<String>,
) -> io::Result<Result<Hit, Miss>> {
    let Some(value) = store.get(key)?.and_then(|bytes| decode::<Value>(&bytes)) else {
        return Ok(Err(Miss::Absent));
    };
    if !Limits::current().cover(value.limits) {
        return Ok(Err(Miss::Limits));
    }
    let executable = match (&value.native, &request.native_store) {
        (None, _) => None,
        (Some(native), Some(native_store)) => {
            match crate::native_debug::NativeImage::load(
                &Store::new(native_store)?,
                native.identity,
            )? {
                Some(binary) => Some(binary),
                None => return Ok(Err(Miss::Executable)),
            }
        }
        (Some(_), None) => return Ok(Err(Miss::Executable)),
    };
    Ok(Ok(Hit {
        value,
        names: names(),
        executable,
    }))
}
/// Publish `value` under `key` by one atomic rename.
pub(crate) fn publish(store: &Path, key: ContentHash, value: &Value) -> io::Result<()> {
    Store::new(store)?.put_keyed(key, &encode(value)?)
}

/// The name a site's file is rendered with: its manifest-relative name, or
/// without a manifest its module.
pub(crate) fn file_name(name: Option<&str>, module: &ModulePath) -> String {
    diagnostics::source_name(name, module)
}

/// What a missed request answered, for its value.
pub(crate) struct Answer<'a> {
    pub(crate) product: &'a CheckProduct,
    pub(crate) diagnostics: &'a [Diagnostic<'a>],
    pub(crate) incomplete: &'a [IncompleteDiagnostic<'a>],
    pub(crate) resources: &'a [ResourceDiagnostic<'a>],
    pub(crate) reach: Option<&'a Reachable<'a>>,
    pub(crate) native: Option<&'a BuildProduct>,
}
impl Value {
    pub(crate) fn of(answer: Answer<'_>) -> Option<Self> {
        let product = answer.product;
        // This shortcut has no index. Even a lookup that yielded no edit
        // needs to go through the render path.
        let has_lookup = if let Some(reach) = answer.reach {
            reach.diagnostics.iter().any(|d| d.unresolved.is_some())
        } else {
            product.diagnostics.iter().any(|d| d.unresolved.is_some())
        };
        if has_lookup {
            return None;
        }
        // A rendered name back to its file: the first request file of that
        // name, which renders as it under this key; else the name itself.
        let mut positions = BTreeMap::new();
        for (position, file) in product.files.iter().enumerate() {
            let module = product.index.module_path(file.module);
            positions
                .entry(file_name(file.name.as_deref(), &module))
                .or_insert(position as u32);
        }
        let file = |module: &str| {
            positions
                .get(module)
                .map_or_else(|| File::Named(module.to_owned()), |p| File::Request(*p))
        };
        let site = |site: &SourceSite| StoredSite {
            file: file(&site.module),
            span: site.span,
            line: site.line,
            column: site.column,
            line_text: site.line_text.clone(),
        };
        let value = Self {
            limits: Limits::current(),
            root: product.root,
            check_accepted: product.accepted(),
            files: product.files.len(),
            checked_definitions: product.checked_definitions(),
            source_bytes: product.bytes,
            diagnostics: answer
                .diagnostics
                .iter()
                .map(|d| Row {
                    rule: d.rule.to_owned(),
                    primary: site(&d.primary),
                    causal: d
                        .causal
                        .iter()
                        .map(|cause| StoredCause {
                            site: site(&cause.site),
                            role: cause.role,
                        })
                        .collect(),
                    declared: d.declared.map(str::to_owned),
                    required: d.required.map(str::to_owned),
                    message: d.message.to_owned(),
                    fixes: d
                        .fixes
                        .iter()
                        .map(|fix| StoredFix {
                            applicability: fix.applicability,
                            edits: fix
                                .edits
                                .iter()
                                .map(|edit| StoredEdit {
                                    file: file(&edit.module),
                                    span: edit.span,
                                    text: edit.text.clone(),
                                })
                                .collect(),
                        })
                        .collect(),
                    missing_patterns: d.missing_patterns.to_vec(),
                })
                .collect(),
            incomplete: answer
                .incomplete
                .iter()
                .map(|g| IncompleteRow {
                    primary: site(&g.primary),
                    causal: g
                        .causal
                        .iter()
                        .map(|cause| StoredCause {
                            site: site(&cause.site),
                            role: cause.role,
                        })
                        .collect(),
                    message: g.message.to_owned(),
                })
                .collect(),
            resources: answer
                .resources
                .iter()
                .map(|r| ResourceRow {
                    primary: site(&r.primary),
                    message: r.message.to_owned(),
                })
                .collect(),
            reach: answer.reach.map(|r| Reach {
                accepted: r.accepted(),
                bodies: r.bodies,
                declarations: r.declarations,
                census: r
                    .census()
                    .into_iter()
                    .map(|(name, n)| (name.to_owned(), n))
                    .collect(),
                first_stops: r
                    .first_stops
                    .iter()
                    .map(|s| Stop {
                        definition: s.definition.clone(),
                        rule: s.rule.clone(),
                        construct: s.construct.clone(),
                        primary: site(&s.primary),
                    })
                    .collect(),
            }),
            native: None,
        };
        Some(value.with_native(answer.native))
    }

    pub(crate) fn with_native(mut self, native: Option<&BuildProduct>) -> Self {
        self.native = native.map(|n| Native {
            private_variants: n.private_variants,
            identity: n.identity,
            address: n.address,
            observation_mode: n.observation_mode,
            debug_lines: n.debug_lines,
        });
        self
    }
}

/// A hit's answer, rendered for the current request.
pub(crate) struct Rendered<'a> {
    pub(crate) accepted: bool,
    pub(crate) root: ContentHash,
    pub(crate) files: usize,
    pub(crate) checked_definitions: usize,
    pub(crate) source_bytes: usize,
    pub(crate) diagnostics: Vec<Diagnostic<'a>>,
    pub(crate) incomplete: Vec<IncompleteDiagnostic<'a>>,
    pub(crate) resources: Vec<ResourceDiagnostic<'a>>,
    pub(crate) reach: Option<(usize, usize, BTreeMap<&'a str, usize>, Vec<GapStop>)>,
}
impl Hit {
    fn name(&self, file: &File) -> String {
        match file {
            File::Request(position) => self.names[*position as usize].clone(),
            File::Named(name) => name.clone(),
        }
    }
    fn site(&self, site: &StoredSite) -> SourceSite {
        SourceSite {
            module: self.name(&site.file),
            span: site.span,
            line: site.line,
            column: site.column,
            line_text: site.line_text.clone(),
        }
    }
    pub(crate) fn render(&self) -> Rendered<'_> {
        let value = &self.value;
        Rendered {
            accepted: value
                .reach
                .as_ref()
                .map_or(value.check_accepted, |r| r.accepted),
            root: value.root,
            files: value.files,
            checked_definitions: value.checked_definitions,
            source_bytes: value.source_bytes,
            diagnostics: value
                .diagnostics
                .iter()
                .map(|r| Diagnostic {
                    rule: &r.rule,
                    primary: self.site(&r.primary),
                    causal: r
                        .causal
                        .iter()
                        .map(|c| DiagnosticCause {
                            site: self.site(&c.site),
                            role: c.role,
                        })
                        .collect(),
                    declared: r.declared.as_deref(),
                    required: r.required.as_deref(),
                    message: &r.message,
                    fixes: r
                        .fixes
                        .iter()
                        .map(|fix| Fix {
                            applicability: fix.applicability,
                            signature: None,
                            edits: fix
                                .edits
                                .iter()
                                .map(|edit| Edit {
                                    module: self.name(&edit.file),
                                    span: edit.span,
                                    text: edit.text.clone(),
                                })
                                .collect(),
                        })
                        .collect(),
                    available_members: Vec::new(),
                    missing_patterns: &r.missing_patterns,
                })
                .collect(),
            incomplete: value
                .incomplete
                .iter()
                .map(|r| IncompleteDiagnostic {
                    primary: self.site(&r.primary),
                    causal: r
                        .causal
                        .iter()
                        .map(|c| DiagnosticCause {
                            site: self.site(&c.site),
                            role: c.role,
                        })
                        .collect(),
                    message: &r.message,
                })
                .collect(),
            resources: value
                .resources
                .iter()
                .map(|r| ResourceDiagnostic {
                    primary: self.site(&r.primary),
                    message: &r.message,
                })
                .collect(),
            reach: value.reach.as_ref().map(|r| {
                (
                    r.bodies,
                    r.declarations,
                    r.census.iter().map(|(k, n)| (k.as_str(), *n)).collect(),
                    r.first_stops
                        .iter()
                        .map(|s| GapStop {
                            definition: s.definition.clone(),
                            rule: s.rule.clone(),
                            construct: s.construct.clone(),
                            primary: self.site(&s.primary),
                        })
                        .collect(),
                )
            }),
        }
    }
    /// The build this hit answers, its executable at `output`.
    pub(crate) fn build(&self, output: &Path) -> Option<BuildProduct> {
        self.value.native.as_ref().map(|n| BuildProduct {
            private_variants: n.private_variants,
            executable: output.to_owned(),
            identity: n.identity,
            address: n.address,
            recipe: None,
            assembly: None,
            revision: hirsel_core_ir::code_revision(n.address),
            compiled: 0,
            linked: false,
            observation_mode: n.observation_mode,
            debug_lines: n.debug_lines,
            reach: Default::default(),
        })
    }
}
