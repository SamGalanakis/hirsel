//! Store authorities (storage.md STORE-11; determinism.md, Authority grants): the
//! runtime entries of `io.authority` and the `authority.*`, `level.*` and `claim.*`
//! operations.
//!
//! What follows from the grant alone (a projection, the declared levels, the floor, the
//! entry bound, a claim's end and the narrowings) is the [`State`]'s, so replay derives it
//! again. What a provider decides (each claim, advance, barrier, read and object request)
//! is a sampled input: a live adapter draws it from its [`Providers`], replay from the
//! tape, and [`State::sample`] admits either the same way, minting a claim's handle and
//! moving its end.
//!
//! A `Level of a` is the word `index * 256 + level`, where `index` is the authority's
//! position among the run's sorted grant names and `level` its index among the levels the
//! provider declares, as the interpreter has it. A `Claim of a` is a handle whose row
//! keeps its authority and end; the provider's claim stays with the adapter, keyed by the
//! handle, until the claim is closed or refused by `claim.narrow`.
use crate::handles::Grant;
use crate::io::{AdapterError, Completion, IoError, Request, State};
use hirsel_store_authority::wire::{self, Outcome};
use hirsel_store_authority::{Advanced, AuthorityError, AuthorityGrant, Error as SeamError};
use std::collections::BTreeMap;

/// A seam request, by the handles and words the program passed.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SeamRequest {
    /// `io.authority`.
    Project {
        io: u64,
        name: String,
    },
    Levels {
        authority: u64,
    },
    Level {
        authority: u64,
        name: String,
    },
    Floor {
        authority: u64,
    },
    LevelName {
        level: u64,
    },
    LevelNarrow {
        authority: u64,
        level: u64,
    },
    MaxEntry {
        authority: u64,
    },
    ClaimEnd {
        claim: u64,
    },
    /// The claim transfers: refused, it ends.
    ClaimNarrow {
        authority: u64,
        claim: u64,
    },
    /// A provider request; `claim` is the claim a claimed one names.
    Provider {
        authority: u64,
        claim: Option<u64>,
        request: wire::Request,
    },
}

/// A seam completion.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SeamCompletion {
    /// The Level words of an authority's declared levels, weakest first.
    Words(Vec<u64>),
    Name(String),
    Word(u64),
    /// An optional Level word or Claim handle.
    Optional(Option<u64>),
    /// A provider's outcome; `claim` is the handle an admitted claim minted.
    Outcome {
        outcome: Outcome,
        claim: Option<u64>,
    },
}

/// One granted authority: its name, declared levels and floor.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Granted {
    pub(crate) name: String,
    pub(crate) grant: AuthorityGrant,
    pub(crate) levels: Vec<String>,
    pub(crate) floor: u8,
}

/// The run's granted authorities, in name order.
pub(crate) fn granted(
    grants: &BTreeMap<String, AuthorityGrant>,
) -> Result<Vec<Granted>, hirsel_run_schema::RunRefusal> {
    grants
        .iter()
        .map(|(name, grant)| {
            let (levels, floor) = hirsel_store_authority::check(grant).map_err(|e| {
                hirsel_run_schema::RunRefusal::Unauthorized {
                    field: format!("io.authorities.{name}: {}", e.0),
                }
            })?;
            Ok(Granted {
                name: name.clone(),
                grant: grant.clone(),
                levels,
                floor: floor.0,
            })
        })
        .collect()
}

fn level_word(index: usize, level: u8) -> u64 {
    index as u64 * 256 + level as u64
}

impl State {
    /// The authority index an Authority handle projects.
    fn authority(&self, id: u64) -> Result<usize, AdapterError> {
        match self.handles.rows.get(&id) {
            Some(Grant::Authority { index }) => Ok(*index),
            _ => Err(AdapterError::InvalidHandle(id)),
        }
    }

    /// A Claim handle's authority index and end.
    fn claim_row(&self, id: u64) -> Result<(usize, u64), AdapterError> {
        match self.handles.rows.get(&id) {
            Some(Grant::Claim { index, end }) => Ok((*index, *end)),
            _ => Err(AdapterError::InvalidHandle(id)),
        }
    }

    /// A level word's authority index and level; the brand makes another authority's a
    /// checker bug, so it is refused as invalid.
    fn level_of(&self, index: usize, word: u64) -> Result<u8, AdapterError> {
        let (i, l) = ((word / 256) as usize, (word % 256) as u8);
        match self.authorities.get(i) {
            Some(g) if i == index && (l as usize) < g.levels.len() => Ok(l),
            _ => Err(AdapterError::InvalidCompletion),
        }
    }

