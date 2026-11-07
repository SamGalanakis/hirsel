use super::*;
use hirsel_lir::abi_types::FnAbiId;
use hirsel_lir::invocations::room::{RoomChecks, UnitKey};
pub(super) type WrappingAdds = BTreeMap<UnitKey, FnAbiId>;

#[derive(Clone)]
enum Operand {
    Value(ValueId),
    Slot(SlotId),
    Constant(ConstantId),
}

#[derive(Clone)]
enum Expression {
    Independent(Operand),
    Sum(Vec<Operand>),
}

pub(super) struct Accumulator<'m> {
    sites: BTreeMap<CallId, Vec<Operand>>,
    header: BasicBlock<'m>,
    parameters: Vec<Value<'m>>,
    total: Value<'m>,
    count: Value<'m>,
    initial_count: Value<'m>,
    at: Value<'m>,
}

fn local(unit: &LirUnit<Ready>, address: AddressId) -> Option<SlotId> {
    let address = &unit.addresses[address.index()];
    if address.projection.len != 0 {
        return None;
    }
    match address.base {
        AddressBase::Slot(slot) => Some(slot),
        _ => None,
    }
}

fn self_call(unit: &LirUnit<Ready>, call: &CallSite) -> bool {
    let Callee::Direct(symbol) = call.callee else {
        return false;
    };
    let symbol = &unit.tables.symbols[symbol.index()];
    symbol.definition == unit.header.definition
        && symbol.instance == Some(unit.header.instance)
        && call.abi == unit.header.abi
        && call.receipts.is_none()
        && call.pretransfer_failure.is_none()
        && call
            .arguments
            .get(&unit.pools.call_arguments)
            .is_some_and(|arguments| {
                arguments
                    .iter()
                    .all(|argument| matches!(argument.value, ArgumentValue::Value(_)))
            })
}

// LLVM TailRecursionElimination.cpp:777-800, 850-940 supplies the parameter
// and identity-accumulator phis. Unlike LLVM's scalar return matcher, this
// walks the checked status continuation. Only wrapping addition reassociates;
// checked arithmetic, cleanup and effects cannot move across a call.
fn continuation(
    unit: &LirUnit<Ready>,
    call: CallId,
    mut edge: EdgeId,
    adds: &WrappingAdds,
    room: &RoomChecks,
) -> Option<Vec<Operand>> {
    let mut values = BTreeMap::from([(
        unit.tables.calls[call.index()].result?,
        Expression::Sum(Vec::new()),
    )]);
    let mut slots = BTreeMap::new();
    let mut visited = BTreeSet::new();
    let mut retired = false;
    let mut wrapper = false;
    let expression = |values: &BTreeMap<_, _>, value| {
        values
            .get(&value)
            .cloned()
            .unwrap_or(Expression::Independent(Operand::Value(value)))
    };
    let combine = |left, right| match (left, right) {
        (Expression::Sum(mut sum), Expression::Independent(operand))
        | (Expression::Independent(operand), Expression::Sum(mut sum)) => {
            sum.push(operand);
            Some(Expression::Sum(sum))
        }
        _ => None,
    };
    loop {
        let target = &unit.tables.edges[edge.index()];
        if !visited.insert(target.target) {
            return None;
        }
        let block = &unit.blocks[target.target.index()];
        let passed: Vec<_> = target
            .arguments
            .get(&unit.pools.values)?
            .iter()
            .map(|&value| expression(&values, value))
            .collect();
        for (&parameter, value) in block.parameters.get(&unit.pools.values)?.iter().zip(passed) {
            values.insert(parameter, value);
        }
        for (offset, inst) in block
            .instructions
            .get(&unit.instructions)?
            .iter()
            .enumerate()
        {
            match *inst {
                Inst::FrameLeave if !retired => retired = true,
                Inst::FrameLeave
                    if wrapper
                        && room
                            .leaves
                            .contains(&(block.instructions.start as usize + offset)) =>
                {
                    wrapper = false
                }
                Inst::FrameEnter { .. }
                    if retired
                        && !wrapper
                        && room
                            .enters
                            .contains(&(block.instructions.start as usize + offset)) =>
                {
                    wrapper = true
                }
                Inst::Const { result, constant } => {
                    values.insert(result, Expression::Independent(Operand::Constant(constant)));
                }
                Inst::CopyValue { result, value } => {
                    values.insert(result, expression(&values, value));
                }
                Inst::Read {
                    result, address, ..
                } => {
                    let slot = local(unit, address)?;
                    values.insert(
                        result,
                        slots
                            .get(&slot)
                            .cloned()
                            .unwrap_or(Expression::Independent(Operand::Slot(slot))),
                    );
                }
                Inst::Store { address, value, .. } => {
                    slots.insert(local(unit, address)?, expression(&values, value));
                }
                Inst::IntArith {
                    result,
                    flag: None,
                    op: BinaryOp::Add,
                    mode: ArithMode::Wrapping,
                    left,
                    right,
                } => {
                    values.insert(
                        result,
                        combine(expression(&values, left), expression(&values, right))?,
                    );
                }
                Inst::Address { address, .. } if local(unit, address).is_some() => (),
                Inst::StorageLive { .. }
                | Inst::StorageDead { .. }
                | Inst::EndScope { .. }
                | Inst::BeginLoan { .. }
                | Inst::EndLoan { .. }
                | Inst::BindOrigin { .. }
                | Inst::BindFields { .. }
                | Inst::EndOrigin { .. }
                | Inst::Forget { .. } => (),
                _ => return None,
            }
        }
        match unit.terminators[target.target.index()] {
            Terminator::Jump(next) => edge = next,
            Terminator::Call {
                call,
                normal: Some(normal),
                ..
            } if wrapper && wrapping_call(unit, &unit.tables.calls[call.index()], adds) => {
                let site = &unit.tables.calls[call.index()];
                let arguments = site.arguments.get(&unit.pools.call_arguments)?;
                let argument = |source| {
                    let ArgumentValue::Value(value) =
                        arguments.iter().find(|a| a.parameter == source)?.value
                    else {
                        return None;
                    };
                    Some(expression(&values, value))
                };
                values.insert(site.result?, combine(argument(0)?, argument(1)?)?);
                edge = normal;
            }
            Terminator::Return(Some(value)) if retired && !wrapper => {
                match expression(&values, value) {
                    Expression::Sum(sum) if !sum.is_empty() => return Some(sum),
                    _ => return None,
                }
            }
            _ => return None,
        }
    }
}

