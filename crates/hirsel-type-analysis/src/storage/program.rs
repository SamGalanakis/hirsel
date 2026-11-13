//! Storage equations over a compact executable graph. Both checkers construct
//! their own graph; neither graph is evidence for the other's verdict.
//!
//! This pass retains placement independently of access loans. In particular a
//! deep unique copy still allocates in the operand's storage. Explicit detached
//! copies and independently proved handle boundaries have no such dependency.
use super::{StorageDomain, StorageSolveError, fixed_point};
use hirsel_syntax::{DefId, NameSite};
use hirsel_types::storage::graph::*;
use hirsel_types::storage::{
    StorageBehavior, StorageCallable, StorageInput, StorageResult, StorageSelector,
};
use hirsel_types::{
    ActualBindings, GenericArgument, GenericParameter, OwnedTypePart, ParamId, ParameterRef,
    SharedPath, TypeInterner, TypeMemo,
};
use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, BTreeSet, VecDeque};

type Path = SharedPath<OwnedTypePart>;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum StorageProgramError {
    Domain(StorageSolveError),
    DuplicateCallable(StorageCallable),
    InvalidEntry(StorageCallable),
    InvalidSuccessor {
        callable: StorageCallable,
        node: usize,
    },
    GrowingActuals,
    DuplicateInput(StorageCallable),
    InvalidInput(StorageCallable),
}
impl From<StorageSolveError> for StorageProgramError {
    fn from(value: StorageSolveError) -> Self {
        Self::Domain(value)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum StorageReadFailure {
    UnknownCallback,
    AllocatesFromRoot(StorageSelector),
    ReturnsRoot(StorageResult),
}

/// A source operation or executable node, never part of a semantic profile.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StorageReadViolation {
    pub callable: StorageCallable,
    pub node: usize,
    pub failure: StorageReadFailure,
    pub origin: Option<StorageOperation>,
}

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub struct StorageOperation {
    pub callable: StorageCallable,
    pub node: usize,
}

#[derive(Clone, Debug)]
pub struct StorageProgramProof {
    /// Correspond to the supplied bodies, not to producer-supplied profiles.
    pub behaviors: Vec<StorageBehavior>,
    pub violations: Vec<StorageReadViolation>,
    pub demanded: BTreeSet<StorageCallable>,
}

/// Identity relations can refine a projected input selector. Result bounds
/// cannot: e.g. a map lookup's Option payload retains the map's placement; its
/// variant index is not an index into the input map's source identity.
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
struct Dependency {
    parts: Path,
    source: StorageSelector,
    identity: bool,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Ord, PartialOrd)]
