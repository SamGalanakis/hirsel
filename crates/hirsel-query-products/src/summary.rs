//! What a checked definition tells the program-level passes about its calls
//! (design r5 §1(c) step 2): CT-5 instance selection, staged-constant
//! selection and GEN-5 read a `DefinitionSummary` and its `ParameterGrowth`,
//! never a TypedBody, so the body can be dropped once it is published.
//!
//! The checker's output is the only producer ([`summarize`]); the consumers
//! and the persisted form belong to the engine's scheduling.
//!
//! Frames are the lexical scopes that select calls: the item, each local
//! function and closure the checker planned, and each comptime region. A
//! frame's parent is the smallest frame whose span strictly encloses it;
//! frames are numbered in source order, so a parent precedes its children.
//!
//! Call arguments are **open**: they may name the owner's generic parameters
//! (its item binder and its local functions'). An S1 selection item closes
//! them with the bindings it carries. Their TypeIds are session interner rows;
//! lexical references inside them are qualified by their owner, so they need
//! no outer context.
use crate::TypedBody;
use hirsel_intern::IdHasher;
use hirsel_intern::work::{self, Counter};
use hirsel_span::Span;
use hirsel_syntax::{DefId, ExprId, ExprKind, LexicalOwner};
use hirsel_types::{
    BinderRef, CallTarget, GenericArgument, NameSite, ResolvedName, SharedSlice, SiteRef, Size,
    SizeParamId, SizeParameterRef, TypeId, TypeInterner, TypeKind, TypeParamId, TypeParameterRef,
};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

/// A generic parameter named by its declaring binder (contract rev 3).
#[derive(
    serde::Serialize, serde::Deserialize, Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash,
)]
pub enum GenericParameterRef {
    Type(TypeParameterRef),
    Size(SizeParameterRef),
}
#[derive(
    serde::Serialize, serde::Deserialize, Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash,
)]
pub enum FrameKind {
    Item,
    LocalFunction,
    Closure,
    ComptimeRegion,
}
/// How a call's instance is selected.
#[derive(serde::Serialize, serde::Deserialize, Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum CallClass {
    /// A written call outside every comptime region: selected when its frame is.
    Runtime,
    /// Any call inside a comptime region: executing the region selects it, so
    /// a branch that never runs selects nothing.
    ComptimeExecuted,
    /// An instance the checker planned with no written call at its site (a
    /// named generic function value, a fact helper, a loop protocol call),
    /// outside every comptime region: selected with its frame.
    StaticSelection,
}
#[derive(serde::Serialize, serde::Deserialize, Clone, Debug, Eq, PartialEq)]
pub struct Frame {
    pub site: NameSite,
    pub parent: Option<u32>,
    pub kind: FrameKind,
    /// The frame declares generic parameters of its own: a selection enters it
    /// only through a call with its actuals, never by enclosing it.
    pub generic: bool,
    /// The enclosing frames' size parameters this frame (or a frame inside
    /// it) names in a call argument or a staged input, each bound to itself:
    /// a selection entering the frame carries exactly these from its parent.
    pub size_bindings: Box<[(SizeParameterRef, Size)]>,
}
#[derive(serde::Serialize, serde::Deserialize, Clone, Debug, Eq, PartialEq)]
pub struct SummaryCall {
    pub callee: DefId,
    /// The callee's generic binder: its item, or for a call of a local
    /// function by name, that function's site in the owner.
    pub binder: SiteRef,
    pub arguments: Box<[GenericArgument]>, /* open, owner-relative */
    pub owning_frame: u32,
    /// The callee's frame when it is a local function of this owner.
    pub target_frame: Option<u32>,
    pub class: CallClass,
    pub site: Span,
}
#[derive(serde::Serialize, serde::Deserialize, Clone, Debug, Eq, PartialEq)]
pub struct DefinitionSummary {
    pub owner: DefId,
    pub frames: SharedSlice<Frame>,
    pub calls: SharedSlice<SummaryCall>,
    /// Each comptime region that reads enclosing sizes (a hidden size-generic
    /// constant), with those inputs in its constant's parameter order. The
    /// region is the `ComptimeRegion` frame with the same site.
    pub staged: SharedSlice<(NameSite, Box<[SizeParameterRef]>)>,
}
/// One `ParameterGrowth` edge: from an owner parameter to a callee parameter.
pub type GrowthEdge = (
    (DefId, GenericParameterRef),
    (DefId, GenericParameterRef),
    bool,
    Span,
);
/// GEN-5's input: each owner parameter that flows into a callee parameter,
/// whether it flows nested (strict growth), and the call site.
#[derive(serde::Serialize, serde::Deserialize, Clone, Debug, Default, Eq, PartialEq)]
pub struct ParameterGrowth {
    pub edges: SharedSlice<GrowthEdge>,
}

