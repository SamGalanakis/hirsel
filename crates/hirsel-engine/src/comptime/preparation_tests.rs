use super::*;
use hirsel_types::{CheckedBody, Elaboration, OwnershipFacts, Resolution, Type, UseKind};
use std::collections::BTreeMap;

fn fixture() -> (Arc<hirsel_types::TypeInterner>, AnalyzedBody, Signature) {
    let mut engine = QueryEngine::new(default_environment(), crate::phases::engine_only_adapters());
    let file = engine
        .sources
        .add(
            "prepare.hir",
            "fn root()->(){comptime { LEAF; comptime () }}".into(),
        )
        .unwrap();
    let syntax = engine
        .parse_file(file, &default_context())
        .unwrap()
        .value
        .items
        .remove(0);
    let types = engine.types.clone();
    let unit = types.intern(Type::unit());
    let boolean = types.intern(Type::boolean());
    let mut resolution = Resolution::default();
    for (i, expression) in syntax.tree.expressions.iter().enumerate() {
        if matches!(expression.kind, ExprKind::Path(_)) {
            resolution.expressions.push((
                ExprId(i as u32),
                ResolvedName::Definition(DefPath::core("LEAF")),
            ));
        }
    }
    let checked = CheckedBody {
        lexical: syntax.owner(),
        places: vec![],
        calls: vec![],
        tails: vec![],
        bindings: vec![],
        patterns: vec![],
        resolution,
        expressions: syntax
            .tree
            .expressions
            .iter()
            .enumerate()
            .map(|(i, _)| Elaboration {
                construction_destination: None,
                expression: ExprId(i as u32),
                ty: unit,
                usage: UseKind::Take,
                instantiated_arguments: vec![],
            })
            .collect(),
        instantiations: vec![],
        closures: vec![],
    };
    let signature = Signature {
        definition: DefId::intern(&syntax.item.definition),
        kind: SignatureKind::Const {
            ty: unit,
            generics: Default::default(),
        },
        metadata: Default::default(),
        fingerprint: syntax.text_hash,
    };
    (
        types,
        AnalyzedBody {
            typed: Arc::new(TypedBody {
                syntax,
                checked,
                plans: Default::default(),
                unit,
                boolean,
            }),
            ownership: OwnershipFacts::default(),
            results: vec![],
            call_allocations: vec![],
        },
        signature,
    )
}
struct Source {
    signature: Arc<Signature>,
    signatures: usize,
    constants: usize,
    evaluations: BTreeMap<ExprId, usize>,
    substitutions: Vec<Vec<ExprId>>,
    fail: bool,
}
impl ComptimeValueSource for Source {
    fn signature(&mut self, _: &DefPath) -> Result<Arc<Signature>, QueryError> {
        self.signatures += 1;
        Ok(self.signature.clone())
    }
    fn constant(&mut self, _: &DefPath, _: Span) -> Result<Materialized, QueryError> {
        self.constants += 1;
        if self.fail {
            Err(QueryError::NotImplemented)
        } else {
            Ok(value(match self.signature.kind {
                SignatureKind::Const { ty, .. } => ty,
                _ => unreachable!(),
            }))
        }
    }
    fn evaluate(&mut self, input: EvaluationInput<'_>) -> Result<Materialized, QueryError> {
        *self.evaluations.entry(input.expression).or_default() += 1;
        self.substitutions
            .push(input.constants.iter().map(|(id, _)| *id).collect());
        Ok(value(input.ty))
    }
}
fn value(ty: TypeId) -> Materialized {
    Materialized {
        root: hirsel_tir::ValueNode {
            ty,
            kind: ValueKind::Constant(Constant::Unit),
        },
    }
}
fn source(signature: &Signature) -> Source {
    Source {
        signature: Arc::new(signature.clone()),
        signatures: 0,
        constants: 0,
        evaluations: BTreeMap::new(),
        substitutions: vec![],
        fail: false,
    }
}
#[test]
fn constants_precede_inner_expressions_and_each_sees_earlier_substitutions() {
    hirsel_intern::jobs::with_compiler_stack(|| {
        let (types, body, signature) = fixture();
        let mut source = source(&signature);
        let values = prepare_values(&types, &body, &[], &signature, &mut source).unwrap();
        assert_eq!(
            values.len(),
            3,
            "one constant, then two nested comptime expressions"
        );
        assert_eq!(source.signatures, 1);
        assert_eq!(source.constants, 1);
        assert_eq!(source.evaluations.len(), 2);
        assert!(source.evaluations.values().all(|calls| *calls == 1));
        assert_eq!(
            source.substitutions,
            [vec![values[0].0], vec![values[0].0, values[1].0]]
        );
    });
}
#[test]
fn failed_dependency_stops_preparation_before_later_expression_selection() {
    hirsel_intern::jobs::with_compiler_stack(|| {
        let (types, body, signature) = fixture();
        let mut source = source(&signature);
        source.fail = true;
        assert!(matches!(
            prepare_values(&types, &body, &[], &signature, &mut source),
            Err(QueryError::NotImplemented)
        ));
        assert_eq!(source.signatures, 1);
        assert_eq!(source.constants, 1);
        assert!(source.evaluations.is_empty());
    });
}
