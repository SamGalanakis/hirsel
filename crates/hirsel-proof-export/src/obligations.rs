//! Obligation identity, computed from TIR alone.
use crate::wire::Cx;
use hirsel_codec::{DecodeError, Reader, Snapshot, Writer, snapshot_enum, snapshot_struct};
use hirsel_diag::RuleId;
use hirsel_span::Span;
use hirsel_syntax::{BinaryOp, ContentHash, DefPath, UnaryOp};
use hirsel_tir::{
    BlockId, Body, BodyId, Callee, Constructor, Definition, Instruction, InstructionKind, LocalId,
    Operand, PlaceId, Projection, Rvalue, Terminator,
};
use hirsel_types::{Primitive, Signature, SignatureKind, TypeInterner, TypeKind};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fmt;

/// PS-2. Closed per format version; later clause forms are new kinds.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub enum ObligationKind {
    Check,
    FailureEdge,
}
impl ObligationKind {
    pub const ALL: [Self; 2] = [Self::Check, Self::FailureEdge];
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Check => "check",
            Self::FailureEdge => "failure-edge",
        }
    }
}
impl Snapshot<Cx> for ObligationKind {
    fn write(&self, w: &mut Writer<Cx>) {
        w.text(self.as_str());
    }
    fn read(r: &mut Reader<'_, Cx>) -> Result<Self, DecodeError> {
        let text = r.text()?;
        Self::ALL
            .into_iter()
            .find(|kind| kind.as_str() == text)
            .ok_or_else(|| r.error(format!("unknown obligation kind {text:?}")))
    }
}

/// PS-1. Positional, never containing arena indices, spans or hashes.
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub struct ObligationId {
    pub definition: DefPath,
    pub rule: RuleId,
    pub kind: ObligationKind,
    pub ordinal: u32,
}
snapshot_struct!(ObligationId {
    definition,
    rule,
    kind,
    ordinal
} in Cx);
impl fmt::Display for ObligationId {
    /// `pkg@ver/mod/name#RULE/kind/ordinal`
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}#{}/{}/{}",
            self.definition,
            self.rule,
            self.kind.as_str(),
            self.ordinal
        )
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SitePosition {
    Instruction(u32),
    Terminator,
}
snapshot_enum!(SitePosition { Instruction(i), Terminator } in Cx);

/// One TIR site of an obligation; `span` is the instruction's, or the block's
/// for a terminator.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Site {
    pub body: BodyId,
    pub block: BlockId,
    pub position: SitePosition,
    pub span: Span,
}
snapshot_struct!(Site {
    body,
    block,
    position,
    span
} in Cx);

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Obligation {
    pub id: ObligationId,
    /// PS-6.
    pub checksum: ContentHash,
    pub sites: Vec<Site>,
}
snapshot_struct!(Obligation {
    id,
    checksum,
    sites
} in Cx);

/// Obligations without checksums, and the definitions called by name.
pub(crate) struct Table {
    pub obligations: Vec<(ObligationId, Vec<Site>)>,
    pub callees: BTreeSet<DefPath>,
    projected: BTreeMap<ObligationId, (BodyId, PlaceId)>,
}

/// Assigns ordinals per (rule, kind) in PS-3 order.
struct Builder<'d> {
    definition: &'d DefPath,
    counters: BTreeMap<(RuleId, ObligationKind), u32>,
    obligations: Vec<(ObligationId, Vec<Site>)>,
    /// MEM-13/14 obligations are per projected place (body, root and
    /// projection; loans and bindings give one access several PlaceIds):
    /// every instruction or terminator naming it is a site of its one
    /// obligation.
    places: HashMap<(usize, LocalId, Vec<Projection>, RuleId), usize>,
}
impl Builder<'_> {
    fn push(&mut self, rule: RuleId, kind: ObligationKind, site: Site) -> usize {
        let counter = self.counters.entry((rule.clone(), kind)).or_default();
        let id = ObligationId {
            definition: self.definition.clone(),
            rule,
            kind,
            ordinal: *counter,
        };
        *counter += 1;
        self.obligations.push((id, vec![site]));
        self.obligations.len() - 1
    }
    /// MEM-14 for a place with an Index projection, MEM-13 for one with a
    /// VariantField projection.
    fn place(&mut self, body: &Body, b: usize, place: PlaceId, site: &Site) {
        let Some(row) = body.places.get(place.0 as usize) else {
            return;
        };
        let projection = row.projection.to_vec();
        let index = projection.iter().any(|p| matches!(p, Projection::Index(_)));
        let variant = projection
            .iter()
            .any(|p| matches!(p, Projection::VariantField { .. }));
        for (code, present) in [
            (const { RuleId::of("MEM-14") }, index),
            (const { RuleId::of("MEM-13") }, variant),
        ] {
            if !present {
                continue;
            }
            let key = (b, row.root, projection.clone(), code.clone());
            match self.places.get(&key) {
                Some(index) => self.obligations[*index].1.push(site.clone()),
                None => {
                    let index = self.push(code, ObligationKind::FailureEdge, site.clone());
                    self.places.insert(key, index);
                }
            }
        }
    }
}

