//! Constant-stack key glue for recursive types (FAIL-17).
//!
//! Equality, ordering and hash folding of a type that reaches itself through a
//! pointer do not recurse through glue calls: the row's body walks every type of the
//! recursive component in one machine. Each member type has a start block taking the
//! two places; a field that leads back into the component jumps to its member's start,
//! after pushing a continuation (the two owner places, a resume number and an index) on
//! the context's glue worklist (`rt.glue_push`). When a node is done, `dispatch` pops
//! the next continuation and switches to its resume block; an empty worklist, down to
//! this row's mark, ends the walk. The last field of a node needs no continuation, so a
//! chain linked through its last field never touches the worklist. An early answer
//! (`==` false, `<` decided) cuts the worklist back to the mark.
//!
//! The worklist is uncharged: `==` never fails with `Capacity` (FAIL-17's ruling).
use super::*;

#[derive(Clone, Copy, PartialEq)]
enum Walk {
    Equal,
    Order,
    Fold,
}

struct Machine {
    walk: Walk,
    members: Vec<TypeId>,
    starts: Vec<BlockId>,
    resumes: Vec<BlockId>,
    dispatch: BlockId,
    /// `no` for Equal (no params), `different` for Order (one i8).
    fail: Option<BlockId>,
    /// The hash state of Fold.
    state: ValueId,
}

/// The types directly named by `field`, through sequences, arrays and views.
fn named(field: &FieldTy, out: &mut Vec<TypeId>) {
    match field {
        FieldTy::Inline(t)
        | FieldTy::Box(t)
        | FieldTy::Rc(t)
        | FieldTy::Arc(t)
        | FieldTy::Shared(t)
        | FieldTy::RegionRef(t) => out.push(TypeId(t.0)),
        FieldTy::Seq(item) | FieldTy::Array(item, _) | FieldTy::View(item) => named(item, out),
        _ => {}
    }
}

fn edges(module: &Module, ty: TypeId) -> Vec<TypeId> {
    let mut out = vec![];
    match &module.types[ty.index()].body {
        ShapeBody::Record(fs) => fs.iter().for_each(|f| named(&f.ty, &mut out)),
        ShapeBody::Variant(cs) => cs
            .iter()
            .flat_map(|c| c.fields.iter())
            .for_each(|f| named(&f.ty, &mut out)),
    }
    out
}

/// The types reachable from `ty` through one or more fields.
fn reach(module: &Module, ty: TypeId) -> Vec<bool> {
    let mut seen = vec![false; module.types.len()];
    let mut work = edges(module, ty);
    while let Some(t) = work.pop() {
        if !std::mem::replace(&mut seen[t.index()], true) {
            work.extend(edges(module, t));
        }
    }
    seen
}

/// The recursive component of `ty`: every type it reaches that reaches it back, `ty`
/// first. Empty when `ty` does not reach itself.
pub(super) fn component(module: &Module, ty: TypeId) -> Vec<TypeId> {
    let from = reach(module, ty);
    if !from[ty.index()] {
        return vec![];
    }
    let mut members = vec![ty];
    for (i, _) in from.iter().enumerate().filter(|(_, r)| **r) {
        let u = TypeId(i as u32);
        if u != ty && reach(module, u)[ty.index()] {
            members.push(u);
        }
    }
    members
}

