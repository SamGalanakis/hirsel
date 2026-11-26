//! Published plain constants lowered in declaration order.
use super::patterns::{Head, Subject};
use super::*;
use hirsel_core_ir::{LayoutQuery, SwitchArm};

impl Cx<'_, '_, '_> {
    /// Store a published aggregate once per module, before native layout.
    /// Every use reads the same read-only block; binding a mutable local copies it.
    pub(super) fn constant_value(
        &mut self,
        value: crate::ConstValue,
        n: NodeId,
    ) -> Option<ValueId> {
        if !self.live() {
            return None;
        }
        match value.data {
            crate::ConstData::Bits(bits) => {
                let ty = self.ir_ty(value.ty)?;
                self.konst(ty, bits, n)
            }
            crate::ConstData::Unit => None,
            crate::ConstData::StaticString(_) => unreachable!("strings use their literal carrier"),
            crate::ConstData::Fields { .. } => {
                let data = if let Some(&id) = self.e.constant_statics.get(&value) {
                    id
                } else {
                    let id = match self.rep_at(value.ty, n)? {
                        Rep::Place(id) => id,
                        // A zero-sized aggregate has no value to read.
                        Rep::None => return None,
                        Rep::Scalar(_) => {
                            self.gap(n, "a published aggregate held as a scalar is not built");
                            return None;
                        }
                    };
                    let data = hirsel_core_ir::StaticId(self.e.module.statics.len() as u32);
                    let value_data = self.static_value(value);
                    self.e
                        .module
                        .statics
                        .push(hirsel_core_ir::Static::Aggregate {
                            ty: id,
                            value: value_data,
                        });
                    self.e.constant_statics.insert(value, data);
                    data
                };
                let result = self.value(Ty::Ptr);
                self.inst(Inst::ReadStatic { result, data }, n);
                Some(result)
            }
        }
    }
    /// A read of computed constant `constant` through its projections: its slot
    /// (PROG-10) is a read-only block the driver binds to the value, which the body
    /// reads at run time. The body names the constant, never its value.
    fn computed_path(
        &mut self,
        n: NodeId,
        constant: u32,
        u: &hirsel_core_resolve::ResolvedUse,
        want: Option<Want>,
    ) -> (TyId, Option<ValueId>) {
        let p = self.p;
        let c = &p.constants[constant as usize];
        let mut ty = c.ty;
        if ty == ERROR {
            self.halt();
            return (ERROR, None);
        }
        let mut value = self.computed_slot(constant, n);
        let role = match self.t.kind(ty) {
            TyKind::Adt(d, _) => p.decls[d.index()].role,
            _ => None,
        };
        if let Some(frozen) = match role {
            Some(Role::String) => Some(false),
            Some(Role::FrozenBytes) => Some(true),
            _ => None,
        } {
            // A static value reads as its literal does: a String literal is the
            // view of its slot where a view is expected.
            let v = value.and_then(|slot| self.computed_bytes(slot, ty, frozen, n));
            let viewed = !frozen
                && want.is_some_and(|w| matches!(self.t.kind(w.ty), TyKind::View(x) if *x == ty));
            if viewed {
                return (self.t.mk(TyKind::View(ty)), v);
            }
            self.temp(ty, v);
            return (ty, v);
        }
        for token_id in u.projections.start..u.projections.end {
            let token_id = hirsel_core_syntax::TokenId(token_id);
            let token = &self.item.tree.tokens[token_id.0 as usize];
            if token.kind != TokenKind::Ident {
                continue;
            }
            let name = p.index.symbols.text(token.symbol);
            let found = match self.t.kind(ty).clone() {
                TyKind::Tuple(xs) => name
                    .parse::<usize>()
                    .ok()
                    .and_then(|i| Some((i, *xs.get(i)?))),
                TyKind::Adt(d, args) => {
                    if self.require_visible_fields(d, n).is_err() {
                        return (ERROR, None);
                    }
                    match &p.decls[d.index()].body {
                        crate::DeclBody::Record(fs) => fs
                            .iter()
                            .position(|f| f.name == name)
                            .map(|i| (i, crate::types::subst(&mut self.t, fs[i].ty, &args))),
                        _ => None,
                    }
                }
                _ => None,
            };
            let Some((i, field)) = found else {
                self.refuse("LANG-11", n, "constant field is absent");
                return (ERROR, None);
            };
            value = (|| {
                let v = value?;
                if !self.live() {
                    return None;
                }
                let Rep::Place(id) = self.rep_at(ty, n)? else {
                    return None;
                };
                let place = self.place_ty(field, n)?;
                let at = self.fields_of(AddressBase::Value(v), id, n);
                let address =
                    self.address(AddressBase::Address(at), &[Step::Field(i as u32)], place, n);
                match self.rep_at(field, n)? {
                    r @ Rep::Scalar(_) => Some(self.read_bits(address, r.ty()?, n)),
                    Rep::Place(_) => Some(self.pointer(address, n)),
                    Rep::None => None,
                }
            })();
            ty = field;
        }
        (ty, value)
    }
    /// The value of computed constant `constant`, read from its slot: a scalar is
    /// loaded, an aggregate is the slot's read-only block.
    fn computed_slot(&mut self, constant: u32, n: NodeId) -> Option<ValueId> {
        if !self.live() {
            return None;
        }
        let ty = self.p.constants[constant as usize].ty;
        self.out
            .computed
            .insert(self.p.constants[constant as usize].path.clone());
        let rep = self.rep_at(ty, n)?;
        if matches!(rep, Rep::None) {
            return None;
        }
        let data = if let Some(&id) = self.e.computed_statics.get(&constant) {
            id
        } else {
            // The slot holds the value as the reader's representation does: a
            // declared row by value, or a scalar.
            let field = match rep {
                Rep::Place(id) => {
                    hirsel_core_layout::FieldTy::Inline(hirsel_core_layout::TypeRef(id.0))
                }
                Rep::Scalar(hirsel_core_layout::Ty::Scalar(s)) => {
                    hirsel_core_layout::FieldTy::Scalar(s)
                }
                _ => {
                    self.gap(n, "a computed constant held as a vector is not built");
                    return None;
                }
            };
            let data = hirsel_core_ir::StaticId(self.e.module.statics.len() as u32);
            self.e
                .module
                .statics
                .push(hirsel_core_ir::Static::Computed {
                    ty: field,
                    constant: self.p.constants[constant as usize].path.clone(),
                });
            self.e.computed_statics.insert(constant, data);
            data
        };
        let pointer = self.value(Ty::Ptr);
        self.inst(
            Inst::ReadStatic {
                result: pointer,
                data,
            },
            n,
        );
        match rep {
            Rep::Scalar(s) => {
                let address = self.address(AddressBase::Value(pointer), &[], PlaceTy::Value(s), n);
                Some(self.read_bits(address, s, n))
            }
            _ => Some(pointer),
        }
    }
    fn static_value(&self, value: crate::ConstValue) -> hirsel_core_ir::StaticValue {
        use hirsel_core_ir::StaticValue;
        match value.data {
            crate::ConstData::Bits(bits) => StaticValue::Bits(bits),
            crate::ConstData::StaticString(_) => unreachable!("constant aggregates are plain"),
            crate::ConstData::Unit => StaticValue::Fields {
                case: None,
                fields: vec![],
            },
            crate::ConstData::Fields { start, len, case } => StaticValue::Fields {
                case,
                fields: (start as usize..start as usize + len as usize)
                    .map(|i| self.static_value(self.p.constant_parts[i]))
                    .collect(),
            },
        }
    }
}