struct Payload {
    dependencies: BTreeSet<Dependency>,
    unknown: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
struct Code {
    callable: StorageCallable,
    actuals: usize,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
enum CaptureTarget {
    Body(Code),
    IntegerOrder(DefId),
}
type CaptureCode = Vec<BTreeMap<Path, BTreeSet<CaptureTarget>>>;

/// Finite original producer equations. References are actual callable IDs,
/// never recursively embedded environments; a recursive wrapper adds an edge.
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
struct ProducerCaptures {
    captures: Vec<Payload>,
    code: CaptureCode,
    unknown: bool,
}
type Producers = BTreeMap<Code, ProducerCaptures>;

fn join_producers(current: &mut Producers, next: &Producers) {
    for (callable, incoming) in next {
        let Some(existing) = current.get_mut(callable) else {
            current.insert(*callable, incoming.clone());
            continue;
        };
        existing.unknown |= incoming.unknown;
        if existing.captures.len() != incoming.captures.len()
            || existing.code.len() != incoming.code.len()
        {
            existing.unknown = true;
        }
        for (left, right) in existing.captures.iter_mut().zip(&incoming.captures) {
            left.unknown |= right.unknown;
            left.dependencies.extend(right.dependencies.iter().cloned());
        }
        for (left, right) in existing.code.iter_mut().zip(&incoming.code) {
            for (path, targets) in right {
                left.entry(path.clone())
                    .or_default()
                    .extend(targets.iter().copied());
            }
        }
    }
}

fn rebase_payload(capture: &Payload, selector: &StorageSelector) -> Payload {
    Payload {
        unknown: capture.unknown,
        dependencies: capture
            .dependencies
            .iter()
            .map(|dependency| Dependency {
                parts: dependency.parts.clone(),
                source: selector.clone(),
                identity: false,
            })
            .collect(),
    }
}

fn map_producers(producers: &Producers, map: impl Fn(&Payload) -> Payload) -> Producers {
    producers
        .iter()
        .map(|(callable, producer)| {
            (
                *callable,
                ProducerCaptures {
                    captures: producer.captures.iter().map(&map).collect(),
                    code: producer.code.clone(),
                    unknown: producer.unknown,
                },
            )
        })
        .collect()
}

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
enum Target {
    Input(StorageSelector),
    IntegerOrder(DefId),
    Body {
        callable: Code,
        captures: Vec<Payload>,
        capture_code: CaptureCode,
        producers: Producers,
        /// Missing or erased actual producer evidence cannot supply a proof.
        unknown_captures: bool,
    },
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct Value {
    payload: Payload,
    targets: BTreeMap<Path, BTreeSet<Target>>,
    sealed: BTreeMap<Path, BTreeSet<SealedRoot>>,
}
/// Private abstract producer facts, never a runtime receipt or a second
/// environment layout. Targets still name original code and capture operands.
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
struct SealedRoot {
    targets: BTreeMap<Path, BTreeSet<Target>>,
    unknown: bool,
}
impl Value {
    fn unknown() -> Self {
        Self {
            payload: Payload {
                unknown: true,
                ..Payload::default()
            },
            ..Self::default()
        }
    }
    fn join(&mut self, other: &Self) -> bool {
        let previous = self.clone();
        self.payload.unknown |= other.payload.unknown;
        self.payload
            .dependencies
            .extend(other.payload.dependencies.iter().cloned());
        for (path, targets) in &other.targets {
            self.targets
                .entry(path.clone())
                .or_default()
                .extend(targets.iter().cloned());
        }
        for (path, roots) in &other.sealed {
            self.sealed
                .entry(path.clone())
                .or_default()
                .extend(roots.iter().cloned());
        }
        *self != previous
    }
    fn project(&self, path: &Path) -> Self {
        let mut value = Self::default();
        value.payload.unknown = self.payload.unknown;
        for dependency in &self.payload.dependencies {
            if dependency.parts.starts_with(path) {
                let mut dependency = dependency.clone();
                dependency.parts = dependency.parts.iter().skip(path.len()).cloned().collect();
                value.payload.dependencies.insert(dependency);
            } else if path.starts_with(&dependency.parts) {
                let mut dependency = dependency.clone();
                if dependency.identity {
                    dependency
                        .source
                        .parts
                        .extend(path.iter().skip(dependency.parts.len()).cloned());
                }
                dependency.parts.clear();
                value.payload.dependencies.insert(dependency);
            }
        }
        for (parts, targets) in &self.targets {
            if parts.starts_with(path) {
                let parts = parts.iter().skip(path.len()).cloned().collect();
                value
                    .targets
                    .entry(parts)
                    .or_default()
                    .extend(targets.iter().cloned());
            } else if path.starts_with(parts) {
                // A target at an aggregate prefix cannot be projected as a
                // callable child without actual structural evidence.
                value.payload.unknown = true;
            }
        }
        for (parts, roots) in &self.sealed {
            if parts.starts_with(path) {
                value
                    .sealed
                    .entry(parts.iter().skip(path.len()).cloned().collect())
                    .or_default()
                    .extend(roots.iter().cloned());
            } else if path.starts_with(parts) {
                value.payload.unknown = true;
            }
        }
        value
    }
    fn prefixed(&self, prefix: &Path) -> Self {
        let mut value = self.clone();
        value.payload.dependencies = self
            .payload
            .dependencies
            .iter()
            .map(|dependency| {
                let mut dependency = dependency.clone();
                dependency.parts = append(prefix, &dependency.parts);
                dependency
            })
            .collect();
        value.targets = self
            .targets
            .iter()
            .map(|(parts, targets)| (append(prefix, parts), targets.clone()))
            .collect();
        value.sealed = self
            .sealed
            .iter()
            .map(|(parts, roots)| (append(prefix, parts), roots.clone()))
            .collect();
        value
    }
    fn normalize(&mut self, domain: &StorageDomain) -> Result<(), StorageProgramError> {
        self.payload.dependencies = self
            .payload
            .dependencies
            .iter()
            .map(|dependency| {
                let mut dependency = dependency.clone();
                let source = domain.selector(&dependency.source)?;
                dependency.identity &= source == dependency.source;
                dependency.source = source;
                let parts = normalize_path(&domain.results, &dependency.parts)?;
                dependency.identity &= parts == dependency.parts;
                dependency.parts = parts;
                Ok(dependency)
            })
            .collect::<Result<_, StorageProgramError>>()?;
        // All code identities are finite. Capture placements use the same
        // finite caller domain rather than growing recursive projection paths.
        let mut targets_by_path: BTreeMap<Path, BTreeSet<Target>> = BTreeMap::new();
        for (path, targets) in &self.targets {
            if normalize_path(&domain.results, path)? != *path {
                self.payload.unknown = true;
                continue;
            }
            let targets: BTreeSet<Target> = targets
                .iter()
                .map(|target| match target {
                    Target::Input(selector) => {
                        if domain.selector(selector)? != *selector {
                            self.payload.unknown = true;
                        }
                        Ok(Target::Input(selector.clone()))
                    }
                    Target::IntegerOrder(definition) => Ok(Target::IntegerOrder(*definition)),
                    Target::Body {
                        callable,
                        captures,
                        capture_code,
                        producers,
                        unknown_captures,
                    } => {
                        let mut captures = captures.clone();
                        for capture in &mut captures {
                            let mut value = Self {
                                payload: capture.clone(),
                                ..Self::default()
                            };
                            value.normalize(domain)?;
                            *capture = value.payload;
                        }
                        let mut producers = producers.clone();
                        for producer in producers.values_mut() {
                            for payload in &mut producer.captures {
                                let mut value = Self {
                                    payload: payload.clone(),
                                    ..Self::default()
                                };
                                value.normalize(domain)?;
                                *payload = value.payload;
                            }
                        }
                        Ok(Target::Body {
                            callable: *callable,
                            captures,
                            capture_code: capture_code.clone(),
                            producers,
                            unknown_captures: *unknown_captures,
                        })
                    }
                })
                .collect::<Result<_, StorageProgramError>>()?;
            targets_by_path
                .entry(path.clone())
                .or_default()
                .extend(targets);
        }
        self.targets = targets_by_path;
        let mut sealed = BTreeMap::<Path, BTreeSet<SealedRoot>>::new();
        for (path, roots) in &self.sealed {
            if normalize_path(&domain.results, path)? != *path {
                self.payload.unknown = true;
                continue;
            }
            for root in roots {
                let mut value = Value {
                    targets: root.targets.clone(),
                    ..Value::default()
                };
                value.normalize(domain)?;
                sealed.entry(path.clone()).or_default().insert(SealedRoot {
                    targets: value.targets,
                    unknown: root.unknown || value.payload.unknown,
                });
            }
        }
        self.sealed = sealed;
        Ok(())
    }
}

fn append(prefix: &Path, suffix: &Path) -> Path {
    prefix.iter().chain(suffix.iter()).cloned().collect()
}
fn normalize_path(domain: &BTreeSet<Path>, path: &Path) -> Result<Path, StorageProgramError> {
    domain
        .iter()
        .filter(|candidate| path.starts_with(candidate.iter()))
        .max_by_key(|candidate| candidate.len())
        .cloned()
        .ok_or_else(|| StorageSolveError::ResultOutsideDomain(path.clone()).into())
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct Summary {
    allocations: BTreeSet<StorageSelector>,
    allocation_sites: BTreeMap<StorageSelector, BTreeSet<StorageOperation>>,
    result: Value,
    updates: BTreeMap<ParamId, Value>,
    unknown: bool,
}
impl Summary {
    fn join(&mut self, other: &Self) -> bool {
        let previous = self.clone();
        self.allocations.extend(other.allocations.iter().cloned());
        for (selector, sites) in &other.allocation_sites {
            self.allocation_sites
                .entry(selector.clone())
                .or_default()
                .extend(sites.iter().cloned());
        }
        self.result.join(&other.result);
        for (parameter, value) in &other.updates {
            self.updates.entry(*parameter).or_default().join(value);
        }
        self.unknown |= other.unknown;
        *self != previous
    }
    fn behavior(&self) -> StorageBehavior {
        if self.unknown || self.result.payload.unknown {
            return StorageBehavior::Unknown;
        }
        StorageBehavior::Known {
            allocations: self.allocations.iter().cloned().collect(),
            results: self
                .result
                .payload
                .dependencies
                .iter()
                .map(|dependency| StorageResult {
                    parts: dependency.parts.clone(),
                    source: dependency.source.clone(),
                })
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect(),
        }
    }
}
type State = BTreeMap<u32, Value>;
fn read(state: &State, place: &StoragePlace) -> Value {
    state
        .get(&place.local)
        .map_or_else(Value::unknown, |value| value.project(&place.parts))
}
fn write(state: &mut State, place: &StoragePlace, value: Value) {
    if place.parts.is_empty() {
        state.insert(place.local, value);
    } else {
        // Retain dependencies at ancestors of the write, which may cover more
        // than the replaced child. Whole-local writes remain strong updates.
        let current = state.entry(place.local).or_insert_with(Value::unknown);
        current
            .payload
            .dependencies
            .retain(|dependency| !dependency.parts.starts_with(&place.parts));
        current
            .targets
            .retain(|path, _| !path.starts_with(&place.parts));
        current
            .sealed
            .retain(|path, _| !path.starts_with(&place.parts));
        current.join(&value.prefixed(&place.parts));
    }
}

struct Program<'a> {
    types: &'a TypeInterner,
    select: &'a dyn Fn(DefId, &[GenericArgument]) -> Option<DefId>,
    memo: TypeMemo,
    actuals: RefCell<Vec<ActualBindings>>,
    bodies: &'a [StorageBody],
    domains: &'a [StorageDomain],
    indices: BTreeMap<StorageCallable, usize>,
    definitions: BTreeMap<DefId, usize>,
    contexts: RefCell<Contexts>,
    current: Cell<usize>,
}

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
struct Context {
    body: usize,
    actuals: usize,
    code: BTreeMap<StorageInput, BTreeMap<Path, BTreeSet<Target>>>,
    unknown_inputs: BTreeSet<StorageInput>,
    sealed: BTreeMap<StorageInput, BTreeMap<Path, BTreeSet<SealedRoot>>>,
}
#[derive(Default)]
struct Contexts {
    keys: Vec<Context>,
    indices: BTreeMap<Context, usize>,
    calls: Vec<BTreeSet<usize>>,
    reads: Vec<BTreeSet<usize>>,
}
impl Contexts {
    fn intern(&mut self, context: Context) -> usize {
        if let Some(index) = self.indices.get(&context) {
            return *index;
        }
        let index = self.keys.len();
        self.indices.insert(context.clone(), index);
        self.keys.push(context);
        self.calls.push(BTreeSet::new());
        self.reads.push(BTreeSet::new());
        index
    }
    fn graph(&self) -> Vec<Vec<usize>> {
        self.calls
            .iter()
            .map(|calls| calls.iter().copied().collect())
            .collect()
    }
}

/// Solve the finite program equations before checking sealed-read obligations.
/// An unresolved callback during convergence is not a permanent diagnostic:
/// e.g. a factory's result code can arrive on a later SCC iteration.
pub fn analyze(
    types: &TypeInterner,
    select: &dyn Fn(DefId, &[GenericArgument]) -> Option<DefId>,
    bodies: &[StorageBody],
    domains: &[StorageDomain],
) -> Result<StorageProgramProof, StorageProgramError> {
    if bodies.len() != domains.len() {
        return Err(StorageSolveError::DomainCount.into());
    }
    if !super::actuals::non_growing(types, bodies) {
        return Err(StorageProgramError::GrowingActuals);
    }
    let mut program = Program {
        types,
        select,
        memo: TypeMemo::new(),
        actuals: RefCell::new(vec![ActualBindings::new()]),
        bodies,
        domains,
        indices: BTreeMap::new(),
        definitions: BTreeMap::new(),
        contexts: RefCell::new(Contexts::default()),
        current: Cell::new(0),
    };
    for (index, body) in bodies.iter().enumerate() {
        if program.indices.insert(body.callable, index).is_some() {
            return Err(StorageProgramError::DuplicateCallable(body.callable));
        }
        if body.callable.scope.site == NameSite::Item {
            if program
                .definitions
                .insert(body.callable.definition, index)
                .is_some()
            {
                return Err(StorageProgramError::DuplicateCallable(body.callable));
            }
        }
        if body.entry >= body.nodes.len() {
            return Err(StorageProgramError::InvalidEntry(body.callable));
        }
        for (node, operation) in body.nodes.iter().enumerate() {
            if operation
                .successors
                .iter()
                .any(|next| *next >= body.nodes.len())
            {
                return Err(StorageProgramError::InvalidSuccessor {
                    callable: body.callable,
                    node,
                });
            }
        }
        let mut inputs = BTreeSet::new();
        let mut places = BTreeSet::new();
        for input in &body.inputs {
            if !inputs.insert(input.input.clone()) || !places.insert(input.place.clone()) {
                return Err(StorageProgramError::DuplicateInput(body.callable));
            }
            if matches!(
                input.input,
                StorageInput::Parameter(
                    ParameterRef::Body { .. } | ParameterRef::Bound { depth: 1.., .. }
                )
            ) {
                return Err(StorageProgramError::InvalidInput(body.callable));
            }
        }
    }
    for body in 0..bodies.len() {
        program.contexts.borrow_mut().intern(Context {
            body,
            actuals: 0,
            code: BTreeMap::new(),
            unknown_inputs: BTreeSet::new(),
            sealed: BTreeMap::new(),
        });
    }
    let mut summaries = vec![Summary::default(); bodies.len()];
    for strict in [false, true] {
        summaries = fixed_point(
            || program.contexts.borrow().graph(),
            summaries,
            Summary::default,
            |context, summaries| program.evaluate(context, summaries, strict, None),
            |_, current, next| current.join(&next),
        )?;
    }
    // Discovery records edges from intermediate approximations. Reconstruct
    // direct calls and sealed-read demands from settled executable states;
    // obsolete contexts must not become required producer profiles.
    {
        let mut contexts = program.contexts.borrow_mut();
        for calls in &mut contexts.calls {
            calls.clear();
        }
        for reads in &mut contexts.reads {
            reads.clear();
        }
    }
    for context in 0..summaries.len() {
        program.evaluate(context, &summaries, true, None)?;
    }
    let keys = program.contexts.borrow().keys.clone();
    let context_graph = program.contexts.borrow().graph();
    let mut body_graph = vec![BTreeSet::new(); bodies.len()];
    for (caller, callees) in context_graph.iter().enumerate() {
        for callee in callees {
            body_graph[keys[caller].body].insert(keys[*callee].body);
        }
    }
    let body_graph: Vec<Vec<_>> = body_graph
        .into_iter()
        .map(|callees| callees.into_iter().collect())
        .collect();
    let components = hirsel_graph::components(&body_graph);
    let mut component_of = vec![0; bodies.len()];
    for (component, members) in components.iter().enumerate() {
        for member in members {
            component_of[*member] = component;
        }
    }
    let mut incoming = vec![false; components.len()];
    for (caller, callees) in body_graph.iter().enumerate() {
        for callee in callees {
            if component_of[caller] != component_of[*callee] {
                incoming[component_of[*callee]] = true;
            }
        }
    }
    let roots = (0..bodies.len()).filter(|body| {
        bodies[*body].callable.scope.site == NameSite::Item
            && bodies[*body].generics.is_empty()
            && (bodies[*body].external_entry || !incoming[component_of[*body]])
    });
    let reachable: BTreeSet<_> = hirsel_graph::postorder(&context_graph, roots)
        .into_iter()
        .collect();
    let read_roots: BTreeSet<_> = {
        let contexts = program.contexts.borrow();
        reachable
            .iter()
            .flat_map(|context| contexts.reads[*context].iter().copied())
            .collect()
    };
    let demanded_contexts = hirsel_graph::postorder(&context_graph, read_roots);
    let mut violations = Vec::new();
    for index in reachable {
        program.evaluate(index, &summaries, true, Some(&mut violations))?;
    }
    let mut behaviors: Vec<_> = summaries[..bodies.len()]
        .iter()
        .map(Summary::behavior)
        .collect();
    let mut demanded = BTreeSet::new();
    for context in demanded_contexts {
        let body = keys[context].body;
        if demanded.insert(bodies[body].callable) {
            behaviors[body] = StorageBehavior::empty();
        }
        behaviors[body].join(&summaries[context].behavior());
    }
    Ok(StorageProgramProof {
        behaviors,
        violations,
        demanded,
    })
}

impl Program<'_> {
    fn intern_actuals(&self, bindings: ActualBindings) -> usize {
        let mut actuals = self.actuals.borrow_mut();
        if let Some(index) = actuals.iter().position(|existing| existing == &bindings) {
            return index;
        }
        let index = actuals.len();
        actuals.push(bindings);
        index
    }
    fn instance(
        &self,
        declaration: DefId,
        arguments: &[GenericArgument],
    ) -> Option<(usize, usize)> {
        let context = self.contexts.borrow().keys[self.current.get()].actuals;
        let bindings = self.actuals.borrow()[context].clone();
        let actuals =
            hirsel_types::close_arguments(self.types, &self.memo, arguments, &bindings).ok()?;
        let selected = (self.select)(declaration, &actuals.arguments);
        // Key only tests closedness. Exact full argument rows remain the
        // context identity; CT5 key equivalence never erases their port modes.
        hirsel_types::closed_actual_key(self.types, &actuals.arguments)?;
        let selected = selected?;
        let body = *self.definitions.get(&selected)?;
        let mut bindings = ActualBindings::new();
        if selected == declaration {
            let generics = &self.bodies[body].generics;
            if generics.len() != actuals.arguments.len()
                || !generics
                    .iter()
                    .zip(&actuals.arguments)
                    .all(|(parameter, argument)| {
                        matches!(
                            (parameter, argument),
                            (GenericParameter::Type { .. }, GenericArgument::Type(_))
                                | (GenericParameter::Size { .. }, GenericArgument::Size(_))
                        )
                    })
            {
                return None;
            }
            if !actuals.arguments.is_empty() {
                bindings.insert(self.bodies[body].callable.scope, actuals);
            }
        } else if !self.bodies[body].generics.is_empty() {
            return None;
        }
        Some((body, self.intern_actuals(bindings)))
    }
    fn relocated_captures(
        &self,
        callable: &Code,
        captures: &[Payload],
        placement: &Payload,
    ) -> Vec<Payload> {
        let body = self
            .indices
            .get(&callable.callable)
            .map(|index| &self.bodies[*index]);
        captures
            .iter()
            .enumerate()
            .map(|(ordinal, capture)| {
                let mut capture = capture.clone();
                let input = body.and_then(|body| {
                    body.inputs
                        .iter()
                        .find(|input| input.input == StorageInput::Capture(ordinal as u32))
                });
                match input {
                    Some(input) if input.holds_storage => {
                        capture.dependencies = placement
                            .dependencies
                            .iter()
                            .map(|dependency| Dependency {
                                parts: Path::new(),
                                source: dependency.source.clone(),
                                identity: false,
                            })
                            .collect();
                        capture.unknown |= placement.unknown;
                    }
                    Some(_) => capture.dependencies.clear(),
                    None => capture.unknown = true,
                }
                capture
            })
            .collect()
    }
    fn root_captures(&self, callable: &Code, captures: &[Payload]) -> Vec<Payload> {
        self.relocated_captures(
            callable,
            captures,
            &Payload {
                dependencies: BTreeSet::from([Dependency {
                    parts: Path::new(),
                    source: StorageSelector {
                        input: StorageInput::Parameter(ParameterRef::bound(ParamId(0))),
                        parts: Path::new(),
                    },
                    identity: false,
                }]),
                unknown: false,
            },
        )
    }
    fn copy_to(
        &self,
        arguments: &BTreeMap<ParamId, Value>,
        holds_storage: bool,
        requires_code: bool,
        strict: bool,
    ) -> Summary {
        let placement = arguments
            .get(&ParamId(0))
            .cloned()
            .unwrap_or_else(Value::unknown);
        let mut value = arguments
            .get(&ParamId(1))
            .cloned()
            .unwrap_or_else(Value::unknown);
        if requires_code && !strict && value.targets.is_empty() && !value.payload.unknown {
            return Summary::default();
        }
        let mut result = Summary::default();
        result.unknown = value.payload.unknown
            || placement.payload.unknown
            || !value.sealed.is_empty()
            || requires_code && value.targets.is_empty();
        value.payload = Payload {
            dependencies: if holds_storage {
                placement
                    .payload
                    .dependencies
                    .iter()
                    .map(|dependency| Dependency {
                        parts: Path::new(),
                        source: dependency.source.clone(),
                        identity: false,
                    })
                    .collect()
            } else {
                BTreeSet::new()
            },
            unknown: result.unknown,
        };
        if holds_storage {
            result.allocations.extend(
                value
                    .payload
                    .dependencies
                    .iter()
                    .map(|dependency| dependency.source.clone()),
            );
        }
        value.targets = value
            .targets
            .iter()
            .map(|(path, targets)| {
                let targets = targets
                    .iter()
                    .map(|target| match target {
                        Target::Input(selector) => Target::Input(selector.clone()),
                        Target::IntegerOrder(definition) => Target::IntegerOrder(*definition),
                        Target::Body {
                            callable,
                            captures,
                            capture_code,
                            producers,
                            unknown_captures,
                        } => Target::Body {
                            callable: *callable,
                            captures: self.relocated_captures(
                                callable,
                                captures,
                                &placement.payload,
                            ),
                            capture_code: capture_code.clone(),
                            producers: producers
                                .iter()
                                .map(|(callable, producer)| {
                                    (
                                        *callable,
                                        ProducerCaptures {
                                            captures: self.relocated_captures(
                                                callable,
                                                &producer.captures,
                                                &placement.payload,
                                            ),
                                            code: producer.code.clone(),
                                            unknown: producer.unknown,
                                        },
                                    )
                                })
                                .collect(),
                            unknown_captures: *unknown_captures,
                        },
                    })
                    .collect();
                (path.clone(), targets)
            })
            .collect();
        value.sealed.clear();
        result.result = value;
        result
    }
    fn needs_root_code(&self, callback: &Value) -> bool {
        callback
            .targets
            .get(&Path::new())
            .into_iter()
            .flatten()
            .any(|target| match target {
                Target::Body { callable, .. } => {
                    self.indices.get(&callable.callable).is_some_and(|index| {
                        self.bodies[*index]
                            .inputs
                            .iter()
                            .any(|input| is_root(&input.input) && !input.callables.is_empty())
                    })
                }
                _ => false,
            })
    }
    fn root_code(&self, owner: &Value) -> Value {
        let mut value = Value::default();
        for root in owner.sealed.get(&Path::new()).into_iter().flatten() {
            value.payload.unknown |= root.unknown;
            for (path, targets) in &root.targets {
                for target in targets {
                    let target = match target {
                        Target::IntegerOrder(definition) => Target::IntegerOrder(*definition),
                        Target::Input(_) => {
                            value.payload.unknown = true;
                            continue;
                        }
                        Target::Body {
                            callable,
                            captures,
                            capture_code,
                            producers,
                            unknown_captures,
                        } => {
                            if !self.indices.contains_key(&callable.callable) {
                                value.payload.unknown = true;
                                continue;
                            }
                            let captures = self.root_captures(callable, captures);
                            let producers = producers
                                .iter()
                                .map(|(callable, producer)| {
                                    (
                                        *callable,
                                        ProducerCaptures {
                                            captures: self
                                                .root_captures(callable, &producer.captures),
                                            code: producer.code.clone(),
                                            unknown: producer.unknown,
                                        },
                                    )
                                })
                                .collect();
                            Target::Body {
                                callable: *callable,
                                captures,
                                capture_code: capture_code.clone(),
                                producers,
                                unknown_captures: *unknown_captures,
                            }
                        }
                    };
                    value
                        .targets
                        .entry(path.clone())
                        .or_default()
                        .insert(target);
                }
            }
        }
        value
    }
    fn rebase_code(
        &self,
        input: &StorageInput,
        code: &BTreeMap<Path, BTreeSet<Target>>,
    ) -> BTreeMap<Path, BTreeSet<Target>> {
        code.iter()
            .map(|(path, targets)| {
                let selector = StorageSelector {
                    input: input.clone(),
                    parts: path.clone(),
                };
                let targets = targets
                    .iter()
                    .map(|target| match target {
                        Target::Input(_) => Target::Input(selector.clone()),
                        Target::IntegerOrder(definition) => Target::IntegerOrder(*definition),
                        Target::Body {
                            callable,
                            captures,
                            capture_code,
                            producers,
                            unknown_captures,
                        } => Target::Body {
                            callable: *callable,
                            captures: captures
                                .iter()
                                .map(|capture| rebase_payload(capture, &selector))
                                .collect(),
                            capture_code: capture_code.clone(),
                            producers: map_producers(producers, |capture| {
                                rebase_payload(capture, &selector)
                            }),
                            unknown_captures: *unknown_captures,
                        },
                    })
                    .collect();
                (path.clone(), targets)
            })
            .collect()
    }
    fn context_index(
        &self,
        body: usize,
        actuals: usize,
        arguments: &BTreeMap<ParamId, Value>,
        captures: &[Payload],
        capture_code: &[BTreeMap<Path, BTreeSet<CaptureTarget>>],
        producers: &Producers,
    ) -> usize {
        let mut code = BTreeMap::new();
        let mut unknown_inputs = BTreeSet::new();
        let mut sealed = BTreeMap::new();
        for (parameter, value) in arguments {
            let input = StorageInput::Parameter(ParameterRef::bound(*parameter));
            if value.payload.unknown {
                unknown_inputs.insert(input.clone());
            }
            if !value.sealed.is_empty() {
                sealed.insert(
                    input.clone(),
                    value
                        .sealed
                        .iter()
                        .map(|(path, roots)| {
                            (
                                path.clone(),
                                roots
                                    .iter()
                                    .map(|root| SealedRoot {
                                        targets: self.rebase_code(&input, &root.targets),
                                        unknown: root.unknown,
                                    })
                                    .collect(),
                            )
                        })
                        .collect(),
                );
            }
            if !value.targets.is_empty() {
                let input = StorageInput::Parameter(ParameterRef::bound(*parameter));
                code.insert(input.clone(), self.rebase_code(&input, &value.targets));
            }
        }
        for (ordinal, capture) in captures.iter().enumerate() {
            if capture.unknown {
                unknown_inputs.insert(StorageInput::Capture(ordinal as u32));
            }
        }
        for (ordinal, targets) in capture_code.iter().enumerate() {
            if targets.is_empty() {
                continue;
            }
            let input = StorageInput::Capture(ordinal as u32);
            let targets = targets
                .iter()
                .map(|(path, targets)| {
                    let targets = targets
                        .iter()
                        .map(|target| {
                            let callable = match target {
                                CaptureTarget::IntegerOrder(definition) => {
                                    return Target::IntegerOrder(*definition);
                                }
                                CaptureTarget::Body(callable) => callable,
                            };
                            let producer = producers.get(callable);
                            Target::Body {
                                callable: *callable,
                                captures: producer.map_or_else(Vec::new, |p| p.captures.clone()),
                                capture_code: producer.map_or_else(Vec::new, |p| p.code.clone()),
                                producers: producers.clone(),
                                unknown_captures: producer.is_none_or(|p| p.unknown),
                            }
                        })
                        .collect();
                    (path.clone(), targets)
                })
                .collect();
            code.insert(input.clone(), self.rebase_code(&input, &targets));
        }
        let mut contexts = self.contexts.borrow_mut();
        let index = contexts.intern(Context {
            body,
            actuals,
            code,
            unknown_inputs,
            sealed,
        });
        contexts.calls[self.current.get()].insert(index);
        index
    }
    fn call_body(
        &self,
        body: usize,
        actuals: usize,
        arguments: &BTreeMap<ParamId, Value>,
        captures: &[Payload],
        capture_code: &[BTreeMap<Path, BTreeSet<CaptureTarget>>],
        producers: &Producers,
        summaries: &[Summary],
        strict: bool,
        demanded: bool,
    ) -> Summary {
        // Missing code during discovery is bottom, not an opaque producer.
        // Strict reconstruction refuses any target that never arrives.
        if !strict
            && self.bodies[body].inputs.iter().any(|input| {
                let StorageInput::Parameter(ParameterRef::Bound {
                    depth: 0,
                    parameter,
                }) = input.input
                else {
                    return false;
                };
                !input.callables.is_empty()
                    && arguments
                        .get(&parameter)
                        .is_some_and(|value| value.targets.is_empty() && !value.payload.unknown)
            })
        {
            return Summary::default();
        }
        let index = self.context_index(body, actuals, arguments, captures, capture_code, producers);
        if demanded {
            self.contexts.borrow_mut().reads[self.current.get()].insert(index);
        }
        summaries
            .get(index)
            .map_or_else(Summary::default, |summary| {
                self.instantiate(summary, arguments, captures)
            })
    }
    fn evaluate(
        &self,
        index: usize,
        summaries: &[Summary],
        strict: bool,
        mut violations: Option<&mut Vec<StorageReadViolation>>,
    ) -> Result<Summary, StorageProgramError> {
        self.current.set(index);
        let context = self.contexts.borrow().keys[index].clone();
        let body = &self.bodies[context.body];
        let domain = &self.domains[context.body];
        let mut initial = State::new();
        for input in &body.inputs {
            let selector = StorageSelector {
                input: input.input.clone(),
                parts: Path::new(),
            };
            let mut value = Value::default();
            value.payload.unknown = context.unknown_inputs.contains(&input.input);
            if input.holds_storage {
                value.payload.dependencies.insert(Dependency {
                    parts: Path::new(),
                    source: selector.clone(),
                    identity: true,
                });
            }
            if let Some(code) = context.code.get(&input.input) {
                value.targets = code.clone();
            } else {
                for path in &input.callables {
                    let mut selector = selector.clone();
                    selector.parts = path.clone();
                    value
                        .targets
                        .insert(path.clone(), BTreeSet::from([Target::Input(selector)]));
                }
            }
            value.sealed = context
                .sealed
                .get(&input.input)
                .cloned()
                .unwrap_or_default();
            write(&mut initial, &input.place, value);
        }
        let mut states = vec![None; body.nodes.len()];
        states[body.entry] = Some(initial);
        let mut work = VecDeque::from([body.entry]);
        let mut queued = vec![false; body.nodes.len()];
        queued[body.entry] = true;
        let mut summary = Summary::default();
        while let Some(node) = work.pop_front() {
            queued[node] = false;
            let mut state = states[node].clone().expect("queued reachable node");
            match &body.nodes[node].operation {
                StorageOp::Nop => {}
                StorageOp::Kill(locals) => {
                    for local in locals {
                        state.remove(local);
                    }
                }
                StorageOp::Return(place) => {
                    summary.result.join(&read(&state, place));
                    for input in &body.inputs {
                        if input.mutable {
                            if let StorageInput::Parameter(ParameterRef::Bound {
                                depth: 0,
                                parameter,
                            }) = input.input
                            {
                                summary
                                    .updates
                                    .entry(parameter)
                                    .or_default()
                                    .join(&read(&state, &input.place));
                            }
                        }
                    }
                }
                StorageOp::Assign(assignments) => {
                    let mut values = Vec::new();
                    for (place, expression) in assignments {
                        let mut value = match expression {
                            StorageValue::Reference {
                                definition,
                                arguments,
                            } => self
                                .instance(*definition, arguments)
                                .map(|(index, actuals)| Value {
                                    targets: BTreeMap::from([(
                                        Path::new(),
                                        BTreeSet::from([Target::Body {
                                            callable: Code {
                                                callable: self.bodies[index].callable,
                                                actuals,
                                            },
                                            captures: vec![],
                                            capture_code: vec![],
                                            producers: Producers::new(),
                                            unknown_captures: false,
                                        }]),
                                    )]),
                                    ..Value::default()
                                })
                                .unwrap_or_else(Value::unknown),
                            StorageValue::ConstantReference { .. } => Value::unknown(),
                            StorageValue::Constant { code, complete } => {
                                let mut value = Value::default();
                                value.payload.unknown = !complete;
                                for reference in code {
                                    if reference.primitive == Some(StoragePrimitive::IntegerOrder)
                                        && reference.arguments.is_empty()
                                        && StoragePrimitive::integer_order_type(
                                            reference.definition,
                                        )
                                        .is_some()
                                    {
                                        value
                                            .targets
                                            .entry(reference.parts.clone())
                                            .or_default()
                                            .insert(Target::IntegerOrder(reference.definition));
                                        continue;
                                    }
                                    // Materialized constants are closed global values. Never
                                    // close a forged open code leaf in a caller context: it
                                    // has no original call edge for the growth preflight.
                                    if hirsel_types::closed_actual_key(
                                        self.types,
                                        &reference.arguments,
                                    )
                                    .is_none()
                                    {
                                        value.payload.unknown = true;
                                        continue;
                                    }
                                    match self.instance(reference.definition, &reference.arguments)
                                    {
                                        Some((index, actuals)) => {
                                            value
                                                .targets
                                                .entry(reference.parts.clone())
                                                .or_default()
                                                .insert(Target::Body {
                                                    callable: Code {
                                                        callable: self.bodies[index].callable,
                                                        actuals,
                                                    },
                                                    captures: vec![],
                                                    capture_code: vec![],
                                                    producers: Producers::new(),
                                                    unknown_captures: false,
                                                });
                                        }
                                        None => value.payload.unknown = true,
                                    }
                                }
                                value
                            }
                            StorageValue::Fresh => Value::default(),
                            StorageValue::Unknown => Value::unknown(),
                            StorageValue::Copy { value, detaches } => {
                                if *detaches {
                                    Value::default()
                                } else {
                                    read(&state, value)
                                }
                            }
                            StorageValue::Unique { value, allocates } => {
                                let mut value = read(&state, value);
                                if *allocates {
                                    summary.unknown |= value.payload.unknown;
                                    summary.allocations.extend(
                                        value
                                            .payload
                                            .dependencies
                                            .iter()
                                            .map(|dependency| dependency.source.clone()),
                                    );
                                }
                                value.payload.dependencies = value
                                    .payload
                                    .dependencies
                                    .into_iter()
                                    .map(|mut dependency| {
                                        dependency.identity = false;
                                        dependency
                                    })
                                    .collect();
                                value
                            }
                            StorageValue::Construct { fields, placement } => {
                                let mut value = Value::default();
                                for (path, field) in fields {
                                    value.join(&read(&state, field).prefixed(path));
                                }
                                match placement {
                                    StoragePlacement::Independent => {}
                                    StoragePlacement::Unknown => {
                                        summary.unknown = true;
                                        value.payload.unknown = true;
                                    }
                                    StoragePlacement::Sources(places) => {
                                        for place in places {
                                            let placement = read(&state, place);
                                            summary.unknown |= placement.payload.unknown;
                                            value.payload.unknown |= placement.payload.unknown;
                                            for dependency in &placement.payload.dependencies {
                                                summary
                                                    .allocations
                                                    .insert(dependency.source.clone());
                                                value.payload.dependencies.insert(Dependency {
                                                    parts: Path::new(),
                                                    source: dependency.source.clone(),
                                                    identity: false,
                                                });
                                            }
                                        }
                                    }
                                }
                                value
                            }
                            StorageValue::Callable { target, captures } => {
                                let captures: Vec<_> =
                                    captures.iter().map(|place| read(&state, place)).collect();
                                let pending_code = !strict
                                    && self.indices.get(target).is_some_and(|index| {
                                        captures.iter().enumerate().any(|(ordinal, capture)| {
                                            capture.targets.is_empty()
                                                && !capture.payload.unknown
                                                && self.bodies[*index].inputs.iter().any(|input| {
                                                    input.input
                                                        == StorageInput::Capture(ordinal as u32)
                                                        && !input.callables.is_empty()
                                                })
                                        })
                                    });
                                if pending_code {
                                    Value::default()
                                } else {
                                    let mut value = Value::default();
                                    for capture in &captures {
                                        value.payload.unknown |= capture.payload.unknown;
                                        value.payload.dependencies.extend(
                                            capture.payload.dependencies.iter().cloned().map(
                                                |mut dependency| {
                                                    dependency.parts.clear();
                                                    dependency.identity = false;
                                                    dependency
                                                },
                                            ),
                                        );
                                    }
                                    let mut unknown_captures = false;
                                    let mut producers = Producers::new();
                                    let capture_code = captures
                                        .iter()
                                        .map(|capture| {
                                            capture
                                                .targets
                                                .iter()
                                                .map(|(path, targets)| {
                                                    let mut code = BTreeSet::new();
                                                    for target in targets {
                                                        match target {
                                                            Target::Body {
                                                                callable,
                                                                captures,
                                                                capture_code,
                                                                producers: nested,
                                                                unknown_captures,
                                                            } => {
                                                                code.insert(CaptureTarget::Body(
                                                                    *callable,
                                                                ));
                                                                join_producers(
                                                                    &mut producers,
                                                                    nested,
                                                                );
                                                                join_producers(
                                                                    &mut producers,
                                                                    &BTreeMap::from([(
                                                                        *callable,
                                                                        ProducerCaptures {
                                                                            captures: captures
                                                                                .clone(),
                                                                            code: capture_code
                                                                                .clone(),
                                                                            unknown:
                                                                                *unknown_captures,
                                                                        },
                                                                    )]),
                                                                );
                                                            }
                                                            Target::Input(_) => {
                                                                unknown_captures = true
                                                            }
                                                            Target::IntegerOrder(definition) => {
                                                                code.insert(
                                                                    CaptureTarget::IntegerOrder(
                                                                        *definition,
                                                                    ),
                                                                );
                                                            }
                                                        }
                                                    }
                                                    (path.clone(), code)
                                                })
                                                .collect()
                                        })
                                        .collect();
                                    value.targets.insert(
                                        Path::new(),
                                        BTreeSet::from([Target::Body {
                                            callable: Code {
                                                callable: *target,
                                                actuals: context.actuals,
                                            },
                                            unknown_captures,
                                            capture_code,
                                            producers,
                                            captures: captures
                                                .into_iter()
                                                .map(|value| value.payload)
                                                .collect(),
                                        }]),
                                    );
                                    value
                                }
                            }
                        };
                        value.normalize(domain)?;
                        values.push((place, value));
                    }
                    for (place, value) in values {
                        write(&mut state, place, value);
                    }
                }
                StorageOp::Call {
                    callee,
                    arguments,
                    mutations,
                    destination,
                } => {
                    let argument_places = arguments;
                    let arguments: BTreeMap<_, _> = arguments
                        .iter()
                        .map(|(parameter, place)| (*parameter, read(&state, place)))
                        .collect();
                    let mut result = match callee {
                        StorageCallee::Apply {
                            definition,
                            arguments: actuals,
                        } => self
                            .instance(*definition, actuals)
                            .map(|(index, actuals)| {
                                self.call_body(
                                    index,
                                    actuals,
                                    &arguments,
                                    &[],
                                    &[],
                                    &Producers::new(),
                                    summaries,
                                    strict,
                                    false,
                                )
                            })
                            .unwrap_or_else(unknown_summary),
                        StorageCallee::Declared {
                            allocations,
                            results,
                            complete,
                        } => {
                            let declared = Summary {
                                allocations: allocations.iter().cloned().collect(),
                                allocation_sites: BTreeMap::new(),
                                result: Value {
                                    payload: Payload {
                                        unknown: !complete,
                                        dependencies: results
                                            .iter()
                                            .map(|result| Dependency {
                                                parts: result.parts.clone(),
                                                source: result.source.clone(),
                                                identity: false,
                                            })
                                            .collect(),
                                    },
                                    ..Value::default()
                                },
                                unknown: !complete,
                                ..Summary::default()
                            };
                            self.instantiate(&declared, &arguments, &[])
                        }
                        StorageCallee::Definition(definition) => self
                            .definitions
                            .get(definition)
                            .map_or_else(unknown_summary, |index| {
                                self.call_body(
                                    *index,
                                    0,
                                    &arguments,
                                    &[],
                                    &[],
                                    &Producers::new(),
                                    summaries,
                                    strict,
                                    false,
                                )
                            }),
                        StorageCallee::Value(place) => {
                            self.invoke(&read(&state, place), &arguments, summaries, strict, false)
                        }
                        StorageCallee::CopyTo {
                            holds_storage,
                            requires_code,
                        } => self.copy_to(&arguments, *holds_storage, *requires_code, strict),
                        StorageCallee::MapInsert | StorageCallee::SortedMapInsert => {
                            let offset =
                                u32::from(matches!(callee, StorageCallee::SortedMapInsert));
                            let mut receiver = arguments
                                .get(&ParamId(offset))
                                .cloned()
                                .unwrap_or_else(Value::unknown);
                            let mut result = Summary::default();
                            result.unknown = receiver.payload.unknown;
                            result.allocations.extend(
                                receiver
                                    .payload
                                    .dependencies
                                    .iter()
                                    .map(|dependency| dependency.source.clone()),
                            );
                            for parameter in 1..=2 {
                                let value = arguments
                                    .get(&ParamId(parameter + offset))
                                    .cloned()
                                    .unwrap_or_else(Value::unknown);
                                result.unknown |= value.payload.unknown;
                                receiver.join(
                                    &value.prefixed(
                                        &vec![
                                            OwnedTypePart::Elements,
                                            OwnedTypePart::Field(parameter - 1),
                                        ]
                                        .into(),
                                    ),
                                );
                            }
                            if offset == 1 {
                                result.join(&self.sorted_comparison(&arguments, summaries, strict));
                            }
                            receiver.normalize(domain)?;
                            if let Some((_, place)) = argument_places
                                .iter()
                                .find(|(parameter, _)| *parameter == ParamId(offset))
                            {
                                write(&mut state, place, receiver);
                            } else {
                                result.unknown = true;
                            }
                            result
                        }
                        StorageCallee::MapGet {
                            result_path,
                            requires_code,
                        }
                        | StorageCallee::MapRemove {
                            result_path,
                            requires_code,
                        }
                        | StorageCallee::SortedMapGet {
                            result_path,
                            requires_code,
                        }
                        | StorageCallee::SortedMapRemove {
                            result_path,
                            requires_code,
                        } => {
                            let ordered = matches!(
                                callee,
                                StorageCallee::SortedMapGet { .. }
                                    | StorageCallee::SortedMapRemove { .. }
                            );
                            let receiver = arguments
                                .get(&ParamId(u32::from(ordered)))
                                .cloned()
                                .unwrap_or_else(Value::unknown);
                            let mut value = receiver.project(
                                &vec![OwnedTypePart::Elements, OwnedTypePart::Field(1)].into(),
                            );
                            let missing_code = *requires_code && value.targets.is_empty();
                            if missing_code && !strict && !value.payload.unknown {
                                Summary::default()
                            } else {
                                value.payload.unknown |= missing_code;
                                let mut result = Summary {
                                    unknown: value.payload.unknown,
                                    result: value.prefixed(result_path),
                                    ..Summary::default()
                                };
                                if ordered {
                                    result.join(
                                        &self.sorted_comparison(&arguments, summaries, strict),
                                    );
                                }
                                if matches!(
                                    callee,
                                    StorageCallee::MapRemove { .. }
                                        | StorageCallee::SortedMapRemove { .. }
                                ) {
                                    result.allocations.extend(
                                        receiver
                                            .payload
                                            .dependencies
                                            .iter()
                                            .map(|dependency| dependency.source.clone()),
                                    );
                                    // Removing an entry only shrinks the set of possible
                                    // stored producers; retaining the original set is sound.
                                    result.updates.insert(ParamId(u32::from(ordered)), receiver);
                                }
                                result
                            }
                        }
                        StorageCallee::SealArena => {
                            let root = arguments
                                .get(&ParamId(1))
                                .cloned()
                                .unwrap_or_else(Value::unknown);
                            let mut result = Summary::default();
                            result.result.sealed.insert(
                                Path::new(),
                                BTreeSet::from([SealedRoot {
                                    targets: root.targets,
                                    unknown: root.payload.unknown || !root.sealed.is_empty(),
                                }]),
                            );
                            result
                        }
                        StorageCallee::TaskAwait => {
                            let value = arguments
                                .get(&ParamId(1))
                                .cloned()
                                .unwrap_or_else(Value::unknown)
                                .project(&vec![OwnedTypePart::Elements].into());
                            Summary {
                                unknown: value.payload.unknown,
                                result: value,
                                ..Summary::default()
                            }
                        }
                        StorageCallee::ScopeCallback {
                            callback,
                            result_path,
                        } => {
                            let callback = arguments
                                .get(callback)
                                .cloned()
                                .unwrap_or_else(Value::unknown);
                            let mut result = self.invoke(
                                &callback,
                                &BTreeMap::from([(ParamId(0), Value::default())]),
                                summaries,
                                strict,
                                false,
                            );
                            result.result = result.result.prefixed(result_path);
                            result
                        }
                        StorageCallee::ReadRegion => {
                            let callback = arguments
                                .get(&ParamId(1))
                                .cloned()
                                .unwrap_or_else(Value::unknown);
                            // The borrowed root's own dependencies are discharged
                            // at this boundary only after its obligation is checked.
                            // Captures still instantiate into the caller's sources.
                            let owner = arguments
                                .get(&ParamId(0))
                                .cloned()
                                .unwrap_or_else(Value::unknown);
                            if !strict && owner.sealed.is_empty() && self.needs_root_code(&callback)
                            {
                                Summary::default()
                            } else {
                                self.invoke(
                                    &callback,
                                    &BTreeMap::from([(ParamId(0), self.root_code(&owner))]),
                                    summaries,
                                    strict,
                                    true,
                                )
                            }
                        }
                    };
                    for parameter in mutations {
                        // MapInsert updated its exact receiver above. All other
                        // writes require the independently reconstructed callee
                        // output; an opaque/missing effect cannot retain old code.
                        if matches!(
                            callee,
                            StorageCallee::MapInsert | StorageCallee::SortedMapInsert
                        ) {
                            continue;
                        }
                        let Some((_, place)) = argument_places
                            .iter()
                            .find(|(actual, _)| actual == parameter)
                        else {
                            summary.unknown = true;
                            continue;
                        };
                        if let Some(update) = result.updates.get(parameter) {
                            let mut current = read(&state, place);
                            current.join(update);
                            if result.unknown {
                                current.join(&Value::unknown());
                            }
                            write(&mut state, place, current);
                        } else if strict
                            || result.unknown
                            || matches!(callee, StorageCallee::Declared { .. })
                        {
                            let mut current = read(&state, place);
                            current.join(&Value::unknown());
                            write(&mut state, place, current);
                        }
                    }
                    summary.unknown |= result.unknown;
                    for selector in &result.allocations {
                        let sites = result.allocation_sites.entry(selector.clone()).or_default();
                        if sites.is_empty() {
                            sites.insert(StorageOperation {
                                callable: body.callable,
                                node,
                            });
                        }
                    }
                    for (selector, sites) in result.allocation_sites {
                        summary
                            .allocation_sites
                            .entry(selector)
                            .or_default()
                            .extend(sites);
                    }
                    summary.allocations.extend(result.allocations);
                    if let Some(destination) = destination {
                        write(&mut state, destination, result.result);
                    }
                }
            }
            summary.result.normalize(domain)?;
            for update in summary.updates.values_mut() {
                update.normalize(domain)?;
            }
            for selector in &summary.allocations {
                summary
                    .allocation_sites
                    .entry(selector.clone())
                    .or_insert_with(|| {
                        BTreeSet::from([StorageOperation {
                            callable: body.callable,
                            node,
                        }])
                    });
            }
            let mut allocation_sites: BTreeMap<_, BTreeSet<_>> = BTreeMap::new();
            for (selector, sites) in std::mem::take(&mut summary.allocation_sites) {
                allocation_sites
                    .entry(domain.selector(&selector)?)
                    .or_default()
                    .extend(sites);
            }
            summary.allocation_sites = allocation_sites;
            summary.allocations = summary
                .allocations
                .iter()
                .map(|selector| domain.selector(selector))
                .collect::<Result<_, _>>()?;
            for value in state.values_mut() {
                value.normalize(domain)?;
            }
            for &successor in &body.nodes[node].successors {
                let changed = match &mut states[successor] {
                    None => {
                        states[successor] = Some(state.clone());
                        true
                    }
                    Some(current) => {
                        let mut changed = false;
                        // Missing on either incoming path cannot become a known
                        // fresh value through the other path's initialization.
                        let locals: BTreeSet<_> =
                            current.keys().chain(state.keys()).copied().collect();
                        for local in locals {
                            let next = state.get(&local).cloned().unwrap_or_else(Value::unknown);
                            changed |= current
                                .entry(local)
                                .or_insert_with(Value::unknown)
                                .join(&next);
                        }
                        changed
                    }
                };
                if changed && !queued[successor] {
                    queued[successor] = true;
                    work.push_back(successor);
                }
            }
        }
        if let Some(violations) = violations.as_mut() {
            for (node, operation) in body.nodes.iter().enumerate() {
                let StorageOp::Call {
                    callee: StorageCallee::ReadRegion,
                    arguments,
                    ..
                } = &operation.operation
                else {
                    continue;
                };
                let Some(state) = &states[node] else { continue };
                let callback = arguments
                    .iter()
                    .find(|(parameter, _)| *parameter == ParamId(1))
                    .map_or_else(Value::unknown, |(_, place)| read(state, place));
                let owner = arguments
                    .iter()
                    .find(|(parameter, _)| *parameter == ParamId(0))
                    .map_or_else(Value::unknown, |(_, place)| read(state, place));
                let mut failures = Vec::new();
                let callback_summary =
                    self.invoke_symbolic(&callback, &self.root_code(&owner), summaries);
                match callback_summary.behavior() {
                    StorageBehavior::Unknown => failures.push(StorageReadFailure::UnknownCallback),
                    StorageBehavior::Known {
                        allocations,
                        results,
                    } => {
                        failures.extend(
                            allocations
                                .iter()
                                .filter(|selector| is_root(&selector.input))
                                .cloned()
                                .map(StorageReadFailure::AllocatesFromRoot),
                        );
                        failures.extend(
                            results
                                .iter()
                                .filter(|result| is_root(&result.source.input))
                                .cloned()
                                .map(StorageReadFailure::ReturnsRoot),
                        );
                    }
                }
                violations.extend(failures.into_iter().map(|failure| {
                    StorageReadViolation {
                        callable: body.callable,
                        node,
                        origin: match &failure {
                            StorageReadFailure::AllocatesFromRoot(selector) => callback_summary
                                .allocation_sites
                                .get(selector)
                                .and_then(|sites| sites.first())
                                .cloned(),
                            _ => None,
                        },
                        failure,
                    }
                }));
            }
        }
        Ok(summary)
    }