/// GEN-5's checked input, before modes add planned calls. Retained separately
/// from the final summary so releasing bodies never changes that input.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct CheckedSummary {
    pub summary: DefinitionSummary,
    pub growth: ParameterGrowth,
}

impl ParameterGrowth {
    /// Sites of growing edges that lie on a cycle, in source order.
    pub fn growing_sites(&self) -> Vec<Span> {
        growing_sites(&self.edges)
    }
}
impl DefinitionSummary {
    /// The owner's item binder, as callers name it.
    pub fn item_binder(&self) -> SiteRef {
        LexicalOwner::first(self.owner.path()).site(NameSite::Item)
    }
}

/// The summary and growth edges of one checked definition.
pub fn summarize<'body>(
    t: &TypeInterner,
    typed: &'body TypedBody,
) -> (DefinitionSummary, ParameterGrowth) {
    let syntax = &*typed.syntax;
    let checked = &typed.checked;
    let tree = &syntax.tree;
    let lexical = syntax.owner();
    let span = |site: NameSite| source_site(syntax, site);
    let comptime = |expression: ExprId| {
        matches!(
            tree.expressions[expression.0 as usize].kind,
            ExprKind::Comptime(_)
        )
    };

    let generic: BTreeSet<_> = checked
        .resolution
        .generics
        .iter()
        .map(|g| SummaryKey(g.owner))
        .collect();
    let generic = |site| generic.contains(&SummaryKey(site));

    // Frames in source order: outer before inner, ties by site.
    let mut sites: BTreeSet<(NameSite, FrameKind)> = BTreeSet::new();
    for plan in &typed.plans.functions {
        let kind = match plan.owner {
            NameSite::Statement(_) => FrameKind::LocalFunction,
            _ => FrameKind::Closure,
        };
        sites.insert((plan.owner, kind));
    }
    for row in &checked.expressions {
        if comptime(row.expression) {
            sites.insert((
                NameSite::Expression(row.expression),
                FrameKind::ComptimeRegion,
            ));
        }
    }
    let mut ordered: Vec<_> = sites
        .into_iter()
        .filter(|(site, _)| *site != NameSite::Item)
        .map(|(site, kind)| (span(site), site, kind))
        .collect();
    ordered.sort_by(|(a, sa, _), (b, sb, _)| {
        summary_compare(&(span_order(*a), *sa), &(span_order(*b), *sb))
    });
    let spans: Vec<_> = std::iter::once(span(NameSite::Item))
        .chain(ordered.iter().map(|(at, _, _)| *at))
        .collect();
    let mut nesting = SpanNesting::new(&spans);
    let mut frames = vec![Frame {
        site: NameSite::Item,
        parent: None,
        kind: FrameKind::Item,
        generic: generic(NameSite::Item),
        size_bindings: Box::new([]),
    }];
    for (index, (_, site, kind)) in ordered.into_iter().enumerate() {
        let parent = nesting.enter(index as u32 + 1);
        frames.push(Frame {
            site,
            parent: Some(parent),
            kind,
            generic: generic(site),
            size_bindings: Box::new([]),
        });
    }
    let mut frame_indices = BTreeMap::new();
    for (i, frame) in frames.iter().enumerate().skip(1) {
        frame_indices
            .entry(SummaryKey(frame.site))
            .or_insert(i as u32);
    }
    let frame_of = |site: NameSite| {
        if site == NameSite::Item {
            Some(0)
        } else {
            frame_indices.get(&SummaryKey(site)).copied()
        }
    };
    let mut staged_frames = Vec::with_capacity(frames.len());
    for frame in &frames {
        staged_frames.push(
            frame.kind == FrameKind::ComptimeRegion
                || frame.parent.is_some_and(|p| staged_frames[p as usize]),
        );
    }
    let call_spans: Vec<_> = checked
        .calls
        .iter()
        .map(|call| call.expression)
        .chain(
            checked
                .instantiations
                .iter()
                .map(|instance| instance.expression),
        )
        .map(|expression| span(NameSite::Expression(expression)))
        .collect();
    let owning_frames = enclosing_frames(&spans, &call_spans);
    // Direct definition calls never query local binders. Build both exact
    // lookup tables together at the first value call, then reuse them.
    let local_binders = std::cell::LazyCell::new(|| {
        let mut local_values = HashMap::with_hasher(IdHasher::default());
        for (expression, name) in &checked.resolution.expressions {
            work::count(Counter::SummaryComparisons);
            if let ResolvedName::Local(local) = name {
                local_values.entry(*expression).or_insert(*local);
            }
        }
        let mut local_functions = HashMap::with_hasher(IdHasher::default());
        for (statement, local) in &checked.resolution.local_functions {
            work::count(Counter::SummaryComparisons);
            local_functions
                .entry(*local)
                .or_insert(NameSite::Statement(*statement));
        }
        (local_values, local_functions)
    });
    let local_binder = |value: ExprId| {
        work::count(Counter::SummaryComparisons);
        let (local_values, local_functions) = &*local_binders;
        local_values
            .get(&value)
            .and_then(|local| local_functions.get(local))
            .copied()
            .or_else(|| {
                let site = NameSite::Expression(value);
                frame_of(site).is_some().then_some(site)
            })
    };

    // Written calls first, then the checker's planned instantiations; a
    // planned instance at a written call's own site adds nothing.
    let mut calls: Vec<SummaryCall> = vec![];
    let mut written = HashSet::with_hasher(IdHasher::default());
    let mut call_members = HashSet::with_hasher(IdHasher::default());
    // Argument vectors have equality-only TypeIds. Assign exact, local
    // membership ordinals; they never order the published calls.
    let mut arguments_index: HashMap<&'body [GenericArgument], usize, IdHasher> =
        HashMap::default();
    let mut push = |calls: &mut Vec<SummaryCall>,
                    expression: ExprId,
                    owning_frame: u32,
                    callee: DefId,
                    binder: SiteRef,
                    arguments: &'body [GenericArgument],
                    target_frame: Option<u32>,
                    planned: bool| {
        let site = span(NameSite::Expression(expression));
        let class = if staged_frames[owning_frame as usize] {
            CallClass::ComptimeExecuted
        } else if planned && !written.contains(&SummaryKey((expression, callee))) {
            CallClass::StaticSelection
        } else {
            CallClass::Runtime
        };
        if !planned {
            written.insert(SummaryKey((expression, callee)));
        }
        work::count_by(Counter::SummaryComparisons, arguments.len() as u64 + 1);
        let arguments_id = if arguments.is_empty() {
            0
        } else {
            let next = arguments_index.len() + 1;
            *arguments_index.entry(arguments).or_insert(next)
        };
        let key = SummaryKey((
            (site.anchor, site.start, site.end),
            callee,
            binder,
            owning_frame,
            arguments_id,
        ));
        if call_members.insert(key) {
            calls.push(SummaryCall {
                callee,
                binder,
                arguments: arguments.into(),
                owning_frame,
                target_frame,
                class,
                site,
            });
        }
    };
    for (index, call) in checked.calls.iter().enumerate() {
        match &call.target {
            CallTarget::Definition(callee) => push(
                &mut calls,
                call.expression,
                owning_frames[index],
                DefId::intern(callee),
                LexicalOwner::first(callee).site(NameSite::Item),
                &call.generic_arguments,
                None,
                false,
            ),
            CallTarget::Value(value) => {
                let Some(binder) = local_binder(*value) else {
                    continue;
                };
                push(
                    &mut calls,
                    call.expression,
                    owning_frames[index],
                    lexical.def,
                    lexical.site(binder),
                    &call.generic_arguments,
                    frame_of(binder),
                    false,
                );
            }
            CallTarget::Constructor(_) => (),
        }
    }
    for (index, instance) in checked.instantiations.iter().enumerate() {
        push(
            &mut calls,
            instance.expression,
            owning_frames[checked.calls.len() + index],
            DefId::intern(&instance.callee),
            LexicalOwner::first(&instance.callee).site(NameSite::Item),
            &instance.arguments,
            None,
            true,
        );
    }

    let staged: Vec<(NameSite, Box<[SizeParameterRef]>)> = checked
        .expressions
        .iter()
        .filter(|row| comptime(row.expression))
        .filter_map(|row| {
            let inputs =
                hirsel_check_protocol::body::comptime_inputs(t, syntax, checked, row.expression);
            (!inputs.is_empty()).then(|| (NameSite::Expression(row.expression), inputs.into()))
        })
        .collect();

    // A frame captures the enclosing size parameters its own or an inner
    // frame's calls and staged inputs name.
    let mut captured: Vec<BTreeSet<SizeParameterRef>> = vec![BTreeSet::new(); frames.len()];
    let mut capture = |frame: u32, parameter: SizeParameterRef| {
        let BinderRef::Body { scope, .. } = parameter else {
            return;
        };
        if scope.owner != lexical {
            return;
        }
        let Some(declaring) = frame_of(scope.site) else {
            return;
        };
        let mut at = frame;
        while at != declaring {
            captured[at as usize].insert(parameter);
            match frames[at as usize].parent {
                Some(parent) => at = parent,
                None => break,
            }
        }
    };
    for call in &calls {
        for argument in call.arguments.iter() {
            for parameter in size_parameters(t, argument) {
                capture(call.owning_frame, parameter);
            }
        }
    }
    for (site, inputs) in &staged {
        let frame = frame_of(*site).expect("staged region frame");
        for parameter in inputs.iter() {
            capture(frame, *parameter);
        }
    }
    for (frame, captured) in frames.iter_mut().zip(captured) {
        frame.size_bindings = captured
            .into_iter()
            .map(|parameter| (parameter, Size::Parameter(parameter)))
            .collect();
    }

    let mut edges = vec![];
    let mut edge_members = HashSet::with_hasher(IdHasher::default());
    for instance in &checked.instantiations {
        let callee = DefId::intern(&instance.callee);
        let binder = LexicalOwner::first(&instance.callee).site(NameSite::Item);
        let site = span(NameSite::Expression(instance.expression));
        for (ordinal, argument) in instance.arguments.iter().enumerate() {
            let to = match argument {
                GenericArgument::Type(_) => GenericParameterRef::Type(TypeParameterRef::body(
                    binder,
                    TypeParamId(ordinal as u32),
                )),
                GenericArgument::Size(_) => GenericParameterRef::Size(SizeParameterRef::body(
                    binder,
                    SizeParamId(ordinal as u32),
                )),
            };
            for (parameter, grows) in parameters(t, argument, None) {
                let edge: GrowthEdge = ((lexical.def, parameter), (callee, to), grows, site);
                if edge_members.insert(SummaryKey((
                    edge.0,
                    edge.1,
                    edge.2,
                    (site.anchor, site.start, site.end),
                ))) {
                    edges.push(edge);
                }
            }
        }
    }

    (
        DefinitionSummary {
            owner: lexical.def,
            frames: frames.into(),
            calls: calls.into(),
            staged: staged.into(),
        },
        ParameterGrowth {
            edges: edges.into(),
        },
    )
}

