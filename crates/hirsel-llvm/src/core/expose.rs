//! Expose bounded helper bodies before exact Step regions are selected.
//! Clone effects and failure continuations in place; charging remains in steps.
use super::*;

fn successors(body: &Body, block: usize) -> Vec<usize> {
    let edge = |e: EdgeId| body.edges[e.index()].target.index();
    match body.terminators[block] {
        Terminator::Jump(e) => vec![edge(e)],
        Terminator::Branch { yes, no, .. } => vec![edge(yes), edge(no)],
        Terminator::Switch {
            arms, otherwise, ..
        } => arms
            .get(&body.pools.arms)
            .unwrap()
            .iter()
            .map(|a| edge(a.edge))
            .chain([edge(otherwise)])
            .collect(),
        _ => Vec::new(),
    }
}
fn cyclic_blocks(body: &Body) -> BTreeSet<usize> {
    fn visit(
        body: &Body,
        b: usize,
        seen: &mut [bool],
        stack: &mut Vec<usize>,
        hot: &mut BTreeSet<usize>,
    ) {
        if let Some(at) = stack.iter().position(|&n| n == b) {
            hot.extend(stack[at..].iter().copied());
        } else if !seen[b] {
            seen[b] = true;
            stack.push(b);
            for next in successors(body, b) {
                visit(body, next, seen, stack, hot);
            }
            stack.pop();
        }
    }
    let mut hot = BTreeSet::new();
    visit(
        body,
        0,
        &mut vec![false; body.blocks.len()],
        &mut Vec::new(),
        &mut hot,
    );
    hot
}
fn supported(i: &Inst) -> bool {
    matches!(
        i,
        Inst::Const { .. }
            | Inst::ReadStatic { .. }
            | Inst::FunctionRef { .. }
            | Inst::DescriptorRef { .. }
            | Inst::Unary { .. }
            | Inst::Binary { .. }
            | Inst::Compare { .. }
            | Inst::Convert { .. }
            | Inst::IntArith { .. }
            | Inst::MulAdd { .. }
            | Inst::ScalarUnary { .. }
            | Inst::Address { .. }
            | Inst::Read { .. }
            | Inst::Store { .. }
            | Inst::StorageLive { .. }
            | Inst::StorageDead { .. }
            | Inst::UpdateMask { .. }
            | Inst::Call { .. }
            | Inst::Pointer { .. }
            | Inst::Copy { .. }
            | Inst::Case { .. }
            | Inst::SetCase { .. }
            | Inst::Step { .. }
    )
}
fn edge(body: &mut Body, target: BlockId, args: PoolRange<ValueId>) -> EdgeId {
    let id = EdgeId(body.edges.len() as u32);
    body.edges.push(Edge {
        target,
        args,
        cold: false,
    });
    id
}

struct Rename {
    values: Vec<ValueId>,
    slot: u32,
    address: u32,
    block: u32,
    call: u32,
    edge: u32,
    value_pool: u32,
    fail: Option<BlockId>,
}
impl Rename {
    fn v(&self, v: ValueId) -> ValueId {
        self.values[v.index()]
    }
    fn a(&self, a: AddressId) -> AddressId {
        AddressId(a.0 + self.address)
    }
    fn b(&self, b: BlockId) -> BlockId {
        BlockId(b.0 + self.block)
    }
    fn e(&self, e: EdgeId) -> EdgeId {
        EdgeId(e.0 + self.edge)
    }
    fn f(&self, f: Option<BlockId>) -> Option<BlockId> {
        f.map(|b| self.b(b)).or(self.fail)
    }
    fn range(&self, r: PoolRange<ValueId>) -> PoolRange<ValueId> {
        PoolRange::new(r.start + self.value_pool, r.len)
    }
    fn inst(&self, inst: &mut Inst) {
        let v = |v: &mut ValueId| *v = self.v(*v);
        match inst {
            Inst::Const { result, .. }
            | Inst::ReadStatic { result, .. }
            | Inst::FunctionRef { result, .. }
            | Inst::DescriptorRef { result, .. } => v(result),
            Inst::Unary {
                result,
                operand,
                fail,
                ..
            } => {
                v(result);
                v(operand);
                *fail = self.f(*fail);
            }
            Inst::Binary {
                result,
                left,
                right,
                fail,
                ..
            } => {
                v(result);
                v(left);
                v(right);
                *fail = self.f(*fail);
            }
            Inst::Compare {
                result,
                left,
                right,
                ..
            } => {
                v(result);
                v(left);
                v(right);
            }
            Inst::Convert {
                result,
                operand,
                flag,
                ..
            } => {
                v(result);
                v(operand);
                if let Some(flag) = flag {
                    v(flag);
                }
            }
            Inst::IntArith {
                result,
                left,
                right,
                flag,
                ..
            } => {
                v(result);
                v(left);
                v(right);
                if let Some(flag) = flag {
                    v(flag);
                }
            }
            Inst::MulAdd {
                result,
                left,
                right,
                addend,
            } => {
                v(result);
                v(left);
                v(right);
                v(addend);
            }
            Inst::ScalarUnary {
                result, operand, ..
            } => {
                v(result);
                v(operand);
            }
            Inst::Address { address, fail } => {
                *address = self.a(*address);
                *fail = self.f(*fail);
            }
            Inst::Read {
                result, address, ..
            }
            | Inst::Pointer { result, address }
            | Inst::Case { result, address } => {
                v(result);
                *address = self.a(*address);
            }
            Inst::Store { address, value } => {
                v(value);
                *address = self.a(*address);
            }
            Inst::StorageLive { slot } | Inst::StorageDead { slot } => slot.0 += self.slot,
            Inst::UpdateMask { result, mask, .. } => {
                v(result);
                v(mask);
            }
            Inst::Call { call } => call.0 += self.call,
            Inst::Copy { to, from, bytes } => {
                *to = self.a(*to);
                *from = self.a(*from);
                v(bytes);
            }
            Inst::SetCase { address, .. } => *address = self.a(*address),
            Inst::Step { fail } => *fail = self.f(*fail),
            _ => unreachable!("selected helper instruction"),
        }
    }
}

