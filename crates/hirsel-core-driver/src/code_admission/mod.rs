//! Bounded admission of attested release recipes on a host's control socket.

mod closure;
#[cfg(test)]
mod code_store;
mod exchange;
mod reservation;

use crate::code_store::CodeStore;
pub use crate::code_store::ObjectKind;
pub use exchange::{ControlConnection, send_release};
pub use reservation::AdmissionStore;

use exchange::Exchange;
use hirsel_core_ids::ContentHash;
use hirsel_core_ir::address::Group;
use hirsel_core_ir::decode::{DecodeBudget, DecodeError, DecodeLimit, DecodeLimits};
use hirsel_core_ir::normal::{self, Annex, DecodedBody, Slot};
use hirsel_core_ir::release_recipe::{Backend, ObjectRef, Recipe};
use hirsel_core_ir::{FuncId, Identity};
use reservation::{ListedObject, Listing, Reservation};
use std::collections::btree_map::Entry;
use std::fmt;
use std::fs::OpenOptions;
use std::io::{self, Read};
use std::path::PathBuf;
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ObjectKey {
    pub kind: ObjectKind,
    pub hash: ContentHash,
}

/// Limits for one admission. The store separately owns the host-wide staging limit.
pub struct AdmissionLimits {
    pub recipe_bytes: usize,
    pub closure_objects: usize,
    pub object_bytes: u64,
    pub decoded_bytes: usize,
    pub members: usize,
    pub edges: usize,
    pub nesting: usize,
    pub outstanding: usize,
    pub timeout: Duration,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Limit {
    RecipeBytes,
    ClosureObjects,
    ObjectBytes,
    StagingBytes,
    DecodeBytes,
    Members,
    Edges,
    Nesting,
    Outstanding,
    Deadline,
}

#[derive(Debug)]
pub enum AdmissionError {
    Hash(ContentHash),
    Missing(ContentHash),
    Extra(ContentHash),
    Unsolicited(ObjectKey),
    Malformed(&'static str),
    Binding(&'static str),
    Conflict(Box<Identity>),
    Unsupported(&'static str),
    Limit(Limit),
    Disconnected,
    Io(io::Error),
}

impl fmt::Display for AdmissionError {
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Hash(hash) => write!(out, "Hash({})", crate::call::hex(&hash.0)),
            Self::Missing(hash) => write!(out, "Missing({})", crate::call::hex(&hash.0)),
            Self::Extra(hash) => write!(out, "Extra({})", crate::call::hex(&hash.0)),
            Self::Unsolicited(key) => write!(
                out,
                "Unsolicited({:?} {})",
                key.kind,
                crate::call::hex(&key.hash.0)
            ),
            Self::Malformed(reason) => write!(out, "Malformed({reason})"),
            Self::Binding(reason) => write!(out, "Binding({reason})"),
            Self::Conflict(identity) => write!(out, "Conflict({identity:?})"),
            Self::Unsupported(reason) => write!(out, "Unsupported({reason})"),
            Self::Limit(limit) => write!(out, "Limit({limit:?})"),
            Self::Disconnected => out.write_str("admission peer disconnected"),
            Self::Io(error) => error.fmt(out),
        }
    }
}

impl std::error::Error for AdmissionError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            _ => None,
        }
    }
}

impl From<DecodeError> for AdmissionError {
    fn from(error: DecodeError) -> Self {
        match error {
            DecodeError::Malformed(reason) => Self::Malformed(reason),
            DecodeError::Unsupported(reason) => Self::Unsupported(reason),
            DecodeError::Limit(limit) => Self::Limit(match limit {
                DecodeLimit::Bytes => Limit::DecodeBytes,
                DecodeLimit::Members => Limit::Members,
                DecodeLimit::Slots => Limit::Edges,
                DecodeLimit::Depth => Limit::Nesting,
                DecodeLimit::Deadline => Limit::Deadline,
            }),
        }
    }
}