    /// The grant a provider request is of, after its handle and level checks.
    pub(crate) fn seam_provider(&self, request: &SeamRequest) -> Result<usize, AdapterError> {
        let SeamRequest::Provider {
            authority,
            claim,
            request,
        } = request
        else {
            return Err(AdapterError::InvalidCompletion);
        };
        let index = self.authority(*authority)?;
        match (claim, request.claimed()) {
            (Some(c), true) => {
                if self.claim_row(*c)?.0 != index {
                    return Err(AdapterError::InvalidHandle(*c));
                }
            }
            (None, false) => {}
            _ => return Err(AdapterError::InvalidCompletion),
        }
        if let Some(l) = request.level() {
            self.level_of(index, level_word(index, l))?;
        }
        Ok(index)
    }

    /// The grant `index`.
    pub(crate) fn granted(&self, index: usize) -> &Granted {
        &self.authorities[index]
    }

    /// The seam requests the grant alone answers.
    pub(crate) fn execute_seam(
        &mut self,
        request: &SeamRequest,
    ) -> Result<Completion, AdapterError> {
        use SeamCompletion as S;
        let done = |c: SeamCompletion| Ok(Completion::Seam(c));
        match request {
            SeamRequest::Project { io, name } => {
                self.expect(*io, Grant::Io)?;
                Ok(Completion::Handle(
                    match self.authorities.iter().position(|g| &g.name == name) {
                        Some(index) => Ok(self.handles.insert(Grant::Authority { index })),
                        None => Err(IoError::NotGranted),
                    },
                ))
            }
            SeamRequest::Levels { authority } => {
                let index = self.authority(*authority)?;
                let count = self.authorities[index].levels.len();
                done(S::Words(
                    (0..count).map(|l| level_word(index, l as u8)).collect(),
                ))
            }
            SeamRequest::Level { authority, name } => {
                let index = self.authority(*authority)?;
                let levels = &self.authorities[index].levels;
                done(S::Optional(
                    levels
                        .iter()
                        .position(|l| l == name)
                        .map(|l| level_word(index, l as u8)),
                ))
            }
            SeamRequest::Floor { authority } => {
                let index = self.authority(*authority)?;
                done(S::Word(level_word(index, self.authorities[index].floor)))
            }
            SeamRequest::LevelName { level } => {
                let index = (level / 256) as usize;
                let l = self.level_of(index, *level)?;
                done(S::Name(self.authorities[index].levels[l as usize].clone()))
            }
            SeamRequest::LevelNarrow { authority, level } => {
                let index = self.authority(*authority)?;
                done(S::Optional(
                    ((level / 256) as usize == index).then_some(*level),
                ))
            }
            SeamRequest::MaxEntry { authority } => {
                self.authority(*authority)?;
                done(S::Word(hirsel_store_authority::MAX_ENTRY))
            }
            SeamRequest::ClaimEnd { claim } => done(S::Word(self.claim_row(*claim)?.1)),
            SeamRequest::ClaimNarrow { authority, claim } => {
                let index = self.authority(*authority)?;
                let (of, _) = self.claim_row(*claim)?;
                // Two projections of one grant are one authority.
                if self.authorities[of].name == self.authorities[index].name {
                    done(S::Optional(Some(*claim)))
                } else {
                    self.handles.rows.remove(claim);
                    done(S::Optional(None))
                }
            }
            SeamRequest::Provider { .. } => Err(AdapterError::InvalidCompletion),
        }
    }

    /// Admit a provider's outcome, live or recorded: it answers its request, an admitted
    /// claim mints its handle (a recorded one must name the same) and an advance moves the
    /// claim's end.
    pub(crate) fn sample_seam(
        &mut self,
        request: &SeamRequest,
        input: &SeamCompletion,
    ) -> Result<Completion, AdapterError> {
        let index = self.seam_provider(request)?;
        let SeamRequest::Provider { claim, request, .. } = request else {
            unreachable!()
        };
        let SeamCompletion::Outcome {
            outcome,
            claim: minted,
        } = input
        else {
            return Err(AdapterError::InvalidCompletion);
        };
        if !request.admits(outcome) {
            return Err(AdapterError::InvalidCompletion);
        }
        let handle = match (request, outcome) {
            (wire::Request::Claim, Outcome::Position(Ok(end))) => {
                let id = self.handles.insert(Grant::Claim { index, end: *end });
                if minted.is_some_and(|m| m != id) {
                    return Err(AdapterError::InvalidCompletion);
                }
                Some(id)
            }
            _ if minted.is_some() => return Err(AdapterError::InvalidCompletion),
            (_, Outcome::Advanced(Advanced::Advanced(end))) => {
                let c = claim.expect("a claimed request");
                self.handles
                    .rows
                    .insert(c, Grant::Claim { index, end: *end });
                None
            }
            _ => None,
        };
        Ok(Completion::Seam(SeamCompletion::Outcome {
            outcome: outcome.clone(),
            claim: handle,
        }))
    }

