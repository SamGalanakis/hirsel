//! The request record (STEP-C 2.1, S3): one `hirsel` request's answer, stored
//! under one key, so that a request whose inputs are all unchanged is answered
//! without opening a check product.
//!
//! The key is the request's snapshot (compiler identity, sources by module,
//! signatures flag, digest and manifest-relative name, image roots, entry)
//! with this request's tuple: its command and check variant, every native
//! option that changes the answer, and the toolchain it observed (link
//! inputs, runtime, recipe and keyed environment). The value holds what the
//! request answers: check root, check and build acceptance kept apart,
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
        ContentHash::of_parts("hirsel.request-record.v1", [&snapshot.0[..], &self.tuple])
    }
}
/// Every native option, by name: those that change the executable enter the
/// key, and those that do not say why.
fn native_tuple(options: &NativeOptions) -> io::Result<(PathBuf, Vec<u8>)> {
    let NativeOptions {
        tape,
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
            net_rules,
            tape,
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
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub(crate) struct Row {
    rule: String,
    primary: StoredSite,
    causal: Vec<StoredSite>,
    declared: Option<String>,
    required: Option<String>,
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
    revision: ContentHash,
    observation_mode: bool,
    debug_lines: bool,
    step_charging_elided: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub(crate) struct Value {
    limits: Limits,
    root: ContentHash,
    /// The check's acceptance; a build's is the reach's.
    check_accepted: bool,
    files: usize,
    source_bytes: usize,
    diagnostics: Vec<Row>,
    incomplete: Vec<Row>,
    resources: Vec<Row>,
    reach: Option<Reach>,
    native: Option<Native>,
}
/// A request the record answers: its value, the current names of the
/// request's files, and a build's executable bytes, already checked.
pub(crate) struct Hit {
    pub(crate) value: Value,
    names: Vec<String>,
    pub(crate) executable: Option<Vec<u8>>,
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
            match Store::new(native_store)?.get(native.identity)? {
                Some(bytes) => Some(bytes),
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
    name.map_or_else(
        || {
            format!(
                "{}@{}/{}",
                module.package.name,
                module.package.version,
                module.modules.join("/")
            )
        },
        str::to_owned,
    )
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
    pub(crate) fn of(answer: Answer<'_>) -> Self {
        let product = answer.product;
        // A rendered name back to its file: the first request file of that
        // name, which renders as it under this key; else the name itself.
        let mut positions = BTreeMap::new();
        for (position, file) in product.files.iter().enumerate() {
            let module = product.index.module_path(file.module);
            positions
                .entry(file_name(file.name.as_deref(), &module))
                .or_insert(position as u32);
        }
        let site = |site: &SourceSite| StoredSite {
            file: positions
                .get(&site.module)
                .map_or_else(|| File::Named(site.module.clone()), |p| File::Request(*p)),
            span: site.span,
        };
        let row = |rule: &str,
                   primary: &SourceSite,
                   causal: &[SourceSite],
                   declared: Option<&str>,
                   required: Option<&str>,
                   message: &str| Row {
            rule: rule.to_owned(),
            primary: site(primary),
            causal: causal.iter().map(site).collect(),
            declared: declared.map(str::to_owned),
            required: required.map(str::to_owned),
            message: message.to_owned(),
        };
        Self {
            limits: Limits::current(),
            root: product.root,
            check_accepted: product.accepted(),
            files: product.files.len(),
            source_bytes: product.bytes,
            diagnostics: answer
                .diagnostics
                .iter()
                .map(|d| {
                    row(
                        d.rule, &d.primary, &d.causal, d.declared, d.required, d.message,
                    )
                })
                .collect(),
            incomplete: answer
                .incomplete
                .iter()
                .map(|g| row("", &g.primary, &g.causal, None, None, g.message))
                .collect(),
            resources: answer
                .resources
                .iter()
                .map(|r| row("", &r.primary, &[], None, None, r.message))
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
            native: answer.native.map(|n| Native {
                private_variants: n.private_variants,
                identity: n.identity,
                revision: n.revision,
                observation_mode: n.observation_mode,
                debug_lines: n.debug_lines,
                step_charging_elided: n.step_charging_elided,
            }),
        }
    }
}

/// A hit's answer, rendered for the current request.
pub(crate) struct Rendered<'a> {
    pub(crate) accepted: bool,
    pub(crate) root: ContentHash,
    pub(crate) files: usize,
    pub(crate) source_bytes: usize,
    pub(crate) diagnostics: Vec<Diagnostic<'a>>,
    pub(crate) incomplete: Vec<IncompleteDiagnostic<'a>>,
    pub(crate) resources: Vec<ResourceDiagnostic<'a>>,
    pub(crate) reach: Option<(usize, usize, BTreeMap<&'a str, usize>, Vec<GapStop>)>,
}
impl Hit {
    fn site(&self, site: &StoredSite) -> SourceSite {
        SourceSite {
            module: match &site.file {
                File::Request(position) => self.names[*position as usize].clone(),
                File::Named(name) => name.clone(),
            },
            span: site.span,
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
            source_bytes: value.source_bytes,
            diagnostics: value
                .diagnostics
                .iter()
                .map(|r| Diagnostic {
                    rule: &r.rule,
                    primary: self.site(&r.primary),
                    causal: r.causal.iter().map(|s| self.site(s)).collect(),
                    declared: r.declared.as_deref(),
                    required: r.required.as_deref(),
                    message: &r.message,
                })
                .collect(),
            incomplete: value
                .incomplete
                .iter()
                .map(|r| IncompleteDiagnostic {
                    primary: self.site(&r.primary),
                    causal: r.causal.iter().map(|s| self.site(s)).collect(),
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
            revision: n.revision,
            compiled: 0,
            linked: false,
            observation_mode: n.observation_mode,
            debug_lines: n.debug_lines,
            step_charging_elided: n.step_charging_elided,
            reach: Default::default(),
        })
    }
}
