//! `cell.*` (MEM-29). A `Cell[T]`'s row is `{node: box T}`, so its node is a
//! `Box[T]`'s: `new` admits it as `box.new` does (refusing before `T` is taken), and
//! `into_inner` frees it as `box.unbox` does. The other operations take the handle by
//! view, load the node pointer from it and act only behind it, so no byte the view
//! exposes changes (OWN-8).
use super::*;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CellOp {
    New,
    Get,
    Replace,
    With,
    IntoInner,
}

impl Cx<'_, '_, '_> {
    pub(super) fn runtime_cell(
        &mut self,
        op: CellOp,
        types: &[TyId],
        vals: &[Option<ValueId>],
        result: TyId,
        n: NodeId,
    ) -> Option<ValueId> {
        let item = types[0];
        match op {
            CellOp::New => return self.runtime_new_owner(None, item, vals[0], result, n),
            CellOp::IntoInner => return self.runtime_unbox(item, vals[0]?, n),
            CellOp::Get | CellOp::Replace | CellOp::With => {}
        }
        let id = self.runtime_type(item, n)?;
        let node = self.runtime_handle(vals[0]?, n);
        let payload = self.owner_payload(node, id, false, n)?;
        match op {
            // `T: plain`: the payload's bits are a complete copy.
            CellOp::Get => self.cell_out(item, payload, n),
            // The old payload moves out whole before the new one is installed; nothing
            // between them can fail, so the node always holds a complete `T`.
            CellOp::Replace => {
                let old = self.cell_out(item, payload, n);
                let rep = self.rep_at(item, n)?;
                self.consume(vals[1], n);
                let at = match rep {
                    Rep::Scalar(_) => {
                        let place = rep.stored()?;
                        self.address(AddressBase::Address(payload), &[Step::Field(0)], place, n)
                    }
                    _ => payload,
                };
                self.install(at, rep, vals[1], n);
                old
            }
            // The payload is lent in place as the plain callback's `mut` parameter; the
            // value call takes `a` and owns the result. It steps on the callback's entry
            // unless the callback is a core function, which is not user code (MEM-30),
            // so `log.append` over `cell.with` stays one step.
            CellOp::With => {
                let (a, r) = (types[1], types[2]);
                let pointer = self.pointer(payload, n);
                let params = vec![(Mode::Mut, item), (Mode::Take, a)];
                let callable = self.t.mk(TyKind::Fn(params, r, false, Callable::Plain));
                let entry = if self.core_callback(n) {
                    crate::body::closures::Entry::Core
                } else {
                    crate::body::closures::Entry::Step
                };
                self.call_lowered_entry(callable, vals[2]?, &[Some(pointer), vals[1]], n, entry)
            }
            CellOp::New | CellOp::IntoInner => unreachable!(),
        }
    }

    /// Whether `cell.with`'s callback argument names a core function.
    fn core_callback(&self, n: NodeId) -> bool {
        let Some(&f) = self.kids(n).get(3) else {
            return false;
        };
        matches!(self.call_target(f), Some(&Target::Definition(id))
            if matches!(self.p.by_definition[id.0 as usize],
                Some(Def::Fn(f)) if self.p.fns[f as usize].path.package_is("core", "1")))
    }

    /// The payload at `payload`, moved or copied into a value this body owns.
    fn cell_out(&mut self, item: TyId, payload: AddressId, n: NodeId) -> Option<ValueId> {
        let rep = self.rep_at(item, n)?;
        let value = self.runtime_read(payload, item, n);
        match rep {
            Rep::Place(id) => {
                let to = self.new_slot(PlaceTy::Declared(id), n);
                self.install(to, rep, value, n);
                let value = self.pointer(to, n);
                self.temp(item, Some(value));
                Some(value)
            }
            _ => value,
        }
    }
}
