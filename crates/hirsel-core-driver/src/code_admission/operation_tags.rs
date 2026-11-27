use super::{Backend, BackendSemantics, EngineTag, NativeSemantics};
use hirsel_core_ids::ContentHash;
use hirsel_core_ir::{
    AddressBase, AddressId, AddressInfo, Arg, Block, Body, CallId, CallSite, Callee, HostOp, Inst,
    Module, Param, PoolRange, RtOp, SigId, Terminator, Ty, TypeId, ValueId, Wait, normal, text,
};
use hirsel_core_layout::flow;
use std::collections::BTreeSet;
use std::fmt::Write as _;

fn runtime_probes() -> Module {
    const ITEM: TypeId = TypeId(0);
    let operations = [
        RtOp::Spawn { x: ITEM, r: ITEM },
        RtOp::Join { r: ITEM },
        RtOp::Cancel,
        RtOp::CheckCancel,
        RtOp::ChannelNew { item: ITEM },
        RtOp::ChannelSender,
        RtOp::Send { item: ITEM },
        RtOp::TrySend { item: ITEM },
        RtOp::Receive { item: ITEM },
        RtOp::TryReceive { item: ITEM },
        RtOp::ReceiveUntil { item: ITEM },
        RtOp::LatestNew { item: ITEM },
        RtOp::LatestHandle,
        RtOp::LatestPublish,
        RtOp::LatestRead,
        RtOp::DestId,
        RtOp::CompareBytes,
        RtOp::Sha256Blocks,
        RtOp::Sha512Blocks,
        RtOp::CauseNext,
        RtOp::CauseValue,
        RtOp::ArgsLen,
        RtOp::ArgsGet,
        RtOp::IsolateEnter,
        RtOp::IsolateLeave,
        RtOp::IsolateCatch,
        RtOp::StepEnter,
        RtOp::StepLeave,
        RtOp::StepCatch,
        RtOp::WorkSteps,
        RtOp::RootLimit,
        RtOp::Argon2id,
        RtOp::LaunchWord,
        RtOp::LaunchTarget { view: ITEM },
        RtOp::DescribeEmit,
        RtOp::Sha1Blocks,
        RtOp::Sha1Compress,
        RtOp::Xxh3Short,
        RtOp::Xxh3Stripes,
        RtOp::LaunchHandle,
        RtOp::MapHashBytes,
        RtOp::MapHashWord,
        RtOp::MapHashStart,
        RtOp::MapHashAddWord,
        RtOp::MapHashAddBytes,
        RtOp::MapHashFinish,
        RtOp::Ed25519Verify,
        RtOp::P256Verify,
        RtOp::P256VerifyDer,
        RtOp::ConnNew { item: ITEM },
        RtOp::ConnClient,
        RtOp::ConnSend { item: ITEM },
        RtOp::ConnTrySend { item: ITEM },
        RtOp::ConnReceive { item: ITEM },
        RtOp::ConnTryReceive { item: ITEM },
        RtOp::SpawnUnit { x: ITEM, r: ITEM },
        RtOp::FrameRecord,
        RtOp::Returnable(Wait::Host(HostOp::RecvBounded)),
        RtOp::Returnable(Wait::Host(HostOp::RecvReady)),
        RtOp::Returnable(Wait::Host(HostOp::SendPair)),
        RtOp::ZeroBytes,
        RtOp::MapPlaceWord,
        RtOp::MapPlaceBytes,
        RtOp::MapStrongWord,
        RtOp::MapStrongBytes,
        RtOp::MapStrongStart,
        RtOp::ReceiveRec { item: ITEM },
        RtOp::Crc32c,
        RtOp::SecretShare,
        RtOp::Crc64Nvme,
        RtOp::Crc32cCombine,
        RtOp::Crc64NvmeCombine,
        RtOp::Transfer { x: ITEM, r: ITEM },
        RtOp::SharedNew { item: ITEM },
        RtOp::SharedShare,
        RtOp::SharedRelease { item: ITEM },
        RtOp::P384Verify,
        RtOp::P384VerifyDer,
        RtOp::RsaVerifyPkcs1Sha256,
        RtOp::RsaVerifyPssSha256,
        RtOp::Closed(0),
    ]
    .into_iter()
    .chain(HostOp::ALL.iter().copied().map(RtOp::Host))
    .chain(flow::Op::ALL.iter().copied().map(RtOp::Flow))
    .collect::<Vec<_>>();

    let mut source = String::from(
        "core v1\ntype t0 = own \"Payload\" { a: u64, b: u64, c: u64 }\nsig g0 = () -> ()\n",
    );
    for i in 0..operations.len() {
        writeln!(
            source,
            "inst n{i} = test@0/admission/operation_{i} [] []\nfn f{i} = n{i} g0 {{\nb0:\nreturn\n}}"
        )
        .unwrap();
    }
    let mut module = text::parse(&source).unwrap();
    for (function, op) in module.functions.iter_mut().zip(operations) {
        let sig = op.signature();
        let signature = SigId(module.signatures.len() as u32);
        let mut body = Body::default();
        let args = sig.params.iter().map(|param| {
            let value = ValueId(body.values.len() as u32);
            match *param {
                Param::Value(ty) => {
                    body.values.push(ty);
                    Arg::Value(value)
                }
                Param::Address { place, .. } => {
                    body.values.push(Ty::Ptr);
                    let address = AddressId(body.addresses.len() as u32);
                    body.addresses.push(AddressInfo {
                        base: AddressBase::Value(value),
                        ty: place,
                        steps: PoolRange::EMPTY,
                    });
                    body.insts.push(Inst::Address {
                        address,
                        fail: None,
                    });
                    Arg::Address(address)
                }
            }
        });
        let args = PoolRange::push(&mut body.pools.args, args);
        let params = PoolRange::push(
            &mut body.pools.values,
            (0..body.values.len() as u32).map(ValueId),
        );
        let first_result = body.values.len() as u32;
        body.values.extend_from_slice(&sig.results);
        let results = PoolRange::push(
            &mut body.pools.values,
            (first_result..body.values.len() as u32).map(ValueId),
        );
        body.calls.push(CallSite {
            callee: Callee::Runtime(op),
            signature,
            args,
            results,
            fail: None,
            cold: false,
        });
        body.insts.push(Inst::Call { call: CallId(0) });
        body.inst_anchors.resize(body.insts.len(), None);
        body.blocks.push(Block {
            params,
            insts: PoolRange::new(0, body.insts.len() as u32),
            anchor: None,
            decision: false,
        });
        body.terminators.push(Terminator::Return(results));
        function.signature = signature;
        function.body = Some(body);
        module.signatures.push(sig);
    }
    module
}

