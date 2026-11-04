//! One-probe updates lend the stored value directly. A missing insertion
//! calls its initializer and copies the borrowed key only on that branch.
use super::*;
use hirsel_lir::abi_types::CallableShape;

pub(super) fn upsert(cx: &mut Cx<'_>) -> R<LirUnit<Value>> {
    edit(cx, true)
}
pub(super) fn update(cx: &mut Cx<'_>) -> R<LirUnit<Value>> {
    edit(cx, false)
}
/// Unlike the unique operations, this callable has no uniqueness proof for
/// parameter 0. P4 therefore detaches every shared block crossed by the value
/// argument's freshly prepared address, outermost first, before lending it.
/// Tracked arena detaches charge before copying; the miss has no write path.
pub(super) fn update_value(cx: &mut Cx<'_>) -> R<LirUnit<Value>> {
    let mut b = Body::new(cx)?;
    let probe = probe_call(&mut b)?;
    let container = b.parameters[0];
    let entries = field(&mut b, container.address, 0)?;
    let count = len(&mut b, entries)?;
    let hit = b.compare(BinaryOp::Lt, probe.ordinal, count)?;
    let before = b.reaching(container.slot)?;
    let found_v = b.version(container.slot);
    let missing_v = b.version(container.slot);
    let found = b.block(BlockKind::Body, &[found_v]);
    let missing = b.block(BlockKind::Body, &[missing_v]);
    let yes = b.edge(found, &[before]);
    let no = b.edge(missing, &[before]);
    b.terminate(Terminator::Branch {
        condition: hit,
        yes,
        no,
    });
    let held = b.held_state();
    b.at(found);
    b.set_reaching(container.slot, found_v);
    // Prepare the entire path after the hit. P4 inserts shallow detaches
    // before these projections and resolves them in the installed owner.
    // Append maintenance could compact and invalidate the saved ordinal.
    let entries = field(&mut b, container.address, 0)?;
    let entry = b.slot_place(entries, probe.ordinal)?;
    let pair = live_pair(&mut b, entry)?;
    let value = field(&mut b, pair, 1)?;
    callback(&mut b, 2, Some(value))?;
    b.ret(Some(hit));
    b.at(missing);
    b.set_reaching(container.slot, missing_v);
    b.restore_held(&held);
    b.ret(Some(hit));
    Ok(b.finish())
}
fn callback(
    b: &mut Body<'_, '_>,
    parameter: usize,
    value: Option<AddressId>,
) -> R<Option<ValueId>> {
    let TypeKind::Function(f) = &b.cx.types.get(b.cx.callable).unwrap().kind else {
        unreachable!()
    };
    let ty = f.parameters[parameter].ty;
    let abi =
        b.cx.layouts
            .fn_abi(b.cx.types, ty, CallableShape::Closure)
            .map_err(|e| PassError::Layout(b.cx.path.clone(), e))?;
    let (callee, loan) = b.borrow(b.parameters[parameter].address, false)?;
    let arguments = if let Some(value) = value {
        vec![mut_argument(b, 0, value)?]
    } else {
        vec![]
    };
    Ok(b.call(
        Callee::Indirect(callee),
        Vec::new(),
        abi,
        arguments,
        &[loan],
    ))
}
fn copied_key(b: &mut Body<'_, '_>) -> R<(SlotId, AddressId)> {
    let key = b.parameters[1].address;
    let layout = b.address_layout(key);
    let (slot, owner) = temporary(b, layout)?;
    if b.record(layout).copy == CopyRecipe::Bits {
        let value = b.read_bits(key)?;
        b.store(owner, value)?;
        b.hold(slot, owner);
        return Ok((slot, owner));
    }
    let (value, loan) = b.borrow(key, false)?;
    let shape = b.cx.payload(layout).unwrap_or(layout);
    let construct = ConstructId(b.ed.unit.tables.constructs.len() as u32);
    b.ed.unit.tables.constructs.push(ConstructSite {
        arena_no_drop: false,
        tracked: false,
        constructor: ConstructorKind::Tuple,
        layout: shape,
        fields: PoolRange::EMPTY,
        destination: DestinationClass::Heap,
        placement: None,
        anchor: b.anchor,
    });
    let result = b.fresh(layout);
    let failure = loan_failure(b, &[loan], None);
    b.push(Inst::UniqueCopy {
        result,
        value,
        construct,
        deep: true,
        failure,
    });
    b.end_loan(loan);
    b.store(owner, result)?;
    b.hold(slot, owner);
    Ok((slot, owner))
}
fn edit(cx: &mut Cx<'_>, insert: bool) -> R<LirUnit<Value>> {
    let mut b = Body::new(cx)?;
    let probe = probe_call(&mut b)?;
    let container = b.parameters[0];
    let entries = field(&mut b, container.address, 0)?;
    let count = len(&mut b, entries)?;
    let hit = b.compare(BinaryOp::Lt, probe.ordinal, count)?;
    let before = b.reaching(container.slot)?;
    let found_v = b.version(container.slot);
    let missing_v = b.version(container.slot);
    let found = b.block(BlockKind::Body, &[found_v]);
    let missing = b.block(BlockKind::Body, &[missing_v]);
    let yes = b.edge(found, &[before]);
    let no = b.edge(missing, &[before]);
    b.terminate(Terminator::Branch {
        condition: hit,
        yes,
        no,
    });
    let held = b.held_state();
    b.at(found);
    b.set_reaching(container.slot, found_v);
    let entry = b.slot_place(entries, probe.ordinal)?;
    let pair = live_pair(&mut b, entry)?;
    let value = field(&mut b, pair, 1)?;
    callback(&mut b, if insert { 3 } else { 2 }, Some(value))?;
    if insert {
        unit(&mut b);
    } else {
        b.ret(Some(hit));
    }
    b.at(missing);
    b.set_reaching(container.slot, missing_v);
    b.restore_held(&held);
    if insert {
        table::prepare_append(&mut b)?;
        let (key_slot, key) = copied_key(&mut b)?;
        let value_type = b.cx.type_actual(1)?;
        let value_layout = b.cx.layout(value_type)?;
        let (value_slot, value) = temporary(&mut b, value_layout)?;
        let initialized = callback(&mut b, 2, None)?.expect("initializer returns a value");
        b.store(value, initialized)?;
        b.hold(value_slot, value);
        table::append_prepared(&mut b, probe.hash, key, value)?;
        for slot in [key_slot, value_slot] {
            b.push(Inst::StorageDead {
                slot,
                if_live: false,
            });
        }
        unit(&mut b);
    } else {
        b.ret(Some(hit));
    }
    Ok(b.finish())
}