    /// Close a Claim: it ends.
    pub(crate) fn close_claim(&mut self, handle: u64) -> Result<Completion, AdapterError> {
        self.claim_row(handle)?;
        self.handles.rows.remove(&handle);
        Ok(Completion::Unit(Ok(())))
    }

    pub(crate) fn is_claim(&self, handle: u64) -> bool {
        matches!(self.handles.rows.get(&handle), Some(Grant::Claim { .. }))
    }
}

/// How a live adapter draws each claim's attempt and the identity it mints.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Draws {
    /// The host's secure generator: every claim of a production run is its own.
    System,
    /// A fixture's seed, as the interpreter's simulation draws, so the two agree.
    Seeded([u8; 16]),
}

/// A live adapter's providers, made at their first request, and the provider claims the
/// program holds, by handle.
pub(crate) struct Providers {
    draws: Draws,
    attempts: u64,
    open: BTreeMap<usize, Box<dyn hirsel_store_authority::Authority>>,
    claims: BTreeMap<u64, hirsel_store_authority::Claim>,
    /// Resolves a log grant's mount and path to its host directory.
    directory: Box<dyn Fn(&AuthorityGrant) -> Result<std::path::PathBuf, String> + Send>,
}

impl Providers {
    pub(crate) fn new(
        draws: Draws,
        directory: Box<dyn Fn(&AuthorityGrant) -> Result<std::path::PathBuf, String> + Send>,
    ) -> Self {
        Self {
            draws,
            attempts: 0,
            open: BTreeMap::new(),
            claims: BTreeMap::new(),
            directory,
        }
    }

    /// Perform a provider request: draw its outcome and have `state` admit it, keeping
    /// an admitted claim's provider claim under its handle.
    pub(crate) fn perform(
        &mut self,
        state: &mut State,
        whole: &Request,
    ) -> Result<Completion, AdapterError> {
        let Request::Seam(seam) = whole else {
            return Err(AdapterError::InvalidCompletion);
        };
        let index = state.seam_provider(seam)?;
        let SeamRequest::Provider { claim, request, .. } = seam else {
            unreachable!()
        };
        if !self.open.contains_key(&index) {
            let grant = &state.granted(index).grant;
            let dir = if grant.provider == "authority.log@1" {
                Some((self.directory)(grant).map_err(AdapterError::Unsupported)?)
            } else {
                None
            };
            let p = hirsel_store_authority::open(grant, dir.as_deref())
                .map_err(|e| AdapterError::Unsupported(e.0))?;
            self.open.insert(index, p);
        }
        let p = self.open.get_mut(&index).unwrap();
        let c = match claim {
            Some(c) => Some(
                self.claims
                    .get_mut(c)
                    .ok_or(AdapterError::InvalidHandle(*c))?,
            ),
            None => None,
        };
        let n = self.attempts;
        let draws = self.draws;
        let mut drawn = Ok(());
        let (outcome, held) = wire::serve(&mut **p, c, request.clone(), || match draws {
            Draws::Seeded(seed) => wire::draw(&seed, n),
            Draws::System => match crate::io::RandomSource::System.draw(48) {
                Ok(b) => (b[..16].try_into().unwrap(), b[16..].try_into().unwrap()),
                Err(_) => {
                    drawn = Err(AdapterError::Unsupported(
                        "the host's secure generator is unavailable".into(),
                    ));
                    ([0; 16], [0; 32])
                }
            },
        })
        .map_err(|_| AdapterError::InvalidCompletion)?;
        drawn?;
        if matches!(request, wire::Request::Claim) {
            self.attempts += 1;
        }
        let completion = state.sample(
            whole,
            Completion::Seam(SeamCompletion::Outcome {
                outcome,
                claim: None,
            }),
        )?;
        if let (Completion::Seam(SeamCompletion::Outcome { claim: Some(h), .. }), Some(c)) =
            (&completion, held)
        {
            self.claims.insert(*h, c);
        }
        Ok(completion)
    }

    /// A claim the program no longer holds: closed, or refused by `claim.narrow`.
    pub(crate) fn forget(&mut self, handle: u64) {
        self.claims.remove(&handle);
    }
}

/// `Result[T, SeamError]` for every seam result: the Result tag at 0, then at 8 the
/// payload (a Claim handle, a u64, a `List[Bytes]` block, an `Option[Bytes]` whose null
/// owner is None, or nothing) or the SeamError: its tag at 8 and its payload at 16 (an
/// AuthorityError, the Corrupt object String, or Format's Bytes at 16 and String at 40).
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct SeamResult {
    pub tag: u32,
    pub padding: u32,
    pub words: [u64; 7],
}

/// `Advanced`: its tag at 0, then at 8 the position or the AuthorityError.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct AdvancedResult {
    pub tag: u32,
    pub padding: u32,
    pub words: [u64; 2],
}

