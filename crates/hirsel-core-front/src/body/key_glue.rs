//! Mechanical `key` glue. Generic calls retain a type recipe until the ordinary
//! instance materializer substitutes it; no source body is checked again.
use super::*;
use hirsel_core_layout::{Body as ShapeBody, FieldTy};

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum KeyOp {
    Hash,
    Fold,
    Equal,
    Order,
    Ordered(CompareOp),
}
impl KeyOp {
    fn definition(self) -> DefPath {
        DefPath::core(match self {
            Self::Hash => "map.$key_hash",
            Self::Fold => "map.$key_fold",
            Self::Equal => "map.$key_equal",
            Self::Order => "order.$compare",
            Self::Ordered(CompareOp::Lt) => "order.$lt",
            Self::Ordered(CompareOp::Le) => "order.$le",
            Self::Ordered(CompareOp::Gt) => "order.$gt",
            Self::Ordered(CompareOp::Ge) => "order.$ge",
            Self::Ordered(_) => unreachable!("ordered predicates"),
        })
    }
    fn signature(self, ty: TypeId) -> Signature {
        let mut params = vec![Param::Address(ty)];
        match self {
            Self::Hash => {}
            Self::Fold => params.push(Param::Value(Ty::Scalar(Scalar::U64))),
            Self::Equal | Self::Order | Self::Ordered(_) => params.push(Param::Address(ty)),
        }
        Signature {
            params,
            results: vec![Ty::Scalar(match self {
                Self::Equal | Self::Ordered(_) => Scalar::Bool,
                Self::Order => Scalar::I8,
                _ => Scalar::U64,
            })],
            dest: None,
            never: false,
        }
    }
}

impl<'c, 'a> Cx<'c, 'a, '_> {
    pub(super) fn key_operation(
        &mut self,
        op: KeyOp,
        ty: TyId,
        values: &[Option<ValueId>],
        n: NodeId,
    ) -> Option<ValueId> {
        let (id, function, rep) = if let Some(&abi) = self.key_functions.get(&(ty, op)) {
            abi
        } else {
            #[cfg(test)]
            {
                self.key_metadata_builds += 1;
            }
            let rep = match self.rep(ty) {
                Ok(rep) => rep,
                Err(what) => {
                    self.gap(n, what);
                    return None;
                }
            };
            let Some(id) = self.e.type_row(self.p, &mut self.t, ty) else {
                self.gap(n, "key payload layout is not built");
                return None;
            };
            let function = if has_param(&self.t, ty) {
                let name = crate::decls::canonical(&self.p.decls, &self.t, ty);
                let function = self.e.add_function(
                    InstanceKey {
                        definition: op.definition(),
                        types: vec![name],
                        dictionaries: vec![],
                    },
                    op.signature(id),
                );
                self.e
                    .generic_calls
                    .entry(function)
                    .or_insert_with(|| GenericCall {
                        arguments: vec![NameRecipe::of(self.p, &self.t, ty)],
                        dictionaries: vec![],
                        operation: Some(op),
                        numeric: None,
                    });
                function
            } else {
                match ensure(&mut self.e.module, id, op) {
                    Ok(function) => function,
                    Err(what) => {
                        self.gap(n, what);
                        return None;
                    }
                }
            };
            self.key_functions.insert((ty, op), (id, function, rep));
            (id, function, rep)
        };
        if !self.live() {
            return None;
        }
        self.key_args.clear();
        for &value in values {
            let pointer = if matches!(rep, Rep::Place(_)) {
                value?
            } else {
                let at = self.slot(ty, value, n)?;
                self.pointer(at, n)
            };
            let at = self.address(AddressBase::Value(pointer), &[], PlaceTy::Declared(id), n);
            self.key_args.push(Arg::Address(at));
        }
        let signature = self.e.module.functions[function.index()].signature;
        let result = self.value(self.e.module.signatures[signature.index()].results[0]);
        let args = PoolRange::push(&mut self.b.pools.args, self.key_args.iter().copied());
        let results = PoolRange::push(&mut self.b.pools.values, [result]);
        let call = CallId(self.b.calls.len() as u32);
        self.b.calls.push(CallSite {
            callee: Callee::Direct(function),
            signature,
            args,
            results,
            fail: None,
        });
        self.inst(Inst::Call { call }, n);
        Some(result)
    }
}

pub(super) fn ensure(module: &mut Module, ty: TypeId, op: KeyOp) -> Result<FuncId, &'static str> {
    let recipes =
        hirsel_core_layout::compute(&module.types).map_err(|_| "key recipe is not built")?;
    Derive {
        module,
        recipes,
        active: vec![],
    }
    .function(ty, op)
}

fn scalar_row(module: &Module, ty: TypeId) -> Option<Scalar> {
    // The row's published carriage (FORM #64), never its field structure: a one-field
    // record or tuple of a scalar is carried by address.
    match module.types[ty.index()].carried {
        hirsel_core_layout::Carried::Value(Ty::Scalar(s))
            if !matches!(s, Scalar::F32 | Scalar::F64) =>
        {
            Some(s)
        }
        _ => None,
    }
}

/// A scalar predicate needs no generated call after substitution. Preserve its
/// two typed loads and replace the call by the same scalar Compare as closed code.
pub(super) fn inline_ordered(module: &Module, body: &mut Body) {
    let old = std::mem::take(&mut body.insts);
    let anchors = std::mem::take(&mut body.inst_anchors);
    body.insts = Vec::with_capacity(old.len());
    body.inst_anchors = Vec::with_capacity(anchors.len());
    for block in 0..body.blocks.len() {
        let range = body.blocks[block].insts;
        let start = body.insts.len() as u32;
        for index in range.start as usize..(range.start + range.len) as usize {
            let inst = &old[index];
            let scalar_call = (|| {
                let Inst::Call { call } = inst else {
                    return None;
                };
                let call = &body.calls[call.index()];
                let Callee::Direct(f) = call.callee else {
                    return None;
                };
                let key = &module.instances[module.functions[f.index()].instance.index()];
                if key.definition.0.package.name != "core" {
                    return None;
                }
                let op = if key.definition.0.modules == ["map"]
                    && key.definition.0.name == "$key_equal"
                {
                    CompareOp::Eq
                } else if key.definition.0.modules == ["order"] {
                    match key.definition.0.name.as_str() {
                        "$lt" => CompareOp::Lt,
                        "$le" => CompareOp::Le,
                        "$gt" => CompareOp::Gt,
                        "$ge" => CompareOp::Ge,
                        _ => return None,
                    }
                } else {
                    return None;
                };
                let Param::Address(ty) = module.signatures[call.signature.index()].params[0] else {
                    return None;
                };
                let scalar = scalar_row(module, ty)?;
                let args = call.args.get(&body.pools.args).unwrap();
                let [Arg::Address(a), Arg::Address(b)] = args else {
                    return None;
                };
                Some((
                    op,
                    scalar,
                    [*a, *b],
                    call.results.get(&body.pools.values).unwrap()[0],
                ))
            })();
            if let Some((op, scalar, args, result)) = scalar_call {
                let mut values = [ValueId(0); 2];
                for (i, arg) in args.into_iter().enumerate() {
                    let steps = PoolRange::push(&mut body.pools.steps, [Step::Field(0)]);
                    let address = AddressId(body.addresses.len() as u32);
                    body.addresses.push(AddressInfo {
                        base: AddressBase::Address(arg),
                        steps,
                        ty: PlaceTy::Value(Ty::Scalar(scalar)),
                    });
                    let value = ValueId(body.values.len() as u32);
                    body.values.push(Ty::Scalar(scalar));
                    body.insts.extend([
                        Inst::Address {
                            address,
                            fail: None,
                        },
                        Inst::Read {
                            result: value,
                            address,
                            mode: ReadMode::Bits,
                        },
                    ]);
                    body.inst_anchors.extend([anchors[index]; 2]);
                    values[i] = value;
                }
                body.insts.push(Inst::Compare {
                    result,
                    op,
                    left: values[0],
                    right: values[1],
                });
                body.inst_anchors.push(anchors[index]);
            } else {
                body.insts.push(inst.clone());
                body.inst_anchors.push(anchors[index]);
            }
        }
        body.blocks[block].insts = PoolRange::new(start, body.insts.len() as u32 - start);
    }
}