/// GEN-5 over a growth graph: the sites of growing edges that lie on a
/// parameter cycle, in source order. Depends only on the edge set.
pub fn growing_sites(edges: &[GrowthEdge]) -> Vec<Span> {
    use std::collections::BTreeMap;
    let mut vertices = BTreeMap::new();
    for (from, to, _, _) in edges {
        for vertex in [from, to] {
            let next = vertices.len();
            vertices.entry(vertex).or_insert(next);
        }
    }
    let mut graph = vec![Vec::new(); vertices.len()];
    for (from, to, _, _) in edges {
        graph[vertices[from]].push(vertices[to]);
    }
    // An edge lies on a cycle exactly when its endpoints share an SCC.
    // Reuse the iterative Tarjan walk, including targets with no outgoing edges.
    let mut component = vec![0; vertices.len()];
    for (id, members) in hirsel_graph::components(&graph).into_iter().enumerate() {
        for vertex in members {
            component[vertex] = id;
        }
    }
    let mut sites = BTreeMap::new();
    for (from, to, _, site) in edges.iter().filter(|edge| edge.2) {
        if component[vertices[from]] == component[vertices[to]] {
            sites.insert((site.anchor, site.start, site.end), *site);
        }
    }
    sites.into_values().collect()
}

// The sets are membership indices only. Output vectors always
// retain the first occurrence, including its class and target frame.
#[derive(Eq, PartialEq)]
struct SummaryKey<T>(T);
impl<T: std::hash::Hash> std::hash::Hash for SummaryKey<T> {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        work::count(Counter::SummaryComparisons);
        self.0.hash(state);
    }
}
impl<T: Ord> Ord for SummaryKey<T> {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        summary_compare(&self.0, &other.0)
    }
}
impl<T: Ord> PartialOrd for SummaryKey<T> {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}
fn summary_compare<T: Ord>(a: &T, b: &T) -> std::cmp::Ordering {
    work::count(Counter::SummaryComparisons);
    a.cmp(b)
}
fn span_order(at: Span) -> (u32, std::cmp::Reverse<u32>) {
    (at.start, std::cmp::Reverse(at.end))
}