/// An `Option` of one word (Level or Claim): its tag at 0 and the word at 8.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct WordOption {
    pub tag: u32,
    pub padding: u32,
    pub value: u64,
}

/// Write `AuthorityError` at `at`: a u32 tag, then at 4 the FileError (its tag and its
/// Fault code) or the u16 status.
unsafe fn write_medium(at: *mut u8, e: &AuthorityError) {
    unsafe {
        let word = |o: usize, v: u32| at.add(o).cast::<u32>().write_unaligned(v);
        match e {
            AuthorityError::Io(0) => {
                word(0, 0);
                word(4, 0);
                word(8, 1);
            }
            AuthorityError::Io(k) => {
                word(0, 0);
                word(4, *k as u32);
            }
            AuthorityError::Status(s) => {
                word(0, 1);
                at.add(4).cast::<u16>().write_unaligned(*s);
            }
            AuthorityError::Unreachable => word(0, 2),
        }
    }
}

/// The Result of a provider outcome, its Ok payload written by `ok`.
unsafe fn seam_result<T>(
    ctx: *mut crate::TaskCtx,
    r: &Result<T, SeamError>,
    ok: impl FnOnce(&T, &mut SeamResult),
) -> SeamResult {
    let mut out = SeamResult::default();
    match r {
        Ok(v) => ok(v, &mut out),
        Err(e) => {
            out.tag = 1;
            let base = std::ptr::addr_of_mut!(out.words).cast::<u8>();
            let (tag, payload) = (base, unsafe { base.add(8) });
            let slice = |at: *mut u8, b: &[u8]| unsafe {
                at.cast::<crate::views::Slice>()
                    .write_unaligned(crate::text::allocate_bytes(ctx, b))
            };
            unsafe {
                match e {
                    SeamError::Storage(m) => write_medium(payload, m),
                    SeamError::Refused(m) => {
                        tag.cast::<u32>().write(1);
                        write_medium(payload, m)
                    }
                    SeamError::Corrupt(o) => {
                        tag.cast::<u32>().write(2);
                        slice(payload, o.as_bytes())
                    }
                    SeamError::Fenced => tag.cast::<u32>().write(3),
                    SeamError::Format { found, object } => {
                        tag.cast::<u32>().write(4);
                        slice(payload, found);
                        slice(payload.add(24), object.as_bytes())
                    }
                }
            }
        }
    }
    out
}

/// Issue `request` and convert its seam completion with `convert`.
unsafe fn seam_call<T>(
    ctx: *mut crate::TaskCtx,
    request: SeamRequest,
    convert: impl FnOnce(&SeamCompletion) -> T,
) -> Option<T> {
    // SAFETY: this semantic entry owns its task until it suspends or returns.
    let _role = unsafe { crate::tasks::role::Guard::new(ctx) };
    let completion = unsafe { crate::io::capability_call(ctx, Request::Seam(request)) }.ok()?;
    Some(unsafe {
        crate::tasks::free::host::native::convert_result(ctx, completion, |c| {
            let Completion::Seam(c) = c else {
                crate::fault("seam completion mismatch")
            };
            convert(c)
        })
    })
}

unsafe fn text(owner: *mut crate::Header, start: u64, len: u64) -> String {
    std::str::from_utf8(unsafe { crate::io::slice_bytes(owner, start, len) })
        .unwrap_or_else(|_| crate::fault("String operand is not UTF-8"))
        .to_owned()
}

/// A provider outcome's Result, its Ok payload written by `ok`.
unsafe fn provider(
    ctx: *mut crate::TaskCtx,
    out: *mut SeamResult,
    authority: u64,
    claim: Option<u64>,
    request: wire::Request,
) -> u8 {
    let r = unsafe {
        seam_call(
            ctx,
            SeamRequest::Provider {
                authority,
                claim,
                request,
            },
            |c| {
                let SeamCompletion::Outcome { outcome, claim } = c else {
                    crate::fault("seam completion mismatch")
                };
                match outcome {
                    Outcome::Position(r) => seam_result(ctx, r, |p, out| {
                        out.words[0] = claim.unwrap_or(*p);
                    }),
                    Outcome::Read(r) => seam_result(ctx, r, |all, out| {
                        out.words[0] = crate::text::allocate_bytes_list(ctx, all) as u64;
                    }),
                    Outcome::Get(r) => seam_result(ctx, r, |v, out| {
                        if let Some(b) = v {
                            std::ptr::addr_of_mut!(out.words)
                                .cast::<crate::views::Slice>()
                                .write_unaligned(crate::text::allocate_bytes(ctx, b));
                        }
                    }),
                    Outcome::Unit(r) => seam_result(ctx, r, |(), _| {}),
                    Outcome::Advanced(_) => crate::fault("seam completion mismatch"),
                }
            },
        )
    };
    match r {
        Some(r) => {
            unsafe { out.write(r) };
            crate::STATUS_OK
        }
        None => crate::STATUS_FAILURE,
    }
}