impl From<io::Error> for AdmissionError {
    fn from(error: io::Error) -> Self {
        match error.kind() {
            io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock => Self::Limit(Limit::Deadline),
            io::ErrorKind::UnexpectedEof
            | io::ErrorKind::BrokenPipe
            | io::ErrorKind::ConnectionReset => Self::Disconnected,
            _ => Self::Io(error),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EngineTag {
    Instruction(u8),
    Runtime(u8),
    Host(u8),
    Flow(u8),
    Terminator(u8),
}

/// The implementation table belongs to the engine installation, not the sender.
pub trait BackendSemantics {
    fn supports_target(&self, backend: Backend, target: &str) -> bool;
    fn supports_builtin(&self, backend: Backend, identity: &Identity, version: u32) -> bool;
    fn supports_tag(&self, backend: Backend, tag: EngineTag) -> bool;
}

/// The semantics implemented by this installation's two native engines.
pub struct NativeSemantics;

impl BackendSemantics for NativeSemantics {
    fn supports_target(&self, backend: Backend, target: &str) -> bool {
        match backend {
            Backend::Cranelift => crate::Backend::Cranelift,
            Backend::Llvm => crate::Backend::Llvm,
        }
        .supports_target(target)
    }

    fn supports_builtin(&self, backend: Backend, identity: &Identity, version: u32) -> bool {
        match backend {
            Backend::Cranelift | Backend::Llvm => {
                identity.dictionaries.is_empty()
                    && identity.closures.is_empty()
                    && hirsel_core_ir::address::builtin_semantics_number(&identity.definition)
                        == Some(version)
            }
        }
    }

    fn supports_tag(&self, backend: Backend, tag: EngineTag) -> bool {
        // Both engines implement these exact v1 operation tags.
        match backend {
            Backend::Cranelift | Backend::Llvm => match tag {
                EngineTag::Instruction(tag) => tag <= 43,
                EngineTag::Runtime(tag) => tag <= 37,
                EngineTag::Host(tag) => tag <= 31,
                EngineTag::Flow(tag) => tag <= 8,
                EngineTag::Terminator(tag) => tag <= 6,
            },
        }
    }
}

struct SlotContract {
    identity: Identity,
    signature: Option<ContentHash>,
}

struct Body {
    decoded: DecodedBody,
    annex: Annex,
    identity: Identity,
    signature: ContentHash,
    slots: Vec<SlotContract>,
}

impl Body {
    fn new(
        mut decoded: DecodedBody,
        annex: Annex,
        budget: &mut DecodeBudget,
    ) -> Result<Self, AdmissionError> {
        let identity = decoded.identity(FuncId(0), budget)?;
        let signature = decoded.signature(FuncId(0), budget)?;
        charge_rows::<SlotContract>(budget, decoded.slots.len())?;
        let mut slots = Vec::with_capacity(decoded.slots.len());
        for index in 0..decoded.slots.len() {
            let identity = decoded.slot_identity(index, budget)?;
            let signature = match decoded.slots[index] {
                Slot::External(function) => Some(decoded.signature(function, budget)?),
                Slot::Builtin(_) => None,
            };
            slots.push(SlotContract {
                identity,
                signature,
            });
        }
        Ok(Self {
            decoded,
            annex,
            identity,
            signature,
            slots,
        })
    }
}

/// A checked recipe and the verified inputs to assembly.
pub struct VerifiedRelease {
    root: ContentHash,
    recipe_bytes: Vec<u8>,
    recipe: Recipe,
    groups: Vec<Group>,
    bodies: Vec<Body>,
}

pub type VerifiedParts = (ContentHash, Recipe, Vec<Group>, Vec<(DecodedBody, Annex)>);

impl VerifiedRelease {
    pub fn root(&self) -> ContentHash {
        self.root
    }

    pub fn recipe_bytes(&self) -> &[u8] {
        &self.recipe_bytes
    }

    pub fn recipe(&self) -> &Recipe {
        &self.recipe
    }

    /// Groups in recipe order.
    pub fn groups(&self) -> &[Group] {
        &self.groups
    }

    /// Bodies with their annexes in recipe order.
    pub fn bodies(&self) -> impl ExactSizeIterator<Item = (&DecodedBody, &Annex)> {
        self.bodies.iter().map(|body| (&body.decoded, &body.annex))
    }

    pub fn into_parts(self) -> VerifiedParts {
        let bodies = self
            .bodies
            .into_iter()
            .map(|body| (body.decoded, body.annex))
            .collect();
        (self.root, self.recipe, self.groups, bodies)
    }
}

/// The verified inputs and their collection and staging leases.
pub struct AdmittedRelease<'a> {
    release: VerifiedRelease,
    lease: ReleaseLease<'a>,
}

impl<'a> AdmittedRelease<'a> {
    pub fn release(&self) -> &VerifiedRelease {
        &self.release
    }

    /// Move the verified inputs into assembly while retaining the host's leases.
    pub fn into_parts(self) -> (VerifiedRelease, ReleaseLease<'a>) {
        (self.release, self.lease)
    }

    pub fn staged_objects(&self) -> impl Iterator<Item = (ObjectKey, PathBuf)> + '_ {
        self.lease.staged_objects()
    }
}

/// Keeps the staged files and collection pins until the host has published its
/// durable roots. Dropping the lease frees the pins and discards staging.
pub struct ReleaseLease<'a>(Reservation<'a>);