/// Target-dependent constants and constant-form branches (PROG-6). Checked code keeps
/// every arm; a targeted selection names the arm each target takes, and retention
/// (`hirsel_core_ir::select_target`) keeps only that arm.
impl Cx<'_, '_, '_> {
    /// A path naming published constant `constant`, through its projections.
    pub(super) fn constant_path(
        &mut self,
        n: NodeId,
        constant: u32,
        u: &hirsel_core_resolve::ResolvedUse,
        want: Option<Want>,
    ) -> (TyId, Option<ValueId>) {
        let p = self.p;
        let c = &p.constants[constant as usize];
        if c.computed {
            return self.computed_path(n, constant, u, want);
        }
        let mut values = match (c.targets, c.value) {
            (Some(values), _) => values,
            (None, Some(value)) => [value; 3],
            (None, None) => {
                self.halt();
                return (ERROR, None);
            }
        };
        for token_id in u.projections.start..u.projections.end {
            let token_id = hirsel_core_syntax::TokenId(token_id);
            let token = &self.item.tree.tokens[token_id.0 as usize];
            if token.kind != TokenKind::Ident {
                continue;
            }
            let name = p.index.symbols.text(token.symbol);
            for value in &mut values {
                match p.constant_field(p.modules[self.at.module as usize].id, *value, name) {
                    Ok(field) => *value = field,
                    Err(rule) => {
                        self.push_refusal(p.constant_field_refusal(
                            rule,
                            self.cause(n),
                            self.item.token_span(token_id),
                            *value,
                            name,
                        ));
                        return (ERROR, None);
                    }
                }
            }
        }
        let value = values[0];
        if let crate::ConstData::StaticString(id) = value.data {
            return self.static_string_value(&p.constant_strings[id as usize], value.ty, n, want);
        }
        if values.iter().all(|v| *v == value) {
            let emitted = self.constant_value(value, n);
            return (value.ty, emitted);
        }
        // A value that differs by target: one block per target, joined.
        let join = self.new_result_join();
        if self.live() {
            let blocks = [(); 3].map(|_| self.new_block(&[]));
            let sel = self.layout_word(LayoutQuery::Target, n);
            let edges = [0, 1].map(|t| SwitchArm {
                value: t as u64,
                edge: self.edge(blocks[t], &[]),
            });
            let otherwise = self.edge(blocks[2], &[]);
            let arms = PoolRange::push(&mut self.b.pools.arms, edges);
            self.end(
                Terminator::Switch {
                    value: sel,
                    arms,
                    otherwise,
                },
                Some(n),
            );
            for (block, value) in blocks.into_iter().zip(values) {
                self.start(block);
                let emitted = self.constant_value(value, n);
                self.goto(join, value.ty, emitted, n);
            }
        }
        (value.ty, self.land(join))
    }

    /// The scrutinee or condition at `n` as a constant expression, on each target.
    fn constant_branch(&self, n: NodeId) -> Option<[crate::ConstValue; 3]> {
        self.p.constant_branches.get(&(self.at, n)).copied()
    }

    /// Leave for `yes` on each target `taken` names and for `no` on the others. The
    /// otherwise edge, which no target takes, keeps both blocks in checked code.
    fn targeted_switch(&mut self, taken: [bool; 3], yes: BlockId, no: BlockId, n: NodeId) {
        let sel = self.layout_word(LayoutQuery::Target, n);
        let edges = [0, 1, 2].map(|t| SwitchArm {
            value: t as u64,
            edge: self.edge(if taken[t] { yes } else { no }, &[]),
        });
        let otherwise = self.edge(if taken == [true; 3] { no } else { yes }, &[]);
        let arms = PoolRange::push(&mut self.b.pools.arms, edges);
        self.end(
            Terminator::Switch {
                value: sel,
                arms,
                otherwise,
            },
            Some(n),
        );
    }

    /// An `if` condition's branch: a constant-form condition keeps only the arm each
    /// target takes in retained code.
    pub(super) fn condition_branch(&mut self, at: NodeId, cv: ValueId, yes: BlockId, no: BlockId) {
        if let Some(values) = self.constant_branch(at)
            && values[0].ty == BOOL
        {
            let taken = values.map(|v| v.data == crate::ConstData::Bits(1));
            return self.targeted_switch(taken, yes, no, at);
        }
        let (yes, no) = (self.edge(yes, &[]), self.edge(no, &[]));
        self.decide(
            Terminator::Branch {
                condition: cv,
                yes,
                no,
            },
            at,
        );
    }

    /// The tests of a `match` arm. On a constant-form scrutinee the pattern is decided
    /// per target, so the arm is selected by target rather than tested.
    pub(super) fn arm_tests(
        &mut self,
        scrutinee: NodeId,
        id: u32,
        subject: Subject,
        no: Option<BlockId>,
    ) {
        let decided = self
            .constant_branch(scrutinee)
            .filter(|values| values[0].ty == subject.ty)
            .and_then(|values| {
                let mut taken = [false; 3];
                for (t, value) in values.into_iter().enumerate() {
                    taken[t] = self.const_admits(id, value)?;
                }
                Some(taken)
            });
        let Some(taken) = decided else {
            return self.pattern_tests(id, subject, no);
        };
        // The last arm takes what the earlier arms leave.
        if let Some(no) = no
            && self.live()
        {
            let yes = self.new_block(&[]);
            self.targeted_switch(taken, yes, no, scrutinee);
            self.start(yes);
        }
    }

    /// Whether pattern `id` admits constant `value`, or None when that is not decided
    /// here (the arm is then tested at run time).
    fn const_admits(&self, id: u32, value: crate::ConstValue) -> Option<bool> {
        if id == 0 {
            return Some(true);
        }
        let pat = self.pat(id);
        match pat.head {
            Head::Any | Head::Bind(..) => Some(true),
            Head::Range(lo, hi) => {
                let crate::ConstData::Bits(bits) = value.data else {
                    return None;
                };
                let TyKind::Scalar(s) = *self.t.kind(value.ty) else {
                    return None;
                };
                if matches!(s, Scalar::F32 | Scalar::F64) {
                    return None;
                }
                let v = crate::constants::integer(s, bits);
                Some(lo <= v && v <= hi)
            }
            Head::Product(case) => {
                let (start, len, held) = match value.data {
                    crate::ConstData::Fields { start, len, case } => (start, len, case),
                    crate::ConstData::Unit => (0, 0, None),
                    _ => return None,
                };
                if case.is_some() && case != held {
                    return Some(false);
                }
                for i in 0..pat.fields.len {
                    if i >= len {
                        return None;
                    }
                    let child = self.patterns.fields[(pat.fields.start + i) as usize];
                    let part = self.p.constant_parts[(start + i) as usize];
                    if !self.const_admits(child, part)? {
                        return Some(false);
                    }
                }
                Some(true)
            }
        }
    }
}