/// `io.authority(rt, name) -> Result[Authority, GrantError]`.
/// # Safety
/// Context bound; Io borrowed; the name a live String slice; out writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_rt_v2_io_authority(
    ctx: *mut crate::TaskCtx,
    _destination: *mut u8,
    out: *mut crate::io::GrantResult,
    io: *const u64,
    owner: *mut crate::Header,
    start: u64,
    len: u64,
) -> u8 {
    // SAFETY: this semantic entry owns its task until it suspends or returns.
    let _role = unsafe { crate::tasks::role::Guard::new(ctx) };
    let request = SeamRequest::Project {
        io: unsafe { io.read() },
        name: unsafe { text(owner, start, len) },
    };
    let Ok(Completion::Handle(value)) =
        (unsafe { crate::io::capability_call(ctx, Request::Seam(request)) })
    else {
        return crate::STATUS_FAILURE;
    };
    unsafe { out.write(crate::io::GrantResult::of(value)) };
    crate::STATUS_OK
}

/// `authority.levels(a) -> List[Level of a]`.
/// # Safety
/// Context bound; Authority borrowed; out writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_rt_v2_authority_levels(
    ctx: *mut crate::TaskCtx,
    _destination: *mut u8,
    out: *mut u64,
    a: *const u64,
) -> u8 {
    let authority = unsafe { a.read() };
    match unsafe {
        seam_call(ctx, SeamRequest::Levels { authority }, |c| {
            let SeamCompletion::Words(levels) = c else {
                crate::fault("seam completion mismatch")
            };
            // The context is live; a Level is one plain word.
            let block = crate::alloc::allocate_sequence(ctx, 0, levels.len() as u64, 8, 8);
            let first = crate::alloc::payload(block)
                .add(crate::alloc::sequence_elements_offset(8) as usize);
            for (i, level) in levels.iter().enumerate() {
                first.add(i * 8).cast::<u64>().write_unaligned(*level);
            }
            block as u64
        })
    } {
        Some(list) => {
            unsafe { out.write(list) };
            crate::STATUS_OK
        }
        None => crate::STATUS_FAILURE,
    }
}

/// A seam call answered with one word.
unsafe fn word_call(ctx: *mut crate::TaskCtx, out: *mut u64, request: SeamRequest) -> u8 {
    match unsafe {
        seam_call(ctx, request, |c| match c {
            SeamCompletion::Word(w) => *w,
            _ => crate::fault("seam completion mismatch"),
        })
    } {
        Some(w) => {
            unsafe { out.write(w) };
            crate::STATUS_OK
        }
        None => crate::STATUS_FAILURE,
    }
}

/// A seam call answered with an optional word.
unsafe fn option_call(ctx: *mut crate::TaskCtx, out: *mut WordOption, request: SeamRequest) -> u8 {
    match unsafe {
        seam_call(ctx, request, |c| match c {
            SeamCompletion::Optional(w) => WordOption {
                tag: u32::from(w.is_some()),
                padding: 0,
                value: w.unwrap_or(0),
            },
            _ => crate::fault("seam completion mismatch"),
        })
    } {
        Some(w) => {
            unsafe { out.write(w) };
            crate::STATUS_OK
        }
        None => crate::STATUS_FAILURE,
    }
}

/// `authority.level(a, name) -> Option[Level of a]`.
/// # Safety
/// Context bound; Authority borrowed; the name a live String slice; out writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_rt_v2_authority_level(
    ctx: *mut crate::TaskCtx,
    _destination: *mut u8,
    out: *mut WordOption,
    a: *const u64,
    owner: *mut crate::Header,
    start: u64,
    len: u64,
) -> u8 {
    let request = SeamRequest::Level {
        authority: unsafe { a.read() },
        name: unsafe { text(owner, start, len) },
    };
    unsafe { option_call(ctx, out, request) }
}

/// `authority.floor(a) -> Level of a`.
/// # Safety
/// Context bound; Authority borrowed; out writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_rt_v2_authority_floor(
    ctx: *mut crate::TaskCtx,
    _destination: *mut u8,
    out: *mut u64,
    a: *const u64,
) -> u8 {
    let authority = unsafe { a.read() };
    unsafe { word_call(ctx, out, SeamRequest::Floor { authority }) }
}

/// `authority.max_entry(a) -> u64`.
/// # Safety
/// Context bound; Authority borrowed; out writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_rt_v2_authority_max_entry(
    ctx: *mut crate::TaskCtx,
    out: *mut u64,
    a: *const u64,
) -> u8 {
    let authority = unsafe { a.read() };
    unsafe { word_call(ctx, out, SeamRequest::MaxEntry { authority }) }
}