/// Rust std collections/hash/map.rs:1012,2539 (1.99.0): select an Entry
/// before invoking an absent initializer. Hirsel retains insertion order,
/// takes the key, forwards mutable caller state and propagates a typed error.
pub(super) fn try_insert(cx: &mut Cx<'_>) -> R<LirUnit<Value>> {
    use super::super::arena::{RESULT_ERROR, RESULT_OK, payload, variant};
    use super::super::iter::local;
    let mut b = Body::new(cx)?;
    let returned = b.cx.fn_abi().result.unwrap().layout;
    let probe = probe_call(&mut b)?;
    let container = b.parameters[0];
    let key = b.parameters[1];
    let state = b.parameters[2];
    let entries = field(&mut b, container.address, 0)?;
    let count = len(&mut b, entries)?;
    let hit = b.compare(BinaryOp::Lt, probe.ordinal, count)?;
    let held = b.held_state();
    let found = b.block(BlockKind::Body, &[]);
    let missing = b.block(BlockKind::Body, &[]);
    b.branch(hit, found, missing);
    b.at(found);
    drop_place(&mut b, key.address);
    let boolean = b.bool_layout()?;
    let no = b.constant(boolean, ConstantValue::Bool(false));
    let result = variant(&mut b, returned, "Result", RESULT_OK, &[no]);
    b.ret(Some(result));
    b.at(missing);
    b.restore_held(&held);
    table::prepare_append(&mut b)?;
    let TypeKind::Function(f) = &b.cx.types.get(b.cx.callable).unwrap().kind else {
        unreachable!()
    };
    let callback_ty = f.parameters[3].ty;
    let abi =
        b.cx.layouts
            .fn_abi(b.cx.types, callback_ty, CallableShape::Closure)
            .map_err(|e| PassError::Layout(b.cx.path.clone(), e))?;
    let (callee, loan) = b.borrow(b.parameters[3].address, false)?;
    let argument = mut_argument(&mut b, 0, state.address)?;
    let initialized = b
        .call(
            Callee::Indirect(callee),
            vec![],
            abi,
            vec![argument],
            &[loan],
        )
        .expect("fallible initializer result");
    let (result_slot, result_address) = local(&mut b, initialized)?;
    let result_layout = b.layout_of(initialized);
    let (tag, tag_loan) = b.borrow(result_address, false)?;
    let ok = b.block(BlockKind::Body, &[]);
    let error = b.block(BlockKind::Body, &[]);
    let ok_edge = b.edge(ok, &[]);
    let error_edge = b.edge(error, &[]);
    let arms = PoolRange::push(
        &mut b.ed.unit.pools.switch_arms,
        [SwitchArm {
            value: RESULT_OK,
            edge: ok_edge,
        }],
    );
    b.terminate(Terminator::Switch {
        value: tag,
        arms,
        otherwise: error_edge,
    });
    let held = b.held_state();
    for (block, ordinal) in [(ok, RESULT_OK), (error, RESULT_ERROR)] {
        b.at(block);
        b.restore_held(&held);
        b.end_loan(tag_loan);
        let layout = payload(&b, result_layout, ordinal).expect("Result payload");
        let address = b.place(
            AddressBase::Address(result_address),
            &[ProjectionStep::VariantField {
                variant: ordinal,
                field: 0,
            }],
            layout,
            None,
        )?;
        let value = b.take(address)?;
        b.emptied(result_slot);
        b.push(Inst::StorageDead {
            slot: result_slot,
            if_live: false,
        });
        let (slot, owner) = local(&mut b, value)?;
        let result = if ordinal == RESULT_OK {
            table::append_prepared(&mut b, probe.hash, key.address, owner)?;
            b.push(Inst::StorageDead {
                slot,
                if_live: false,
            });
            let yes = b.constant(boolean, ConstantValue::Bool(true));
            variant(&mut b, returned, "Result", RESULT_OK, &[yes])
        } else {
            drop_place(&mut b, key.address);
            let error = b.take(owner)?;
            b.push(Inst::StorageDead {
                slot,
                if_live: false,
            });
            variant(&mut b, returned, "Result", RESULT_ERROR, &[error])
        };
        b.ret(Some(result));
    }
    Ok(b.finish())
}
