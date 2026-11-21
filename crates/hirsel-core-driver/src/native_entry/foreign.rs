//! One shared pair of Abi adapters: Hirsel slot -> runtime -> System V C.
//! Each C thunk sees real typed parameters while the runtime only handles words.
use super::*;
use crate::foreign_header::{Abi, Argument, Return};
use cranelift_codegen::ir::{AbiParam, InstBuilder, MemFlagsData, Value};
use hirsel_core_ir::release_recipe::ForeignLeaf;
use hirsel_core_ir::{Loan, Param, PlaceTy, Signature, TypeId};
use hirsel_core_layout::{Body, Carried, Case, Field, FieldTy, Repr, Scalar, Shape, Ty};
use hirsel_cranelift::core::{NativeSignature, memory_layout as abi};
use std::collections::BTreeMap;

#[derive(Serialize)]
pub(super) struct Function<'a> {
    hirsel_symbol: String,
    c_symbol: String,
    header: &'a str,
}

impl<'a> Function<'a> {
    pub(super) fn collect(leaves: impl IntoIterator<Item = &'a ForeignLeaf>) -> Vec<Self> {
        leaves
            .into_iter()
            .flat_map(|leaf| {
                leaf.functions.iter().map(|function| Self {
                    hirsel_symbol: function.identity.symbol(),
                    c_symbol: format!("{}{}", leaf.symbols, function.identity.definition.0.name),
                    header: &function.header,
                })
            })
            .collect()
    }
}