/// `level.name(l) -> String`.
/// # Safety
/// Context bound; Level borrowed; out writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_rt_v2_level_name(
    ctx: *mut crate::TaskCtx,
    _destination: *mut u8,
    out: *mut crate::views::Slice,
    l: *const u64,
) -> u8 {
    let level = unsafe { l.read() };
    match unsafe {
        seam_call(ctx, SeamRequest::LevelName { level }, |c| match c {
            SeamCompletion::Name(n) => crate::text::allocate_bytes(ctx, n.as_bytes()),
            _ => crate::fault("seam completion mismatch"),
        })
    } {
        Some(s) => {
            unsafe { out.write(s) };
            crate::STATUS_OK
        }
        None => crate::STATUS_FAILURE,
    }
}

/// `level.narrow(a, l) -> Option[Level of a]`.
/// # Safety
/// Context bound; Authority and Level borrowed; out writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_rt_v2_level_narrow(
    ctx: *mut crate::TaskCtx,
    _destination: *mut u8,
    out: *mut WordOption,
    a: *const u64,
    l: *const u64,
) -> u8 {
    let request = SeamRequest::LevelNarrow {
        authority: unsafe { a.read() },
        level: unsafe { l.read() },
    };
    unsafe { option_call(ctx, out, request) }
}

/// `claim.end(c) -> u64`.
/// # Safety
/// Context bound; Claim borrowed; out writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_rt_v2_claim_end(
    ctx: *mut crate::TaskCtx,
    out: *mut u64,
    c: *const u64,
) -> u8 {
    let claim = unsafe { c.read() };
    unsafe { word_call(ctx, out, SeamRequest::ClaimEnd { claim }) }
}

/// `claim.narrow(a, take c) -> Option[Claim of a]`: the claim transfers at entry.
/// # Safety
/// Context bound; Authority borrowed; the Claim word transfers; out writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_rt_v2_claim_narrow(
    ctx: *mut crate::TaskCtx,
    _destination: *mut u8,
    out: *mut WordOption,
    a: *const u64,
    c: *mut u64,
) -> u8 {
    let claim = unsafe { c.read() };
    // Erasing the transferred physical word makes accidental runtime reuse fail closed.
    unsafe { c.write(0) };
    let request = SeamRequest::ClaimNarrow {
        authority: unsafe { a.read() },
        claim,
    };
    unsafe { option_call(ctx, out, request) }
}

/// `authority.claim(a) -> Result[Claim of a, SeamError]`.
/// # Safety
/// Context bound; Authority borrowed; out writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_rt_v2_authority_claim(
    ctx: *mut crate::TaskCtx,
    _destination: *mut u8,
    out: *mut SeamResult,
    a: *const u64,
) -> u8 {
    unsafe { provider(ctx, out, a.read(), None, wire::Request::Claim) }
}

/// `authority.advance(a, mut c, from, take entry, at) -> Advanced`.
/// # Safety
/// Context bound; Authority, Claim and Level borrowed; the entry transfers one owner
/// credit; out writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_rt_v2_authority_advance(
    ctx: *mut crate::TaskCtx,
    _destination: *mut u8,
    out: *mut AdvancedResult,
    a: *const u64,
    c: *mut u64,
    from: u64,
    owner: *mut crate::Header,
    start: u64,
    len: u64,
    at: *const u64,
) -> u8 {
    let entry = unsafe { crate::io::slice_bytes(owner, start, len) }.to_vec();
    unsafe {
        crate::release::hirsel_rt_v2_release(ctx, owner, (*owner).index(), std::ptr::null(), 0, 0)
    };
    let request = SeamRequest::Provider {
        authority: unsafe { a.read() },
        claim: Some(unsafe { c.read() }),
        request: wire::Request::Advance {
            from,
            at: (unsafe { at.read() } % 256) as u8,
            entry,
        },
    };
    match unsafe {
        seam_call(ctx, request, |c| {
            let SeamCompletion::Outcome {
                outcome: Outcome::Advanced(a),
                ..
            } = c
            else {
                crate::fault("seam completion mismatch")
            };
            let mut out = AdvancedResult::default();
            let payload = std::ptr::addr_of_mut!(out.words).cast::<u8>();
            match a {
                Advanced::Advanced(p) => out.words[0] = *p,
                Advanced::Fenced => out.tag = 1,
                Advanced::Refused(m) => {
                    out.tag = 2;
                    write_medium(payload, m)
                }
                Advanced::Undecided(m) => {
                    out.tag = 3;
                    write_medium(payload, m)
                }
            }
            out
        })
    } {
        Some(r) => {
            unsafe { out.write(r) };
            crate::STATUS_OK
        }
        None => crate::STATUS_FAILURE,
    }
}