    fn instantiate(
        &self,
        summary: &Summary,
        arguments: &BTreeMap<ParamId, Value>,
        captures: &[Payload],
    ) -> Summary {
        let source = |selector: &StorageSelector| match selector.input {
            StorageInput::Parameter(ParameterRef::Bound {
                depth: 0,
                parameter,
            }) => arguments
                .get(&parameter)
                .cloned()
                .unwrap_or_else(Value::unknown)
                .project(&selector.parts),
            StorageInput::Capture(ordinal) => {
                captures
                    .get(ordinal as usize)
                    .map_or_else(Value::unknown, |payload| {
                        Value {
                            payload: payload.clone(),
                            ..Value::default()
                        }
                        .project(&selector.parts)
                    })
            }
            _ => Value::unknown(),
        };
        let mut result = Summary {
            unknown: summary.unknown,
            ..Summary::default()
        };
        for selector in &summary.allocations {
            let value = source(selector);
            result.unknown |= value.payload.unknown;
            for dependency in value.payload.dependencies {
                result.allocations.insert(dependency.source.clone());
                if let Some(sites) = summary.allocation_sites.get(selector) {
                    result
                        .allocation_sites
                        .entry(dependency.source)
                        .or_default()
                        .extend(sites.iter().cloned());
                }
            }
        }
        result.result.payload.unknown = summary.result.payload.unknown;
        for dependency in &summary.result.payload.dependencies {
            let mut value = source(&dependency.source);
            // These are storage bounds, not alias claims about a source child.
            value.targets.clear();
            value.payload.dependencies = value
                .payload
                .dependencies
                .into_iter()
                .map(|mut dependency| {
                    dependency.parts.clear();
                    dependency.identity = false;
                    dependency
                })
                .collect();
            result.result.join(&value.prefixed(&dependency.parts));
        }
        for (path, targets) in &summary.result.targets {
            for target in targets {
                let value = match target {
                    Target::Input(selector) => source(selector),
                    Target::IntegerOrder(definition) => Value {
                        targets: BTreeMap::from([(
                            Path::new(),
                            BTreeSet::from([Target::IntegerOrder(*definition)]),
                        )]),
                        ..Value::default()
                    },
                    Target::Body {
                        callable,
                        captures,
                        capture_code,
                        producers,
                        unknown_captures,
                    } => {
                        let map_payload = |payload: &Payload| {
                            let mut value = Value::default();
                            value.payload.unknown = payload.unknown;
                            for dependency in &payload.dependencies {
                                value.join(&source(&dependency.source).prefixed(&dependency.parts));
                            }
                            value.payload
                        };
                        let mapped: Vec<_> = captures.iter().map(&map_payload).collect();
                        let unknown =
                            *unknown_captures || mapped.iter().any(|payload| payload.unknown);
                        let producers = map_producers(producers, map_payload);
                        Value {
                            targets: BTreeMap::from([(
                                Path::new(),
                                BTreeSet::from([Target::Body {
                                    callable: *callable,
                                    captures: mapped,
                                    capture_code: capture_code.clone(),
                                    producers,
                                    unknown_captures: unknown,
                                }]),
                            )]),
                            ..Value::default()
                        }
                    }
                };
                result.result.join(&value.prefixed(path));
            }
        }
        for (parameter, update) in &summary.updates {
            let carrier = Summary {
                result: update.clone(),
                ..Summary::default()
            };
            result.updates.insert(
                *parameter,
                self.instantiate(&carrier, arguments, captures).result,
            );
        }
        for (path, roots) in &summary.result.sealed {
            for root in roots {
                let carrier = Summary {
                    result: Value {
                        targets: root.targets.clone(),
                        ..Value::default()
                    },
                    ..Summary::default()
                };
                let mapped = self.instantiate(&carrier, arguments, captures).result;
                result
                    .result
                    .sealed
                    .entry(path.clone())
                    .or_default()
                    .insert(SealedRoot {
                        targets: mapped.targets,
                        unknown: root.unknown || mapped.payload.unknown,
                    });
            }
        }
        result
    }
    /// Ordered lookup may compare the requested key with any stored key.
    /// Execute actual code with both provenances; the scalar comparison result
    /// adds no stored-value producer and never replaces the map result.
    fn sorted_comparison(
        &self,
        arguments: &BTreeMap<ParamId, Value>,
        summaries: &[Summary],
        strict: bool,
    ) -> Summary {
        let by = arguments
            .get(&ParamId(0))
            .cloned()
            .unwrap_or_else(Value::unknown)
            .project(&vec![OwnedTypePart::Field(0)].into());
        let mut key = arguments
            .get(&ParamId(1))
            .cloned()
            .unwrap_or_else(Value::unknown)
            .project(&vec![OwnedTypePart::Elements, OwnedTypePart::Field(0)].into());
        key.join(
            &arguments
                .get(&ParamId(2))
                .cloned()
                .unwrap_or_else(Value::unknown),
        );
        let mut result = self.invoke(
            &by,
            &BTreeMap::from([(ParamId(0), key.clone()), (ParamId(1), key)]),
            summaries,
            strict,
            false,
        );
        result.result = Value::default();
        result.unknown |= !result.updates.is_empty();
        result.updates.clear();
        result
    }