/// PS-2/PS-3 over one definition. `signature` returns a referenced
/// definition's public signature.
pub(crate) fn table<'a>(
    types: &TypeInterner,
    definition: &Definition,
    _signature: impl Fn(&DefPath) -> Option<&'a Signature>,
) -> Table {
    let mut builder = Builder {
        definition: definition.path.path(),
        counters: BTreeMap::new(),
        obligations: vec![],
        places: HashMap::new(),
    };
    let mut callees = BTreeSet::new();
    for (b, body) in definition.bodies.iter().enumerate() {
        for (k, block) in body.blocks.iter().enumerate() {
            for (i, instruction) in block.instructions.iter().enumerate() {
                let site = Site {
                    body: BodyId(b as u32),
                    block: BlockId(k as u32),
                    position: SitePosition::Instruction(i as u32),
                    span: instruction.span,
                };
                match &instruction.kind {
                    InstructionKind::Check { rule, .. } => {
                        builder.push(rule.clone(), ObligationKind::Check, site.clone());
                    }
                    InstructionKind::Assign { value, .. }
                        if body
                            .rvalues
                            .get(value.0 as usize)
                            .is_some_and(|rvalue| checked_arithmetic(types, body, rvalue)) =>
                    {
                        builder.push(
                            const { RuleId::of("SEM-1") },
                            ObligationKind::FailureEdge,
                            site.clone(),
                        );
                    }
                    _ => {}
                }
                for place in instruction_places(body, instruction) {
                    builder.place(body, b, place, &site);
                }
            }
            let site = Site {
                body: BodyId(b as u32),
                block: BlockId(k as u32),
                position: SitePosition::Terminator,
                span: block.span,
            };
            match &block.terminator {
                Terminator::Fail { rule, .. } => {
                    builder.push(rule.clone(), ObligationKind::FailureEdge, site.clone());
                }
                Terminator::Call {
                    callee: Callee::Definition { path, .. },
                    ..
                } => {
                    callees.insert(path.clone());
                }
                _ => {}
            }
            for place in terminator_places(&block.terminator) {
                builder.place(body, b, place, &site);
            }
        }
    }
    let mut canonical = HashMap::new();
    for (b, body) in definition.bodies.iter().enumerate() {
        for (p, place) in body.places.iter().enumerate() {
            canonical
                .entry((b, place.root, place.projection.to_vec()))
                .or_insert(PlaceId(p as u32));
        }
    }
    let projected = builder
        .places
        .iter()
        .map(|((body, root, projection, _), index)| {
            (
                builder.obligations[*index].0.clone(),
                (
                    BodyId(*body as u32),
                    canonical[&(*body, *root, projection.clone())],
                ),
            )
        })
        .collect();
    let obligations = builder.obligations;
    Table {
        obligations,
        callees,
        projected,
    }
}

fn integer(types: &TypeInterner, body: &Body, operand: &Operand) -> bool {
    let Some(place) = body.places.get(operand.place.0 as usize) else {
        return false;
    };
    matches!(
        types.get(place.ty).map(|row| &row.kind),
        Some(TypeKind::Primitive(
            Primitive::U8
                | Primitive::U16
                | Primitive::U32
                | Primitive::U64
                | Primitive::U128
                | Primitive::I8
                | Primitive::I16
                | Primitive::I32
                | Primitive::I64
                | Primitive::I128
        ))
    )
}