/// `authority.durable(a, mut c, at) -> Result[u64, SeamError]`.
/// # Safety
/// Context bound; Authority, Claim and Level borrowed; out writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_rt_v2_authority_durable(
    ctx: *mut crate::TaskCtx,
    _destination: *mut u8,
    out: *mut SeamResult,
    a: *const u64,
    c: *mut u64,
    at: *const u64,
) -> u8 {
    let request = wire::Request::Durable {
        at: (unsafe { at.read() } % 256) as u8,
    };
    unsafe { provider(ctx, out, a.read(), Some(c.read()), request) }
}

/// `authority.read(a, from, at, max) -> Result[List[Bytes], SeamError]`.
/// # Safety
/// Context bound; Authority and Level borrowed; out writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_rt_v2_authority_read(
    ctx: *mut crate::TaskCtx,
    _destination: *mut u8,
    out: *mut SeamResult,
    a: *const u64,
    from: u64,
    at: *const u64,
    max: u64,
) -> u8 {
    let request = wire::Request::Read {
        from,
        at: (unsafe { at.read() } % 256) as u8,
        max,
    };
    unsafe { provider(ctx, out, a.read(), None, request) }
}

/// `authority.get(a, name) -> Result[Option[Bytes], SeamError]`.
/// # Safety
/// Context bound; Authority borrowed; the name a live String slice; out writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_rt_v2_authority_get(
    ctx: *mut crate::TaskCtx,
    _destination: *mut u8,
    out: *mut SeamResult,
    a: *const u64,
    owner: *mut crate::Header,
    start: u64,
    len: u64,
) -> u8 {
    let request = wire::Request::Get {
        name: unsafe { text(owner, start, len) },
    };
    unsafe { provider(ctx, out, a.read(), None, request) }
}

/// `authority.put(a, name, take data) -> Result[(), SeamError]`.
/// # Safety
/// Context bound; Authority borrowed; the name a live String slice; the data transfers one
/// owner credit; out writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_rt_v2_authority_put(
    ctx: *mut crate::TaskCtx,
    _destination: *mut u8,
    out: *mut SeamResult,
    a: *const u64,
    name_owner: *mut crate::Header,
    name_start: u64,
    name_len: u64,
    owner: *mut crate::Header,
    start: u64,
    len: u64,
) -> u8 {
    let name = unsafe { text(name_owner, name_start, name_len) };
    let data = unsafe { crate::io::slice_bytes(owner, start, len) }.to_vec();
    unsafe {
        crate::release::hirsel_rt_v2_release(ctx, owner, (*owner).index(), std::ptr::null(), 0, 0)
    };
    unsafe { provider(ctx, out, a.read(), None, wire::Request::Put { name, data }) }
}

/// `authority.reclaim(a, mut c) -> Result[(), SeamError]`.
/// # Safety
/// Context bound; Authority and Claim borrowed; out writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_rt_v2_authority_reclaim(
    ctx: *mut crate::TaskCtx,
    _destination: *mut u8,
    out: *mut SeamResult,
    a: *const u64,
    c: *mut u64,
) -> u8 {
    unsafe { provider(ctx, out, a.read(), Some(c.read()), wire::Request::Reclaim) }
}

/// The tape form of a seam request and completion: one byte string each.
impl SeamRequest {
    pub(crate) fn to_bytes(&self) -> Vec<u8> {
        let mut w = Vec::new();
        let word = |w: &mut Vec<u8>, n: u64| w.extend_from_slice(&n.to_le_bytes());
        let text = |w: &mut Vec<u8>, t: &str| {
            w.extend_from_slice(&(t.len() as u64).to_le_bytes());
            w.extend_from_slice(t.as_bytes());
        };
        match self {
            SeamRequest::Project { io, name } => {
                w.push(0);
                word(&mut w, *io);
                text(&mut w, name);
            }
            SeamRequest::Levels { authority } => {
                w.push(1);
                word(&mut w, *authority);
            }
            SeamRequest::Level { authority, name } => {
                w.push(2);
                word(&mut w, *authority);
                text(&mut w, name);
            }
            SeamRequest::Floor { authority } => {
                w.push(3);
                word(&mut w, *authority);
            }
            SeamRequest::LevelName { level } => {
                w.push(4);
                word(&mut w, *level);
            }
            SeamRequest::LevelNarrow { authority, level } => {
                w.push(5);
                word(&mut w, *authority);
                word(&mut w, *level);
            }
            SeamRequest::MaxEntry { authority } => {
                w.push(6);
                word(&mut w, *authority);
            }
            SeamRequest::ClaimEnd { claim } => {
                w.push(7);
                word(&mut w, *claim);
            }
            SeamRequest::ClaimNarrow { authority, claim } => {
                w.push(8);
                word(&mut w, *authority);
                word(&mut w, *claim);
            }
            SeamRequest::Provider {
                authority,
                claim,
                request,
            } => {
                w.push(9);
                word(&mut w, *authority);
                match claim {
                    Some(c) => {
                        w.push(1);
                        word(&mut w, *c);
                    }
                    None => w.push(0),
                }
                w.extend_from_slice(&request.encode());
            }
        }
        w
    }