    fn invoke(
        &self,
        value: &Value,
        arguments: &BTreeMap<ParamId, Value>,
        summaries: &[Summary],
        strict: bool,
        demanded: bool,
    ) -> Summary {
        let mut result = Summary::default();
        result.unknown = value.payload.unknown;
        // Missing actual code makes its returned value unavailable too. A
        // known alternative may add producers, but cannot discharge this one.
        result.result.payload.unknown = value.payload.unknown;
        match value.targets.get(&Path::new()) {
            None => {
                result.unknown |= strict;
                result.result.payload.unknown |= strict;
            }
            Some(targets) => {
                for target in targets {
                    match target {
                        Target::IntegerOrder(definition) => {
                            result.unknown |= StoragePrimitive::integer_order_type(*definition)
                                .is_none()
                                || arguments.len() != 2
                                || !arguments.contains_key(&ParamId(0))
                                || !arguments.contains_key(&ParamId(1));
                        }
                        Target::Input(_) => {
                            result.join(&unknown_summary());
                        }
                        Target::Body {
                            callable,
                            captures,
                            capture_code,
                            producers,
                            unknown_captures,
                        } => {
                            if *unknown_captures {
                                result.unknown = true;
                            }
                            // The flat code table retains concrete identities;
                            // unresolved alternatives must also enter the body
                            // context, so an inert alternative cannot erase them
                            // from a returned aggregate or sealed owner.
                            let captures: Vec<_> = captures
                                .iter()
                                .map(|capture| {
                                    let mut capture = capture.clone();
                                    capture.unknown |= *unknown_captures;
                                    capture
                                })
                                .collect();
                            if let Some(index) = self.indices.get(&callable.callable) {
                                result.join(&self.call_body(
                                    *index,
                                    callable.actuals,
                                    arguments,
                                    &captures,
                                    capture_code,
                                    producers,
                                    summaries,
                                    strict,
                                    demanded,
                                ));
                            } else {
                                result.join(&unknown_summary());
                            }
                        }
                    }
                }
            }
        }
        result
    }
    fn invoke_symbolic(&self, value: &Value, root: &Value, summaries: &[Summary]) -> Summary {
        // Use a distinct root symbol for the callback parameter, so captures
        // of the enclosing callable's parameter0 cannot be mistaken for it.
        // The caller's captures do not establish root independence; unknown
        // capture evidence still causes an explicit refusal.
        let mut result = Summary::default();
        result.unknown = value.payload.unknown;
        match value.targets.get(&Path::new()) {
            None => result.unknown = true,
            Some(targets) => {
                for target in targets {
                    match target {
                        // A region callback takes one root; integer comparison
                        // has two operands and is only reached through an actual call.
                        Target::IntegerOrder(_) => result.unknown = true,
                        Target::Input(_) => result.unknown = true,
                        Target::Body {
                            callable,
                            captures,
                            capture_code,
                            producers,
                            unknown_captures,
                            ..
                        } => {
                            result.unknown |= *unknown_captures;
                            if let Some(index) = self.indices.get(&callable.callable) {
                                // Raw callback-relative selectors retain parameter vs
                                // capture distinction, independent of the caller.
                                let captures: Vec<_> = captures
                                    .iter()
                                    .map(|capture| {
                                        let mut capture = capture.clone();
                                        capture.unknown |= *unknown_captures;
                                        capture
                                    })
                                    .collect();
                                let context = self.context_index(
                                    *index,
                                    callable.actuals,
                                    &BTreeMap::from([(ParamId(0), root.clone())]),
                                    &captures,
                                    capture_code,
                                    producers,
                                );
                                if let Some(summary) = summaries.get(context) {
                                    result.join(summary);
                                }
                            } else {
                                result.unknown = true;
                            }
                        }
                    }
                }
            }
        }
        result
    }
}
fn unknown_summary() -> Summary {
    Summary {
        unknown: true,
        result: Value::unknown(),
        ..Summary::default()
    }
}
fn is_root(input: &StorageInput) -> bool {
    matches!(
        input,
        StorageInput::Parameter(ParameterRef::Bound {
            depth: 0,
            parameter: ParamId(0)
        })
    )
}
