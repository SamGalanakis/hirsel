//! Stack backing for small String copies in immutable locals. A local or its
//! directly constructed record may only be passed to named view parameters.
//! The existing lent-frame record lets ordinary cleanup find the original payer.

use super::*;
use hirsel_core_layout::{FRAME_LIST_BYTES, FRAME_LIST_LINK_BYTES};

impl<'c, 'a> Cx<'c, 'a, '_> {
    fn frame_string_length(&self, n: NodeId) -> Option<u64> {
        if self.kind(n) != NodeKind::Call {
            return None;
        }
        let &[callee, argument] = self.kids(n) else {
            return None;
        };
        let f = self.named_callee(callee)?;
        if !self.p.fns[f].path.is_core("string.from")
            || self.kind(argument) != NodeKind::Literal
            || self.token_kind(argument) != Some(TokenKind::String)
        {
            return None;
        }
        let len = values::quoted_bytes(self.token_text(argument), false)?.len() as u64;
        (len != 0 && len <= FRAME_LIST_BYTES).then_some(len)
    }

    pub(super) fn frame_string_let(&mut self, s: NodeId, pat: NodeId, value: NodeId) {
        if self.kind(s) != NodeKind::Let
            || self.kind(pat) != NodeKind::BindPattern
            || self.kind(value) != NodeKind::Call
        {
            return;
        }
        let Some(binder) = self.binder(pat) else {
            return;
        };
        let calls = if let Some(len) = self.frame_string_length(value) {
            vec![(value, len)]
        } else {
            let [callee, fields @ ..] = self.kids(value) else {
                return;
            };
            let Some(Target::Definition(d)) = self.call_target(*callee) else {
                return;
            };
            let Some(Def::Type(d)) = self.p.by_definition[d.0 as usize] else {
                return;
            };
            let decl = &self.p.decls[d.index()];
            if decl.repr != crate::decls::Repr::Own || !matches!(decl.body, DeclBody::Record(_)) {
                return;
            }
            fields
                .iter()
                .filter(|&&field| self.kind(field) == NodeKind::LabelledArgument)
                .filter_map(|&field| {
                    let call = self.kids(field)[0];
                    self.frame_string_length(call).map(|len| (call, len))
                })
                .collect()
        };
        if calls.is_empty() || !self.frame_string_view_only(binder) {
            return;
        }
        self.frame_string_copies.extend(calls);
    }

    fn frame_string_view_only(&self, binder: u32) -> bool {
        let info = &self.p.items[self.at.module as usize][self.at.item as usize];
        let mut uses = HashSet::default();
        for name in &info.resolution.names {
            if name.target == Target::Binder(binder) {
                if !name.projections.is_empty() {
                    return false;
                }
                uses.insert(name.node);
            }
        }
        let Some(body) = self.fd.body else {
            return false;
        };
        let mut pending = vec![body];
        while let Some(n) = pending.pop() {
            if uses.is_empty() {
                return true;
            }
            if matches!(self.kind(n), NodeKind::Closure | NodeKind::ScopedClosure) {
                continue;
            }
            pending.extend(self.kids(n));
            if self.kind(n) != NodeKind::Call {
                continue;
            }
            let [callee, args @ ..] = self.kids(n) else {
                continue;
            };
            if !args.iter().any(|arg| uses.contains(arg)) {
                continue;
            }
            let Some(f) = self.named_callee(*callee) else {
                continue;
            };
            for (&arg, param) in args.iter().zip(&self.p.fns[f].params) {
                if param.mode == Mode::View && !param.bytes {
                    uses.remove(&arg);
                }
            }
        }
        uses.is_empty()
    }

    pub(super) fn frame_string_copy(
        &mut self,
        source: ValueId,
        len: u64,
        ty: TyId,
        n: NodeId,
    ) -> Option<ValueId> {
        let id = self.runtime_type(ty, n)?;
        let to = self.result_slot(PlaceTy::Declared(id), n);
        let storage = self.t.mk(TyKind::Array(
            scalar(Scalar::U64),
            (FRAME_LIST_BYTES + FRAME_LIST_LINK_BYTES) / 8,
        ));
        let row = self.place_row(storage, "frame String backing without a place", n)?;
        let slot = self.entry_slot(PlaceTy::Declared(row), n);
        let frame = self.pointer(slot, n);
        let u = Ty::Scalar(Scalar::U64);
        let length = self.konst(u, len, n)?;
        let charge = self.konst(u, len.next_multiple_of(8), n)?;
        let null = self.konst(Ty::Ptr, 0, n)?;
        let block = self.value(Ty::Ptr);
        let available = self.value(u);
        self.inst(
            Inst::Alloc {
                result: block,
                available,
                region_entry: None,
                kind: AllocKind::own(FieldTy::Seq(Box::new(FieldTy::Scalar(Scalar::U8)))),
                place: AllocPlace::LentFrame(frame),
                charge,
                overflow: None,
                size: length,
                align: 8,
            },
            n,
        );
        let admitted = self.list_compare(CompareOp::Ne, block, null, n);
        self.raise_unless(admitted, charge, available, n);

        let data = self.byte_word(AddressBase::Value(source), 0, Ty::Ptr, n)?;
        let data = self.read_bits(data, Ty::Ptr, n);
        let byte = PlaceTy::Value(Ty::Scalar(Scalar::U8));
        let from = self.address(AddressBase::Value(data), &[], byte, n);
        let dst = self.address(AddressBase::Value(block), &[], byte, n);
        self.inst(
            Inst::Copy {
                to: dst,
                from,
                bytes: length,
            },
            n,
        );
        for (index, ty, value) in [(0, Ty::Ptr, block), (1, u, length), (2, u, length)] {
            let address = self.byte_word(AddressBase::Address(to), index, ty, n)?;
            self.inst(Inst::Store { address, value }, n);
        }
        let value = self.pointer(to, n);
        self.temp(ty, Some(value));
        Some(value)
    }
}