impl ReleaseLease<'_> {
    /// The private files containing verified objects received in this attempt.
    pub fn staged_objects(&self) -> impl Iterator<Item = (ObjectKey, PathBuf)> + '_ {
        self.0
            .objects
            .iter()
            .filter(|(_, row)| row.received)
            .map(|(&key, _)| (key, self.0.staged_path(key).expect("received in staging")))
    }
}

/// Verify the closure of a recipe explicitly selected by a local build caller.
/// A local build reads no socket and owns the verified inputs it assembles.
pub fn admit_local(
    store: &CodeStore,
    root: ContentHash,
    limits: &AdmissionLimits,
    semantics: &impl BackendSemantics,
) -> Result<VerifiedRelease, AdmissionError> {
    let deadline = deadline(limits)?;
    let mut budget = budget(limits, deadline);
    let bytes = read_recipe(store, root, limits.recipe_bytes, &budget)?;
    let recipe = Recipe::decode(&bytes, &mut budget)?;
    listed_objects(&recipe, limits, &mut budget)?;
    verify_release(
        root,
        bytes,
        recipe,
        &mut budget,
        semantics,
        |kind, object, budget| {
            let file = open_object(store, kind, object.hash)?;
            read_verified(file, kind, object, budget)
        },
    )
}

impl AdmissionStore {
    /// Admit the complete recipe attested by the caller on the host control
    /// socket. The returned release can proceed to assembly.
    pub fn admit(
        &self,
        connection: &mut ControlConnection,
        limits: &AdmissionLimits,
        semantics: &impl BackendSemantics,
    ) -> Result<AdmittedRelease<'_>, AdmissionError> {
        let deadline = deadline(limits)?;
        let mut exchange = Exchange::new(connection, deadline);
        let (root, recipe_bytes) = exchange.recipe(limits.recipe_bytes)?;
        check_hash(Recipe::hash(&recipe_bytes), root)?;

        let mut budget = budget(limits, deadline);
        let recipe = Recipe::decode(&recipe_bytes, &mut budget)?;
        let objects = listed_objects(&recipe, limits, &mut budget)?;
        let mut reservation = self.reserve(root, objects, limits.outstanding, &mut budget)?;
        transfer(
            &mut exchange,
            &mut reservation,
            limits.object_bytes,
            &budget,
        )?;

        let release = verify_release(
            root,
            recipe_bytes,
            recipe,
            &mut budget,
            semantics,
            |kind, object, budget| {
                let key = ObjectKey {
                    kind,
                    hash: object.hash,
                };
                let file = reservation
                    .open(key)
                    .map_err(|error| object_error(key.hash, error))?;
                read_verified(file, kind, object, budget)
            },
        )?;
        Ok(AdmittedRelease {
            release,
            lease: ReleaseLease(reservation),
        })
    }
}