fn expand(
    body: &mut Body,
    block: usize,
    offset: usize,
    callee: &Body,
    module: &Module,
    inherits: bool,
) -> Vec<usize> {
    let range = body.blocks[block].insts;
    let at = range.start as usize + offset;
    let Inst::Call { call } = body.insts[at] else {
        unreachable!()
    };
    let call = body.calls[call.index()].clone();
    let caller_anchor = body.inst_anchors[at];
    let anchor = |a: Option<Anchor>| {
        if inherits && a.is_none_or(|a| module.items[a.item.index()].package_is("core", "1")) {
            caller_anchor
        } else {
            a
        }
    };
    let args = call.args.get(&body.pools.args).unwrap().to_vec();
    let mut values = callee
        .values
        .iter()
        .map(|&ty| {
            let v = ValueId(body.values.len() as u32);
            body.values.push(ty);
            v
        })
        .collect::<Vec<_>>();
    let mut pointers = Vec::new();
    for (&param, arg) in callee.blocks[0]
        .params
        .get(&callee.pools.values)
        .unwrap()
        .iter()
        .zip(args)
    {
        match arg {
            Arg::Value(v) => values[param.index()] = v,
            Arg::Address(address) => pointers.push(Inst::Pointer {
                result: values[param.index()],
                address,
            }),
        }
    }
    let continuation = BlockId(body.blocks.len() as u32);
    body.blocks.push(Block {
        params: call.results,
        insts: PoolRange::new(at as u32 + 1, range.len - offset as u32 - 1),
        anchor: body.blocks[block].anchor,
    });
    body.terminators.push(body.terminators[block].clone());
    let rename = Rename {
        values,
        slot: body.slots.len() as u32,
        address: body.addresses.len() as u32,
        block: body.blocks.len() as u32,
        call: body.calls.len() as u32,
        edge: body.edges.len() as u32,
        value_pool: body.pools.values.len() as u32,
        fail: call.fail,
    };
    body.slots.extend_from_slice(&callee.slots);
    body.pools
        .values
        .extend(callee.pools.values.iter().map(|&v| rename.v(v)));
    for a in &callee.addresses {
        let mut a = a.clone();
        a.base = match a.base {
            AddressBase::Slot(s) => AddressBase::Slot(SlotId(s.0 + rename.slot)),
            AddressBase::Value(v) => AddressBase::Value(rename.v(v)),
            AddressBase::Address(a) => AddressBase::Address(rename.a(a)),
        };
        a.steps = PoolRange::push(
            &mut body.pools.steps,
            a.steps
                .get(&callee.pools.steps)
                .unwrap()
                .iter()
                .map(|&s| match s {
                    Step::Index(v) => Step::Index(rename.v(v)),
                    Step::Element(v) => Step::Element(rename.v(v)),
                    Step::Byte(v) => Step::Byte(rename.v(v)),
                    other => other,
                }),
        );
        body.addresses.push(a);
    }
    for c in &callee.calls {
        let mut c = c.clone();
        if let Callee::Indirect(v) = c.callee {
            c.callee = Callee::Indirect(rename.v(v));
        }
        c.args = PoolRange::push(
            &mut body.pools.args,
            c.args
                .get(&callee.pools.args)
                .unwrap()
                .iter()
                .map(|&a| match a {
                    Arg::Value(v) => Arg::Value(rename.v(v)),
                    Arg::Address(a) => Arg::Address(rename.a(a)),
                }),
        );
        c.results = rename.range(c.results);
        c.fail = rename.f(c.fail);
        body.calls.push(c);
    }
    for e in &callee.edges {
        body.edges.push(Edge {
            target: rename.b(e.target),
            args: rename.range(e.args),
            cold: e.cold,
        });
    }
    for (b, row) in callee.blocks.iter().enumerate() {
        let start = body.insts.len() as u32;
        for i in row.insts.start as usize..(row.insts.start + row.insts.len) as usize {
            let mut inst = callee.insts[i].clone();
            rename.inst(&mut inst);
            body.insts.push(inst);
            body.inst_anchors.push(anchor(callee.inst_anchors[i]));
        }
        body.blocks.push(Block {
            params: if b == 0 {
                PoolRange::EMPTY
            } else {
                rename.range(row.params)
            },
            insts: PoolRange::new(start, body.insts.len() as u32 - start),
            anchor: anchor(row.anchor),
        });
        let term = match callee.terminators[b] {
            Terminator::Jump(e) => Terminator::Jump(rename.e(e)),
            Terminator::Branch { condition, yes, no } => Terminator::Branch {
                condition: rename.v(condition),
                yes: rename.e(yes),
                no: rename.e(no),
            },
            Terminator::Switch {
                value,
                arms,
                otherwise,
            } => Terminator::Switch {
                value: rename.v(value),
                otherwise: rename.e(otherwise),
                arms: PoolRange::push(
                    &mut body.pools.arms,
                    arms.get(&callee.pools.arms)
                        .unwrap()
                        .iter()
                        .map(|a| SwitchArm {
                            value: a.value,
                            edge: rename.e(a.edge),
                        }),
                ),
            },
            Terminator::Return(args) => {
                Terminator::Jump(edge(body, continuation, rename.range(args)))
            }
            Terminator::Propagate => match call.fail {
                Some(fail) => Terminator::Jump(edge(body, fail, PoolRange::EMPTY)),
                None => Terminator::Propagate,
            },
            Terminator::Raise { cause, cleanup } => Terminator::Raise {
                cause: match cause {
                    RaiseCause::Fail(a) => RaiseCause::Fail(rename.a(a)),
                    other => other,
                },
                cleanup: rename.f(cleanup),
            },
            Terminator::Unreachable => Terminator::Unreachable,
        };
        body.terminators.push(term);
    }
    let start = body.insts.len() as u32;
    body.insts.extend_from_within(range.start as usize..at);
    body.inst_anchors
        .extend_from_within(range.start as usize..at);
    for pointer in pointers {
        body.insts.push(pointer);
        body.inst_anchors.push(None);
    }
    body.blocks[block].insts = PoolRange::new(start, body.insts.len() as u32 - start);
    body.terminators[block] = Terminator::Jump(edge(body, BlockId(rename.block), PoolRange::EMPTY));
    (continuation.index()..body.blocks.len()).collect()
}