/// SEM-1 checked integer operations; comparisons, bitwise operations and
/// floats cannot fail.
pub(crate) fn checked_arithmetic(types: &TypeInterner, body: &Body, rvalue: &Rvalue) -> bool {
    match rvalue {
        Rvalue::Binary { op, left, .. } => {
            matches!(
                op,
                BinaryOp::Add
                    | BinaryOp::Sub
                    | BinaryOp::Mul
                    | BinaryOp::Div
                    | BinaryOp::Rem
                    | BinaryOp::Shl
                    | BinaryOp::Shr
            ) && integer(types, body, left)
        }
        Rvalue::Unary {
            op: UnaryOp::Neg,
            value,
        } => integer(types, body, value),
        _ => false,
    }
}

/// Places an instruction names, in field order.
fn instruction_places(body: &Body, instruction: &Instruction) -> Vec<PlaceId> {
    match &instruction.kind {
        InstructionKind::Defuse { place }
        | InstructionKind::EndOrigin(place)
        | InstructionKind::StorageDead { place, .. }
        | InstructionKind::BeginLoan { place, .. }
        | InstructionKind::Drop { place, .. } => vec![*place],
        InstructionKind::BindOrigin { source, place, .. } => vec![*source, *place],
        InstructionKind::BindFields { source, fields } => std::iter::once(*source)
            .chain(fields.iter().copied())
            .collect(),
        InstructionKind::Assign { destination, value } => {
            let mut places = vec![*destination];
            if let Some(rvalue) = body.rvalues.get(value.0 as usize) {
                places.extend(rvalue_places(rvalue));
            }
            places
        }
        InstructionKind::Replace {
            destination,
            value,
            displaced,
            ..
        } => vec![*destination, value.place, *displaced],
        InstructionKind::Check { condition, .. } => vec![condition.place],
        InstructionKind::StorageLive(_)
        | InstructionKind::EndScope(_)
        | InstructionKind::EndLoan { .. }
        | InstructionKind::RunCleanup(_)
        | InstructionKind::Charge(_) => vec![],
    }
}

fn rvalue_places(rvalue: &Rvalue) -> Vec<PlaceId> {
    match rvalue {
        Rvalue::Use(value) | Rvalue::Unary { value, .. } => vec![value.place],
        Rvalue::Binary { left, right, .. } => vec![left.place, right.place],
        Rvalue::Construct {
            constructor: Constructor::SortedMap { by },
            fields,
            ..
        } => std::iter::once(by.place)
            .chain(fields.iter().map(|f| f.value.place))
            .collect(),
        Rvalue::Construct { fields, .. } => fields.iter().map(|f| f.value.place).collect(),
        Rvalue::Closure { captures, .. } => captures.iter().map(|c| c.place).collect(),
        Rvalue::Constant(_)
        | Rvalue::ReadConstant { .. }
        | Rvalue::FunctionReference { .. }
        | Rvalue::Size(_) => vec![],
    }
}

fn terminator_places(terminator: &Terminator) -> Vec<PlaceId> {
    match terminator {
        Terminator::Branch { condition, .. } => vec![condition.place],
        Terminator::Switch { value, .. } | Terminator::Return { value, .. } => vec![value.place],
        Terminator::Call {
            callee, arguments, ..
        } => {
            let mut places = match callee {
                Callee::Indirect { value, .. } => vec![value.place],
                Callee::Definition { .. } => vec![],
            };
            places.extend(arguments.iter().map(|a| a.value.place));
            places
        }
        Terminator::Goto(_)
        | Terminator::Match { .. }
        | Terminator::Fail { .. }
        | Terminator::Unreachable => vec![],
    }
}

/// The canonical place of a MEM-13/14 obligation: the first PlaceId with
/// its (body, root, projection), ignoring bindings and loans, as PS-2 does.
/// This reuses the obligation traversal; no export or identity changes.
pub fn projected_places(
    types: &TypeInterner,
    definition: &Definition,
) -> BTreeMap<ObligationId, (BodyId, PlaceId)> {
    table(types, definition, |_| None).projected
}