fn wrapping_call(unit: &LirUnit<Ready>, call: &CallSite, adds: &WrappingAdds) -> bool {
    let Callee::Direct(symbol) = call.callee else {
        return false;
    };
    let symbol = &unit.tables.symbols[symbol.index()];
    adds.get(&(symbol.definition.clone(), symbol.instance)) == Some(&call.abi)
        && call.receipts.is_none()
        && call.pretransfer_failure.is_none()
}

/// Read the validated body, rather than trusting a numeric wrapper's name.
/// The normal path must return exactly wrapping_add(parameter 0, parameter 1),
/// using only scalar local storage and instructions that cannot fail or call out.
fn wrapping_add(unit: &LirUnit<Ready>, layouts: &LayoutTable) -> bool {
    let Some(abi) = layouts.fn_abis.get(unit.header.abi.0 as usize) else {
        return false;
    };
    let Some(result) = &abi.result else {
        return false;
    };
    if abi.environment
        || abi.destination
        || abi.receipts.is_some()
        || abi.shape != CallableShape::Definition
        || abi.parameters.len() != 2
        || abi.parameters.iter().enumerate().any(|(i, p)| {
            p.source != i as u32
                || p.convention != Passing::Borrow
                || p.passing != ParameterPassing::ByValue
                || p.layout != result.layout
        })
        || !layouts.layout(result.layout).is_some_and(|l| {
            l.scan.is_empty()
                && l.drop.is_empty()
                && l.finalizer.is_none()
                && scalar_shape(&l.kind).is_some_and(|k| scalar::integer(k).is_some())
        })
    {
        return false;
    }
    let mut values = BTreeMap::from([(unit.parameters[0], 1u8), (unit.parameters[1], 2u8)]);
    let mut slots: BTreeMap<_, _> = unit
        .slots
        .iter()
        .enumerate()
        .filter_map(|(i, s)| match s.kind {
            SlotKind::Parameter(source) if source < 2 => Some((SlotId(i as u32), 1u8 << source)),
            _ => None,
        })
        .collect();
    let mut block = unit.entry;
    let mut visited = BTreeSet::new();
    loop {
        if !visited.insert(block) {
            return false;
        }
        let Some(instructions) = unit.blocks[block.index()]
            .instructions
            .get(&unit.instructions)
        else {
            return false;
        };
        for inst in instructions {
            match *inst {
                Inst::Read {
                    result, address, ..
                } => {
                    let Some(value) =
                        local(unit, address).and_then(|slot| slots.get(&slot).copied())
                    else {
                        return false;
                    };
                    values.insert(result, value);
                }
                Inst::Store {
                    address,
                    value,
                    version,
                } => {
                    let (Some(slot), Some(&value)) = (local(unit, address), values.get(&value))
                    else {
                        return false;
                    };
                    slots.insert(slot, value);
                    values.insert(version, value);
                }
                Inst::CopyValue { result, value } => {
                    let Some(&value) = values.get(&value) else {
                        return false;
                    };
                    values.insert(result, value);
                }
                Inst::Const { result, .. } => {
                    values.insert(result, 0);
                }
                Inst::IntArith {
                    result,
                    flag: None,
                    op: BinaryOp::Add,
                    mode: ArithMode::Wrapping,
                    left,
                    right,
                } if matches!(
                    (values.get(&left), values.get(&right)),
                    (Some(1), Some(2)) | (Some(2), Some(1))
                ) =>
                {
                    values.insert(result, 3);
                }
                Inst::Address { address, .. } if local(unit, address).is_some() => (),
                Inst::StorageLive { .. }
                | Inst::StorageDead { .. }
                | Inst::EndScope { .. }
                | Inst::BeginLoan { .. }
                | Inst::EndLoan { .. }
                | Inst::BindOrigin { .. }
                | Inst::BindFields { .. }
                | Inst::EndOrigin { .. }
                | Inst::Forget { .. } => (),
                _ => return false,
            }
        }
        match unit.terminators[block.index()] {
            Terminator::Return(Some(value)) => return values.get(&value) == Some(&3),
            Terminator::Jump(edge) => {
                let edge = &unit.tables.edges[edge.index()];
                let Some(args) = edge.arguments.get(&unit.pools.values) else {
                    return false;
                };
                let Some(parameters) = unit.blocks[edge.target.index()]
                    .parameters
                    .get(&unit.pools.values)
                else {
                    return false;
                };
                let Some(passed) = args
                    .iter()
                    .map(|a| values.get(a).copied())
                    .collect::<Option<Vec<_>>>()
                else {
                    return false;
                };
                for (&parameter, value) in parameters.iter().zip(passed) {
                    values.insert(parameter, value);
                }
                block = edge.target;
            }
            _ => return false,
        }
    }
}