/// Source-ordered frames with nonincreasing ends. A later frame with a
/// larger end dominates each popped frame for every later starting position.
/// Equal spans remain on the stack, preserving the previous last match.
struct SpanNesting<'a> {
    spans: &'a [Span],
    stack: Vec<u32>,
}
impl<'a> SpanNesting<'a> {
    fn new(spans: &'a [Span]) -> Self {
        Self {
            spans,
            stack: Vec::new(),
        }
    }
    fn enclosing(&mut self, at: Span) -> u32 {
        while let Some(&index) = self.stack.last() {
            let span = self.spans[index as usize];
            work::count(Counter::SummaryComparisons);
            if span.start <= at.start && span.end >= at.end {
                return index;
            }
            self.stack.pop();
        }
        0
    }
    fn enter(&mut self, index: u32) -> u32 {
        let parent = self.enclosing(self.spans[index as usize]);
        self.stack.push(index);
        parent
    }
    fn owner(&self, at: Span) -> u32 {
        let containing = self.stack.partition_point(|&index| {
            work::count(Counter::SummaryComparisons);
            self.spans[index as usize].end >= at.end
        });
        containing.checked_sub(1).map_or(0, |i| self.stack[i])
    }
}
fn enclosing_frames(spans: &[Span], queries: &[Span]) -> Vec<u32> {
    if spans.len() == 1 {
        work::count_by(Counter::SummaryComparisons, queries.len() as u64);
        return vec![0; queries.len()];
    }
    let mut ordered: Vec<_> = (0..queries.len()).collect();
    ordered.sort_by(|&a, &b| summary_compare(&span_order(queries[a]), &span_order(queries[b])));
    let mut owners = vec![0; queries.len()];
    let mut nesting = SpanNesting::new(spans);
    let mut next = 1;
    for query in ordered {
        while next < spans.len()
            && summary_compare(&spans[next].start, &queries[query].start).is_le()
        {
            nesting.enter(next as u32);
            next += 1;
        }
        owners[query] = nesting.owner(queries[query]);
    }
    owners
}