fn verify_release(
    root: ContentHash,
    recipe_bytes: Vec<u8>,
    recipe: Recipe,
    budget: &mut DecodeBudget,
    semantics: &impl BackendSemantics,
    mut load: impl FnMut(ObjectKind, &ObjectRef, &DecodeBudget) -> Result<Vec<u8>, AdmissionError>,
) -> Result<VerifiedRelease, AdmissionError> {
    let groups = decode_rows(&recipe.groups, budget, |object, budget| {
        let raw = load(ObjectKind::Group, object, budget)?;
        Ok(Group::decode(&raw, budget)?)
    })?;
    let bodies = decode_rows(&recipe.bodies, budget, |pair, budget| {
        let raw = load(ObjectKind::Body, &pair.body, budget)?;
        let body = normal::decode(&raw, budget)?;
        drop(raw);
        let raw = load(ObjectKind::Annex, &pair.annex, budget)?;
        let annex = Annex::decode(&raw, &body.module, budget)?;
        Body::new(body, annex, budget)
    })?;
    closure::check(&recipe, &groups, &bodies, semantics, budget)?;
    budget.check()?;
    Ok(VerifiedRelease {
        root,
        recipe_bytes,
        recipe,
        groups,
        bodies,
    })
}

fn deadline(limits: &AdmissionLimits) -> Result<Instant, AdmissionError> {
    Instant::now()
        .checked_add(limits.timeout)
        .ok_or(AdmissionError::Limit(Limit::Deadline))
}

fn budget(limits: &AdmissionLimits, deadline: Instant) -> DecodeBudget {
    DecodeBudget::new(DecodeLimits {
        bytes: limits.decoded_bytes,
        members: limits.members,
        slots: limits.edges,
        depth: limits.nesting,
        deadline: Some(deadline),
    })
}

fn listed_objects(
    recipe: &Recipe,
    limits: &AdmissionLimits,
    budget: &mut DecodeBudget,
) -> Result<Listing, AdmissionError> {
    let count = recipe
        .bodies
        .len()
        .checked_mul(2)
        .and_then(|n| n.checked_add(recipe.groups.len()))
        .filter(|&n| n <= limits.closure_objects)
        .ok_or(AdmissionError::Limit(Limit::ClosureObjects))?;
    if recipe.groups.len() > limits.members {
        return Err(AdmissionError::Limit(Limit::Members));
    }
    charge_rows::<(ObjectKey, ListedObject)>(budget, count.saturating_mul(4))?;
    let mut objects = Listing::new();
    let mut total = 0_u64;
    let rows = recipe
        .groups
        .iter()
        .map(|object| (ObjectKind::Group, object))
        .chain(recipe.bodies.iter().flat_map(|row| {
            [
                (ObjectKind::Body, &row.body),
                (ObjectKind::Annex, &row.annex),
            ]
        }));
    for (kind, row) in rows {
        budget.check()?;
        let key = ObjectKey {
            kind,
            hash: row.hash,
        };
        match objects.entry(key) {
            Entry::Occupied(entry) if entry.get().len != row.len => {
                return Err(AdmissionError::Malformed(
                    "inconsistent lengths for an annex",
                ));
            }
            Entry::Occupied(_) => {}
            Entry::Vacant(entry) => {
                total = total
                    .checked_add(row.len)
                    .filter(|&total| total <= limits.object_bytes)
                    .ok_or(AdmissionError::Limit(Limit::ObjectBytes))?;
                entry.insert(ListedObject::new(row.len));
            }
        }
    }
    Ok(objects)
}

