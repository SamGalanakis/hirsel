//! SEM-7 equality and order of values carried as places: tuples, records and variants
//! (Option and Result among them), and a view by its payload. Order is lexicographic,
//! fields in declaration order, variants by declaration ordinal then payload. The
//! comparison walks both places once, leaving at the first component that differs, with
//! the ordering -1, 0 or 1 as the join's parameter; `l op r` is then `ordering op 0`.
use super::*;

impl<'c, 'a> Cx<'c, 'a, '_> {
    /// `l op r` for two values of type `t`, each a pointer to its place (or, for a view,
    /// to its referent). Err names the component type not built yet; nothing is emitted
    /// then.
    pub(super) fn compare_places(
        &mut self,
        t: TyId,
        op: CompareOp,
        l: Option<ValueId>,
        r: Option<ValueId>,
        n: NodeId,
    ) -> Result<Option<ValueId>, &'static str> {
        let t = match self.t.kind(t) {
            TyKind::View(x) => *x,
            _ => t,
        };
        self.comparable(t, 0)?;
        let (Some(l), Some(r)) = (l, r) else {
            return Ok(None);
        };
        if !self.live() {
            return Ok(None);
        }
        let Some(place) = self.place_of(t) else {
            // Unit components: always equal.
            let eq = matches!(op, CompareOp::Eq | CompareOp::Le | CompareOp::Ge);
            return Ok(self.konst(Ty::Scalar(Scalar::Bool), u64::from(eq), n));
        };
        let eq_only = matches!(op, CompareOp::Eq | CompareOp::Ne);
        let ord = Ty::Scalar(Scalar::I8);
        let join = self.new_block(&[ord]);
        let a = self.address(AddressBase::Value(l), &[], place, n);
        let b = self.address(AddressBase::Value(r), &[], place, n);
        self.walk(a, b, t, join, eq_only, n);
        let zero = self.konst(ord, 0, n);
        if let Some(zero) = zero {
            self.jump(join, &[zero], n);
        }
        self.start(join);
        let ordering = self.block_params(join)[0];
        let zero = self.konst(ord, 0, n);
        let result = self.value(Ty::Scalar(Scalar::Bool));
        if let Some(zero) = zero {
            self.inst(
                Inst::Compare {
                    result,
                    op,
                    left: ordering,
                    right: zero,
                },
                n,
            );
        }
        Ok(Some(result))
    }

    /// Ok when every component of `t` is built: scalars, unit, tuples, records and
    /// variants of them, and views of those.
    fn comparable(&mut self, t: TyId, depth: u32) -> Result<(), &'static str> {
        const NOT: &str = "comparison of this type is not built";
        if depth > 64 {
            return Err(NOT);
        }
        match self.t.kind(t).clone() {
            TyKind::Scalar(_) | TyKind::Unit => Ok(()),
            TyKind::Tuple(xs) => xs.iter().try_for_each(|&x| self.comparable(x, depth + 1)),
            TyKind::View(x) => self.comparable(x, depth + 1),
            TyKind::Adt(d, args) => {
                let decl = &self.p.decls[d.index()];
                if decl.role.is_some() {
                    return Err(NOT);
                }
                let fields: Vec<TyId> = match &decl.body {
                    DeclBody::Record(fs) => fs.iter().map(|f| f.ty).collect(),
                    DeclBody::Variant(cs) => cs
                        .iter()
                        .flat_map(|c| c.fields.iter().map(|f| f.ty))
                        .collect(),
                    DeclBody::Alias(_) => return Err(NOT),
                };
                self.rep(t).map(|_| ()).map_err(|_| NOT)?;
                for f in fields {
                    let f = subst(&mut self.t, f, &args);
                    self.comparable(f, depth + 1)?;
                }
                Ok(())
            }
            _ => Err(NOT),
        }
    }

    /// The place a value of `t` occupies; None for unit.
    fn place_of(&mut self, t: TyId) -> Option<PlaceTy> {
        match self.rep(t).ok()? {
            Rep::None => None,
            Rep::Scalar(s) => Some(PlaceTy::Value(s)),
            Rep::Place(id) => Some(PlaceTy::Declared(id)),
            Rep::View(_) => Some(PlaceTy::Value(Ty::Ptr)),
        }
    }

    /// Compare the places `a` and `b` of type `t`; a difference jumps to `join` with
    /// the ordering, equality falls through in the current block.
    fn walk(
        &mut self,
        a: AddressId,
        b: AddressId,
        t: TyId,
        join: BlockId,
        eq_only: bool,
        n: NodeId,
    ) {
        if !self.live() {
            return;
        }
        match self.t.kind(t).clone() {
            TyKind::Unit => {}
            TyKind::Scalar(s) => {
                let (x, y) = (
                    self.read_bits(a, Ty::Scalar(s), n),
                    self.read_bits(b, Ty::Scalar(s), n),
                );
                self.scalars(x, y, join, eq_only, n);
            }
            TyKind::View(x) => {
                let (pa, pb) = (self.read_bits(a, Ty::Ptr, n), self.read_bits(b, Ty::Ptr, n));
                let Some(place) = self.place_of(x) else {
                    return;
                };
                let a = self.address(AddressBase::Value(pa), &[], place, n);
                let b = self.address(AddressBase::Value(pb), &[], place, n);
                self.walk(a, b, x, join, eq_only, n);
            }
            TyKind::Tuple(xs) => {
                for (i, x) in xs.into_iter().enumerate() {
                    self.component(a, b, Step::Field(i as u32), x, join, eq_only, n);
                }
            }
            TyKind::Adt(d, args) => match self.p.decls[d.index()].body.clone() {
                DeclBody::Record(fs) => {
                    for (i, f) in fs.iter().enumerate() {
                        let x = subst(&mut self.t, f.ty, &args);
                        self.component(a, b, Step::Field(i as u32), x, join, eq_only, n);
                    }
                }
                DeclBody::Variant(cs) => {
                    let u32t = Ty::Scalar(Scalar::U32);
                    let (ta, tb) = (self.value(u32t), self.value(u32t));
                    self.inst(
                        Inst::Case {
                            result: ta,
                            address: a,
                        },
                        n,
                    );
                    self.inst(
                        Inst::Case {
                            result: tb,
                            address: b,
                        },
                        n,
                    );
                    self.scalars(ta, tb, join, eq_only, n);
                    let after = self.new_block(&[]);
                    for (k, c) in cs.iter().enumerate() {
                        let tys: Vec<TyId> = c
                            .fields
                            .iter()
                            .map(|f| subst(&mut self.t, f.ty, &args))
                            .collect();
                        if tys.iter().all(|&x| self.place_of(x).is_none()) {
                            continue;
                        }
                        let Some(kv) = self.konst(u32t, k as u64, n) else {
                            return;
                        };
                        let is = self.value(Ty::Scalar(Scalar::Bool));
                        self.inst(
                            Inst::Compare {
                                result: is,
                                op: CompareOp::Eq,
                                left: ta,
                                right: kv,
                            },
                            n,
                        );
                        let (yes, next) = (self.new_block(&[]), self.new_block(&[]));
                        let (ye, ne) = (self.edge(yes, &[]), self.edge(next, &[]));
                        self.end(
                            Terminator::Branch {
                                condition: is,
                                yes: ye,
                                no: ne,
                            },
                            Some(n),
                        );
                        self.start(yes);
                        for (j, x) in tys.into_iter().enumerate() {
                            let step = Step::CaseField {
                                case: k as u32,
                                field: j as u32,
                            };
                            self.component(a, b, step, x, join, eq_only, n);
                        }
                        self.jump(after, &[], n);
                        self.start(next);
                    }
                    self.jump(after, &[], n);
                    self.start(after);
                }
                DeclBody::Alias(_) => unreachable!("refused by comparable"),
            },
            _ => unreachable!("refused by comparable"),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn component(
        &mut self,
        a: AddressId,
        b: AddressId,
        step: Step,
        x: TyId,
        join: BlockId,
        eq_only: bool,
        n: NodeId,
    ) {
        let Some(place) = self.place_of(x) else {
            return;
        };
        let fa = self.address(AddressBase::Address(a), &[step], place, n);
        let fb = self.address(AddressBase::Address(b), &[step], place, n);
        self.walk(fa, fb, x, join, eq_only, n);
    }

    /// Two scalars: unequal jumps to `join` with 1 (or, ordered, -1 when `x < y`).
    fn scalars(&mut self, x: ValueId, y: ValueId, join: BlockId, eq_only: bool, n: NodeId) {
        let ord = Ty::Scalar(Scalar::I8);
        let tests: &[(CompareOp, u64)] = if eq_only {
            &[(CompareOp::Ne, 1)]
        } else {
            &[(CompareOp::Lt, 0xff), (CompareOp::Gt, 1)]
        };
        for &(op, bits) in tests {
            let Some(k) = self.konst(ord, bits, n) else {
                return;
            };
            let c = self.value(Ty::Scalar(Scalar::Bool));
            self.inst(
                Inst::Compare {
                    result: c,
                    op,
                    left: x,
                    right: y,
                },
                n,
            );
            let next = self.new_block(&[]);
            let (ye, ne) = (self.edge(join, &[k]), self.edge(next, &[]));
            self.end(
                Terminator::Branch {
                    condition: c,
                    yes: ye,
                    no: ne,
                },
                Some(n),
            );
            self.start(next);
        }
    }
}