pub(super) fn emit(object: &mut ObjectModule, functions: &[Function<'_>]) -> Result<bool> {
    let mut entries = Vec::with_capacity(functions.len());
    let mut kinds = BTreeMap::new();
    let mut waits = false;
    for (index, function) in functions.iter().enumerate() {
        let parsed = Abi::parse(function.header).ok_or("invalid checked foreign header")?;
        let thunk = thunk(object, &function.c_symbol, index, &parsed)?;
        let mut release = false;
        for argument in parsed.arguments() {
            if let Argument::Handle {
                kind,
                release: true,
            } = argument
            {
                if kinds
                    .insert(kind, kind_data(object, index, thunk, parsed.waits)?)
                    .is_some()
                {
                    return Err(format!("FFI-5: two releases for {kind}"));
                }
                release = true;
            }
        }
        waits |= parsed.waits;
        entries.push((function, parsed, thunk, release));
    }
    for (index, (function, parsed, thunk, release)) in entries.into_iter().enumerate() {
        let descriptor = descriptor(object, index, &parsed, thunk, release, &kinds)?;
        slot(object, &function.hirsel_symbol, &parsed, descriptor)?;
    }
    Ok(waits)
}

fn kind_data(
    object: &mut ObjectModule,
    index: usize,
    thunk: FuncId,
    waits: bool,
) -> Result<DataId> {
    let mut bytes = vec![0; abi::FOREIGN_KIND_BYTES];
    bytes[abi::FOREIGN_KIND_WAITS] = u8::from(waits);
    let mut data = DataDescription::new();
    data.define(bytes.into_boxed_slice());
    data.set_align(8);
    let function = object.declare_func_in_data(thunk, &mut data);
    data.write_function_addr(abi::FOREIGN_KIND_RELEASE, function);
    define(object, &format!("hirsel.foreign.kind.{index}"), &data)
}

fn descriptor(
    object: &mut ObjectModule,
    index: usize,
    parsed: &Abi<'_>,
    thunk: FuncId,
    release: bool,
    kinds: &BTreeMap<&str, DataId>,
) -> Result<DataId> {
    let count = parsed.arguments().count();
    let mut parameters = vec![0; count.max(1) * abi::FOREIGN_PARAMETER_BYTES];
    for (index, argument) in parsed.arguments().enumerate() {
        parameters[index * abi::FOREIGN_PARAMETER_BYTES + abi::FOREIGN_PARAMETER_DIR] =
            u8::from(matches!(argument, Argument::Dir));
    }
    let mut data = DataDescription::new();
    data.define(parameters.into_boxed_slice());
    data.set_align(8);
    for (word, argument) in parsed.arguments().enumerate() {
        if let Argument::Handle { kind, .. } = argument {
            let id = *kinds
                .get(kind)
                .ok_or_else(|| format!("FFI-5: no reached release for {kind}"))?;
            let global = object.declare_data_in_data(id, &mut data);
            data.write_data_addr((word * abi::FOREIGN_PARAMETER_BYTES) as u32, global, 0);
        }
    }
    let params = define(object, &format!("hirsel.foreign.parameters.{index}"), &data)?;
    let mut bytes = vec![0; abi::FOREIGN_CALL_BYTES];
    bytes[abi::FOREIGN_CALL_COUNT..abi::FOREIGN_CALL_COUNT + 8]
        .copy_from_slice(&(count as u64).to_le_bytes());
    bytes[abi::FOREIGN_CALL_RESULT] = match parsed.result {
        Return::Scalar(Scalar::F32) => abi::FOREIGN_RESULT_F32,
        Return::Scalar(Scalar::F64) => abi::FOREIGN_RESULT_F64,
        _ => 0,
    };
    bytes[abi::FOREIGN_CALL_WAITS] = u8::from(parsed.waits);
    bytes[abi::FOREIGN_CALL_RELEASE] = u8::from(release);
    let mut data = DataDescription::new();
    data.define(bytes.into_boxed_slice());
    data.set_align(8);
    let function = object.declare_func_in_data(thunk, &mut data);
    data.write_function_addr(abi::FOREIGN_CALL_THUNK, function);
    let params = object.declare_data_in_data(params, &mut data);
    data.write_data_addr(abi::FOREIGN_CALL_PARAMETERS, params, 0);
    if let Return::New(kind) = parsed.result {
        let id = *kinds
            .get(kind)
            .ok_or_else(|| format!("FFI-5: no reached release for {kind}"))?;
        let global = object.declare_data_in_data(id, &mut data);
        data.write_data_addr(abi::FOREIGN_CALL_NEW, global, 0);
    }
    define(object, &format!("hirsel.foreign.call.{index}"), &data)
}

fn define(object: &mut ObjectModule, name: &str, data: &DataDescription) -> Result<DataId> {
    let id = object
        .declare_data(name, Linkage::Local, false, false)
        .map_err(error)?;
    object.define_data(id, data).map_err(error)?;
    Ok(id)
}

fn thunk(object: &mut ObjectModule, name: &str, index: usize, parsed: &Abi<'_>) -> Result<FuncId> {
    let mut context = object.make_context();
    context.func.signature = signature(object, &[I64], Some(I64));
    let id = object
        .declare_function(
            &format!("hirsel.foreign.thunk.{index}"),
            Linkage::Local,
            &context.func.signature,
        )
        .map_err(error)?;
    let mut frontend = FunctionBuilderContext::new();
    let mut f = Code {
        object,
        b: FunctionBuilder::new(&mut context.func, &mut frontend),
    };
    let entry = f.b.create_block();
    f.b.append_block_params_for_function_params(entry);
    f.b.switch_to_block(entry);
    let words = f.b.block_params(entry)[0];
    let mut c_signature = f.object.make_signature();
    let mut params = Vec::new();
    for (index, argument) in parsed.arguments().enumerate() {
        let offset = (index * 16) as i32;
        match argument {
            Argument::Scalar(s) => {
                c_signature.params.push(c_scalar(s));
                params.push(f.load(scalar_type(s), words, offset));
            }
            Argument::Bytes { .. } => {
                c_signature
                    .params
                    .extend([AbiParam::new(I64), AbiParam::new(I64)]);
                params.extend([f.load(I64, words, offset), f.load(I64, words, offset + 8)]);
            }
            Argument::Handle { .. } => {
                c_signature.params.push(AbiParam::new(I64));
                params.push(f.load(I64, words, offset + 8));
            }
            Argument::Dir => {
                c_signature
                    .params
                    .extend([AbiParam::new(I32), AbiParam::new(I32)]);
                for at in [offset, offset + 8] {
                    let value = f.load(I64, words, at);
                    params.push(f.b.ins().ireduce(I32, value));
                }
            }
        }
    }
    c_signature.returns.extend(match parsed.result {
        Return::Unit => None,
        Return::Scalar(s) => Some(c_scalar(s)),
        Return::New(_) => Some(AbiParam::new(I64)),
    });
    let native = f
        .object
        .declare_function(name, Linkage::Import, &c_signature)
        .map_err(error)?;
    let native = f.object.declare_func_in_func(native, f.b.func);
    let call = f.b.ins().call(native, &params);
    let result = f.b.inst_results(call).first().copied();
    let word = match result {
        Some(value) => as_word(&mut f, value),
        None => f.constant(0),
    };
    f.b.ins().return_(&[word]);
    f.b.seal_all_blocks();
    f.b.finalize(f.object.target_config());
    object.define_function(id, &mut context).map_err(error)?;
    Ok(id)
}

fn slot(object: &mut ObjectModule, name: &str, parsed: &Abi<'_>, descriptor: DataId) -> Result<()> {
    let params = std::iter::once(Param::Value(Ty::Scalar(Scalar::U64)))
        .chain(parsed.arguments().map(|arg| match arg {
            Argument::Scalar(s) => Param::Value(Ty::Scalar(s)),
            Argument::Handle { .. } | Argument::Dir => Param::Value(Ty::Scalar(Scalar::U64)),
            Argument::Bytes { mutable } => Param::Address {
                place: PlaceTy::Value(Ty::Ptr),
                loan: if mutable { Loan::Mut } else { Loan::View },
            },
        }))
        .collect();
    let (results, dest) = match parsed.result {
        Return::Unit => (vec![], None),
        Return::Scalar(s) => (vec![Ty::Scalar(s)], None),
        Return::New(_) => (vec![], Some(TypeId(0))),
    };
    let sig = NativeSignature::foreign(
        &Signature {
            params,
            results,
            dest,
            never: false,
        },
        object,
    );
    let mut context = object.make_context();
    context.func.signature = sig.clif.clone();
    let id = object
        .declare_function(name, Linkage::Export, &context.func.signature)
        .map_err(error)?;
    let mut frontend = FunctionBuilderContext::new();
    let mut f = Code {
        object,
        b: FunctionBuilder::new(&mut context.func, &mut frontend),
    };
    let entry = f.b.create_block();
    f.b.append_block_params_for_function_params(entry);
    f.b.switch_to_block(entry);
    let incoming = f.b.block_params(entry).to_vec();
    let ctx = incoming[0];
    let site = *incoming.last().unwrap();
    let words = f.stack(
        u32::try_from(parsed.arguments().count().max(1) * 16).map_err(error)?,
        3,
    );
    let first = 2 + usize::from(dest.is_some());
    for (index, (arg, value)) in parsed
        .arguments()
        .zip(incoming[first..].iter().copied())
        .enumerate()
    {
        let offset = (index * 16) as i32;
        match arg {
            Argument::Bytes { .. } => {
                let ptr = f.load(I64, value, 0);
                let len = f.load(I64, value, 8);
                let empty = f
                    .object
                    .declare_data("hirsel_core_foreign_empty", Linkage::Import, false, false)
                    .map_err(error)?;
                let empty = f.address(empty);
                let pointer = f.b.ins().select(len, ptr, empty);
                f.store(pointer, words, offset);
                f.store(len, words, offset + 8);
            }
            _ => {
                let word = as_word(&mut f, value);
                f.store(word, words, offset);
            }
        }
    }
    let result = f.stack(8, 3);
    let zero = f.constant(0);
    f.store(zero, result, 0);
    let descriptor = f.address(descriptor);
    let status = f.value_call(
        "hirsel_core_foreign_call",
        I8,
        &[ctx, descriptor, words, result, site],
    )?;
    let bits = f.load(I64, result, 0);
    match parsed.result {
        Return::Unit => sig.return_(&mut f.b, &[status], None),
        Return::Scalar(s) => {
            let value = from_word(&mut f, bits, scalar_type(s));
            sig.return_(&mut f.b, &[value, status], None);
        }
        Return::New(_) => {
            let (failed, done) = f.if_nonzero(status);
            f.b.switch_to_block(failed);
            sig.return_(&mut f.b, &[status], None);
            f.b.switch_to_block(done);
            option(&mut f, incoming[1], bits)?;
            sig.return_(&mut f.b, &[status], None);
        }
    }
    f.b.seal_all_blocks();
    f.b.finalize(f.object.target_config());
    object.define_function(id, &mut context).map_err(error)
}

fn option(f: &mut Code<'_, '_>, dest: Value, bits: Value) -> Result<()> {
    let shape = Shape {
        name: "Option[foreign.Ptr]".into(),
        repr: Repr::Own,
        carried: Carried::Address,
        body: Body::Variant(vec![
            Case {
                name: "None".into(),
                fields: vec![],
            },
            Case {
                name: "Some".into(),
                fields: vec![Field {
                    name: "0".into(),
                    ty: FieldTy::Scalar(Scalar::U64),
                }],
            },
        ]),
        finalizer: None,
        clone: None,
        codec: None,
        grant: 0,
        send: true,
        held: None,
    };
    let recipes = hirsel_core_layout::compute(&[shape]).map_err(|e| format!("{e:?}"))?;
    let Layout::Fixed(layout) = &recipes[0].layout else {
        unreachable!()
    };
    let tag = layout.tag.expect("Option with a u64 field has a tag");
    let none = f.constant(hirsel_core_layout::case_word(&tag, 0).unwrap());
    let some = f.constant(hirsel_core_layout::case_word(&tag, 1).unwrap());
    let case = f.b.ins().select(bits, some, none);
    let width = match tag.width {
        1 => I8,
        2 => ir::types::I16,
        4 => I32,
        8 => I64,
        _ => unreachable!(),
    };
    let case = from_word(f, case, width);
    f.store(case, dest, tag.offset as i32);
    f.store(bits, dest, layout.offsets[1][0] as i32);
    Ok(())
}

fn scalar_type(scalar: Scalar) -> ir::Type {
    match scalar {
        Scalar::F32 => ir::types::F32,
        Scalar::F64 => ir::types::F64,
        _ => ir::Type::int((scalar.size() * 8) as u16).unwrap(),
    }
}

fn c_scalar(scalar: Scalar) -> AbiParam {
    let param = AbiParam::new(scalar_type(scalar));
    match scalar {
        Scalar::I8 | Scalar::I16 => param.sext(),
        Scalar::U8 | Scalar::U16 => param.uext(),
        _ => param,
    }
}

fn as_word(f: &mut Code<'_, '_>, value: Value) -> Value {
    let ty = f.b.func.dfg.value_type(value);
    let value = if ty.is_float() {
        f.b.ins().bitcast(
            ir::Type::int(ty.bits() as u16).unwrap(),
            MemFlagsData::new(),
            value,
        )
    } else {
        value
    };
    if ty == I64 || ty == ir::types::F64 {
        value
    } else {
        f.b.ins().uextend(I64, value)
    }
}

fn from_word(f: &mut Code<'_, '_>, bits: Value, ty: ir::Type) -> Value {
    let value = if ty.bits() < 64 {
        f.b.ins()
            .ireduce(ir::Type::int(ty.bits() as u16).unwrap(), bits)
    } else {
        bits
    };
    if ty.is_float() {
        f.b.ins().bitcast(ty, MemFlagsData::new(), value)
    } else {
        value
    }
}
