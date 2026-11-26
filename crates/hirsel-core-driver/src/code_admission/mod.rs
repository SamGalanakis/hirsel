//! Bounded admission of attested release recipes on a host's control socket.

mod closure;
#[cfg(test)]
pub(crate) mod code_store;
mod exchange;
#[cfg(test)]
mod operation_tags;
mod reservation;

pub use crate::code_store::ObjectKind;
use crate::code_store::{CodeStore, Object};
pub use exchange::{ControlConnection, send_release};
pub use reservation::AdmissionStore;

use exchange::Exchange;
use hirsel_core_ids::ContentHash;
use hirsel_core_ir::address::Group;
use hirsel_core_ir::decode::{Debit, DecodeBudget, DecodeError, DecodeLimit, DecodeLimits};
pub use hirsel_core_ir::normal::EngineTag;
use hirsel_core_ir::normal::{self, Annex, DecodedBody, Slot};
use hirsel_core_ir::release_recipe::{Backend, ObjectRef, Recipe};
use hirsel_core_ir::{FuncId, Identity};
use reservation::{ListedObject, Listing, Reservation};
use std::collections::btree_map::Entry;
use std::fmt;
use std::fs::OpenOptions;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
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
    /// Required for the socket exchange. Local verification has no deadline.
    pub timeout: Option<Duration>,
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
    Refused(String),
    RecordingForeign(String),
    Io(io::Error),
}

impl fmt::Display for AdmissionError {
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Hash(hash) => write!(out, "Hash({hash})"),
            Self::Missing(hash) => write!(out, "Missing({hash})"),
            Self::Extra(hash) => write!(out, "Extra({hash})"),
            Self::Unsolicited(key) => {
                write!(out, "Unsolicited({:?} {})", key.kind, key.hash)
            }
            Self::Malformed(reason) => write!(out, "Malformed({reason})"),
            Self::Binding(reason) => write!(out, "Binding({reason})"),
            Self::Conflict(identity) => write!(out, "Conflict({identity:?})"),
            Self::Unsupported(reason) => write!(out, "Unsupported({reason})"),
            Self::Limit(limit) => write!(out, "Limit({limit:?})"),
            Self::Disconnected => out.write_str("admission peer disconnected"),
            Self::Refused(reason) | Self::RecordingForeign(reason) => out.write_str(reason),
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
        match backend {
            Backend::Cranelift | Backend::Llvm => tag.is_defined(),
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
        budget: &Debit<'_>,
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
    pub(crate) link_archives: crate::foreign_archive::LinkArchives,
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
    pub(crate) exec_lines: String,
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

    pub(crate) fn publish(&self, store: &CodeStore, recipe: &[u8]) -> io::Result<()> {
        for (key, path) in self.staged_objects() {
            store.publish_staged(key.kind, key.hash, &path)?;
        }
        store.put(ObjectKind::Recipe, recipe)?;
        Ok(())
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
    let budget = budget(limits, None);
    let held = budget.debit();
    let bytes = read_recipe(store, root, limits.recipe_bytes, &held)?;
    let recipe = Recipe::decode(&bytes, &held)?;
    listed_objects(&recipe, limits, &held.scratch())?;
    verify_release(
        root,
        bytes,
        recipe,
        &held,
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
        self.admit_for_runtime(connection, limits, semantics, false, None, None, None)
    }

    pub(crate) fn admit_for_runtime(
        &self,
        connection: &mut ControlConnection,
        limits: &AdmissionLimits,
        semantics: &impl BackendSemantics,
        recording: bool,
        approved_foreign: Option<&std::collections::BTreeSet<String>>,
        approved_exec: Option<(&std::collections::BTreeSet<String>, &Path)>,
        attested: Option<ContentHash>,
    ) -> Result<AdmittedRelease<'_>, AdmissionError> {
        let deadline = deadline(limits)?;
        let mut exchange = Exchange::new(connection, deadline);
        let (root, recipe_bytes) = exchange.recipe(limits.recipe_bytes)?;
        check_hash(Recipe::hash(&recipe_bytes), root)?;
        if attested.is_some_and(|attested| attested != root) {
            return Err(AdmissionError::Malformed(
                "the recipe is not the attested root",
            ));
        }

        let budget = budget(limits, Some(deadline));
        let held = budget.debit();
        let recipe = Recipe::decode(&recipe_bytes, &held)?;
        if recording
            && let Some(reason) = crate::native::recipe_build::foreign_recording_refusal(&recipe)
        {
            return Err(AdmissionError::RecordingForeign(reason));
        }
        if let Some(approved) = approved_foreign
            && let Some(reason) =
                crate::native::recipe_build::foreign_grant_refusal(&recipe, approved)
        {
            return Err(AdmissionError::Refused(reason));
        }
        let exec_lines = if let Some((approved, workdir)) = approved_exec {
            if let Some(reason) = crate::exec_launch::refuse_missing(&recipe, approved) {
                return Err(AdmissionError::Refused(reason));
            }
            crate::exec_launch::approval_lines(&recipe, workdir).map_err(AdmissionError::Refused)?
        } else {
            String::new()
        };
        let objects = listed_objects(&recipe, limits, &held)?;
        let mut reservation = self.reserve(root, objects, limits.outstanding, &held)?;
        transfer(&mut exchange, &mut reservation, limits.object_bytes, &held)?;

        let release = verify_release(
            root,
            recipe_bytes,
            recipe,
            &held,
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
            exec_lines,
            release,
            lease: ReleaseLease(reservation),
        })
    }
}