fn transfer(
    exchange: &mut Exchange<'_>,
    reservation: &mut Reservation<'_>,
    max_bytes: u64,
    budget: &DecodeBudget,
) -> Result<(), AdmissionError> {
    let needs = reservation
        .objects
        .iter()
        .filter_map(|(&key, row)| row.needed().then_some(key));
    exchange.need(needs.clone().count(), needs)?;
    while let Some(key) = exchange.next_key()? {
        let len = reservation
            .objects
            .get(&key)
            .filter(|row| row.needed())
            .ok_or(AdmissionError::Unsolicited(key))?
            .len;
        let frame_len = exchange.length()?;
        if frame_len > len {
            return Err(AdmissionError::Limit(Limit::ObjectBytes));
        }
        if frame_len != len {
            return Err(AdmissionError::Malformed(
                "object frame length differs from the recipe",
            ));
        }
        let path = reservation
            .staged_path(key)
            .expect("needed object has staging");
        let mut file = OpenOptions::new().write(true).create_new(true).open(path)?;
        exchange.object(key, len, max_bytes, &mut file)?;
        reservation
            .objects
            .get_mut(&key)
            .expect("listed object")
            .received = true;
    }
    if let Some((key, _)) = reservation.objects.iter().find(|(_, row)| row.needed()) {
        return Err(AdmissionError::Missing(key.hash));
    }
    budget.check()?;
    Ok(())
}

fn decode_rows<T, U>(
    rows: &[T],
    budget: &mut DecodeBudget,
    mut decode: impl FnMut(&T, &mut DecodeBudget) -> Result<U, AdmissionError>,
) -> Result<Vec<U>, AdmissionError> {
    charge_rows::<U>(budget, rows.len())?;
    let mut decoded = Vec::with_capacity(rows.len());
    for row in rows {
        budget.check()?;
        decoded.push(decode(row, budget)?);
    }
    Ok(decoded)
}

fn read_recipe(
    store: &CodeStore,
    root: ContentHash,
    max_bytes: usize,
    budget: &DecodeBudget,
) -> Result<Vec<u8>, AdmissionError> {
    let file = open_object(store, ObjectKind::Recipe, root)?;
    let len = file.metadata().map_err(AdmissionError::Io)?.len();
    if len > max_bytes as u64 {
        return Err(AdmissionError::Limit(Limit::RecipeBytes));
    }
    read_verified(
        file,
        ObjectKind::Recipe,
        &ObjectRef { hash: root, len },
        budget,
    )
}

fn open_object(
    store: &CodeStore,
    kind: ObjectKind,
    hash: ContentHash,
) -> Result<std::fs::File, AdmissionError> {
    store
        .open(kind, hash)
        .map_err(|error| object_error(hash, error))
}

fn object_error(hash: ContentHash, error: io::Error) -> AdmissionError {
    match error.kind() {
        io::ErrorKind::NotFound => AdmissionError::Missing(hash),
        _ => AdmissionError::Io(error),
    }
}

fn read_verified(
    mut file: std::fs::File,
    kind: ObjectKind,
    object: &ObjectRef,
    budget: &DecodeBudget,
) -> Result<Vec<u8>, AdmissionError> {
    if file.metadata().map_err(AdmissionError::Io)?.len() != object.len {
        return Err(AdmissionError::Hash(object.hash));
    }
    let length =
        usize::try_from(object.len).map_err(|_| AdmissionError::Limit(Limit::ObjectBytes))?;
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(length)
        .map_err(|_| AdmissionError::Limit(Limit::ObjectBytes))?;
    let mut scratch = [0; 8192];
    while bytes.len() < length {
        budget.check()?;
        let size = (length - bytes.len()).min(scratch.len());
        let n = file
            .read(&mut scratch[..size])
            .map_err(AdmissionError::Io)?;
        if n == 0 {
            return Err(AdmissionError::Hash(object.hash));
        }
        bytes.extend_from_slice(&scratch[..n]);
    }
    if file.read(&mut [0; 1]).map_err(AdmissionError::Io)? != 0 {
        return Err(AdmissionError::Hash(object.hash));
    }
    check_hash(kind.hash(&bytes), object.hash)?;
    budget.check()?;
    Ok(bytes)
}

fn check_hash(actual: ContentHash, expected: ContentHash) -> Result<(), AdmissionError> {
    if actual != expected {
        return Err(AdmissionError::Hash(expected));
    }
    Ok(())
}

fn charge_rows<T>(budget: &mut DecodeBudget, count: usize) -> Result<(), AdmissionError> {
    let size = count
        .checked_mul(size_of::<T>())
        .ok_or(AdmissionError::Limit(Limit::DecodeBytes))?;
    budget.charge_bytes(size)?;
    Ok(())
}