pub fn source_site(syntax: &hirsel_syntax::ItemSyntax, site: NameSite) -> Span {
    let tree = &syntax.tree;
    match site {
        NameSite::Item => syntax.item.origin.span,
        NameSite::Statement(id) => tree.statements[id.0 as usize].origin.span,
        NameSite::Expression(id) => tree.expressions[id.0 as usize].origin.span,
        NameSite::Pattern(id) => tree.patterns[id.0 as usize].origin.span,
        NameSite::Type(id) => tree.types[id.0 as usize].origin.span,
    }
}

/// Every size parameter an argument names, at any depth of its type.
fn size_parameters(t: &TypeInterner, argument: &GenericArgument) -> BTreeSet<SizeParameterRef> {
    parameters(t, argument, None)
        .into_iter()
        .filter_map(|(parameter, _)| match parameter {
            GenericParameterRef::Size(parameter) => Some(parameter),
            GenericParameterRef::Type(_) => None,
        })
        .collect()
}

/// Owner parameters inside one argument, each with whether it is nested
/// (strict growth). A callable type introduces a binder, so `Bound` references
/// name the owner only at the owner's own depth.
pub fn parameters(
    types: &TypeInterner,
    argument: &GenericArgument,
    owner_binder: Option<SiteRef>,
) -> Vec<(GenericParameterRef, bool)> {
    let mut result = BTreeSet::new();
    let size = |size: &Size, depth: u32, nested: bool, result: &mut BTreeSet<_>| {
        if let Size::Parameter(parameter) = size
            && let Some(parameter) = owned(*parameter, depth, owner_binder)
        {
            result.insert((GenericParameterRef::Size(parameter), nested));
        }
    };
    let root = match argument {
        GenericArgument::Type(ty) => *ty,
        GenericArgument::Size(s) => {
            size(s, 0, false, &mut result);
            return result.into_iter().collect();
        }
    };
    let mut todo: Vec<(TypeId, bool, u32)> = vec![(root, false, 0)];
    let mut seen = HashSet::new();
    while let Some((ty, nested, depth)) = todo.pop() {
        if !seen.insert((ty, nested, depth)) {
            continue;
        }
        match &types.get(ty).expect("instance argument type").kind {
            TypeKind::Parameter(parameter) => {
                if let Some(parameter) = owned(*parameter, depth, owner_binder) {
                    result.insert((GenericParameterRef::Type(parameter), nested));
                }
            }
            TypeKind::Tuple(ts) => todo.extend(ts.iter().map(|t| (*t, true, depth))),
            TypeKind::Record(fs) => todo.extend(fs.iter().map(|f| (f.ty, true, depth))),
            TypeKind::Variant(vs) => todo.extend(
                vs.iter()
                    .flat_map(|v| v.fields.iter().map(move |(_, t)| (*t, true, depth))),
            ),
            TypeKind::Named { arguments, .. } | TypeKind::Builtin { arguments, .. } => {
                for argument in arguments.iter() {
                    match argument {
                        GenericArgument::Type(t) => todo.push((*t, true, depth)),
                        GenericArgument::Size(s) => size(s, depth, true, &mut result),
                    }
                }
            }
            TypeKind::Box(t) => todo.push((*t, true, depth)),
            TypeKind::Function(f) => {
                todo.extend(f.parameters.iter().map(|p| (p.ty, true, depth + 1)));
                todo.push((f.result, true, depth + 1));
            }
            _ => (),
        }
    }
    result.into_iter().collect()
}
/// The owner's view of a reference: its own body binders, or its signature
/// binder at the owner's depth when the store is signature-owned.
fn owned<P: Copy>(
    reference: BinderRef<P>,
    depth: u32,
    owner_binder: Option<SiteRef>,
) -> Option<BinderRef<P>> {
    match reference {
        BinderRef::Body { .. } => Some(reference),
        BinderRef::Bound {
            depth: bound,
            parameter,
        } if bound == depth => owner_binder.map(|scope| BinderRef::Body { scope, parameter }),
        BinderRef::Bound { .. } => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn span_queries_preserve_crossing_ranges_equal_spans_and_inclusive_boundaries() {
        let span = |(start, end)| Span {
            start,
            end,
            ..Span::default()
        };
        let spans = [
            (0, 200),
            (10, 50),
            (20, 80),
            (30, 40),
            (100, 140),
            (100, 140),
            (110, 120),
        ]
        .map(span);
        let mut nesting = SpanNesting::new(&spans);
        let parents: Vec<_> = (1..spans.len()).map(|i| nesting.enter(i as u32)).collect();
        assert_eq!(parents, [0, 0, 2, 0, 4, 5]);
        let queries = [
            (25, 90),
            (26, 38),
            (105, 150),
            (106, 130),
            (100, 140),
            (15, 45),
            (35, 38),
            (100, 100),
            (140, 140),
            (201, 201),
        ]
        .map(span);
        let expected = [0, 2, 0, 5, 5, 1, 3, 5, 5, 0];
        assert_eq!(enclosing_frames(&spans, &queries), expected);
        // The defining relation is the last containing frame, including for
        // recovered spans that cross lexical boundaries or lie outside the item.
        let queries: Vec<_> = (0..=201)
            .flat_map(|start| (start..=201).map(move |end| span((start, end))))
            .collect();
        let expected: Vec<_> = queries
            .iter()
            .map(|at| {
                spans
                    .iter()
                    .rposition(|s| s.start <= at.start && s.end >= at.end)
                    .unwrap_or(0) as u32
            })
            .collect();
        assert_eq!(enclosing_frames(&spans, &queries), expected);
    }
}
