use super::*;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum SimdCall {
    Value(SimdOp),
    PutLanes,
    SetLanes,
    InlineLanes,
    InlineSetLanes,
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
        if let SimdCall::InlineLanes | SimdCall::InlineSetLanes = op {
            let shape = expected;
            return self.inline_lane_call(op == SimdCall::InlineSetLanes, f, n, args, shape);
        }
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
                SimdCall::Value(_) | SimdCall::InlineLanes | SimdCall::InlineSetLanes => {
                    unreachable!()
                }
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
                SimdCall::PutLanes => self.put_lanes(header, value, other, shape, n),
                SimdCall::SetLanes => self.set_lanes(header, other, value, shape, n),
                SimdCall::Value(_) | SimdCall::InlineLanes | SimdCall::InlineSetLanes => {
                    unreachable!()
                }
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

    /// Types `inline.lanes(a, at)` and `inline.set_lanes(mut a, at, v)` at the call:
    /// the array from the argument, the vector from the expected type or from `v`.
    fn inline_lane_call(
        &mut self,
        write: bool,
        f: usize,
        n: NodeId,
        args: &[NodeId],
        expected: Option<Vector>,
    ) -> (TyId, Option<ValueId>) {
        if write && self.patterns.guard {
            self.refuse("LANG-17", n, "a match guard cannot call a mut parameter");
            return (ERROR, None);
        }
        if !write && expected.is_none() {
            return self.simd_expected(n);
        }
        let g = &self.p.fns[f];
        let (param, result, generics) = (g.params[0].ty, g.result, g.generics.len());
        self.call_seq += 1;
        let call = self.call_seq;
        let held = self.freezes.len();
        let mut found = vec![None; generics];
        let mut data = None;
        let mode = if write { Mode::Mut } else { Mode::View };
        if !self.argument(mode, param, call, 0, args[0], &mut found, &mut data) {
            let (ty, value) = self.expr(args[0], None);
            found[0] = Some(ty);
            data = value;
        }
        let (_, at) = self.expr(args[1], expect(scalar(Scalar::U64)));
        let (shape, value) = if write {
            let (ty, value) = self.expr(args[2], None);
            (self.require_vector(ty, args[2]), value)
        } else {
            (expected, None)
        };
        self.unfreeze(held, |h| match h {
            Hold::CallView(c, _) | Hold::CallMut(c, _) if c == call => None,
            _ => Some(h),
        });
        let Some(shape) = shape else {
            return (ERROR, None);
        };
        let Some(array) = found[0].map(|ty| self.bare(ty)) else {
            self.refuse(
                "SEM-11",
                args[0],
                "an Inline lane access takes an Inline array of u8",
            );
            return (ERROR, None);
        };
        self.builtin_use(f, n);
        self.source_step(n);
        let vector = self.vector_type(shape);
        let value = match at {
            Some(at) if self.live() => self.inline_lanes(array, vector, data, at, value, write, n),
            _ => None,
        };
        (if write { result } else { vector }, value)
    }

    /// `inline.lanes` and `inline.set_lanes`: one vector of u8 lanes read from or
    /// written to an `Inline[u8, N]` at a byte offset. Bounds unless
    /// `at + lanes <= N` without overflow, checked before the access.
    #[allow(clippy::too_many_arguments)]
    fn inline_lanes(
        &mut self,
        array: TyId,
        vector: TyId,
        data: Option<ValueId>,
        at: ValueId,
        value: Option<ValueId>,
        write: bool,
        n: NodeId,
    ) -> Option<ValueId> {
        let len = match *self.t.kind(array) {
            TyKind::Array(item, len) if item == scalar(Scalar::U8) => len,
            _ => {
                self.refuse(
                    "SEM-11",
                    n,
                    "an Inline lane access takes an Inline array of u8",
                );
                return None;
            }
        };
        let Some(shape) = self.vector_shape(vector).filter(|v| v.lane() == Scalar::U8) else {
            self.refuse(
                "SEM-11",
                n,
                "an Inline lane access takes a vector of u8 lanes",
            );
            return None;
        };
        let u64_ty = Ty::Scalar(Scalar::U64);
        let bound = self.konst(u64_ty, len, n)?;
        let width = self.konst(u64_ty, shape.lanes(), n)?;
        let (end, overflow) = self.list_arith(ArithOp::Add, ArithMode::Checked, at, width, n);
        let no_overflow = self.emit_unary(UnaryOp::Not, BOOL, overflow, n)?;
        let within = self.list_compare(CompareOp::Le, end, bound, n);
        let valid = self.list_bool(BinaryOp::BitAnd, no_overflow, within, n);
        self.simd_bounds(valid, n);
        let address = self.address(
            AddressBase::Value(data?),
            &[Step::Byte(at)],
            PlaceTy::Value(Ty::Scalar(Scalar::U8)),
            n,
        );
        if write {
            self.inst(
                Inst::PutLanes {
                    to: address,
                    vector: value?,
                    count: width,
                },
                n,
            );
            return None;
        }
        let pointer = self.pointer(address, n);
        self.emit_simd(SimdOp::Lanes, vector, Some(pointer), None, n)
    }

    fn put_lanes(
        &mut self,
        header: Option<ValueId>,
        vector: Option<ValueId>,
        count: Option<ValueId>,
        shape: Vector,
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
        let len = self.carrier_word(header, carrier, SeqWord::Len, n);
        self.list_reserve(byte_ty, header, count, lists::Ask::Bounded, n)?;
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
        None
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