fn verify_release(
    root: ContentHash,
    recipe_bytes: Vec<u8>,
    recipe: Recipe,
    budget: &Debit<'_>,
    semantics: &impl BackendSemantics,
    mut load: impl FnMut(ObjectKind, &ObjectRef, &Debit<'_>) -> Result<Vec<u8>, AdmissionError>,
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
    if recipe.has_foreign() {
        check_foreign_archive_inventory(&recipe, &budget.scratch())?;
    }
    closure::check(&recipe, &groups, &bodies, semantics, &budget.scratch())?;
    let values = decode_rows(&recipe.values, budget, |object, budget| {
        let raw = load(ObjectKind::Value, object, budget)?;
        Ok(hirsel_core_ir::static_data::decode_value_object(
            &raw, budget,
        )?)
    })?;
    let mut bodies = bodies;
    bind_constant_slots(&recipe, &groups, &mut bodies, values, budget)?;
    let mut link_archives = crate::foreign_archive::LinkArchives::default();
    if recipe.has_foreign() {
        charge_rows::<crate::foreign_archive::ArchiveOrigin<'_>>(
            budget,
            recipe.foreign_modules.len(),
        )?;
        for leaf in &recipe.foreign_modules {
            let target = &recipe.launch.target;
            if let Some(module) = leaf.wasm() {
                let bytes = load(ObjectKind::ForeignArchive, &module, budget)?;
                let artifact = crate::foreign_archive::contained::compile_bytes(
                    leaf,
                    module.hash.0,
                    &module.hash.to_string(),
                    &bytes,
                    target,
                )?;
                link_archives.contain(module.hash, artifact);
                continue;
            }
            let row = leaf
                .targets()
                .binary_search_by(|row| row.target.cmp(target))
                .expect("the checked archive inventory selects a row for each leaf");
            let archive = leaf.targets()[row].archive;
            link_archives.admit(archive, leaf, target, None, budget, || {
                load(ObjectKind::ForeignArchive, &archive, budget)
            })?;
        }
        link_archives.definitions(&budget.scratch())?;
        for archive in &recipe.foreign_archives {
            if !link_archives.contains(archive.hash) {
                load(ObjectKind::ForeignArchive, archive, budget)?;
            }
        }
    }
    budget.check()?;
    Ok(VerifiedRelease {
        root,
        recipe_bytes,
        recipe,
        groups,
        bodies,
        link_archives,
    })
}

/// Bind every constant slot of the closure to its value (PROG-10), so no
/// verified body holds an unbound computed constant: a member's group binds
/// each slot `Value(h)`, and the value object whose claim is `h` must have the
/// slot's type and hash to `h` under it. A value no slot binds is `Extra`, a
/// slot whose value is not listed `Missing`.
fn bind_constant_slots(
    recipe: &Recipe,
    groups: &[Group],
    bodies: &mut [Body],
    values: Vec<(ContentHash, hirsel_core_ir::StaticValue)>,
    budget: &Debit<'_>,
) -> Result<(), AdmissionError> {
    use hirsel_core_ir::static_data::{fits, value_closure, value_hash};
    use hirsel_core_ir::{Static, StaticValue};
    use std::collections::BTreeMap;
    let mut by_hash: BTreeMap<ContentHash, (StaticValue, bool)> = BTreeMap::new();
    for (hash, value) in values {
        if by_hash.insert(hash, (value, false)).is_some() {
            return Err(AdmissionError::Malformed(
                "two value objects claim one value",
            ));
        }
    }
    for member in groups.iter().flat_map(|group| &group.members) {
        budget.check()?;
        let at = recipe
            .bodies
            .binary_search_by_key(&member.body, |row| row.body.hash)
            .map_err(|_| AdmissionError::Missing(member.body))?;
        let decoded = &mut bodies[at].decoded;
        if decoded.constant_slots.is_empty() {
            continue;
        }
        let function_slots = decoded.slots.len();
        let module = &mut decoded.module;
        let recipes = hirsel_core_layout::compute(&module.types)
            .map_err(|_| AdmissionError::Malformed("a constant slot's type has no layout"))?;
        for (path, binding) in decoded
            .constant_slots
            .iter()
            .zip(member.slots.get(function_slots..).unwrap_or_default())
        {
            let hirsel_core_ir::address::Binding::Value(hash) = *binding else {
                return Err(AdmissionError::Binding(
                    "a value binds exactly the constant slots",
                ));
            };
            let (value, used) = by_hash
                .get_mut(&hash)
                .ok_or(AdmissionError::Missing(hash))?;
            *used = true;
            for row in 0..module.statics.len() {
                let Static::Computed { ty, constant } = &module.statics[row] else {
                    continue;
                };
                if constant != path {
                    continue;
                }
                let closure = value_closure(module, ty).ok_or(AdmissionError::Malformed(
                    "a constant slot's type holds no value",
                ))?;
                if !fits(ty, value, &module.types, &recipes)
                    || value_hash(closure, ty, value, &module.types, &recipes) != hash
                {
                    return Err(AdmissionError::Binding(
                        "a value object differs from the value its slot binds",
                    ));
                }
                module.statics[row] = hirsel_core_ir::static_data::bound_static(ty, value.clone());
            }
        }
    }
    if let Some((&hash, _)) = by_hash.iter().find(|(_, (_, used))| !used) {
        return Err(AdmissionError::Extra(hash));
    }
    Ok(())
}

