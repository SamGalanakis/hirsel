use super::*;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum SimdCall {
    Value(SimdOp),
    PutLanes,
    SetLanes,
}

impl Cx<'_, '_, '_> {
    pub(super) fn vector_shape(&self, ty: TyId) -> Option<Vector> {
        match *self.t.kind(ty) {
            TyKind::Vector(shape) => Some(shape),
            _ => None,
        }
    }

    fn vector_type(&mut self, shape: Vector) -> TyId {
        self.t.mk(TyKind::Vector(shape))
    }

    fn require_vector(&mut self, ty: TyId, n: NodeId) -> Option<Vector> {
        let shape = self.vector_shape(ty);
        if shape.is_none() && !matches!(ty, ERROR | NEVER) {
            self.refuse("SEM-11", n, "this operand must be an integer vector");
        }
        shape
    }

    pub(super) fn simd_call(
        &mut self,
        op: SimdCall,
        f: usize,
        n: NodeId,
        args: &[NodeId],
        want: Option<Want>,
    ) -> (TyId, Option<ValueId>) {
        let g = &self.p.fns[f];
        if args.len() != g.params.len() {
            let k = g.params.len();
            let at = args
                .get(k)
                .map_or_else(|| self.close_brace(n), |&arg| self.span(arg));
            self.refuse_arity_at(at, args.len(), k, format!("expected {k} arguments"));
            return (ERROR, None);
        }
        let expected = want
            .filter(|w| w.payload == 0)
            .and_then(|w| self.vector_shape(w.ty));
        let SimdCall::Value(op) = op else {
            if self.patterns.guard {
                self.refuse("LANG-17", n, "a match guard cannot call a mut parameter");
                return (ERROR, None);
            }
            self.call_seq += 1;
            let call = self.call_seq;
            let held = self.freezes.len();
            let mut header = None;
            self.argument(
                Mode::Mut,
                g.params[0].ty,
                call,
                0,
                args[0],
                &mut [],
                &mut header,
            );
            let (value_arg, value, other, shape, label) = match op {
                SimdCall::PutLanes => {
                    let (ty, value) = self.expr(args[1], None);
                    let shape = self.require_vector(ty, args[1]);
                    let (_, count) = self.expr(args[2], expect(scalar(Scalar::U64)));
                    (args[1], value, count, shape, "put_lanes")
                }
                SimdCall::SetLanes => {
                    let (_, at) = self.expr(args[1], expect(scalar(Scalar::U64)));
                    let (ty, value) = self.expr(args[2], None);
                    let shape = self.require_vector(ty, args[2]);
                    (args[2], value, at, shape, "set_lanes")
                }
                SimdCall::Value(_) => unreachable!(),
            };
            self.unfreeze(held, |h| match h {
                Hold::CallView(c, _) | Hold::CallMut(c, _) if c == call => None,
                _ => Some(h),
            });
            let Some(shape) = shape else {
                return (ERROR, None);
            };
            if shape.lane() != Scalar::U8 {
                self.refuse(
                    "SEM-11",
                    value_arg,
                    format!("{label} takes a vector of u8 lanes"),
                );
                return (ERROR, None);
            }
            self.builtin_use(f, n);
            self.source_step(n);
            let result = g.result;
            let value = match op {
                SimdCall::PutLanes => self.put_lanes(header, value, other, shape, result, n),
                SimdCall::SetLanes => self.set_lanes(header, other, value, shape, n),
                SimdCall::Value(_) => unreachable!(),
            };
            return (result, value);
        };
        use SimdOp::*;
        let a = args[0];
        let (result, first, second) = match op {
            Splat => {
                let Some(shape) = expected else {
                    return self.simd_expected(n);
                };
                let (_, value) = self.expr(a, expect(scalar(shape.lane())));
                (self.vector_type(shape), value, None)
            }
            From => {
                let hint = expected.map(|v| self.t.mk(TyKind::Array(scalar(v.lane()), v.lanes())));
                let (input, value) = self.expr(a, hint.and_then(expect));
                let TyKind::Array(lane, count) = *self.t.kind(input) else {
                    if !matches!(input, ERROR | NEVER) {
                        self.refuse(
                            "SEM-11",
                            a,
                            "simd.from takes an Inline array of integer lanes",
                        );
                    }
                    return (ERROR, None);
                };
                match crate::decls::vector_shape(&self.t, lane, Some(count)) {
                    Ok(shape) => (self.vector_type(shape), value, None),
                    Err(message) => {
                        self.refuse("SEM-11", a, message);
                        return (ERROR, None);
                    }
                }
            }
            Cast => {
                let Some(shape) = expected else {
                    return self.simd_expected(n);
                };
                let (input, value) = self.expr(a, None);
                let Some(from) = self.require_vector(input, a) else {
                    return (ERROR, None);
                };
                if from.size() != shape.size() {
                    self.refuse("SEM-11", n, "simd.cast preserves the vector's byte width");
                    return (ERROR, None);
                }
                (self.vector_type(shape), value, None)
            }
            Lanes => {
                let Some(shape) = expected else {
                    return self.simd_expected(n);
                };
                if shape.lane() != Scalar::U8 {
                    self.refuse("SEM-11", n, "list.lanes reads a vector of u8 lanes");
                    return (ERROR, None);
                }
                self.call_seq += 1;
                let call = self.call_seq;
                let held = self.freezes.len();
                let input = g.params[0].ty;
                let mut header = None;
                if !self.argument(Mode::View, input, call, 0, a, &mut [], &mut header) {
                    header = self
                        .expr(
                            a,
                            Some(Want {
                                ty: input,
                                view: true,
                                payload: 0,
                            }),
                        )
                        .1;
                }
                let (_, offset) = self.expr(args[1], expect(scalar(Scalar::U64)));
                self.unfreeze(held, |h| match h {
                    Hold::CallView(c, _) if c == call => None,
                    _ => Some(h),
                });
                self.builtin_use(f, n);
                self.source_step(n);
                let address = self.lanes_address(header, offset, shape, n);
                let ty = self.vector_type(shape);
                return (ty, self.emit_simd(op, ty, address, None, n));
            }
            Any | Bitmask => {
                let (input, value) = self.expr(a, None);
                if self.require_vector(input, a).is_none() {
                    return (ERROR, None);
                }
                (g.result, value, None)
            }
            Swizzle => {
                let (table_ty, table) = self.expr(a, None);
                let Some(table_shape) = self.require_vector(table_ty, a) else {
                    return (ERROR, None);
                };
                if !matches!(table_shape.lane(), Scalar::U8 | Scalar::I8) {
                    self.refuse("SEM-11", a, "a swizzle table has u8 or i8 lanes");
                    return (ERROR, None);
                }
                let hint = expected.map(|v| {
                    self.vector_type(if v.lanes() == 16 {
                        Vector::U8x16
                    } else {
                        Vector::U8x32
                    })
                });
                let (index_ty, indices) = self.expr(args[1], hint.and_then(expect));
                let Some(index_shape) = self.require_vector(index_ty, args[1]) else {
                    return (ERROR, None);
                };
                if index_shape.lane() != Scalar::U8 {
                    self.refuse("SEM-11", args[1], "swizzle indices have u8 lanes");
                    return (ERROR, None);
                }
                let shape = Vector::new(table_shape.lane(), index_shape.lanes()).unwrap();
                (self.vector_type(shape), table, indices)
            }
            WrappingAdd | WrappingSub | SaturatingAdd | SaturatingSub | Eq | Lt => {
                let hint = expected.map(|v| self.vector_type(v));
                let (ty, left) = self.expr(a, hint.and_then(expect));
                if self.require_vector(ty, a).is_none() {
                    return (ERROR, None);
                }
                let (_, right) = self.expr(args[1], expect(ty));
                (ty, left, right)
            }
            BitAnd | BitOr | BitXor | BitNot | Shl | Shr => unreachable!(),
        };
        self.builtin_use(f, n);
        self.source_step(n);
        (result, self.emit_simd(op, result, first, second, n))
    }

    fn simd_expected(&mut self, n: NodeId) -> (TyId, Option<ValueId>) {
        self.refuse("SEM-11", n, "this operation needs an expected vector type");
        (ERROR, None)
    }

    pub(super) fn emit_simd(
        &mut self,
        op: SimdOp,
        ty: TyId,
        first: Option<ValueId>,
        second: Option<ValueId>,
        n: NodeId,
    ) -> Option<ValueId> {
        if !self.live() || (op.binary() && second.is_none()) {
            return None;
        }
        let first = first?;
        let result = self.value(value_ty(&self.t, ty).ok().flatten()?);
        let fail = if op.can_fail() { self.fail_to(n) } else { None };
        self.inst(
            Inst::Simd {
                result,
                op,
                first,
                second,
                fail,
            },
            n,
        );
        if op.can_fail() {
            let span = self.op_span(n);
            if let Some(Some(anchor)) = self.open_anchors.last_mut() {
                anchor.start = span.start;
                anchor.end = span.end;
            }
        }
        Some(result)
    }

    fn simd_bounds(&mut self, valid: ValueId, n: NodeId) {
        let ok = self.new_block(&[]);
        let fail = self.new_block(&[]);
        self.list_branch(valid, ok, fail, n);
        self.start(fail);
        let cleanup = self.fail_to(n);
        self.end(
            Terminator::Raise {
                cause: hirsel_core_ir::RaiseCause::Bounds,
                cleanup,
            },
            Some(n),
        );
        self.start(ok);
    }

    fn lanes_address(
        &mut self,
        header: Option<ValueId>,
        at: Option<ValueId>,
        shape: Vector,
        n: NodeId,
    ) -> Option<ValueId> {
        let (header, at) = (header?, at?);
        let u64_ty = Ty::Scalar(Scalar::U64);
        let carrier = self.list_carrier(scalar(Scalar::U8), n)?;
        let len = self.carrier_word(header, carrier, SeqWord::Len, n);
        let width = self.konst(u64_ty, shape.lanes(), n)?;
        let (end, overflow) = self.list_arith(ArithOp::Add, ArithMode::Checked, at, width, n);
        let no_overflow = self.emit_unary(UnaryOp::Not, BOOL, overflow, n)?;
        let within = self.list_compare(CompareOp::Le, end, len, n);
        let valid = self.list_bool(BinaryOp::BitAnd, no_overflow, within, n);
        self.simd_bounds(valid, n);
        let data = self.carrier_word(header, carrier, SeqWord::Data, n);
        let from = self.address(
            AddressBase::Value(data),
            &[Step::Byte(at)],
            PlaceTy::Value(Ty::Scalar(Scalar::U8)),
            n,
        );
        Some(self.pointer(from, n))
    }

    fn put_lanes(
        &mut self,
        header: Option<ValueId>,
        vector: Option<ValueId>,
        count: Option<ValueId>,
        shape: Vector,
        result: TyId,
        n: NodeId,
    ) -> Option<ValueId> {
        if !self.live() {
            return None;
        }
        let (header, vector, count) = (header?, vector?, count?);
        let u64_ty = Ty::Scalar(Scalar::U64);
        let width = self.konst(u64_ty, shape.lanes(), n)?;
        let valid = self.list_compare(CompareOp::Le, count, width, n);
        self.simd_bounds(valid, n);

        let byte_ty = scalar(Scalar::U8);
        let carrier = self.list_carrier(byte_ty, n)?;
        let row = self.place_row(result, "put_lanes requires a Result place", n)?;
        let len = self.carrier_word(header, carrier, SeqWord::Len, n);
        let reserved = self.list_reserve(byte_ty, header, count, lists::Ask::Bounded, result, n)?;
        let result_at = self.address(AddressBase::Value(reserved), &[], PlaceTy::Declared(row), n);
        let tag = self.value(Ty::Scalar(Scalar::U32));
        self.inst(
            Inst::Case {
                result: tag,
                address: result_at,
            },
            n,
        );
        let zero = self.konst(Ty::Scalar(Scalar::U32), 0, n)?;
        let ok = self.list_compare(CompareOp::Eq, tag, zero, n);
        let store = self.new_block(&[]);
        let done = self.new_block(&[]);
        self.list_branch(ok, store, done, n);
        self.start(store);
        let data = self.carrier_word(header, carrier, SeqWord::Data, n);
        let to = self.address(
            AddressBase::Value(data),
            &[Step::Byte(len)],
            PlaceTy::Value(Ty::Scalar(Scalar::U8)),
            n,
        );
        self.inst(Inst::PutLanes { to, vector, count }, n);
        let (len, _) = self.list_arith(ArithOp::Add, ArithMode::Wrapping, len, count, n);
        self.set_carrier_word(header, carrier, SeqWord::Len, len, n);
        self.jump(done, &[], n);
        self.start(done);
        Some(reserved)
    }

    fn set_lanes(
        &mut self,
        header: Option<ValueId>,
        at: Option<ValueId>,
        vector: Option<ValueId>,
        shape: Vector,
        n: NodeId,
    ) -> Option<ValueId> {
        if !self.live() {
            return None;
        }
        let (header, at, vector) = (header?, at?, vector?);
        let u64_ty = Ty::Scalar(Scalar::U64);
        let carrier = self.list_carrier(scalar(Scalar::U8), n)?;
        let len = self.carrier_word(header, carrier, SeqWord::Len, n);
        let width = self.konst(u64_ty, shape.lanes(), n)?;
        let (end, overflow) = self.list_arith(ArithOp::Add, ArithMode::Checked, at, width, n);
        let no_overflow = self.emit_unary(UnaryOp::Not, BOOL, overflow, n)?;
        let within = self.list_compare(CompareOp::Le, end, len, n);
        let valid = self.list_bool(BinaryOp::BitAnd, no_overflow, within, n);
        self.simd_bounds(valid, n);
        let data = self.carrier_word(header, carrier, SeqWord::Data, n);
        let to = self.address(
            AddressBase::Value(data),
            &[Step::Byte(at)],
            PlaceTy::Value(Ty::Scalar(Scalar::U8)),
            n,
        );
        self.inst(
            Inst::PutLanes {
                to,
                vector,
                count: width,
            },
            n,
        );
        None
    }
}
