use super::*;
use hirsel_core_layout::flow::Op;

const WORD: Ty = Ty::Scalar(Scalar::U64);
const INDEX: Ty = Ty::Scalar(Scalar::U32);
const BYTE: Ty = Ty::Scalar(Scalar::U8);

impl Cx<'_, '_, '_> {
    fn runtime_flow_stage(
        &mut self,
        flow: ValueId,
        types: &[TyId],
        vals: &[Option<ValueId>],
        result: TyId,
        n: NodeId,
    ) -> Option<ValueId> {
        let [x, a, b] = [types[0], types[1], types[2]];
        let option = self.prelude("Option", n)?;
        let option = self.t.mk(TyKind::Adt(option, vec![b]));
        let returned = self.t.mk(TyKind::Tuple(vec![x, option]));
        let callable = self.t.mk(TyKind::Fn(
            vec![
                (Mode::Take, x),
                (Mode::View, scalar(Scalar::U64)),
                (Mode::View, a),
            ],
            returned,
            false,
            Callable::Plain,
        ));
        let code = self.runtime_code_callable(vals[6]?, callable, n)?;
        let adapter = self.flow_transform(x, a, b, option, returned, callable, n)?;
        let (_, state) = self.runtime_place(x, vals[5], n)?;
        let state = self.pointer(state, n);
        let physical_state = self.layout_size(x, false, n)?;
        let logical_state = self.layout_size(x, true, n)?;
        let physical_output = self.layout_size(b, false, n)?;
        let logical_output = self.layout_size(b, true, n)?;
        let words = self.flow_call(
            Op::Stage,
            flow,
            &[
                vals[1]?,
                vals[2]?,
                vals[3]?,
                vals[4]?,
                state,
                physical_state,
                logical_state,
                physical_output,
                logical_output,
                code,
                adapter,
            ],
            n,
        )?;
        self.flow_wire(result, words, false, n)
    }

    #[allow(clippy::too_many_arguments)]
    fn flow_transform(
        &mut self,
        x: TyId,
        a: TyId,
        b: TyId,
        option: TyId,
        returned: TyId,
        callable: TyId,
        n: NodeId,
    ) -> Option<ValueId> {
        let arguments = [Ty::Ptr, Ty::Ptr, WORD, Ty::Ptr, Ty::Ptr, Ty::Ptr];
        let sig = Signature::new(
            arguments.iter().copied().map(Param::Value).collect(),
            Vec::new(),
            None,
        );
        let mut fd = self.fd.as_ref().clone();
        fd.path.0.name = self.generated_name(GeneratedKind::FlowTransform, n);
        fd.params.clear();
        fd.result = UNIT;
        fd.drop_of = None;
        fd.clone_of = None;
        fd.body = None;
        let mut child = Cx::new(self.p, self.definition, self.e);
        child.fd = std::borrow::Cow::Owned(fd);
        child.entry = false;
        std::mem::swap(&mut child.t, &mut self.t);
        let row = child.e.function(&child.fd.path, sig);
        child.func = Some(row);
        let entry = child.new_block(&arguments);
        child.start(entry);
        let [code, state, key, item, output, present] = child.block_params(entry)[..] else {
            unreachable!()
        };
        child.emit_flow_transform(
            x, a, b, option, returned, callable, code, state, key, item, output, present, n,
        );
        std::mem::swap(&mut child.t, &mut self.t);
        let out = child.finish();
        if let Some(template) = out.template {
            self.nested.insert(row, template);
        }
        self.out.merge_refusals(out.refusals, out.resource_refusal);
        self.out.gaps.extend(out.gaps);
        self.out.counts += out.counts;
        self.out.computed.extend(out.computed);
        if out.function.is_none() {
            self.halt();
            return None;
        }
        let code = self.value(Ty::Ptr);
        self.inst(
            Inst::FunctionRef {
                result: code,
                function: row,
            },
            n,
        );
        Some(code)
    }

    #[allow(clippy::too_many_arguments)]
    fn emit_flow_transform(
        &mut self,
        x: TyId,
        a: TyId,
        b: TyId,
        option: TyId,
        returned: TyId,
        callable: TyId,
        code: ValueId,
        state: ValueId,
        key: ValueId,
        item: ValueId,
        output: ValueId,
        present: ValueId,
        n: NodeId,
    ) -> Option<()> {
        let x_rep = self.rep_at(x, n)?;
        let a_rep = self.rep_at(a, n)?;
        let state_at = x_rep
            .stored()
            .map(|t| self.address(AddressBase::Value(state), &[], t, n));
        let item_at = a_rep
            .stored()
            .map(|t| self.address(AddressBase::Value(item), &[], t, n));
        let state_value = state_at.and_then(|at| self.read_carried_rep(at, x_rep, n));
        let item_value = item_at.and_then(|at| self.read_carried_rep(at, a_rep, n));
        let value = self.call_lowered(callable, code, &[state_value, Some(key), item_value], n)?;
        let tuple_row = self.runtime_type(returned, n)?;
        let tuple = self.address(
            AddressBase::Value(value),
            &[],
            PlaceTy::Declared(tuple_row),
            n,
        );
        if let Some(to) = state_at {
            let from = self.runtime_field(tuple, &[Step::Field(0)], x, n)?;
            let value = self.read_carried_rep(from, x_rep, n);
            self.install(to, x_rep, value, n);
        }
        let option_row = self.runtime_type(option, n)?;
        let option = self.address(
            AddressBase::Address(tuple),
            &[Step::Field(1)],
            PlaceTy::Declared(option_row),
            n,
        );
        let tag = self.value(INDEX);
        self.inst(
            Inst::Case {
                result: tag,
                address: option,
            },
            n,
        );
        let bit = self.flow_convert(tag, BYTE, n)?;
        let at = self.address(AddressBase::Value(present), &[], PlaceTy::Value(BYTE), n);
        self.inst(
            Inst::Store {
                address: at,
                value: bit,
            },
            n,
        );
        let some = self.runtime_compare(INDEX, tag, 1, n)?;
        let write = self.new_block(&[]);
        let done = self.new_block(&[]);
        self.runtime_branch(some, write, done, n);
        self.start(write);
        let rep = self.rep_at(b, n)?;
        if let Some(stored) = rep.stored() {
            let from =
                self.runtime_field(option, &[Step::CaseField { case: 1, field: 0 }], b, n)?;
            let value = self.read_carried_rep(from, rep, n);
            let to = self.address(AddressBase::Value(output), &[], stored, n);
            self.install(to, rep, value, n);
        }
        self.jump(done, &[], n);
        self.start(done);
        self.ret(None, n);
        Some(())
    }

    fn flow_convert(&mut self, value: ValueId, ty: Ty, n: NodeId) -> Option<ValueId> {
        let from = self.b.values[value.index()];
        if from == ty {
            return Some(value);
        }
        if from == Ty::Ptr || ty == Ty::Ptr {
            return self.reinterpret(value, from, ty, n);
        }
        let result = self.value(ty);
        self.inst(
            Inst::Convert {
                result,
                flag: None,
                operand: value,
                mode: if from.int()?.1 < ty.int()?.1 {
                    ConvertMode::Widen
                } else {
                    ConvertMode::Truncate
                },
            },
            n,
        );
        Some(result)
    }

    fn flow_words(&mut self, len: usize, n: NodeId) -> Option<AddressId> {
        let ty = self.t.mk(TyKind::Tuple(vec![scalar(Scalar::U64); len]));
        let row = self.runtime_type(ty, n)?;
        Some(self.new_slot(PlaceTy::Declared(row), n))
    }

    fn flow_call(
        &mut self,
        op: Op,
        flow: ValueId,
        args: &[ValueId],
        n: NodeId,
    ) -> Option<[ValueId; 4]> {
        let words = self.flow_words(args.len().max(1), n)?;
        for (i, &value) in args.iter().enumerate() {
            let value = self.flow_convert(value, WORD, n)?;
            self.runtime_store(words, &[Step::Field(i as u32)], WORD, value, n);
        }
        let out = self.flow_words(4, n)?;
        let args = self.pointer(words, n);
        let output = self.pointer(out, n);
        self.runtime_row(
            RtOp::Flow(op),
            &[Arg::Value(flow), Arg::Value(args), Arg::Value(output)],
            n,
        )?;
        Some(std::array::from_fn(|i| {
            let addr = self.address(
                AddressBase::Address(out),
                &[Step::Field(i as u32)],
                PlaceTy::Value(WORD),
                n,
            );
            self.read_bits(addr, WORD, n)
        }))
    }

    /// Initialize one variant arm for a status encoded in the runtime's word packet.
    fn flow_cases(
        &mut self,
        to: AddressId,
        code: ValueId,
        count: u32,
        n: NodeId,
        mut payload: impl FnMut(&mut Self, u32),
    ) {
        let join = self.new_block(&[]);
        for case in 0..count {
            let next = if case + 1 < count {
                let arm = self.new_block(&[]);
                let next = self.new_block(&[]);
                let yes = self
                    .runtime_compare(WORD, code, u64::from(case), n)
                    .unwrap();
                self.runtime_branch(yes, arm, next, n);
                self.start(arm);
                Some(next)
            } else {
                None
            };
            self.inst(Inst::SetCase { address: to, case }, n);
            payload(self, case);
            self.jump(join, &[], n);
            if let Some(next) = next {
                self.start(next)
            }
        }
        self.start(join);
    }

    fn flow_wire(
        &mut self,
        result: TyId,
        words: [ValueId; 4],
        pair: bool,
        n: NodeId,
    ) -> Option<ValueId> {
        let [code, token, available, _] = words;
        let ok = self.runtime_compare(WORD, code, 0, n)?;
        self.runtime_result(
            result,
            ok,
            n,
            |cx, to, ty| {
                if pair {
                    let at = cx
                        .runtime_field(to, &[Step::CaseField { case: 0, field: 0 }], ty, n)
                        .unwrap();
                    for field in [0, 1] {
                        cx.runtime_store(at, &[Step::Field(field)], WORD, token, n);
                    }
                } else {
                    cx.runtime_store(to, &[Step::CaseField { case: 0, field: 0 }], WORD, token, n);
                }
            },
            |cx, to, wire| {
                let at = cx
                    .runtime_field(to, &[Step::CaseField { case: 1, field: 0 }], wire, n)
                    .unwrap();
                let one = cx.konst(WORD, 1, n).unwrap();
                let status = cx.value(WORD);
                cx.inst(
                    Inst::IntArith {
                        result: status,
                        op: ArithOp::Sub,
                        left: code,
                        right: one,
                        mode: ArithMode::Wrapping,
                        flag: None,
                    },
                    n,
                );
                cx.flow_cases(at, status, 4, n, |cx, case| {
                    if case == 3 {
                        let DeclBody::Variant(variants) = &cx.p.decls[match cx.t.kind(wire) {
                            TyKind::Adt(id, _) => id.index(),
                            _ => unreachable!(),
                        }]
                        .body
                        else {
                            unreachable!()
                        };
                        let capacity = variants[3].fields[0].ty;
                        let out = cx
                            .runtime_field(at, &[Step::CaseField { case, field: 0 }], capacity, n)
                            .unwrap();
                        cx.runtime_capacity(out, token, available, n);
                    }
                });
            },
        )
    }

    pub(super) fn runtime_flow(
        &mut self,
        op: Op,
        types: &[TyId],
        vals: &[Option<ValueId>],
        result: TyId,
        n: NodeId,
    ) -> Option<ValueId> {
        let flow = if op == Op::New {
            self.konst(Ty::Ptr, 0, n)?
        } else {
            self.runtime_handle(vals[0]?, n)
        };
        match op {
            Op::New => {
                let [_, handle, ..] = self.flow_call(op, flow, &[], n)?;
                let handle = self.flow_convert(handle, Ty::Ptr, n)?;
                self.runtime_carrier(result, handle, n)
            }
            Op::Sink => {
                let words = self.flow_call(op, flow, &[], n)?;
                self.flow_wire(result, words, true, n)
            }
            Op::Port => {
                let size = self.layout_size(types[0], true, n)?;
                let physical = self.layout_size(types[0], false, n)?;
                let words = self.flow_call(op, flow, &[vals[1]?, vals[2]?, size, physical], n)?;
                self.flow_wire(result, words, false, n)
            }
            Op::Advance => {
                self.flow_call(op, flow, &[vals[1]?, vals[2]?], n)?;
                None
            }
            Op::End => {
                self.flow_call(op, flow, &[vals[1]?], n)?;
                None
            }
            Op::Push => {
                let item = types[0];
                let (_, place) = self.runtime_place(item, vals[3], n)?;
                let value = self.pointer(place, n);
                let size = self.layout_size(item, false, n)?;
                let [status, ..] =
                    self.flow_call(op, flow, &[vals[1]?, vals[2]?, value, size], n)?;
                let row = self.runtime_type(result, n)?;
                let out = self.new_slot(PlaceTy::Declared(row), n);
                let rep = self.rep_at(item, n)?;
                self.flow_cases(out, status, 5, n, |cx, case| {
                    if case != 0
                        && let Some(to) =
                            cx.runtime_field(out, &[Step::CaseField { case, field: 0 }], item, n)
                    {
                        cx.install(to, rep, vals[3], n);
                    }
                });
                let value = self.pointer(out, n);
                self.temp(result, Some(value));
                Some(value)
            }
            Op::Next => {
                let item = types[0];
                let id = self.runtime_type(item, n)?;
                let place = self.new_slot(PlaceTy::Declared(id), n);
                let value = self.pointer(place, n);
                let [status, key, from, _] = self.flow_call(op, flow, &[vals[1]?, value], n)?;
                let from = self.flow_convert(from, INDEX, n)?;
                let row = self.runtime_type(result, n)?;
                let out = self.new_slot(PlaceTy::Declared(row), n);
                let rep = self.rep_at(item, n)?;
                self.flow_cases(out, status, 4, n, |cx, case| match case {
                    0 => {
                        cx.runtime_store(out, &[Step::CaseField { case, field: 0 }], WORD, key, n);
                        cx.runtime_store(
                            out,
                            &[Step::CaseField { case, field: 1 }],
                            INDEX,
                            from,
                            n,
                        );
                        if let Some(to) =
                            cx.runtime_field(out, &[Step::CaseField { case, field: 2 }], item, n)
                        {
                            let value = match rep {
                                Rep::Scalar(s) => {
                                    let field = cx.address(
                                        AddressBase::Address(place),
                                        &[Step::Field(0)],
                                        PlaceTy::Value(s),
                                        n,
                                    );
                                    Some(cx.read_bits(field, s, n))
                                }
                                _ => Some(value),
                            };
                            cx.install(to, rep, value, n);
                        }
                    }
                    1 => {
                        cx.runtime_store(
                            out,
                            &[Step::CaseField { case, field: 0 }],
                            INDEX,
                            from,
                            n,
                        );
                        cx.runtime_store(out, &[Step::CaseField { case, field: 1 }], WORD, key, n);
                    }
                    _ => {}
                });
                let value = self.pointer(out, n);
                self.temp(result, Some(value));
                Some(value)
            }
            Op::Finish => {
                self.consume(vals[0], n);
                let [status, index, key, halt] = self.flow_call(op, flow, &[], n)?;
                let ok = self.runtime_compare(WORD, status, 0, n)?;
                let index = self.flow_convert(index, INDEX, n)?;
                self.runtime_result(
                    result,
                    ok,
                    n,
                    |_, _, _| {},
                    |cx, to, failure| {
                        let at = cx
                            .runtime_field(to, &[Step::CaseField { case: 1, field: 0 }], failure, n)
                            .unwrap();
                        cx.runtime_store(at, &[Step::Field(0)], INDEX, index, n);
                        cx.runtime_store(at, &[Step::Field(1)], WORD, key, n);
                        let value = cx.flow_convert(halt, BYTE, n).unwrap();
                        cx.runtime_store(at, &[Step::Field(2)], BYTE, value, n);
                    },
                )
            }
            Op::Stage => self.runtime_flow_stage(flow, types, vals, result, n),
        }
    }
}
