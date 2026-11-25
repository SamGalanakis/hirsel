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
            let mut found = vec![None; g.generics.len()];
            let (param, result) = (g.params[0].ty, g.result);
            self.argument(Mode::Mut, param, call, 0, args[0], &mut found, &mut header);
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
            let Some(item) = self.lane_element(found.first().copied().flatten(), shape) else {
                self.refuse(
                    "SEM-11",
                    value_arg,
                    format!("{label} takes a vector whose lanes are the List's element type"),
                );
                return (ERROR, None);
            };
            self.builtin_use(f, n);
            self.source_step(n);
            let value = match op {
                SimdCall::PutLanes => self.put_lanes(item, header, value, other, shape, n),
                SimdCall::SetLanes => self.set_lanes(item, header, other, value, shape, n),
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
                self.call_seq += 1;
                let call = self.call_seq;
                let held = self.freezes.len();
                let input = g.params[0].ty;
                let mut found = vec![None; g.generics.len()];
                let mut header = None;
                if !self.argument(Mode::View, input, call, 0, a, &mut found, &mut header) {
                    let (ty, value) = self.expr(a, None);
                    let ty = self.bare(ty);
                    self.arg_ty_rooted(input, ty, a, &mut found, &[]);
                    header = value;
                }
                let (_, offset) = self.expr(args[1], expect(scalar(Scalar::U64)));
                self.unfreeze(held, |h| match h {
                    Hold::CallView(c, _) if c == call => None,
                    _ => Some(h),
                });
                let Some(item) = self.lane_element(found.first().copied().flatten(), shape) else {
                    self.refuse(
                        "SEM-11",
                        n,
                        "list.lanes reads a vector whose lanes are the List's element type",
                    );
                    return (ERROR, None);
                };
                self.builtin_use(f, n);
                self.source_step(n);
                let address = self.lanes_address(item, header, offset, shape, n);
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
            WrappingAdd | WrappingSub | WrappingMul | SaturatingAdd | SaturatingSub | Eq | Lt
            | MulhiI16 => {
                let hint = expected.map(|v| self.vector_type(v));
                let (ty, left) = self.expr(a, hint.and_then(expect));
                let Some(shape) = self.require_vector(ty, a) else {
                    return (ERROR, None);
                };
                if op == MulhiI16 && shape.lane() != Scalar::I16 {
                    self.refuse("SEM-11", a, "simd.mulhi_i16 takes vectors of i16 lanes");
                    return (ERROR, None);
                }
                let (_, right) = self.expr(args[1], expect(ty));
                (ty, left, right)
            }
            MaddI16 => {
                let hint = expected
                    .filter(|v| v.lane() == Scalar::I32)
                    .and_then(|v| Vector::new(Scalar::I16, v.lanes() * 2))
                    .map(|v| self.vector_type(v));
                let (ty, left) = self.expr(a, hint.and_then(expect));
                let Some(shape) = self.require_vector(ty, a) else {
                    return (ERROR, None);
                };
                if shape.lane() != Scalar::I16 {
                    self.refuse("SEM-11", a, "simd.madd_i16 takes vectors of i16 lanes");
                    return (ERROR, None);
                }
                let (_, right) = self.expr(args[1], expect(ty));
                let out = Vector::new(Scalar::I32, shape.lanes() / 2).unwrap();
                (self.vector_type(out), left, right)
            }
            PackSaturating => {
                let Some(out) = expected else {
                    return self.simd_expected(n);
                };
                let wide = match out.lane() {
                    Scalar::I8 | Scalar::U8 => Scalar::I16,
                    Scalar::I16 | Scalar::U16 => Scalar::I32,
                    _ => {
                        self.refuse(
                            "SEM-11",
                            n,
                            "simd.pack_saturating makes a vector of i8, u8, i16 or u16 lanes",
                        );
                        return (ERROR, None);
                    }
                };
                let input = Vector::new(wide, out.lanes() / 2).unwrap();
                let input = self.vector_type(input);
                let (_, left) = self.expr(a, expect(input));
                let (_, right) = self.expr(args[1], expect(input));
                (self.vector_type(out), left, right)
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

    /// The element type of a lane access whose array or List holds `item`:
    /// `item` itself when it is the vector's lane type (SEM-11), `None` otherwise.
    fn lane_element(&self, item: Option<TyId>, shape: Vector) -> Option<TyId> {
        let item = self.bare(item?);
        (*self.t.kind(item) == TyKind::Scalar(shape.lane())).then_some(item)
    }

    /// The byte offset of element `at` of a lane access over `shape`'s lanes.
    /// Its caller has checked `at + lanes <= len`, so the product does not wrap.
    fn lane_bytes(&mut self, at: ValueId, shape: Vector, n: NodeId) -> Option<ValueId> {
        let size = shape.lane().size();
        if size == 1 {
            return Some(at);
        }
        let size = self.konst(Ty::Scalar(Scalar::U64), size, n)?;
        Some(
            self.list_arith(ArithOp::Mul, ArithMode::Wrapping, at, size, n)
                .0,
        )
    }

    /// Bounds unless `at + lanes <= len` without overflow, checked before the access.
    fn lanes_within(&mut self, at: ValueId, len: ValueId, shape: Vector, n: NodeId) -> Option<()> {
        let width = self.konst(Ty::Scalar(Scalar::U64), shape.lanes(), n)?;
        let (end, overflow) = self.list_arith(ArithOp::Add, ArithMode::Checked, at, width, n);
        let no_overflow = self.emit_unary(UnaryOp::Not, BOOL, overflow, n)?;
        let within = self.list_compare(CompareOp::Le, end, len, n);
        let valid = self.list_bool(BinaryOp::BitAnd, no_overflow, within, n);
        self.simd_bounds(valid, n);
        Some(())
    }

    /// The address of element `at` of a List of `item`, after the bounds check.
    fn list_lanes_at(
        &mut self,
        item: TyId,
        header: ValueId,
        at: ValueId,
        shape: Vector,
        n: NodeId,
    ) -> Option<AddressId> {
        let carrier = self.list_carrier(item, n)?;
        let len = self.carrier_word(header, carrier, SeqWord::Len, n);
        self.lanes_within(at, len, shape, n)?;
        let data = self.carrier_word(header, carrier, SeqWord::Data, n);
        let offset = self.lane_bytes(at, shape, n)?;
        Some(self.address(
            AddressBase::Value(data),
            &[Step::Byte(offset)],
            PlaceTy::Value(Ty::Scalar(Scalar::U8)),
            n,
        ))
    }

    fn lanes_address(
        &mut self,
        item: TyId,
        header: Option<ValueId>,
        at: Option<ValueId>,
        shape: Vector,
        n: NodeId,
    ) -> Option<ValueId> {
        let from = self.list_lanes_at(item, header?, at?, shape, n)?;
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
                "an Inline lane access takes an Inline array of integer lanes",
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

    /// `inline.lanes` and `inline.set_lanes`: one vector read from or written to an
    /// `Inline[T, N]` of its lane type at element `at`. Bounds unless
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
        let shape = self.vector_shape(vector)?;
        let len = match *self.t.kind(array) {
            TyKind::Array(item, len) if self.lane_element(Some(item), shape).is_some() => len,
            _ => {
                self.refuse(
                    "SEM-11",
                    n,
                    "an Inline lane access takes an Inline array of the vector's lane type",
                );
                return None;
            }
        };
        let u64_ty = Ty::Scalar(Scalar::U64);
        let bound = self.konst(u64_ty, len, n)?;
        self.lanes_within(at, bound, shape, n)?;
        let offset = self.lane_bytes(at, shape, n)?;
        let address = self.address(
            AddressBase::Value(data?),
            &[Step::Byte(offset)],
            PlaceTy::Value(Ty::Scalar(Scalar::U8)),
            n,
        );
        if write {
            let count = self.konst(u64_ty, shape.size(), n)?;
            self.inst(
                Inst::PutLanes {
                    to: address,
                    vector: value?,
                    count,
                },
                n,
            );
            return None;
        }
        let pointer = self.pointer(address, n);
        self.emit_simd(SimdOp::Lanes, vector, Some(pointer), None, n)
    }

    /// `list.put_lanes(mut xs, v, k)`: appends lanes 0 to `k - 1` of `v` to a List of
    /// its lane type. Bounds when `k > lanes`, before the growth.
    fn put_lanes(
        &mut self,
        item: TyId,
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

        let carrier = self.list_carrier(item, n)?;
        let len = self.carrier_word(header, carrier, SeqWord::Len, n);
        self.list_reserve(item, header, count, lists::Ask::Bounded, n)?;
        let data = self.carrier_word(header, carrier, SeqWord::Data, n);
        let offset = self.lane_bytes(len, shape, n)?;
        let to = self.address(
            AddressBase::Value(data),
            &[Step::Byte(offset)],
            PlaceTy::Value(Ty::Scalar(Scalar::U8)),
            n,
        );
        let bytes = self.lane_bytes(count, shape, n)?;
        self.inst(
            Inst::PutLanes {
                to,
                vector,
                count: bytes,
            },
            n,
        );
        let (len, _) = self.list_arith(ArithOp::Add, ArithMode::Wrapping, len, count, n);
        self.set_carrier_word(header, carrier, SeqWord::Len, len, n);
        None
    }

    /// `list.set_lanes(mut xs, at, v)`: overwrites elements `at` to `at + lanes - 1`
    /// with the lanes of `v`, keeping the length.
    fn set_lanes(
        &mut self,
        item: TyId,
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
        let to = self.list_lanes_at(item, header, at, shape, n)?;
        let count = self.konst(Ty::Scalar(Scalar::U64), shape.size(), n)?;
        self.inst(Inst::PutLanes { to, vector, count }, n);
        None
    }
}
