//! Return destinations belong to the run boundary. Use the ordinary FORM drop
//! instruction so an Error's owner is cleaned before the process reports it.
use hirsel_core_ids::DefPath;
use hirsel_core_ir::*;

pub(super) fn result_cleanup(source: &Module, result: TypeId, entry: &DefPath) -> Module {
    let mut module = Module {
        types: source.types.clone(),
        signatures: source.signatures.clone(),
        instances: source.instances.clone(),
        functions: source
            .functions
            .iter()
            .map(|f| Function {
                instance: f.instance,
                signature: f.signature,
                body: None,
            })
            .collect(),
        ..Module::default()
    };
    let mut path = entry.clone();
    path.0.name.push_str(".$result_cleanup");
    let instance = InstanceId(module.instances.len() as u32);
    module.instances.push(InstanceKey {
        definition: path,
        types: vec![],
        dictionaries: vec![],
    });
    let signature = SigId(module.signatures.len() as u32);
    module.signatures.push(Signature {
        params: vec![Param::Address {
            ty: result,
            loan: Loan::Take,
        }],
        results: vec![],
        dest: None,
        never: false,
    });
    module.functions.push(Function {
        instance,
        signature,
        body: Some(Body {
            values: vec![Ty::Ptr],
            addresses: vec![AddressInfo {
                base: AddressBase::Value(ValueId(0)),
                steps: PoolRange::EMPTY,
                ty: PlaceTy::Declared(result),
            }],
            blocks: vec![
                Block {
                    params: PoolRange::new(0, 1),
                    insts: PoolRange::new(0, 2),
                    anchor: None,
                },
                Block {
                    params: PoolRange::EMPTY,
                    insts: PoolRange::new(2, 0),
                    anchor: None,
                },
            ],
            terminators: vec![Terminator::Return(PoolRange::EMPTY), Terminator::Propagate],
            insts: vec![
                Inst::Address {
                    address: AddressId(0),
                    fail: None,
                },
                Inst::Drop {
                    address: AddressId(0),
                    ty: result,
                    fail: Some(BlockId(1)),
                },
            ],
            inst_anchors: vec![None, None],
            pools: Pools {
                values: vec![ValueId(0)],
                ..Pools::default()
            },
            ..Body::default()
        }),
    });
    module
}