struct Derive<'a> {
    module: &'a mut Module,
    recipes: Vec<Recipe>,
    active: Vec<(TypeId, KeyOp)>,
}
impl Derive<'_> {
    fn function(&mut self, ty: TypeId, op: KeyOp) -> Result<FuncId, &'static str> {
        let key = InstanceKey {
            definition: op.definition(),
            types: vec![self.module.types[ty.index()].name.clone()],
            dictionaries: vec![],
        };
        let row = if let Some(i) = self
            .module
            .functions
            .iter()
            .position(|f| self.module.instances[f.instance.index()] == key)
        {
            let row = FuncId(i as u32);
            if self.module.functions[i].body.is_some() || self.active.contains(&(ty, op)) {
                return Ok(row);
            }
            row
        } else {
            let sig = op.signature(ty);
            let signature = if let Some(i) = self.module.signatures.iter().position(|s| *s == sig) {
                SigId(i as u32)
            } else {
                let id = SigId(self.module.signatures.len() as u32);
                self.module.signatures.push(sig);
                id
            };
            let instance = InstanceId(self.module.instances.len() as u32);
            self.module.instances.push(key);
            let row = FuncId(self.module.functions.len() as u32);
            self.module.functions.push(Function {
                instance,
                signature,
                body: None,
            });
            row
        };
        self.active.push((ty, op));
        let mut c = Code::new(if op == KeyOp::Hash {
            &[Ty::Ptr]
        } else if op == KeyOp::Fold {
            &[Ty::Ptr, Ty::Scalar(Scalar::U64)]
        } else {
            &[Ty::Ptr, Ty::Ptr]
        });
        let ps = c.params(BlockId(0));
        let result = match op {
            KeyOp::Hash => {
                let zero = c.constant(0);
                let fold = self.function(ty, KeyOp::Fold)?;
                let at = c.address(ps[0], PlaceTy::Declared(ty));
                let h = self.call(&mut c, fold, &[Arg::Address(at), Arg::Value(zero)]);
                let shift = c.constant(32);
                let top = c.binary(BinaryOp::Shr, h, shift);
                c.binary(BinaryOp::BitXor, h, top)
            }
            KeyOp::Fold => self.fold_type(&mut c, ty, ps[0], ps[1])?,
            KeyOp::Equal => {
                let no = c.block(&[]);
                self.equal_type(&mut c, ty, ps[0], ps[1], no)?;
                let yes = c.boolean(true);
                let returns = PoolRange::push(&mut c.body.pools.values, [yes]);
                c.end(Terminator::Return(returns));
                c.start(no);
                c.boolean(false)
            }
            KeyOp::Ordered(op) if scalar_row(self.module, ty).is_some() => {
                let scalar = scalar_row(self.module, ty).unwrap();
                let a = c.read(ps[0], Ty::Scalar(scalar));
                let b = c.read(ps[1], Ty::Scalar(scalar));
                c.compare(op, a, b)
            }
            KeyOp::Order | KeyOp::Ordered(_) => {
                let different = c.block(&[Ty::Scalar(Scalar::I8)]);
                self.order_type(&mut c, ty, ps[0], ps[1], different)?;
                let zero = c.typed_constant(Scalar::I8, 0);
                c.jump(different, &[zero]);
                c.start(different);
                let order = c.params(different)[0];
                if let KeyOp::Ordered(op) = op {
                    let zero = c.typed_constant(Scalar::I8, 0);
                    c.compare(op, order, zero)
                } else {
                    order
                }
            }
        };
        let returns = PoolRange::push(&mut c.body.pools.values, [result]);
        c.end(Terminator::Return(returns));
        order_instructions(&mut c.body);
        self.module.functions[row.index()].body = Some(c.body);
        self.active.pop();
        Ok(row)
    }
    fn call(&self, c: &mut Code, f: FuncId, args: &[Arg]) -> ValueId {
        let signature = self.module.functions[f.index()].signature;
        let result = c.value(self.module.signatures[signature.index()].results[0]);
        let args = PoolRange::push(&mut c.body.pools.args, args.iter().copied());
        let results = PoolRange::push(&mut c.body.pools.values, [result]);
        let call = CallId(c.body.calls.len() as u32);
        c.body.calls.push(CallSite {
            callee: Callee::Direct(f),
            signature,
            args,
            results,
            fail: None,
        });
        c.inst(Inst::Call { call });
        result
    }
    fn runtime(&mut self, c: &mut Code, op: hirsel_core_ir::RtOp, args: &[ValueId]) -> ValueId {
        let sig = op.signature();
        let signature = if let Some(i) = self.module.signatures.iter().position(|s| *s == sig) {
            SigId(i as u32)
        } else {
            let id = SigId(self.module.signatures.len() as u32);
            self.module.signatures.push(sig);
            id
        };
        let result = c.value(self.module.signatures[signature.index()].results[0]);
        let args = PoolRange::push(&mut c.body.pools.args, args.iter().copied().map(Arg::Value));
        let results = PoolRange::push(&mut c.body.pools.values, [result]);
        let call = CallId(c.body.calls.len() as u32);
        c.body.calls.push(CallSite {
            callee: Callee::Runtime(op),
            signature,
            args,
            results,
            fail: None,
        });
        c.inst(Inst::Call { call });
        result
    }
    fn order_type(
        &mut self,
        c: &mut Code,
        ty: TypeId,
        a: ValueId,
        b: ValueId,
        different: BlockId,
    ) -> Result<(), &'static str> {
        let Layout::Fixed(layout) = self.recipes[ty.index()].layout.clone() else {
            return self.order_located(c, &FieldTy::Inline(TypeRef(ty.0)), a, b, different);
        };
        match self.module.types[ty.index()].body.clone() {
            ShapeBody::Record(fs) => {
                for (i, f) in fs.iter().enumerate() {
                    let x = c.offset(a, layout.offsets[0][i]);
                    let y = c.offset(b, layout.offsets[0][i]);
                    self.order_field(c, &f.ty, x, y, different)?;
                }
            }
            ShapeBody::Variant(cs) => {
                let mut tags = [ValueId(0); 2];
                for (i, p) in [a, b].into_iter().enumerate() {
                    let at = c.address(p, PlaceTy::Declared(ty));
                    let tag = c.value(Ty::Scalar(Scalar::U32));
                    c.inst(Inst::Case {
                        result: tag,
                        address: at,
                    });
                    tags[i] = tag;
                }
                self.order_scalars(c, tags[0], tags[1], different);
                let done = c.block(&[]);
                let mut arms = vec![];
                for i in 0..cs.len() {
                    let block = c.block(&[]);
                    arms.push(hirsel_core_ir::SwitchArm {
                        value: i as u64,
                        edge: c.edge(block, &[]),
                    });
                }
                let otherwise = arms.last().unwrap().edge;
                let range = PoolRange::push(
                    &mut c.body.pools.arms,
                    arms[..arms.len() - 1].iter().copied(),
                );
                c.end(Terminator::Switch {
                    value: tags[0],
                    arms: range,
                    otherwise,
                });
                for (i, case) in cs.iter().enumerate() {
                    c.start(c.body.edges[arms[i].edge.index()].target);
                    for (j, f) in case.fields.iter().enumerate() {
                        let x = c.offset(a, layout.offsets[i][j]);
                        let y = c.offset(b, layout.offsets[i][j]);
                        self.order_field(c, &f.ty, x, y, different)?;
                    }
                    c.jump(done, &[]);
                }
                c.start(done);
            }
        }
        Ok(())
    }
    fn order_located(
        &mut self,
        c: &mut Code,
        target: &FieldTy,
        a: ValueId,
        b: ValueId,
        different: BlockId,
    ) -> Result<(), &'static str> {
        let mut blobs = [ValueId(0); 2];
        let mut ats = [ValueId(0); 2];
        let mut widths = [ValueId(0); 2];
        for (i, p) in [a, b].into_iter().enumerate() {
            blobs[i] = c.read(p, Ty::Ptr);
            let at = c.offset(p, 8);
            ats[i] = c.read(at, Ty::Scalar(Scalar::U64));
            let at = c.offset(blobs[i], 8);
            widths[i] = c.read(at, Ty::Scalar(Scalar::U64));
        }
        self.order_packed(c, target, blobs, ats, widths, different)?;
        Ok(())
    }
    /// All addresses into a blob retain a byte step, so scalar reads are unaligned.
    fn packed_word(c: &mut Code, blob: ValueId, at: ValueId, scalar: Scalar) -> ValueId {
        let steps = PoolRange::push(&mut c.body.pools.steps, [Step::Byte(at)]);
        let address = AddressId(c.body.addresses.len() as u32);
        let ty = Ty::Scalar(scalar);
        c.body.addresses.push(AddressInfo {
            base: AddressBase::Value(blob),
            steps,
            ty: PlaceTy::Value(ty),
        });
        c.inst(Inst::Address {
            address,
            fail: None,
        });
        let result = c.value(ty);
        c.inst(Inst::Read {
            result,
            address,
            mode: ReadMode::Bits,
        });
        result
    }
    fn packed_count(c: &mut Code, blob: ValueId, at: ValueId, width: ValueId) -> ValueId {
        let done = c.block(&[Ty::Scalar(Scalar::U64)]);
        for (bytes, scalar) in [(2, Scalar::U16), (4, Scalar::U32), (8, Scalar::U64)] {
            if bytes != 8 {
                let k = c.constant(bytes);
                let on = c.compare(CompareOp::Eq, width, k);
                let load = c.block(&[]);
                let next = c.block(&[]);
                c.branch(on, load, next);
                c.start(load);
                let v = Self::packed_word(c, blob, at, scalar);
                let v = c.widen(v);
                c.jump(done, &[v]);
                c.start(next);
            } else {
                let v = Self::packed_word(c, blob, at, scalar);
                c.jump(done, &[v]);
            }
        }
        c.start(done);
        c.params(done)[0]
    }
    /// Return both cursors after the equal value. A difference leaves immediately.
    fn order_packed(
        &mut self,
        c: &mut Code,
        field: &FieldTy,
        blobs: [ValueId; 2],
        mut ats: [ValueId; 2],
        widths: [ValueId; 2],
        different: BlockId,
    ) -> Result<[ValueId; 2], &'static str> {
        match field {
            FieldTy::Scalar(s) if !matches!(s, Scalar::F32 | Scalar::F64) => {
                let a = Self::packed_word(c, blobs[0], ats[0], *s);
                let b = Self::packed_word(c, blobs[1], ats[1], *s);
                self.order_scalars(c, a, b, different);
                let size = c.constant(s.size());
                for at in &mut ats {
                    *at = c.binary(BinaryOp::Add, *at, size);
                }
            }
            FieldTy::Text => {
                let mut lengths = [ValueId(0); 2];
                let mut ps = [ValueId(0); 2];
                for i in 0..2 {
                    lengths[i] = Self::packed_count(c, blobs[i], ats[i], widths[i]);
                    ats[i] = c.binary(BinaryOp::Add, ats[i], widths[i]);
                    ps[i] = c.offset_value(blobs[i], ats[i]);
                    ats[i] = c.binary(BinaryOp::Add, ats[i], lengths[i]);
                }
                let result = self.runtime(
                    c,
                    hirsel_core_ir::RtOp::CompareBytes,
                    &[ps[0], lengths[0], ps[1], lengths[1]],
                );
                self.order_result(c, result, different);
            }
            FieldTy::Array(item, count) => {
                let len = c.constant(*count);
                ats =
                    self.order_packed_sequence(c, item, blobs, ats, widths, [len, len], different)?;
            }
            FieldTy::FixedSpan(item) | FieldTy::SeqSpan(item) | FieldTy::IndexedSpan(item) => {
                let mut lengths = [ValueId(0); 2];
                for i in 0..2 {
                    lengths[i] = Self::packed_count(c, blobs[i], ats[i], widths[i]);
                    ats[i] = c.binary(BinaryOp::Add, ats[i], widths[i]);
                    if !matches!(field, FieldTy::FixedSpan(_)) {
                        ats[i] = c.binary(BinaryOp::Add, ats[i], widths[i]);
                    }
                }
                ats =
                    self.order_packed_sequence(c, item, blobs, ats, widths, lengths, different)?;
                if matches!(field, FieldTy::IndexedSpan(_)) {
                    for i in 0..2 {
                        let trailer = c.mul(lengths[i], widths[i]);
                        ats[i] = c.binary(BinaryOp::Add, ats[i], trailer);
                    }
                }
            }
            FieldTy::Inline(ty) => {
                let compact = matches!(self.recipes[ty.index()].layout, Layout::Compact(_));
                match self.module.types[ty.index()].body.clone() {
                    ShapeBody::Record(fields) => {
                        for f in &fields {
                            ats = self.order_packed(c, &f.ty, blobs, ats, widths, different)?;
                        }
                    }
                    ShapeBody::Variant(cases) => {
                        // Compact tags use the recipe's ordinal width. The private plain
                        // encoding has a u64 ordinal and pads to its logical stride (#58).
                        let tag_width = if compact {
                            hirsel_core_layout::tag_width(cases.len())
                        } else {
                            8
                        };
                        let scalar = match tag_width {
                            1 => Scalar::U8,
                            2 => Scalar::U16,
                            4 => Scalar::U32,
                            _ => Scalar::U64,
                        };
                        let a = Self::packed_word(c, blobs[0], ats[0], scalar);
                        let b = Self::packed_word(c, blobs[1], ats[1], scalar);
                        self.order_scalars(c, a, b, different);
                        let tag_size = c.constant(tag_width);
                        let starts = ats.map(|at| c.binary(BinaryOp::Add, at, tag_size));
                        let done = c.block(&[Ty::Scalar(Scalar::U64); 2]);
                        let mut arms = vec![];
                        for i in 0..cases.len() {
                            let block = c.block(&[]);
                            arms.push(hirsel_core_ir::SwitchArm {
                                value: i as u64,
                                edge: c.edge(block, &[]),
                            });
                        }
                        let otherwise = arms.last().unwrap().edge;
                        let range = PoolRange::push(
                            &mut c.body.pools.arms,
                            arms[..arms.len() - 1].iter().copied(),
                        );
                        c.end(Terminator::Switch {
                            value: a,
                            arms: range,
                            otherwise,
                        });
                        for (i, case) in cases.iter().enumerate() {
                            c.start(c.body.edges[arms[i].edge.index()].target);
                            let mut ends = starts;
                            for f in &case.fields {
                                ends =
                                    self.order_packed(c, &f.ty, blobs, ends, widths, different)?;
                            }
                            if !compact {
                                let stride = c.constant(
                                    self.recipes[ty.index()]
                                        .logical_size
                                        .ok_or("plain variant logical size is not built")?,
                                );
                                ends = ats.map(|at| c.binary(BinaryOp::Add, at, stride));
                            }
                            c.jump(done, &ends);
                        }
                        c.start(done);
                        ats = c.params(done);
                    }
                }
            }
            _ => return Err("ordering this packed recipe field is not built"),
        }
        Ok(ats)
    }
    fn order_packed_sequence(
        &mut self,
        c: &mut Code,
        item: &FieldTy,
        blobs: [ValueId; 2],
        ats: [ValueId; 2],
        widths: [ValueId; 2],
        lengths: [ValueId; 2],
        different: BlockId,
    ) -> Result<[ValueId; 2], &'static str> {
        let head = c.block(&[Ty::Scalar(Scalar::U64); 3]);
        let check = c.block(&[]);
        let body = c.block(&[]);
        let done = c.block(&[]);
        let zero = c.constant(0);
        c.jump(head, &[zero, ats[0], ats[1]]);
        c.start(head);
        let params = c.body.blocks[head.index()]
            .params
            .get(&c.body.pools.values)
            .unwrap();
        let (i, cursors) = (params[0], [params[1], params[2]]);
        let on = c.compare(CompareOp::Lt, i, lengths[0]);
        c.branch(on, check, done);
        c.start(check);
        let on = c.compare(CompareOp::Lt, i, lengths[1]);
        c.branch(on, body, done);
        c.start(body);
        let next = self.order_packed(c, item, blobs, cursors, widths, different)?;
        let one = c.constant(1);
        let i = c.binary(BinaryOp::Add, i, one);
        c.jump(head, &[i, next[0], next[1]]);
        c.start(done);
        self.order_scalars(c, lengths[0], lengths[1], different);
        Ok(cursors)
    }
    fn order_scalars(&mut self, c: &mut Code, a: ValueId, b: ValueId, different: BlockId) {
        for (op, bits) in [(CompareOp::Lt, 255), (CompareOp::Gt, 1)] {
            let value = c.typed_constant(Scalar::I8, bits);
            let on = c.compare(op, a, b);
            let same = c.block(&[]);
            let yes = c.edge(different, &[value]);
            let no = c.edge(same, &[]);
            c.end(Terminator::Branch {
                condition: on,
                yes,
                no,
            });
            c.start(same);
        }
    }
    fn order_result(&mut self, c: &mut Code, result: ValueId, different: BlockId) {
        let zero = c.typed_constant(Scalar::I8, 0);
        let on = c.compare(CompareOp::Ne, result, zero);
        let same = c.block(&[]);
        let yes = c.edge(different, &[result]);
        let no = c.edge(same, &[]);
        c.end(Terminator::Branch {
            condition: on,
            yes,
            no,
        });
        c.start(same);
    }
    fn order_field(
        &mut self,
        c: &mut Code,
        field: &FieldTy,
        a: ValueId,
        b: ValueId,
        different: BlockId,
    ) -> Result<(), &'static str> {
        match field {
            FieldTy::Scalar(s) if !matches!(s, Scalar::F32 | Scalar::F64) => {
                let x = c.read(a, Ty::Scalar(*s));
                let y = c.read(b, Ty::Scalar(*s));
                self.order_scalars(c, x, y, different);
            }
            FieldTy::Inline(t) => {
                let f = self.function(TypeId(t.0), KeyOp::Order)?;
                let x = c.address(a, PlaceTy::Declared(TypeId(t.0)));
                let y = c.address(b, PlaceTy::Declared(TypeId(t.0)));
                let order = self.call(c, f, &[Arg::Address(x), Arg::Address(y)]);
                self.order_result(c, order, different);
            }
            FieldTy::View(inner) => self.order_field(c, inner, a, b, different)?,
            FieldTy::Locator(target) => self.order_located(c, target, a, b, different)?,
            FieldTy::Stable(t) => {
                let x = c.read(a, Ty::Ptr);
                let y = c.read(b, Ty::Ptr);
                self.order_type(c, TypeId(t.0), x, y, different)?;
            }
            FieldTy::Seq(item) if **item == FieldTy::Scalar(Scalar::U8) => {
                let mut args = [ValueId(0); 4];
                for (i, p) in [a, b].into_iter().enumerate() {
                    args[2 * i] = c.read(p, Ty::Ptr);
                    let at = c.offset(p, 8);
                    args[2 * i + 1] = c.read(at, Ty::Scalar(Scalar::U64));
                }
                let order = self.runtime(c, hirsel_core_ir::RtOp::CompareBytes, &args);
                self.order_result(c, order, different);
            }
            FieldTy::Seq(item) => {
                let mut ps = [ValueId(0); 2];
                let mut ns = [ValueId(0); 2];
                for (i, p) in [a, b].into_iter().enumerate() {
                    ps[i] = c.read(p, Ty::Ptr);
                    let at = c.offset(p, 8);
                    ns[i] = c.read(at, Ty::Scalar(Scalar::U64));
                }
                self.order_sequence(c, item, ps, ns, different)?;
            }
            FieldTy::Array(item, len) => {
                let n = c.constant(*len);
                self.order_sequence(c, item, [a, b], [n, n], different)?;
            }
            FieldTy::Box(t) | FieldTy::Rc(t) | FieldTy::Arc(t) => {
                let mut x = c.read(a, Ty::Ptr);
                let mut y = c.read(b, Ty::Ptr);
                if !matches!(field, FieldTy::Box(_)) {
                    x = c.offset(x, 8);
                    y = c.offset(y, 8);
                }
                let f = self.function(TypeId(t.0), KeyOp::Order)?;
                let x = c.address(x, PlaceTy::Declared(TypeId(t.0)));
                let y = c.address(y, PlaceTy::Declared(TypeId(t.0)));
                let order = self.call(c, f, &[Arg::Address(x), Arg::Address(y)]);
                self.order_result(c, order, different);
            }
            _ => return Err("ordering this recipe field is not built"),
        }
        Ok(())
    }
    fn order_sequence(
        &mut self,
        c: &mut Code,
        item: &FieldTy,
        data: [ValueId; 2],
        len: [ValueId; 2],
        different: BlockId,
    ) -> Result<(), &'static str> {
        let stride = hirsel_core_layout::field_size(&self.module.types, &self.recipes, item).0;
        let zero = c.constant(0);
        let head = c.block(&[Ty::Scalar(Scalar::U64)]);
        let check = c.block(&[]);
        let body = c.block(&[]);
        let done = c.block(&[]);
        c.jump(head, &[zero]);
        c.start(head);
        let i = c.params(head)[0];
        let more = c.compare(CompareOp::Lt, i, len[0]);
        c.branch(more, check, done);
        c.start(check);
        let more = c.compare(CompareOp::Lt, i, len[1]);
        c.branch(more, body, done);
        c.start(body);
        let stride = c.constant(stride);
        let offset = c.mul(i, stride);
        let a = c.offset_value(data[0], offset);
        let b = c.offset_value(data[1], offset);
        self.order_field(c, item, a, b, different)?;
        let one = c.constant(1);
        let next = c.binary(BinaryOp::Add, i, one);
        c.jump(head, &[next]);
        c.start(done);
        self.order_scalars(c, len[0], len[1], different);
        Ok(())
    }
    fn fold_type(
        &mut self,
        c: &mut Code,
        ty: TypeId,
        p: ValueId,
        mut h: ValueId,
    ) -> Result<ValueId, &'static str> {
        let Layout::Fixed(layout) = self.recipes[ty.index()].layout.clone() else {
            return Err("compact key recipe glue is not built");
        };
        match self.module.types[ty.index()].body.clone() {
            ShapeBody::Record(fs) => {
                for (i, f) in fs.into_iter().enumerate() {
                    let at = c.offset(p, layout.offsets[0][i]);
                    h = self.fold_field(c, &f.ty, at, h)?;
                }
            }
            ShapeBody::Variant(cs) => {
                let at = c.address(p, PlaceTy::Declared(ty));
                let tag = c.value(Ty::Scalar(Scalar::U32));
                c.inst(Inst::Case {
                    result: tag,
                    address: at,
                });
                let word = c.widen(tag);
                h = c.mix(h, word);
                let done = c.block(&[Ty::Scalar(Scalar::U64)]);
                let mut arms = vec![];
                for (i, _) in cs.iter().enumerate() {
                    let b = c.block(&[]);
                    arms.push(hirsel_core_ir::SwitchArm {
                        value: i as u64,
                        edge: c.edge(b, &[]),
                    });
                }
                let otherwise = arms.last().unwrap().edge;
                let arms_range = PoolRange::push(
                    &mut c.body.pools.arms,
                    arms[..arms.len() - 1].iter().copied(),
                );
                c.end(Terminator::Switch {
                    value: tag,
                    arms: arms_range,
                    otherwise,
                });
                for (i, case) in cs.into_iter().enumerate() {
                    c.start(c.body.edges[arms[i].edge.index()].target);
                    let mut current = h;
                    for (j, f) in case.fields.into_iter().enumerate() {
                        let at = c.offset(p, layout.offsets[i][j]);
                        current = self.fold_field(c, &f.ty, at, current)?;
                    }
                    c.jump(done, &[current]);
                }
                c.start(done);
                h = c.params(done)[0];
            }
        }
        Ok(h)
    }
    fn fold_field(
        &mut self,
        c: &mut Code,
        field: &FieldTy,
        p: ValueId,
        h: ValueId,
    ) -> Result<ValueId, &'static str> {
        Ok(match field {
            FieldTy::Scalar(s) if !matches!(s, Scalar::F32 | Scalar::F64) => {
                let word = c.word(p, s.size());
                c.mix(h, word)
            }
            FieldTy::Inline(t) => {
                let f = self.function(TypeId(t.0), KeyOp::Fold)?;
                let at = c.address(p, PlaceTy::Declared(TypeId(t.0)));
                self.call(c, f, &[Arg::Address(at), Arg::Value(h)])
            }
            FieldTy::Seq(item) if **item == FieldTy::Scalar(Scalar::U8) => {
                let data = c.read(p, Ty::Ptr);
                let at = c.offset(p, 8);
                let len = c.read(at, Ty::Scalar(Scalar::U64));
                let word = self.runtime(c, hirsel_core_ir::RtOp::HashBytes, &[data, len]);
                c.mix(h, word)
            }
            FieldTy::Seq(item) => {
                let data = c.read(p, Ty::Ptr);
                let at = c.offset(p, 8);
                let len = c.read(at, Ty::Scalar(Scalar::U64));
                self.fold_sequence(c, item, data, len, h)?
            }
            FieldTy::Array(item, len) => {
                let len = c.constant(*len);
                self.fold_sequence(c, item, p, len, h)?
            }
            FieldTy::Box(t) | FieldTy::Rc(t) | FieldTy::Arc(t) => {
                let mut data = c.read(p, Ty::Ptr);
                if !matches!(field, FieldTy::Box(_)) {
                    data = c.offset(data, 8);
                }
                let f = self.function(TypeId(t.0), KeyOp::Fold)?;
                let at = c.address(data, PlaceTy::Declared(TypeId(t.0)));
                self.call(c, f, &[Arg::Address(at), Arg::Value(h)])
            }
            // A located value hashes its packed encoding's values, as order and
            // equality read them (FORM #63, #65), never the locator.
            FieldTy::Locator(target) => {
                let blob = c.read(p, Ty::Ptr);
                let at = c.offset(p, 8);
                let at = c.read(at, Ty::Scalar(Scalar::U64));
                let w = c.offset(blob, 8);
                let width = c.read(w, Ty::Scalar(Scalar::U64));
                self.fold_packed(c, target, blob, at, width, h)?.0
            }
            _ => return Err("hashing this key recipe is not built"),
        })
    }
    /// Fold the packed value at byte `at` of `blob` into `h`; returns the hash and the
    /// cursor after the value. Mirrors [`Self::order_packed`].
    fn fold_packed(
        &mut self,
        c: &mut Code,
        field: &FieldTy,
        blob: ValueId,
        mut at: ValueId,
        width: ValueId,
        mut h: ValueId,
    ) -> Result<(ValueId, ValueId), &'static str> {
        match field {
            FieldTy::Scalar(s) if !matches!(s, Scalar::F32 | Scalar::F64) => {
                // The word's bits at its width, unsigned, as `Code::word` folds them.
                let bits = match s.size() {
                    1 => Scalar::U8,
                    2 => Scalar::U16,
                    4 => Scalar::U32,
                    _ => Scalar::U64,
                };
                let word = Self::packed_word(c, blob, at, bits);
                let word = c.widen(word);
                h = c.mix(h, word);
                let size = c.constant(s.size());
                at = c.binary(BinaryOp::Add, at, size);
            }
            FieldTy::Text => {
                let len = Self::packed_count(c, blob, at, width);
                at = c.binary(BinaryOp::Add, at, width);
                let bytes = c.offset_value(blob, at);
                at = c.binary(BinaryOp::Add, at, len);
                let word = self.runtime(c, hirsel_core_ir::RtOp::HashBytes, &[bytes, len]);
                h = c.mix(h, word);
            }
            FieldTy::Array(item, count) => {
                let len = c.constant(*count);
                (h, at) = self.fold_packed_sequence(c, item, blob, at, width, len, h)?;
            }
            FieldTy::FixedSpan(item) | FieldTy::SeqSpan(item) | FieldTy::IndexedSpan(item) => {
                let len = Self::packed_count(c, blob, at, width);
                at = c.binary(BinaryOp::Add, at, width);
                if !matches!(field, FieldTy::FixedSpan(_)) {
                    at = c.binary(BinaryOp::Add, at, width);
                }
                h = c.mix(h, len);
                (h, at) = self.fold_packed_sequence(c, item, blob, at, width, len, h)?;
                if matches!(field, FieldTy::IndexedSpan(_)) {
                    let trailer = c.mul(len, width);
                    at = c.binary(BinaryOp::Add, at, trailer);
                }
            }
            FieldTy::Inline(ty) => {
                let compact = matches!(self.recipes[ty.index()].layout, Layout::Compact(_));
                match self.module.types[ty.index()].body.clone() {
                    ShapeBody::Record(fields) => {
                        for f in &fields {
                            (h, at) = self.fold_packed(c, &f.ty, blob, at, width, h)?;
                        }
                    }
                    ShapeBody::Variant(cases) => {
                        let tag_width = if compact {
                            hirsel_core_layout::tag_width(cases.len())
                        } else {
                            8
                        };
                        let scalar = match tag_width {
                            1 => Scalar::U8,
                            2 => Scalar::U16,
                            4 => Scalar::U32,
                            _ => Scalar::U64,
                        };
                        let tag = Self::packed_word(c, blob, at, scalar);
                        let word = c.widen(tag);
                        let mixed = c.mix(h, word);
                        let tag_size = c.constant(tag_width);
                        let start = c.binary(BinaryOp::Add, at, tag_size);
                        let done = c.block(&[Ty::Scalar(Scalar::U64); 2]);
                        let mut arms = vec![];
                        for i in 0..cases.len() {
                            let block = c.block(&[]);
                            arms.push(hirsel_core_ir::SwitchArm {
                                value: i as u64,
                                edge: c.edge(block, &[]),
                            });
                        }
                        let otherwise = arms.last().unwrap().edge;
                        let range = PoolRange::push(
                            &mut c.body.pools.arms,
                            arms[..arms.len() - 1].iter().copied(),
                        );
                        c.end(Terminator::Switch {
                            value: tag,
                            arms: range,
                            otherwise,
                        });
                        for (i, case) in cases.iter().enumerate() {
                            c.start(c.body.edges[arms[i].edge.index()].target);
                            let (mut hash, mut end) = (mixed, start);
                            for f in &case.fields {
                                (hash, end) = self.fold_packed(c, &f.ty, blob, end, width, hash)?;
                            }
                            if !compact {
                                let stride = c.constant(
                                    self.recipes[ty.index()]
                                        .logical_size
                                        .ok_or("plain variant logical size is not built")?,
                                );
                                end = c.binary(BinaryOp::Add, at, stride);
                            }
                            c.jump(done, &[hash, end]);
                        }
                        c.start(done);
                        [h, at] = c.params(done);
                    }
                }
            }
            _ => return Err("hashing this packed recipe field is not built"),
        }
        Ok((h, at))
    }
    #[allow(clippy::too_many_arguments)]
    fn fold_packed_sequence(
        &mut self,
        c: &mut Code,
        item: &FieldTy,
        blob: ValueId,
        at: ValueId,
        width: ValueId,
        len: ValueId,
        h: ValueId,
    ) -> Result<(ValueId, ValueId), &'static str> {
        let head = c.block(&[Ty::Scalar(Scalar::U64); 3]);
        let body = c.block(&[]);
        let done = c.block(&[]);
        let zero = c.constant(0);
        c.jump(head, &[zero, at, h]);
        c.start(head);
        let params = c.body.blocks[head.index()]
            .params
            .get(&c.body.pools.values)
            .unwrap();
        let (i, cursor, hash) = (params[0], params[1], params[2]);
        let on = c.compare(CompareOp::Lt, i, len);
        c.branch(on, body, done);
        c.start(body);
        let (next_hash, next) = self.fold_packed(c, item, blob, cursor, width, hash)?;
        let one = c.constant(1);
        let i = c.binary(BinaryOp::Add, i, one);
        c.jump(head, &[i, next, next_hash]);
        c.start(done);
        Ok((hash, cursor))
    }
    fn fold_sequence(
        &mut self,
        c: &mut Code,
        item: &FieldTy,
        data: ValueId,
        len: ValueId,
        h: ValueId,
    ) -> Result<ValueId, &'static str> {
        let stride = hirsel_core_layout::field_size(&self.module.types, &self.recipes, item).0;
        let zero = c.constant(0);
        let head = c.block(&[Ty::Scalar(Scalar::U64), Ty::Scalar(Scalar::U64)]);
        let body = c.block(&[]);
        let done = c.block(&[]);
        c.jump(head, &[zero, h]);
        c.start(head);
        let ps = c.params(head);
        let on = c.compare(CompareOp::Lt, ps[0], len);
        c.branch(on, body, done);
        c.start(body);
        let stride = c.constant(stride);
        let offset = c.mul(ps[0], stride);
        let p = c.offset_value(data, offset);
        let h = self.fold_field(c, item, p, ps[1])?;
        let one = c.constant(1);
        let next = c.binary(BinaryOp::Add, ps[0], one);
        c.jump(head, &[next, h]);
        c.start(done);
        Ok(ps[1])
    }
    fn equal_type(
        &mut self,
        c: &mut Code,
        ty: TypeId,
        a: ValueId,
        b: ValueId,
        no: BlockId,
    ) -> Result<(), &'static str> {
        let Layout::Fixed(layout) = self.recipes[ty.index()].layout.clone() else {
            return Err("compact key recipe equality is not built");
        };
        match self.module.types[ty.index()].body.clone() {
            ShapeBody::Record(fs) => {
                for (i, f) in fs.into_iter().enumerate() {
                    let x = c.offset(a, layout.offsets[0][i]);
                    let y = c.offset(b, layout.offsets[0][i]);
                    self.equal_field(c, &f.ty, x, y, no)?;
                }
            }
            ShapeBody::Variant(cs) => {
                let mut tags = vec![];
                for p in [a, b] {
                    let at = c.address(p, PlaceTy::Declared(ty));
                    let tag = c.value(Ty::Scalar(Scalar::U32));
                    c.inst(Inst::Case {
                        result: tag,
                        address: at,
                    });
                    tags.push(tag);
                }
                let on = c.compare(CompareOp::Eq, tags[0], tags[1]);
                c.require(on, no);
                let done = c.block(&[]);
                let mut arms = vec![];
                for (i, _) in cs.iter().enumerate() {
                    let target = c.block(&[]);
                    arms.push(hirsel_core_ir::SwitchArm {
                        value: i as u64,
                        edge: c.edge(target, &[]),
                    });
                }
                let otherwise = arms.last().unwrap().edge;
                let arms_range = PoolRange::push(
                    &mut c.body.pools.arms,
                    arms[..arms.len() - 1].iter().copied(),
                );
                c.end(Terminator::Switch {
                    value: tags[0],
                    arms: arms_range,
                    otherwise,
                });
                for (i, case) in cs.into_iter().enumerate() {
                    c.start(c.body.edges[arms[i].edge.index()].target);
                    for (j, f) in case.fields.into_iter().enumerate() {
                        let x = c.offset(a, layout.offsets[i][j]);
                        let y = c.offset(b, layout.offsets[i][j]);
                        self.equal_field(c, &f.ty, x, y, no)?;
                    }
                    c.jump(done, &[]);
                }
                c.start(done);
            }
        }
        Ok(())
    }
    fn equal_field(
        &mut self,
        c: &mut Code,
        field: &FieldTy,
        a: ValueId,
        b: ValueId,
        no: BlockId,
    ) -> Result<(), &'static str> {
        match field {
            FieldTy::Scalar(s) if !matches!(s, Scalar::F32 | Scalar::F64) => {
                let x = c.word(a, s.size());
                let y = c.word(b, s.size());
                let on = c.compare(CompareOp::Eq, x, y);
                c.require(on, no);
            }
            FieldTy::Inline(t) => {
                let f = self.function(TypeId(t.0), KeyOp::Equal)?;
                let a = c.address(a, PlaceTy::Declared(TypeId(t.0)));
                let b = c.address(b, PlaceTy::Declared(TypeId(t.0)));
                let on = self.call(c, f, &[Arg::Address(a), Arg::Address(b)]);
                c.require(on, no);
            }
            FieldTy::Seq(item) if **item == FieldTy::Scalar(Scalar::U8) => {
                let mut args = vec![];
                for p in [a, b] {
                    args.push(c.read(p, Ty::Ptr));
                    let at = c.offset(p, 8);
                    args.push(c.read(at, Ty::Scalar(Scalar::U64)));
                }
                let ord = self.runtime(c, hirsel_core_ir::RtOp::CompareBytes, &args);
                let zero = c.value(Ty::Scalar(Scalar::I8));
                c.inst(Inst::Const {
                    result: zero,
                    bits: 0,
                });
                let on = c.compare(CompareOp::Eq, ord, zero);
                c.require(on, no);
            }
            FieldTy::Seq(item) => {
                let x = c.read(a, Ty::Ptr);
                let y = c.read(b, Ty::Ptr);
                let at = c.offset(a, 8);
                let len = c.read(at, Ty::Scalar(Scalar::U64));
                let at = c.offset(b, 8);
                let other = c.read(at, Ty::Scalar(Scalar::U64));
                let on = c.compare(CompareOp::Eq, len, other);
                c.require(on, no);
                self.equal_sequence(c, item, x, y, len, no)?;
            }
            FieldTy::Array(item, len) => {
                let len = c.constant(*len);
                self.equal_sequence(c, item, a, b, len, no)?;
            }
            FieldTy::Box(t) | FieldTy::Rc(t) | FieldTy::Arc(t) => {
                let mut x = c.read(a, Ty::Ptr);
                let mut y = c.read(b, Ty::Ptr);
                if !matches!(field, FieldTy::Box(_)) {
                    x = c.offset(x, 8);
                    y = c.offset(y, 8);
                }
                let f = self.function(TypeId(t.0), KeyOp::Equal)?;
                let x = c.address(x, PlaceTy::Declared(TypeId(t.0)));
                let y = c.address(y, PlaceTy::Declared(TypeId(t.0)));
                let on = self.call(c, f, &[Arg::Address(x), Arg::Address(y)]);
                c.require(on, no);
            }
            // Two located values are equal when their packed encodings order equal
            // (FORM #63, #65); the locators' addresses never matter.
            FieldTy::Locator(target) => {
                let different = c.block(&[Ty::Scalar(Scalar::I8)]);
                self.order_located(c, target, a, b, different)?;
                let same = c.block(&[]);
                c.jump(same, &[]);
                c.start(different);
                c.jump(no, &[]);
                c.start(same);
            }
            _ => return Err("comparison of this key recipe is not built"),
        }
        Ok(())
    }
    #[allow(clippy::too_many_arguments)]
    fn equal_sequence(
        &mut self,
        c: &mut Code,
        item: &FieldTy,
        a: ValueId,
        b: ValueId,
        len: ValueId,
        no: BlockId,
    ) -> Result<(), &'static str> {
        let stride = hirsel_core_layout::field_size(&self.module.types, &self.recipes, item).0;
        let zero = c.constant(0);
        let head = c.block(&[Ty::Scalar(Scalar::U64)]);
        let body = c.block(&[]);
        let done = c.block(&[]);
        c.jump(head, &[zero]);
        c.start(head);
        let i = c.params(head)[0];
        let on = c.compare(CompareOp::Lt, i, len);
        c.branch(on, body, done);
        c.start(body);
        let stride = c.constant(stride);
        let offset = c.mul(i, stride);
        let x = c.offset_value(a, offset);
        let y = c.offset_value(b, offset);
        self.equal_field(c, item, x, y, no)?;
        let one = c.constant(1);
        let next = c.binary(BinaryOp::Add, i, one);
        c.jump(head, &[next]);
        c.start(done);
        Ok(())
    }
}

