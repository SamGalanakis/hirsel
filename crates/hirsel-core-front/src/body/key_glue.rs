//! Mechanical `key` glue. Generic calls retain a type recipe until the ordinary
//! instance materializer substitutes it; no source body is checked again.
use super::*;
use hirsel_core_layout::{Body as ShapeBody, FieldTy};

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum KeyOp {
    Hash,
    Fold,
    Equal,
}
impl KeyOp {
    fn definition(self) -> DefPath {
        DefPath::core(match self {
            Self::Hash => "map.$key_hash",
            Self::Fold => "map.$key_fold",
            Self::Equal => "map.$key_equal",
        })
    }
    fn signature(self, ty: TypeId) -> Signature {
        let mut params = vec![Param::Address(ty)];
        match self {
            Self::Hash => {}
            Self::Fold => params.push(Param::Value(Ty::Scalar(Scalar::U64))),
            Self::Equal => params.push(Param::Address(ty)),
        }
        Signature {
            params,
            results: vec![Ty::Scalar(if self == Self::Equal {
                Scalar::Bool
            } else {
                Scalar::U64
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
            _ => return Err("hashing this key recipe is not built"),
        })
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

struct Code {
    body: Body,
    current: BlockId,
    open: Vec<Inst>,
}
impl Code {
    fn new(params: &[Ty]) -> Self {
        let mut c = Self {
            body: Body::default(),
            current: BlockId(0),
            open: vec![],
        };
        c.block(params);
        c
    }
    fn value(&mut self, ty: Ty) -> ValueId {
        let id = ValueId(self.body.values.len() as u32);
        self.body.values.push(ty);
        id
    }
    fn inst(&mut self, inst: Inst) {
        self.open.push(inst);
    }
    fn block(&mut self, tys: &[Ty]) -> BlockId {
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
    fn start(&mut self, b: BlockId) {
        self.current = b;
    }
    fn end(&mut self, t: Terminator) {
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
    fn branch(&mut self, on: ValueId, yes: BlockId, no: BlockId) {
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
    fn compare(&mut self, op: CompareOp, left: ValueId, right: ValueId) -> ValueId {
        let result = self.value(Ty::Scalar(Scalar::Bool));
        self.inst(Inst::Compare {
            result,
            op,
            left,
            right,
        });
        result
    }
    fn address(&mut self, p: ValueId, ty: PlaceTy) -> AddressId {
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
    fn read(&mut self, p: ValueId, ty: Ty) -> ValueId {
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