impl Derive<'_> {
    /// The body of `op` (Equal, Order or Fold) at the recursive `ty`, from the row's
    /// raw parameters. Returns the result to return; the current block is open.
    pub(super) fn walk(
        &mut self,
        c: &mut Code,
        ty: TypeId,
        op: KeyOp,
        ps: [ValueId; 2],
    ) -> Result<Option<ValueId>, &'static str> {
        let walk = match op {
            KeyOp::Equal => Walk::Equal,
            KeyOp::Order => Walk::Order,
            KeyOp::Fold => Walk::Fold,
            _ => unreachable!("only Equal, Order and Fold walk"),
        };
        let members = component(self.module, ty);
        let mark = self.runtime(c, RtOp::GlueMark, &[]).unwrap();
        let fail = match walk {
            Walk::Equal => Some(c.block(&[])),
            Walk::Order => Some(c.block(&[Ty::Scalar(Scalar::I8)])),
            Walk::Fold => None,
        };
        let finish = c.block(&[]);
        let dispatch = c.block(&[]);
        // A failure inside the walk (a depth step, MEM-30, or a glue call it makes)
        // cuts the worklist back to the mark, then leaves failing.
        let stop = c.block(&[]);
        c.unwind = Some(stop);
        let starts = members
            .iter()
            .map(|_| c.block(&[Ty::Ptr, Ty::Ptr]))
            .collect();
        let (b, state) = match walk {
            Walk::Fold => (ps[0], ps[1]),
            _ => (ps[1], ps[0]),
        };
        let mut m = Machine {
            walk,
            members,
            starts,
            resumes: vec![],
            dispatch,
            fail,
            state,
        };
        c.jump(m.starts[0], &[ps[0], b]);
        for i in 0..m.members.len() {
            c.start(m.starts[i]);
            let [a, b] = c.params(m.starts[i]);
            // MEM-30 depth steps: one step per node the walk visits.
            c.inst(Inst::Step { fail: Some(stop) });
            let u = m.members[i];
            self.walk_type(c, &mut m, u, a, b)?;
        }
        c.start(dispatch);
        if m.resumes.is_empty() {
            c.jump(finish, &[]);
        } else {
            let e = self.runtime(c, RtOp::GluePop, &[mark]).unwrap();
            let null = c.value(Ty::Ptr);
            c.inst(Inst::Const {
                result: null,
                bits: 0,
            });
            let empty = c.compare(CompareOp::Eq, e, null);
            let popped = c.block(&[]);
            c.branch(empty, finish, popped);
            c.start(popped);
            let w0 = c.read(e, Ty::Ptr);
            let at = c.offset(e, 8);
            let w1 = c.read(at, Ty::Ptr);
            let at = c.offset(e, 16);
            let k = c.read(at, Ty::Scalar(Scalar::U64));
            let at = c.offset(e, 24);
            let w3 = c.read(at, Ty::Scalar(Scalar::U64));
            let mut arms = vec![];
            for (i, &r) in m.resumes.iter().enumerate() {
                arms.push(hirsel_core_ir::SwitchArm {
                    value: i as u64,
                    edge: c.edge(r, &[w0, w1, w3]),
                });
            }
            let otherwise = arms.last().unwrap().edge;
            let range = PoolRange::push(
                &mut c.body.pools.arms,
                arms[..arms.len() - 1].iter().copied(),
            );
            c.end(Terminator::Switch {
                value: k,
                arms: range,
                otherwise,
            });
        }
        c.unwind = None;
        c.start(stop);
        self.runtime(c, RtOp::GlueCut, &[mark]);
        c.end(Terminator::Propagate);
        c.start(finish);
        Ok(match walk {
            Walk::Fold => None,
            Walk::Equal => {
                let yes = c.boolean(true);
                let returns = PoolRange::push(&mut c.body.pools.values, [yes]);
                c.end(Terminator::Return(returns));
                c.start(fail.unwrap());
                self.runtime(c, RtOp::GlueCut, &[mark]);
                Some(c.boolean(false))
            }
            Walk::Order => {
                let zero = c.typed_constant(Scalar::I8, 0);
                c.jump(fail.unwrap(), &[zero]);
                c.start(fail.unwrap());
                self.runtime(c, RtOp::GlueCut, &[mark]);
                Some(c.params(fail.unwrap())[0])
            }
        })
    }

    /// The node of member `u` at places `a` and `b`. Every path ends in `dispatch` or
    /// in a member's start.
    fn walk_type(
        &mut self,
        c: &mut Code,
        m: &mut Machine,
        u: TypeId,
        mut a: ValueId,
        mut b: ValueId,
    ) -> Result<(), &'static str> {
        // As at the row's entry: a region value's place holds its block pointer.
        if self.module.types[u.index()].repr == LRepr::Region {
            a = c.read(a, Ty::Ptr);
            b = if m.walk == Walk::Fold {
                a
            } else {
                c.read(b, Ty::Ptr)
            };
        }
        if m.walk == Walk::Order {
            if matches!(self.recipes[u.index()].layout, Layout::Compact(_)) {
                let target = FieldTy::Inline(TypeRef(u.0));
                self.order_located(c, &target, a, b, m.fail.unwrap())?;
                c.jump(m.dispatch, &[]);
                return Ok(());
            }
            if super::super::ordered_codec::is_desc_row(&self.module.types[u.index()].name) {
                (a, b) = (b, a);
            }
        }
        match self.module.types[u.index()].body.clone() {
            ShapeBody::Record(fs) => {
                let fields = fs.into_iter().map(|f| f.ty).collect::<Vec<_>>();
                self.walk_fields(c, m, u, None, &fields, a, b)
            }
            ShapeBody::Variant(cs) => {
                let mut tags = [ValueId(0); 2];
                for (i, p) in [a, b].into_iter().enumerate() {
                    if i == 1 && m.walk == Walk::Fold {
                        tags[1] = tags[0];
                        break;
                    }
                    let at = c.address(p, PlaceTy::Declared(u));
                    tags[i] = c.value(Ty::Scalar(Scalar::U32));
                    c.inst(Inst::Case {
                        result: tags[i],
                        address: at,
                    });
                }
                match m.walk {
                    Walk::Equal => {
                        let on = c.compare(CompareOp::Eq, tags[0], tags[1]);
                        c.require(on, m.fail.unwrap());
                    }
                    Walk::Order => self.order_scalars(c, tags[0], tags[1], m.fail.unwrap()),
                    Walk::Fold => {
                        let word = c.widen(tags[0]);
                        self.runtime(c, RtOp::MapHashAddWord, &[m.state, word]);
                    }
                }
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
                for (i, case) in cs.into_iter().enumerate() {
                    c.start(c.body.edges[arms[i].edge.index()].target);
                    let fields = case.fields.into_iter().map(|f| f.ty).collect::<Vec<_>>();
                    self.walk_fields(c, m, u, Some(i as u32), &fields, a, b)?;
                }
                Ok(())
            }
        }
    }

    /// The fields of one record or case of `u` in order, then `dispatch`. A field that
    /// leads into the component descends; the last one descends with no continuation.
    #[allow(clippy::too_many_arguments)]
    fn walk_fields(
        &mut self,
        c: &mut Code,
        m: &mut Machine,
        u: TypeId,
        case: Option<u32>,
        fields: &[FieldTy],
        mut a: ValueId,
        mut b: ValueId,
    ) -> Result<(), &'static str> {
        for (j, field) in fields.iter().enumerate() {
            let x = c.field_pointer(self.module, a, u, case, j as u32);
            let y = if m.walk == Walk::Fold {
                x
            } else {
                c.field_pointer(self.module, b, u, case, j as u32)
            };
            if !self.leads_in(m, field) {
                self.field_once(c, m, field, x, y)?;
                continue;
            }
            if j + 1 == fields.len() {
                self.walk_field(c, m, field, x, y, None)?;
                return Ok(());
            }
            let zero = c.constant(0);
            let [ra, rb, _] = self
                .walk_field(c, m, field, x, y, Some([a, b, zero]))?
                .unwrap();
            (a, b) = (ra, rb);
        }
        c.jump(m.dispatch, &[]);
        Ok(())
    }

    /// Whether `field` names a member of the component.
    fn leads_in(&self, m: &Machine, field: &FieldTy) -> bool {
        let mut out = vec![];
        named(field, &mut out);
        out.iter().any(|t| m.members.contains(t))
    }

    /// The existing per-field glue of a field outside the component.
    fn field_once(
        &mut self,
        c: &mut Code,
        m: &Machine,
        field: &FieldTy,
        x: ValueId,
        y: ValueId,
    ) -> Result<(), &'static str> {
        match m.walk {
            Walk::Equal => self.equal_field(c, field, x, y, m.fail.unwrap()),
            Walk::Order => self.order_field(c, field, x, y, m.fail.unwrap()),
            Walk::Fold => self.fold_field(c, field, x, m.state),
        }
    }

    /// Descend into `field`, which leads into the component. With `then`, push it as a
    /// continuation and return the resume block's parameters (codegen continues there);
    /// without, the path ends.
    fn walk_field(
        &mut self,
        c: &mut Code,
        m: &mut Machine,
        field: &FieldTy,
        x: ValueId,
        y: ValueId,
        then: Option<[ValueId; 3]>,
    ) -> Result<Option<[ValueId; 3]>, &'static str> {
        let fold = m.walk == Walk::Fold;
        match field {
            FieldTy::Inline(t) | FieldTy::RegionRef(t) => {
                Ok(self.descend(c, m, TypeId(t.0), x, y, then))
            }
            FieldTy::Box(t) | FieldTy::Rc(t) | FieldTy::Arc(t) | FieldTy::Shared(t) => {
                let mut px = c.read(x, Ty::Ptr);
                let mut py = if fold { px } else { c.read(y, Ty::Ptr) };
                if !matches!(field, FieldTy::Box(_)) {
                    px = c.offset(px, 8);
                    py = if fold { px } else { c.offset(py, 8) };
                }
                Ok(self.descend(c, m, TypeId(t.0), px, py, then))
            }
            FieldTy::View(inner) => self.walk_field(c, m, inner, x, y, then),
            FieldTy::Seq(item) | FieldTy::Array(item, _) => {
                let array = match field {
                    FieldTy::Array(_, n) => Some(*n),
                    _ => None,
                };
                self.walk_sequence(c, m, item, array, x, y, then)
            }
            _ => unreachable!("only pointer, inline and sequence fields name a member"),
        }
    }

    /// Push `then` (if any) and jump to the start of member `t` at `x`, `y`.
    fn descend(
        &mut self,
        c: &mut Code,
        m: &mut Machine,
        t: TypeId,
        x: ValueId,
        y: ValueId,
        then: Option<[ValueId; 3]>,
    ) -> Option<[ValueId; 3]> {
        let k = m.members.iter().position(|&u| u == t).unwrap();
        let resume = then.map(|[a, b, i]| self.push(c, m, a, b, i));
        c.jump(m.starts[k], &[x, y]);
        resume.map(|r| Self::resume(c, r))
    }

    fn push(
        &mut self,
        c: &mut Code,
        m: &mut Machine,
        a: ValueId,
        b: ValueId,
        i: ValueId,
    ) -> BlockId {
        let r = c.block(&[Ty::Ptr, Ty::Ptr, Ty::Scalar(Scalar::U64)]);
        let k = c.constant(m.resumes.len() as u64);
        m.resumes.push(r);
        self.runtime(c, RtOp::GluePush, &[a, b, k, i]);
        r
    }

    fn resume(c: &mut Code, r: BlockId) -> [ValueId; 3] {
        c.start(r);
        let ps = c.body.blocks[r.index()]
            .params
            .get(&c.body.pools.values)
            .unwrap();
        [ps[0], ps[1], ps[2]]
    }

    /// A list or array whose items lead into the component. The loop head takes the
    /// two headers and the index, so a resumed item visit reloads data and length.
    #[allow(clippy::too_many_arguments)]
    fn walk_sequence(
        &mut self,
        c: &mut Code,
        m: &mut Machine,
        item: &FieldTy,
        array: Option<u64>,
        x: ValueId,
        y: ValueId,
        then: Option<[ValueId; 3]>,
    ) -> Result<Option<[ValueId; 3]>, &'static str> {
        let fold = m.walk == Walk::Fold;
        let length = |c: &mut Code, header: ValueId| match array {
            Some(n) => c.constant(n),
            None => {
                let at = c.offset(header, 8);
                c.read(at, Ty::Scalar(Scalar::U64))
            }
        };
        match m.walk {
            Walk::Equal if array.is_none() => {
                let la = length(c, x);
                let lb = length(c, y);
                let on = c.compare(CompareOp::Eq, la, lb);
                c.require(on, m.fail.unwrap());
            }
            Walk::Fold => {
                let len = length(c, x);
                self.runtime(c, RtOp::MapHashAddWord, &[m.state, len]);
            }
            _ => {}
        }
        let owner = then.map(|[a, b, i]| self.push(c, m, a, b, i));
        let index = Ty::Scalar(Scalar::U64);
        let head = c.block(&[Ty::Ptr, Ty::Ptr, index]);
        let body = c.block(&[]);
        let done = c.block(&[]);
        let zero = c.constant(0);
        c.jump(head, &[x, y, zero]);
        let [ha, hb, i] = Self::resume(c, head);
        let data = |c: &mut Code, header: ValueId| match array {
            Some(_) => header,
            None => c.read(header, Ty::Ptr),
        };
        let da = data(c, ha);
        let la = length(c, ha);
        let (db, lb) = if fold {
            (da, la)
        } else {
            (data(c, hb), length(c, hb))
        };
        let more = c.compare(CompareOp::Lt, i, la);
        if m.walk == Walk::Order {
            let check = c.block(&[]);
            c.branch(more, check, done);
            c.start(check);
            let more = c.compare(CompareOp::Lt, i, lb);
            c.branch(more, body, done);
        } else {
            c.branch(more, body, done);
        }
        c.start(body);
        let stride = c.field_size(item);
        let offset = c.mul(i, stride);
        let px = c.offset_value(da, offset);
        let py = if fold { px } else { c.offset_value(db, offset) };
        let one = c.constant(1);
        let next = c.binary(BinaryOp::Add, i, one);
        let back = self
            .walk_field(c, m, item, px, py, Some([ha, hb, next]))?
            .unwrap();
        c.jump(head, &back);
        c.start(done);
        if m.walk == Walk::Order {
            self.order_scalars(c, la, lb, m.fail.unwrap());
        }
        c.jump(m.dispatch, &[]);
        Ok(owner.map(|r| Self::resume(c, r)))
    }
}