fn check_foreign_archive_inventory(
    recipe: &Recipe,
    budget: &Debit<'_>,
) -> Result<(), AdmissionError> {
    charge_rows::<bool>(budget, recipe.foreign_archives.len())?;
    let mut selected = vec![false; recipe.foreign_archives.len()];
    let mut visit = |archive: ObjectRef| {
        budget.check()?;
        let at = recipe
            .foreign_archives
            .binary_search_by_key(&archive.hash, |row| row.hash)
            .map_err(|_| AdmissionError::Missing(archive.hash))?;
        if recipe.foreign_archives[at].len != archive.len {
            return Err(AdmissionError::Binding(
                "inconsistent lengths for a foreign archive",
            ));
        }
        selected[at] = true;
        Ok(())
    };
    for leaf in &recipe.foreign_modules {
        if let Some(module) = leaf.wasm() {
            visit(module)?;
            continue;
        }
        let target = &recipe.launch.target;
        let i = leaf
            .targets()
            .binary_search_by(|row| row.target.cmp(target))
            .map_err(|_| {
                AdmissionError::Refused(format!("Missing(archive of {} for {target})", leaf.module))
            })?;
        visit(leaf.targets()[i].archive)?;
    }
    for grant in &recipe.launch.grants {
        if let Some(archive) = grant.archive {
            visit(archive)?;
        }
    }
    match recipe
        .foreign_archives
        .iter()
        .zip(selected)
        .find(|(_, selected)| !selected)
    {
        Some((extra, _)) => Err(AdmissionError::Extra(extra.hash)),
        None => Ok(()),
    }
}

fn deadline(limits: &AdmissionLimits) -> Result<Instant, AdmissionError> {
    limits
        .timeout
        .and_then(|timeout| Instant::now().checked_add(timeout))
        .ok_or(AdmissionError::Limit(Limit::Deadline))
}

fn budget(limits: &AdmissionLimits, deadline: Option<Instant>) -> DecodeBudget {
    DecodeBudget::new(DecodeLimits {
        bytes: limits.decoded_bytes,
        members: limits.members,
        slots: limits.edges,
        depth: limits.nesting,
        deadline,
    })
}

fn listed_objects(
    recipe: &Recipe,
    limits: &AdmissionLimits,
    budget: &Debit<'_>,
) -> Result<Listing, AdmissionError> {
    let count = recipe
        .bodies
        .len()
        .checked_mul(2)
        .and_then(|n| n.checked_add(recipe.groups.len()))
        .and_then(|n| n.checked_add(recipe.values.len()))
        .and_then(|n| n.checked_add(recipe.foreign_archives.len()))
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
        }))
        .chain(
            recipe
                .values
                .iter()
                .map(|object| (ObjectKind::Value, object)),
        )
        .chain(
            recipe
                .foreign_archives
                .iter()
                .map(|object| (ObjectKind::ForeignArchive, object)),
        );
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
    budget: &Debit<'_>,
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
    budget: &Debit<'_>,
    mut decode: impl FnMut(&T, &Debit<'_>) -> Result<U, AdmissionError>,
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
    budget: &Debit<'_>,
) -> Result<Vec<u8>, AdmissionError> {
    let file = open_object(store, ObjectKind::Recipe, root)?;
    let len = file.len();
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
) -> Result<Object, AdmissionError> {
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
    mut file: Object,
    kind: ObjectKind,
    object: &ObjectRef,
    budget: &Debit<'_>,
) -> Result<Vec<u8>, AdmissionError> {
    if file.len() != object.len {
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

fn charge_rows<T>(budget: &Debit<'_>, count: usize) -> Result<(), AdmissionError> {
    let size = count
        .checked_mul(size_of::<T>())
        .ok_or(AdmissionError::Limit(Limit::DecodeBytes))?;
    budget.charge_bytes(size)?;
    Ok(())
}