    pub(crate) fn from_bytes(b: &[u8]) -> Option<Self> {
        let mut r = Reader(b);
        let q = match r.u8()? {
            0 => SeamRequest::Project {
                io: r.u64()?,
                name: r.text()?,
            },
            1 => SeamRequest::Levels {
                authority: r.u64()?,
            },
            2 => SeamRequest::Level {
                authority: r.u64()?,
                name: r.text()?,
            },
            3 => SeamRequest::Floor {
                authority: r.u64()?,
            },
            4 => SeamRequest::LevelName { level: r.u64()? },
            5 => SeamRequest::LevelNarrow {
                authority: r.u64()?,
                level: r.u64()?,
            },
            6 => SeamRequest::MaxEntry {
                authority: r.u64()?,
            },
            7 => SeamRequest::ClaimEnd { claim: r.u64()? },
            8 => SeamRequest::ClaimNarrow {
                authority: r.u64()?,
                claim: r.u64()?,
            },
            9 => {
                let authority = r.u64()?;
                let claim = match r.u8()? {
                    0 => None,
                    1 => Some(r.u64()?),
                    _ => return None,
                };
                return Some(SeamRequest::Provider {
                    authority,
                    claim,
                    request: wire::Request::decode(r.0)?,
                });
            }
            _ => return None,
        };
        r.0.is_empty().then_some(q)
    }
}

impl SeamCompletion {
    pub(crate) fn to_bytes(&self) -> Vec<u8> {
        let mut w = Vec::new();
        let word = |w: &mut Vec<u8>, n: u64| w.extend_from_slice(&n.to_le_bytes());
        let text = |w: &mut Vec<u8>, t: &str| {
            w.extend_from_slice(&(t.len() as u64).to_le_bytes());
            w.extend_from_slice(t.as_bytes());
        };
        let option = |w: &mut Vec<u8>, o: &Option<u64>| match o {
            Some(n) => {
                w.push(1);
                w.extend_from_slice(&n.to_le_bytes());
            }
            None => w.push(0),
        };
        match self {
            SeamCompletion::Words(levels) => {
                w.push(0);
                word(&mut w, levels.len() as u64);
                for l in levels {
                    word(&mut w, *l);
                }
            }
            SeamCompletion::Name(n) => {
                w.push(1);
                text(&mut w, n);
            }
            SeamCompletion::Word(n) => {
                w.push(2);
                word(&mut w, *n);
            }
            SeamCompletion::Optional(o) => {
                w.push(3);
                option(&mut w, o);
            }
            SeamCompletion::Outcome { outcome, claim } => {
                w.push(4);
                option(&mut w, claim);
                w.extend_from_slice(&wire::encode(outcome));
            }
        }
        w
    }

    pub(crate) fn from_bytes(b: &[u8]) -> Option<Self> {
        let mut r = Reader(b);
        let option = |r: &mut Reader<'_>| match r.u8()? {
            0 => Some(None),
            1 => Some(Some(r.u64()?)),
            _ => None,
        };
        let c = match r.u8()? {
            0 => {
                let n = r.u64()?;
                let mut levels = Vec::new();
                for _ in 0..n {
                    levels.push(r.u64()?);
                }
                SeamCompletion::Words(levels)
            }
            1 => SeamCompletion::Name(r.text()?),
            2 => SeamCompletion::Word(r.u64()?),
            3 => SeamCompletion::Optional(option(&mut r)?),
            4 => {
                let claim = option(&mut r)?;
                return Some(SeamCompletion::Outcome {
                    outcome: wire::decode(r.0)?,
                    claim,
                });
            }
            _ => return None,
        };
        r.0.is_empty().then_some(c)
    }
}

struct Reader<'a>(&'a [u8]);

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        if self.0.len() < n {
            return None;
        }
        let (h, t) = self.0.split_at(n);
        self.0 = t;
        Some(h)
    }
    fn u8(&mut self) -> Option<u8> {
        Some(self.take(1)?[0])
    }
    fn u64(&mut self) -> Option<u64> {
        Some(u64::from_le_bytes(self.take(8)?.try_into().ok()?))
    }
    fn text(&mut self) -> Option<String> {
        let n = usize::try_from(self.u64()?).ok()?;
        String::from_utf8(self.take(n)?.to_vec()).ok()
    }
}