pub(super) struct Code {
    pub(super) body: Body,
    current: BlockId,
    open: Vec<Inst>,
}
impl Code {
    pub(super) fn new(params: &[Ty]) -> Self {
        let mut c = Self {
            body: Body::default(),
            current: BlockId(0),
            open: vec![],
        };
        c.block(params);
        c
    }
    pub(super) fn value(&mut self, ty: Ty) -> ValueId {
        let id = ValueId(self.body.values.len() as u32);
        self.body.values.push(ty);
        id
    }
    pub(super) fn inst(&mut self, inst: Inst) {
        self.open.push(inst);
    }
    pub(super) fn block(&mut self, tys: &[Ty]) -> BlockId {
        let start = self.body.pools.values.len();
        for &ty in tys {
            let value = self.value(ty);
            self.body.pools.values.push(value);
        }
        let params = PoolRange::new(start as u32, tys.len() as u32);
        let id = BlockId(self.body.blocks.len() as u32);
        self.body.blocks.push(Block {
            params,
            insts: PoolRange::EMPTY,
            anchor: None,
        });
        self.body.terminators.push(Terminator::Unreachable);
        id
    }
    fn params(&self, b: BlockId) -> [ValueId; 2] {
        let ps = self.body.blocks[b.index()]
            .params
            .get(&self.body.pools.values)
            .unwrap();
        [ps[0], ps.get(1).copied().unwrap_or(ps[0])]
    }
    pub(super) fn start(&mut self, b: BlockId) {
        self.current = b;
    }
    pub(super) fn end(&mut self, t: Terminator) {
        self.body.blocks[self.current.index()].insts =
            PoolRange::push(&mut self.body.insts, self.open.drain(..));
        self.body.inst_anchors.resize(self.body.insts.len(), None);
        self.body.terminators[self.current.index()] = t;
    }
    fn edge(&mut self, target: BlockId, args: &[ValueId]) -> EdgeId {
        let args = PoolRange::push(&mut self.body.pools.values, args.iter().copied());
        let id = EdgeId(self.body.edges.len() as u32);
        self.body.edges.push(Edge { target, args });
        id
    }
    fn jump(&mut self, target: BlockId, args: &[ValueId]) {
        let edge = self.edge(target, args);
        self.end(Terminator::Jump(edge));
    }
    pub(super) fn branch(&mut self, on: ValueId, yes: BlockId, no: BlockId) {
        let yes = self.edge(yes, &[]);
        let no = self.edge(no, &[]);
        self.end(Terminator::Branch {
            condition: on,
            yes,
            no,
        });
    }
    fn require(&mut self, on: ValueId, no: BlockId) {
        let yes = self.block(&[]);
        self.branch(on, yes, no);
        self.start(yes);
    }
    fn typed_constant(&mut self, ty: Scalar, bits: u64) -> ValueId {
        let result = self.value(Ty::Scalar(ty));
        self.inst(Inst::Const { result, bits });
        result
    }
    fn constant(&mut self, bits: u64) -> ValueId {
        let result = self.value(Ty::Scalar(Scalar::U64));
        self.inst(Inst::Const { result, bits });
        result
    }
    fn boolean(&mut self, value: bool) -> ValueId {
        let result = self.value(Ty::Scalar(Scalar::Bool));
        self.inst(Inst::Const {
            result,
            bits: u64::from(value),
        });
        result
    }
    fn binary(&mut self, op: BinaryOp, left: ValueId, right: ValueId) -> ValueId {
        let result = self.value(Ty::Scalar(Scalar::U64));
        self.inst(Inst::Binary {
            result,
            op,
            left,
            right,
            fail: None,
        });
        result
    }
    fn mul(&mut self, left: ValueId, right: ValueId) -> ValueId {
        let result = self.value(Ty::Scalar(Scalar::U64));
        self.inst(Inst::IntArith {
            result,
            flag: None,
            op: ArithOp::Mul,
            mode: ArithMode::Wrapping,
            left,
            right,
        });
        result
    }
    pub(super) fn compare(&mut self, op: CompareOp, left: ValueId, right: ValueId) -> ValueId {
        let result = self.value(Ty::Scalar(Scalar::Bool));
        self.inst(Inst::Compare {
            result,
            op,
            left,
            right,
        });
        result
    }
    pub(super) fn address(&mut self, p: ValueId, ty: PlaceTy) -> AddressId {
        let id = AddressId(self.body.addresses.len() as u32);
        self.body.addresses.push(AddressInfo {
            base: AddressBase::Value(p),
            steps: PoolRange::EMPTY,
            ty,
        });
        self.inst(Inst::Address {
            address: id,
            fail: None,
        });
        id
    }
    fn offset_value(&mut self, p: ValueId, offset: ValueId) -> ValueId {
        let steps = PoolRange::push(&mut self.body.pools.steps, [Step::Byte(offset)]);
        let id = AddressId(self.body.addresses.len() as u32);
        self.body.addresses.push(AddressInfo {
            base: AddressBase::Value(p),
            steps,
            ty: PlaceTy::Value(Ty::Scalar(Scalar::U8)),
        });
        self.inst(Inst::Address {
            address: id,
            fail: None,
        });
        let result = self.value(Ty::Ptr);
        self.inst(Inst::Pointer {
            result,
            address: id,
        });
        result
    }
    fn offset(&mut self, p: ValueId, offset: u64) -> ValueId {
        if offset == 0 {
            p
        } else {
            let offset = self.constant(offset);
            self.offset_value(p, offset)
        }
    }
    pub(super) fn read(&mut self, p: ValueId, ty: Ty) -> ValueId {
        let address = self.address(p, PlaceTy::Value(ty));
        let result = self.value(ty);
        self.inst(Inst::Read {
            result,
            address,
            mode: ReadMode::Bits,
        });
        result
    }
    fn widen(&mut self, operand: ValueId) -> ValueId {
        if self.body.values[operand.index()] == Ty::Scalar(Scalar::U64) {
            operand
        } else {
            let result = self.value(Ty::Scalar(Scalar::U64));
            self.inst(Inst::Convert {
                result,
                flag: None,
                operand,
                mode: ConvertMode::Widen,
            });
            result
        }
    }
    fn word(&mut self, p: ValueId, width: u64) -> ValueId {
        let s = match width {
            1 => Scalar::U8,
            2 => Scalar::U16,
            4 => Scalar::U32,
            _ => Scalar::U64,
        };
        let value = self.read(p, Ty::Scalar(s));
        self.widen(value)
    }
    fn mix(&mut self, h: ValueId, w: ValueId) -> ValueId {
        let x = self.binary(BinaryOp::BitXor, h, w);
        let factor = self.constant(0x9e3779b97f4a7c15);
        let x = self.mul(x, factor);
        let left = self.constant(26);
        let right = self.constant(38);
        let a = self.binary(BinaryOp::Shl, x, left);
        let b = self.binary(BinaryOp::Shr, x, right);
        self.binary(BinaryOp::BitOr, a, b)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hirsel_core_ir::{eval, text, wf};

    #[test]
    fn metadata_is_built_once_per_key_type() {
        let mut source = String::from("fn repeated[T:key,U:key](a:T,b:T,c:U,d:U)->bool {\n");
        for _ in 0..256 {
            source.push_str("let _ = a == b; let _ = c == d;\n");
        }
        source.push_str("true }");
        let (index, inputs) = crate::tests::program_inputs(&source);
        let p = Program::declare(&index, &inputs);
        let f = p
            .fns
            .iter()
            .position(|f| f.path.0.name == "repeated")
            .unwrap();
        let mut emitter = Emitter::new();
        let mut cx = Cx::new(&p, f, &mut emitter);
        cx.run(f, p.fns[f].body.unwrap());
        assert!(
            cx.out.refusals.is_empty() && cx.out.gaps.is_empty(),
            "{:?} {:?}",
            cx.out.refusals,
            cx.out.gaps
        );
        assert_eq!(cx.key_metadata_builds, 2);
        assert_eq!(cx.b.calls.len(), 512);
        assert!(cx.finish().function.is_some());
    }

    #[test]
    fn fixed_hash_fold_oracles() {
        // Constants derived from wrapping multiplication by 9e3779b97f4a7c15,
        // a left rotation by26 per word, and one final h^(h>>32). A nested
        // record streams its fields into h, rather than finishing a child hash.
        let vectors = [
            (
                "type t0 = own \"word\" { n:u64 }",
                "",
                vec![(0, "u64", "const 7")],
                None,
                0x49ec259204a234c2,
            ),
            (
                "type t0 = own \"signed\" { n:i8 }",
                "",
                vec![(0, "u8", "const 255")],
                None,
                0x172cc663b949ce9c,
            ),
            (
                "type t0 = own \"outer\" { a:t1, b:u8 }\ntype t1 = own \"inner\" { a:u64 }",
                "",
                vec![(0, "u64", "const 7"), (8, "u8", "const 9")],
                None,
                0xbd4a22218871e85e,
            ),
            (
                "type t0 = own \"variant\" | Empty { } | Value { n:u64 }",
                "",
                vec![(8, "u64", "const 7")],
                Some(1),
                0xd595b4bd0195bebd,
            ),
            (
                "type t0 = own \"String\" { bytes:seq u8 }",
                "static d0 = align 1 hex 616263",
                vec![
                    (0, "ptr", "static d0"),
                    (8, "u64", "const 3"),
                    (16, "u64", "const 0"),
                ],
                None,
                0xfcfedf8da784a013,
            ),
            (
                "type t0 = own \"String\" { bytes:seq u8 }",
                "",
                vec![
                    (0, "ptr", "const 0x0"),
                    (8, "u64", "const 0"),
                    (16, "u64", "const 0"),
                ],
                None,
                0,
            ),
            (
                "type t0 = own \"List\" { items:seq u64 }",
                "static d0 = align 8 hex 07000000000000000900000000000000",
                vec![
                    (0, "ptr", "static d0"),
                    (8, "u64", "const 2"),
                    (16, "u64", "const 0"),
                ],
                None,
                0xbd4a22218871e85e,
            ),
        ];
        for (types, statics, fields, case, want) in vectors {
            let mut m = text::parse(&format!("core v1\n{types}\n{statics}\n")).unwrap();
            let hash = ensure(&mut m, TypeId(0), KeyOp::Hash).unwrap();
            let sig = m.functions[hash.index()].signature;
            assert_eq!(ensure(&mut m, TypeId(0), KeyOp::Hash), Ok(hash));
            let mut addresses = String::new();
            let mut stores = String::new();
            for (i, (offset, ty, init)) in fields.iter().enumerate() {
                let a = i + 1;
                let v = 2 * i;
                addresses.push_str(&format!("addr a{a} = a0 byte v{v} : {ty}\n"));
                stores.push_str(&format!(
                    "v{v}: u64 = const {offset}\nv{}: {ty} = {init}\naddress a{a}\nstore a{a}, v{}\n",
                    v + 1,
                    v + 1
                ));
            }
            let result = fields.len() * 2;
            let g = m.signatures.len();
            let n = m.instances.len();
            let f = m.functions.len();
            let set = case
                .map(|k| format!("set_case a0, {k}\n"))
                .unwrap_or_default();
            let fixture = format!(
                "{}\nsig g{g} = () -> (u64)\ninst n{n} = app@1/hash/check [] []\nfn f{f} = n{n} g{g} {{\nslot s0 = t0\naddr a0 = s0 : t0\n{addresses}b0:\nlive s0\naddress a0\n{set}{stores}v{result}: u64 = call f{} g{} (&a0)\nreturn v{result}\n}}\n",
                text::print(&m),
                hash.0,
                sig.0
            );
            let m = text::parse(&fixture).unwrap();
            wf::module(&m).unwrap();
            let run = eval::run(&m, FuncId(f as u32), &[], 0);
            assert_eq!(run.outcome, eval::Outcome::Returned(vec![want]), "{types}");
            assert_eq!(run.used, 0);
        }
    }
}

#[cfg(test)]
mod ordered_cost_tests {
    use super::*;
    #[test]
    fn scalar_order_instances_are_direct_predicates() {
        let source = r#"
fn less[T:ordered](a:View[T],b:View[T])->bool { a < b }
fn equal[T:key](a:T,b:T)->bool { a == b }
fn equal_same()->bool { equal(7u64,7u64) }
fn equal_different()->bool { equal(7u64,9u64) }
fn closed(a:View[u64],b:View[u64])->bool { a < b }
fn closed_text(a:View[String],b:View[String])->bool { a < b }
fn closed_min(a:u64,b:u64)->u64 { if b<a {b} else {a} }
fn min[T:ordered+plain](a:T,b:T)->T { if b<a {b} else {a} }
fn min_scalar()->u64 { min(7u64,9u64) }
fn scalar()->bool { let a=7u64; let b=9u64; less(view a,view b) }
fn text()->bool { let a="a"; let b="b"; less(view a,view b) }
"#;
        let (index, inputs) = crate::tests::program_inputs(source);
        let p = Program::declare(&index, &inputs);
        assert!(p.refusals.is_empty(), "{:?}", p.refusals);
        let mut e = Emitter::new();
        for (f, fd) in p
            .fns
            .iter()
            .enumerate()
            .filter(|(_, f)| f.path.package_is("app", "1") && f.body.is_some())
        {
            let c = p.check_lower(f, &mut e);
            assert!(
                c.refusals.is_empty() && c.gaps.is_empty(),
                "{}: {:?} {:?}",
                fd.path,
                c.refusals,
                c.gaps
            );
        }
        e.materialize_instances().unwrap();
        hirsel_core_ir::wf::module(&e.module).unwrap();
        for f in &e.module.functions {
            let key = &e.module.instances[f.instance.index()];
            if key.definition.0.modules == ["order"]
                || key.definition.0.name == "closed"
                || key.definition.0.name == "closed_text"
                || matches!(
                    key.definition.0.name.as_str(),
                    "less" | "equal" | "min" | "borrowed_min" | "closed_min" | "closed_borrowed"
                )
            {
                if let Some(body) = &f.body {
                    let calls = body
                        .insts
                        .iter()
                        .filter(|i| matches!(i, Inst::Call { .. }))
                        .count();
                    let comparisons = body
                        .insts
                        .iter()
                        .filter(|i| matches!(i, Inst::Compare { .. }))
                        .count();
                    eprintln!(
                        "ORDER COST {:?}: {} insts, {comparisons} compares, {calls} calls",
                        key,
                        body.insts.len()
                    );
                    if key.types == ["u64"]
                        && matches!(key.definition.0.name.as_str(), "less" | "equal")
                    {
                        assert_eq!(calls, 0);
                        assert_eq!(comparisons, 1);
                    }
                }
            }
        }
        for (name, expected) in [("equal_same", 1), ("equal_different", 0)] {
            let f = e
                .module
                .functions
                .iter()
                .position(|f| e.module.instances[f.instance.index()].definition.0.name == name)
                .unwrap();
            assert_eq!(
                hirsel_core_ir::eval::run(&e.module, FuncId(f as u32), &[], 0).outcome,
                hirsel_core_ir::eval::Outcome::Returned(vec![expected]),
            );
        }
        eprintln!("{}", hirsel_core_ir::text::print(&e.module));
    }
}

#[cfg(test)]
mod located_order_tests {
    use super::*;
    use hirsel_core_ir::{eval, text, wf};
    fn word(bytes: &mut Vec<u8>, value: u64, width: usize) {
        bytes.extend_from_slice(&value.to_le_bytes()[..width]);
    }
    fn text_bytes(width: usize, value: &str) -> Vec<u8> {
        let mut out = vec![];
        word(&mut out, value.len() as u64, width);
        out.extend_from_slice(value.as_bytes());
        out
    }
    fn seq(width: usize, values: &[&str], indexed: bool) -> Vec<u8> {
        let mut data = vec![];
        let mut offsets = vec![];
        for value in values {
            offsets.push(data.len() as u64);
            data.extend(text_bytes(width, value));
        }
        let mut out = vec![];
        word(&mut out, values.len() as u64, width);
        word(&mut out, data.len() as u64, width);
        out.extend(data);
        if indexed {
            for offset in offsets {
                word(&mut out, offset, width);
            }
        }
        out
    }
    fn blob(width: usize, value: &[u8]) -> String {
        let mut bytes = vec![];
        word(&mut bytes, value.len() as u64, 8);
        word(&mut bytes, width as u64, 8);
        bytes.extend(value);
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }
    #[test]
    fn typed_locator_order_uses_values_across_widths_and_advances_private_encodings() {
        // Each name is opaque to the derivation. Only its target recipe determines the walk.
        let mut vectors = vec![
            (
                "locator text".to_owned(),
                "".to_owned(),
                2,
                text_bytes(2, "abc"),
                4,
                text_bytes(4, "abc"),
                0,
            ),
            (
                "locator text".to_owned(),
                "".to_owned(),
                8,
                text_bytes(8, "ab"),
                2,
                text_bytes(2, "abc"),
                255,
            ),
            (
                "locator seqspan text".to_owned(),
                "".to_owned(),
                2,
                seq(2, &["ab", "c"], false),
                8,
                seq(8, &["ab", "d"], false),
                255,
            ),
            (
                "locator indexed text".to_owned(),
                "".to_owned(),
                4,
                seq(4, &["ab", "c"], true),
                2,
                seq(2, &["ab"], true),
                1,
            ),
        ];
        let mut a = vec![];
        word(&mut a, 2, 2);
        word(&mut a, 1, 8);
        word(&mut a, 9, 8);
        let mut b = vec![];
        word(&mut b, 2, 4);
        word(&mut b, 1, 8);
        word(&mut b, 10, 8);
        vectors.push(("locator fixed u64".into(), "".into(), 2, a, 4, b, 255));
        let mut a = vec![0u8; 16];
        a[0] = 128;
        a[8..16].copy_from_slice(&9u64.to_le_bytes());
        let mut b = vec![0u8; 16];
        b[0] = 127;
        b[8..16].copy_from_slice(&1u64.to_le_bytes());
        vectors.push((
            "stable t1".into(),
            r#"type t1 = stable "NativeStable" { first:i8, next:u64 }"#.into(),
            2,
            a,
            8,
            b,
            255,
        ));
        let types =
            "type t1 = compact u16 \"OpaqueRecord\" { label:text, rows:indexed text, tail:i8 }";
        let mut a = text_bytes(2, "same");
        a.extend(seq(2, &["a", "b"], true));
        a.push(128);
        let mut b = text_bytes(8, "same");
        b.extend(seq(8, &["a", "b"], true));
        b.push(127);
        vectors.push(("locator t1".into(), types.into(), 2, a, 8, b, 255));
        let types = "type t1 = compact u16 \"OpaqueVariant\" | First { rows:seqspan text } | Second { value:text }";
        let mut a = vec![0];
        a.extend(seq(2, &["z"], false));
        let mut b = vec![1];
        b.extend(text_bytes(4, "a"));
        vectors.push(("locator t1".into(), types.into(), 2, a, 4, b, 255));
        let types = "type t1 = own \"Private\" | Small { n:i8 } | Large { n:u64 }";
        // Private variant stride16, two per array; the first cell is equal Small(-1),
        // and the second is Small(-128) versus Small(127), including zero tails.
        let mut a = vec![];
        word(&mut a, 1, 2);
        a.extend([0u8; 32]);
        a[2 + 8] = 255;
        a[2 + 16 + 8] = 128;
        let mut b = vec![];
        word(&mut b, 1, 8);
        b.extend([0u8; 32]);
        b[8 + 8] = 255;
        b[8 + 16 + 8] = 127;
        vectors.push((
            "locator fixed array 2 t1".into(),
            types.into(),
            2,
            a,
            8,
            b,
            255,
        ));
        // Equal values at other widths and offsets (FORM #65: hash and equality read
        // the compact recipe, never the encoding's bytes or the locator).
        let types =
            "type t1 = compact u16 \"OpaqueRecord\" { label:text, rows:indexed text, tail:i8 }";
        let mut a = text_bytes(2, "same");
        a.extend(seq(2, &["a", "bc"], true));
        a.push(7);
        let mut b = text_bytes(8, "same");
        b.extend(seq(8, &["a", "bc"], true));
        b.push(7);
        vectors.push(("locator t1".into(), types.into(), 2, a, 8, b, 0));
        let types = "type t1 = compact u16 \"OpaqueVariant\" | First { rows:seqspan text } | Second { value:text }";
        let mut a = vec![1];
        a.extend(text_bytes(4, "v"));
        let mut b = vec![1];
        b.extend(text_bytes(2, "v"));
        vectors.push(("locator t1".into(), types.into(), 4, a, 2, b, 0));
        for (field, types, aw, a, bw, b, want) in vectors {
            let native_reference = field == "stable t1";
            let hex = |width, value: &[u8]| {
                if native_reference {
                    value.iter().map(|b| format!("{b:02x}")).collect()
                } else {
                    blob(width, value)
                }
            };
            let check = |op: KeyOp, result: &str, call: &str| {
                let mut m=text::parse(&format!("core v1\ntype t0 = own \"OpaqueCarrier\" {{ at:{field} }}\n{types}\nstatic d0 = align 8 hex {}\nstatic d1 = align 8 hex {}\n",hex(aw,&a),hex(bw,&b))).unwrap();
                let f = ensure(&mut m, TypeId(0), op).unwrap();
                let g = m.functions[f.index()].signature.0;
                let n = m.instances.len();
                let row = m.functions.len();
                let sig = m.signatures.len();
                let source = format!(
                    "{}\nsig g{sig} = () -> (i8)\ninst n{n} = app@1/located/check [] []\nfn f{row} = n{n} g{sig} {{\nslot s0=t0\nslot s1=t0\naddr a0=s0:t0\naddr a1=s1:t0\naddr a2=a0 byte v0:ptr\naddr a3=a0 byte v1:u64\naddr a4=a1 byte v0:ptr\naddr a5=a1 byte v1:u64\nb0:\nlive s0\nlive s1\naddress a0\naddress a1\nv0:u64=const 0\nv1:u64=const 8\nv2:u64=const 16\nv3:ptr=static d0\nv4:ptr=static d1\naddress a2\nstore a2,v3\naddress a3\nstore a3,v2\naddress a4\nstore a4,v4\naddress a5\nstore a5,v2\nv5:{result}=call f{} g{g} ({call})\nreturn v5\n}}\n",
                    text::print(&m),
                    f.0
                );
                let source = source.replace("() -> (i8)", &format!("() -> ({result})"));
                let source = if native_reference {
                    source
                        .replace("address a3\nstore a3,v2\n", "")
                        .replace("address a5\nstore a5,v2\n", "")
                } else {
                    source
                };
                let m = text::parse(&source).unwrap();
                wf::module(&m).unwrap();
                let run = eval::run(&m, FuncId(row as u32), &[], 0);
                assert_eq!(run.used, 0);
                match run.outcome {
                    eval::Outcome::Returned(v) => v[0],
                    other => panic!("{field}: {types}: {other:?}"),
                }
            };
            assert_eq!(
                check(KeyOp::Order, "i8", "&a0,&a1"),
                want,
                "{field}: {types}"
            );
            if native_reference {
                continue;
            }
            let equal = check(KeyOp::Equal, "bool", "&a0,&a1");
            assert_eq!(equal, u64::from(want == 0), "equal {field}: {types}");
            let hashes = (
                check(KeyOp::Hash, "u64", "&a0"),
                check(KeyOp::Hash, "u64", "&a1"),
            );
            assert_eq!(hashes.0 == hashes.1, want == 0, "hash {field}: {types}");
        }
    }
}