pub(super) fn module(module: &Module) -> Option<Module> {
    let eligible = module
        .functions
        .iter()
        .map(|f| {
            f.body.as_ref().filter(|b| {
                b.insts.len() <= 192
                    && b.blocks.len() <= 24
                    && b.insts.iter().all(supported)
                    && cyclic_blocks(b).is_empty()
            })
        })
        .collect::<Vec<_>>();
    let mut result = None;
    for (i, f) in module.functions.iter().enumerate() {
        let Some(original) = &f.body else {
            continue;
        };
        if original.calls.is_empty() {
            continue;
        }
        let mut hot = cyclic_blocks(original);
        if hot.is_empty() {
            continue;
        }
        let mut body = original.clone();
        let mut budget = 1024;
        loop {
            let selected = hot.iter().find_map(|&b| {
                body.blocks[b]
                    .insts
                    .get(&body.insts)
                    .unwrap()
                    .iter()
                    .enumerate()
                    .find_map(|(offset, inst)| {
                        let Inst::Call { call } = *inst else {
                            return None;
                        };
                        let call = &body.calls[call.index()];
                        let Callee::Direct(f) = call.callee else {
                            return None;
                        };
                        let callee = eligible[f.index()]?;
                        (!call.cold && f.index() != i && callee.insts.len().max(1) <= budget)
                            .then_some((b, offset, callee))
                    })
            });
            let Some((b, offset, callee)) = selected else {
                break;
            };
            budget -= callee.insts.len().max(1);
            let Inst::Call { call } = body.insts[body.blocks[b].insts.start as usize + offset]
            else {
                unreachable!()
            };
            let Callee::Direct(f) = body.calls[call.index()].callee else {
                unreachable!()
            };
            let inherits = hirsel_core_ir::source::inherits_site(
                &module.instances[module.functions[f.index()].instance.index()],
            );
            hot.extend(expand(&mut body, b, offset, callee, module, inherits));
        }
        if budget != 1024 {
            let old = std::mem::take(&mut body.insts);
            let anchors = std::mem::take(&mut body.inst_anchors);
            for block in &mut body.blocks {
                let range = block.insts;
                let start = body.insts.len() as u32;
                body.insts.extend_from_slice(range.get(&old).unwrap());
                body.inst_anchors.extend_from_slice(
                    &anchors[range.start as usize..(range.start + range.len) as usize],
                );
                block.insts = PoolRange::new(start, range.len);
            }
            result.get_or_insert_with(|| module.clone()).functions[i].body = Some(body);
        }
    }
    result
}