#[test]
fn every_admitted_tag_lowers_in_both_engines() {
    let samples = [
        (
            "arithmetic",
            include_str!("../../../hirsel-core-ir/samples/arith.core"),
        ),
        (
            "comparison",
            include_str!("../../../hirsel-core-ir/samples/compare-convert.core"),
        ),
        (
            "scalars",
            include_str!("../../../hirsel-core-ir/samples/scalar-rows.core"),
        ),
        (
            "sequences",
            include_str!("../../../hirsel-core-ir/samples/sequences.core"),
        ),
        (
            "counted storage",
            include_str!("../../../hirsel-core-ir/samples/counted.core"),
        ),
        (
            "channel cleanup",
            include_str!("../../../hirsel-core-ir/samples/receiver-chain.core"),
        ),
        (
            "kernel",
            include_str!("../../../hirsel-core-ir/samples/kernel.core"),
        ),
        (
            "budget",
            include_str!("../../../hirsel-core-ir/samples/admission.core"),
        ),
        (
            "moves",
            include_str!("../../../hirsel-core-ir/samples/counted-frames.core"),
        ),
        (
            "variants",
            include_str!("../../../hirsel-core-ir/samples/variants.core"),
        ),
        ("other instructions", include_str!("operation_tags.core")),
    ];
    let mut exercised = [const { BTreeSet::new() }; 5];
    let modules = [("runtime operation probes", runtime_probes())]
        .into_iter()
        .chain(samples.into_iter().map(|(name, source)| {
            (
                name,
                text::parse(source).unwrap_or_else(|err| panic!("{name}: {err:?}")),
            )
        }));
    for (name, module) in modules {
        let layouts = hirsel_core_layout::compute(&module.types).unwrap();
        hirsel_cranelift::core::emit_object_with_layouts(
            &module,
            &layouts,
            &hirsel_cranelift::core::Options {
                target: "x86_64-unknown-linux-gnu",
                optimize: false,
                debug_lines: false,
            },
            ContentHash([0; 32]),
        )
        .unwrap_or_else(|err| panic!("Cranelift {name}: {err}"));
        hirsel_llvm_lazy::core::emit_object(&module, hirsel_llvm_lazy::core::Optimization::None)
            .unwrap_or_else(|err| panic!("LLVM {name}: {err}"));

        for body in module
            .functions
            .iter()
            .filter_map(|function| function.body.as_ref())
        {
            exercised[0].extend(body.insts.iter().map(normal::instruction_tag));
            exercised[4].extend(body.terminators.iter().map(normal::terminator_tag));
            for call in &body.calls {
                if let Callee::Runtime(op) = call.callee {
                    exercised[1].insert(normal::runtime_tag(&op));
                    match op {
                        RtOp::Host(op) | RtOp::Returnable(Wait::Host(op)) => {
                            exercised[2].insert(normal::host_tag(&op));
                        }
                        RtOp::Flow(op) => {
                            exercised[3].insert(normal::flow_tag(&op));
                        }
                        _ => {}
                    }
                }
            }
        }
    }

    let families = [
        EngineTag::Instruction,
        EngineTag::Runtime,
        EngineTag::Host,
        EngineTag::Flow,
        EngineTag::Terminator,
    ];
    for (family, exercised) in families.into_iter().zip(exercised) {
        for number in 0..=u8::MAX {
            let tag = family(number);
            for backend in [Backend::Cranelift, Backend::Llvm] {
                assert_eq!(
                    NativeSemantics.supports_tag(backend, tag),
                    exercised.contains(&number),
                    "{backend:?} {tag:?}: admission and native emission disagree"
                );
            }
        }
    }
}
