use crate::support::*;
use hirsel_interp::{NoOperations, Outcome, Value, run_with_runtime};
use hirsel_syntax::{BinaryOp, DefPath};
use hirsel_tir::*;
use hirsel_types::{GenericArgument, ParamId, Primitive, QueryError, TypeKind};
use std::sync::Arc;

struct Fixture {
    definitions: Vec<DefinitionRef>,
    constant: MaterializedRef,
}
fn fixture() -> Fixture {
    let mut b = Builder::new();
    let before = b.int(Primitive::U32, 5);
    let data = b.local(TypeKind::Primitive(Primitive::U32), Storage::Temporary);
    let ty = b.body.places[data.0 as usize].ty;
    b.assign(
        data,
        Rvalue::ReadConstant {
            definition: path("DATA"),
            arguments: vec![],
        },
        3,
        CleanupId(0),
    );
    let sum = b.binary(
        BinaryOp::Add,
        before,
        data,
        TypeKind::Primitive(Primitive::U32),
        4,
        CleanupId(0),
    );
    let result = b.local(TypeKind::Primitive(Primitive::U32), Storage::Temporary);
    b.block(
        Terminator::Call {
            callee: Callee::Definition {
                path: path("helper"),
                arguments: vec![],
            },
            arguments: vec![CallArgument {
                parameter: ParamId(0),
                value: take(sum),
            }],
            returns: Some(CallReturn {
                destination: result,
                next: BlockId(1),
            }),
            on_failure: CleanupId(0),
        },
        5,
    );
    let main = Arc::new(b.finish("main", result));
    let mut helper = Builder::new();
    let argument = helper.parameter(TypeKind::Primitive(Primitive::U32));
    let helper = Arc::new(helper.finish("helper", argument));
    Fixture {
        definitions: vec![main, helper],
        constant: Arc::new(Materialized {
            root: ValueNode {
                ty,
                kind: ValueKind::Constant(Constant::Integer(7)),
            },
        }),
    }
}
fn configuration() -> RunConfiguration {
    RunConfiguration {
        entry: path("main"),
        hash_seed: 0,
        inputs: vec![],
    }
}
#[test]
fn completed_calls_release_definition_backing_before_the_next_demand() {
    struct Consumer {
        main: DefinitionRef,
        helper: Definition,
        last: std::sync::Weak<Definition>,
        demands: usize,
        constant: MaterializedRef,
    }
    impl TirProvider for Consumer {
        fn definition(&mut self, name: &DefPath) -> Result<DefinitionRef, QueryError> {
            if name == self.main.path.path() {
                return Ok(self.main.clone());
            }
            assert_eq!(name, self.helper.path.path());
            let definition = Arc::new(self.helper.clone());
            self.last = Arc::downgrade(&definition);
            self.demands += 1;
            Ok(definition)
        }
        fn constant(&mut self, _: &DefPath) -> Result<MaterializedRef, QueryError> {
            assert_eq!(self.demands, 1);
            assert!(
                self.last.upgrade().is_none(),
                "completed callee pinned by an optional execution cache"
            );
            Ok(self.constant.clone())
        }
        fn constant_instance(
            &mut self,
            _: &DefPath,
            _: &[u64],
        ) -> Result<MaterializedRef, QueryError> {
            Err(QueryError::NotImplemented)
        }
        fn constant_definitions(&mut self) -> Result<Vec<DefPath>, QueryError> {
            Ok(vec![])
        }
        fn instance_body(
            &mut self,
            _: &DefPath,
            _: &[GenericArgument],
        ) -> Result<Option<DefPath>, QueryError> {
            Ok(None)
        }
    }
    let fixture = fixture();
    let mut b = Builder::new();
    let before = b.int(Primitive::U32, 5);
    let first = b.local(TypeKind::Primitive(Primitive::U32), Storage::Temporary);
    b.block(
        Terminator::Call {
            callee: Callee::Definition {
                path: path("helper"),
                arguments: vec![],
            },
            arguments: vec![CallArgument {
                parameter: ParamId(0),
                value: take(before),
            }],
            returns: Some(CallReturn {
                destination: first,
                next: BlockId(1),
            }),
            on_failure: CleanupId(0),
        },
        3,
    );
    let data = b.local(TypeKind::Primitive(Primitive::U32), Storage::Temporary);
    b.assign(
        data,
        Rvalue::ReadConstant {
            definition: path("DATA"),
            arguments: vec![],
        },
        4,
        CleanupId(0),
    );
    let sum = b.binary(
        BinaryOp::Add,
        first,
        data,
        TypeKind::Primitive(Primitive::U32),
        5,
        CleanupId(0),
    );
    let result = b.local(TypeKind::Primitive(Primitive::U32), Storage::Temporary);
    b.block(
        Terminator::Call {
            callee: Callee::Definition {
                path: path("helper"),
                arguments: vec![],
            },
            arguments: vec![CallArgument {
                parameter: ParamId(0),
                value: take(sum),
            }],
            returns: Some(CallReturn {
                destination: result,
                next: BlockId(2),
            }),
            on_failure: CleanupId(0),
        },
        6,
    );
    let mut consumer = Consumer {
        main: Arc::new(b.finish("main", result)),
        helper: (*fixture.definitions[1]).clone(),
        last: Default::default(),
        demands: 0,
        constant: fixture.constant,
    };
    let mut operations = NoOperations;
    let runtime = runtime();
    let sources = sources();
    let configuration = configuration();
    let result = run_with_runtime(
        &mut consumer,
        &sources,
        &configuration,
        vec![],
        Default::default(),
        &mut operations,
        &runtime,
    )
    .unwrap();
    assert_eq!(
        consumer.demands, 2,
        "expired completed read must be demanded again"
    );
    assert!(
        matches!(result.outcome, Outcome::Returned { value: Value::Integer(n), .. } if n.bits() == 12)
    );
}