pub(super) fn wrapping_adds<'a>(
    units: impl Iterator<Item = &'a LirUnit<Ready>>,
    layouts: &LayoutTable,
) -> WrappingAdds {
    units
        .filter(|unit| wrapping_add(unit, layouts))
        .map(|unit| {
            (
                (unit.header.definition.clone(), Some(unit.header.instance)),
                unit.header.abi,
            )
        })
        .collect()
}

fn invariant_parameter(unit: &LirUnit<Ready>, mut value: ValueId, source: u32) -> bool {
    let expected = unit.parameters[source as usize];
    let mut visited = BTreeSet::new();
    while visited.insert(value) {
        if value == expected {
            return true;
        }
        let definition=unit.instructions.iter().find(|inst|matches!(inst,Inst::CopyValue {result,..}|Inst::Read {result,..} if *result==value));
        match definition {
            Some(Inst::CopyValue { value: operand, .. }) => value = *operand,
            Some(Inst::Read { address, .. }) => {
                return local(unit, *address).is_some_and(|slot| {
                    unit.slots[slot.index()].kind == SlotKind::Parameter(source)
                });
            }
            _ => return false,
        }
    }
    false
}

impl<'m, 'a> FnEmitter<'m, 'a> {
    pub(super) fn accumulator_entry(&self) -> Option<BasicBlock<'m>> {
        self.accumulator.as_ref().map(|a| a.header)
    }
    pub(super) fn accumulator_init(&mut self) -> Emitted {
        let unit = self.unit;
        if self.foreground_calls.is_none()
            || self.inputs.opt == OptLevel::None
            || self.inputs.fuel
            || self.inputs.measurement
            || self.ownership.is_some()
            || self.signature.abi.environment
            || self.signature.abi.destination
            || unit.header.track_caller
            || self
                .signature
                .hidden
                .iter()
                .any(|h| !matches!(h, Hidden::Context))
            || self
                .signature
                .abi
                .parameters
                .iter()
                .any(|p| p.passing != ParameterPassing::ByValue || p.convention != Passing::Borrow)
            || !self.signature.roots.is_empty()
        {
            return Ok(());
        }
        let Some((kind, _)) = self.signature.value else {
            return Ok(());
        };
        if scalar::integer(kind).is_none() {
            return Ok(());
        }
        if !unit.terminators.iter().all(|terminator| match *terminator {
            Terminator::Call { call, .. } => {
                self_call(unit, &unit.tables.calls[call.index()])
                    || wrapping_call(unit, &unit.tables.calls[call.index()], &self.wrapping_adds)
            }
            Terminator::Jump(_)
            | Terminator::Branch { .. }
            | Terminator::Switch { .. }
            | Terminator::Return(_)
            | Terminator::Raise(_)
            | Terminator::Propagate
            | Terminator::Unreachable
            | Terminator::InvariantFault { .. } => true,
            _ => false,
        }) {
            return Ok(());
        }
        for (index, inst) in unit.instructions.iter().enumerate() {
            let safe = match inst {
                Inst::FrameEnter { .. } => {
                    self.counters.enters.contains(&index) || self.room.enters.contains(&index)
                }
                Inst::FrameLeave => {
                    self.counters.leaves.contains(&index) || self.room.leaves.contains(&index)
                }
                Inst::Store { address, .. } => local(unit, *address).is_some_and(|slot| {
                    !matches!(unit.slots[slot.index()].kind, SlotKind::Parameter(_))
                }),
                Inst::Const { .. }
                | Inst::Unary { .. }
                | Inst::Binary { .. }
                | Inst::Compare { .. }
                | Inst::Convert { .. }
                | Inst::IntArith { .. }
                | Inst::ScalarUnary { .. }
                | Inst::Check { .. }
                | Inst::StorageLive { .. }
                | Inst::StorageDead { .. }
                | Inst::EndScope { .. }
                | Inst::Address { .. }
                | Inst::Read { .. }
                | Inst::CopyValue { .. }
                | Inst::BeginLoan { .. }
                | Inst::EndLoan { .. }
                | Inst::BindOrigin { .. }
                | Inst::BindFields { .. }
                | Inst::EndOrigin { .. }
                | Inst::Forget { .. }
                | Inst::SeqLen { .. } => true,
                _ => false,
            };
            if !safe {
                return Ok(());
            }
        }
        let sites: BTreeMap<_, _> = unit
            .terminators
            .iter()
            .filter_map(|terminator| {
                let Terminator::Call {
                    call,
                    normal: Some(normal),
                    ..
                } = *terminator
                else {
                    return None;
                };
                if !self_call(unit, &unit.tables.calls[call.index()]) {
                    return None;
                }
                Some((
                    call,
                    continuation(unit, call, normal, &self.wrapping_adds, &self.room)?,
                ))
            })
            .collect();
        if sites.is_empty() {
            return Ok(());
        }
        let CounterCell::Snapshot { at, count, .. } = self.counter_cell() else {
            return Ok(());
        };
        let header = self.module.append_block(self.function);
        self.builder.position(header);
        let mut parameters = Vec::new();
        for parameter in &self.signature.abi.parameters {
            parameters.push(
                self.builder
                    .phi(self.llvm_type(self.layout_shape(parameter.layout)?)),
            );
        }
        let total = self
            .builder
            .phi(self.module.int_type(scalar::integer(kind).unwrap().0));
        let current = self.builder.phi(self.module.int_type(16));
        self.builder.add_incoming(
            total,
            self.module.const_zero(total.ty()),
            self.prologue_block,
        );
        self.builder
            .add_incoming(current, count, self.prologue_block);
        self.builder.position(self.prologue_block);
        let mut stores = Vec::new();
        for (i, (&value, &phi)) in unit.parameters.iter().zip(&parameters).enumerate() {
            let source = self.signature.abi.parameters[i].source;
            let slot = unit
                .slots
                .iter()
                .position(|s| s.kind == SlotKind::Parameter(source))
                .and_then(|s| self.slots[s]);
            let initial = match slot {
                Some(place) => self.load(self.value_shape(value)?, place),
                None => self.get(value)?,
            };
            self.builder.add_incoming(phi, initial, self.prologue_block);
            if let Some(place) = slot {
                stores.push((place, phi, self.value_shape(value)?));
            } else {
                self.values[value.index()] = Some(phi);
            }
        }
        self.builder.position(header);
        let full = self.builder.icmp(
            IntPredicate::Uge,
            current,
            self.module.const_int(
                self.module.int_type(16),
                hirsel_core_contract::depth::FOREGROUND_LIMIT.into(),
            ),
        );
        self.counter_cell = Some(CounterCell::Snapshot {
            at,
            count: current,
            full,
        });
        for (place, phi, shape) in stores {
            self.store(shape, phi, place);
        }
        self.builder.br(self.blocks[unit.entry.index()]);
        self.accumulator = Some(Accumulator {
            sites,
            header,
            parameters,
            total,
            count: current,
            initial_count: count,
            at,
        });
        Ok(())
    }

    pub(super) fn accumulator_call(&mut self, call: CallId) -> Emitted {
        let Some(state) = &self.accumulator else {
            return Ok(());
        };
        let Some(operands) = state.sites.get(&call) else {
            return Ok(());
        };
        let (header, parameters, mut total, count) = (
            state.header,
            state.parameters.clone(),
            state.total,
            state.count,
        );
        // Some shared continuations name values which do not dominate this call.
        // Keep the ordinary call unless every independent operand is available.
        if operands
            .iter()
            .any(|operand| matches!(operand,Operand::Value(v) if self.values[v.index()].is_none()))
        {
            return Ok(());
        }
        let operands = operands.clone();
        let arguments = self.pool(
            self.unit.tables.calls[call.index()].arguments,
            &self.unit.pools.call_arguments,
        )?;
        let mut passed = Vec::new();
        for parameter in &self.signature.abi.parameters {
            let Some(argument) = arguments.iter().find(|a| a.parameter == parameter.source) else {
                return Ok(());
            };
            let ArgumentValue::Value(value) = argument.value else {
                return Ok(());
            };
            if !matches!(self.layout_shape(parameter.layout)?,Shape::Scalar(kind) if scalar::integer(kind).is_some())
                && !invariant_parameter(self.unit, value, parameter.source)
            {
                return Ok(());
            }
            passed.push(self.get(value)?);
        }
        for operand in operands {
            let value = match operand {
                Operand::Value(value) => self.get(value)?,
                Operand::Constant(value) => self.constant(value)?,
                Operand::Slot(slot) => {
                    let Some(place) = self.slots[slot.index()] else {
                        return self.invalid("an accumulator operand without storage");
                    };
                    self.load(self.layout_shape(place.layout)?, place)
                }
            };
            total = self.builder.binary(BinOp::Add, total, value);
        }
        let here = self.builder.current();
        for (phi, value) in parameters.into_iter().zip(passed) {
            self.builder.add_incoming(phi, value, here);
        }
        let state = self.accumulator.as_ref().unwrap();
        self.builder.add_incoming(state.total, total, here);
        self.builder.add_incoming(
            state.count,
            self.builder.binary(
                BinOp::Add,
                count,
                self.module.const_int(self.module.int_type(16), 1),
            ),
            here,
        );
        self.builder.br(header);
        // Keep emitting the original, now unreachable call. Its SSA result and
        // continuation still bind shared LIR block parameters during emission;
        // LLVM removes this unreachable predecessor before object generation.
        self.builder
            .position(self.module.append_block(self.function));
        Ok(())
    }

    pub(super) fn accumulator_return(
        &self,
        status: u128,
        value: Option<Value<'m>>,
    ) -> Option<Value<'m>> {
        let Some(state) = &self.accumulator else {
            return value;
        };
        // Pending combines remain semantic LANG-10 activations. Retire them
        // together on either exit; a physical loop never changes the limit.
        self.builder
            .store_access(state.initial_count, state.at, 2, Access::Depth);
        if status == 0 {
            value.map(|value| self.builder.binary(BinOp::Add, state.total, value))
        } else {
            value
        }
    }
}
