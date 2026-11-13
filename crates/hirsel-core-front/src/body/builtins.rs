//! Core operations whose definition is the typed value operation itself.
use super::*;

impl<'c, 'a> Cx<'c, 'a> {
    /// None means this operation needs a different kernel. Some(None) is a handled
    /// operation with a unit result or an already reported emission gap.
    pub(super) fn core_value(
        &mut self,
        f: usize,
        types: &[TyId],
        vals: &[Option<ValueId>],
        result: TyId,
        n: NodeId,
    ) -> Option<Option<ValueId>> {
        let g = &self.p.fns[f];
        if !g.path.package_is("core", "1") || g.body.is_some() {
            return None;
        }
        let module = g.path.0.modules.as_slice();
        let name = g.path.0.name.as_str();
        if module == ["list"] {
            match name {
                "new" => return Some(self.empty_list(result, n)),
                "len" | "capacity" => {
                    let value = vals[0]?;
                    let offset = if name == "len" { 8 } else { 16 };
                    return Some(Some(self.read_sequence_word(
                        value,
                        offset,
                        Ty::Scalar(Scalar::U64),
                        n,
                    )));
                }
                _ => {}
            }
        }
        if module == ["core"] && name == "replace" {
            return Some(self.replace_value(types[0], vals[0], vals[1], n));
        }
        if (module == ["option"] || module == ["result"]) && name == "unwrap" {
            let input = subst(&mut self.t, g.params[0].ty, types);
            return Some(self.unwrap_value(input, result, vals[0], module == ["option"], n));
        }
        None
    }

    fn empty_list(&mut self, ty: TyId, n: NodeId) -> Option<ValueId> {
        let id = match self.rep(ty) {
            Ok(Rep::Place(id)) => id,
            Err(what) => {
                self.gap(n, what);
                return None;
            }
            _ => return None,
        };
        let slot = self.new_slot(PlaceTy::Declared(id), n);
        let pointer = self.pointer(slot, n);
        let null = self.konst(Ty::Ptr, 0, n)?;
        let zero = self.konst(Ty::Scalar(Scalar::U64), 0, n)?;
        self.sequence_word(pointer, 0, Ty::Ptr, null, n);
        self.sequence_word(pointer, 8, Ty::Scalar(Scalar::U64), zero, n);
        self.sequence_word(pointer, 16, Ty::Scalar(Scalar::U64), zero, n);
        self.temp(ty, Some(pointer));
        Some(pointer)
    }

    fn sequence_word(&mut self, pointer: ValueId, offset: u64, ty: Ty, value: ValueId, n: NodeId) {
        let offset = self.konst(Ty::Scalar(Scalar::U64), offset, n).unwrap();
        let address = self.address(
            AddressBase::Value(pointer),
            &[Step::Byte(offset)],
            PlaceTy::Value(ty),
            n,
        );
        self.inst(Inst::Store { address, value }, n);
    }

    fn replace_value(
        &mut self,
        ty: TyId,
        pointer: Option<ValueId>,
        value: Option<ValueId>,
        n: NodeId,
    ) -> Option<ValueId> {
        let rep = match self.rep(ty) {
            Ok(rep) => rep,
            Err(what) => {
                self.gap(n, what);
                return None;
            }
        };
        let place = rep.stored()?;
        let address = self.address(AddressBase::Value(pointer?), &[], place, n);
        let old = match rep {
            Rep::Scalar(t) => Some(self.read_bits(address, t, n)),
            Rep::View(_) => Some(self.read_bits(address, Ty::Ptr, n)),
            Rep::Place(_) => Some(self.pointer(address, n)),
            Rep::None => None,
        };
        let saved = self.slot(ty, old, n)?;
        self.install(address, rep, value, n);
        match rep {
            Rep::Scalar(t) => Some(self.read_bits(saved, t, n)),
            Rep::View(_) => Some(self.read_bits(saved, Ty::Ptr, n)),
            Rep::Place(_) => {
                let value = self.pointer(saved, n);
                self.temp(ty, Some(value));
                Some(value)
            }
            Rep::None => None,
        }
    }

    fn unwrap_value(
        &mut self,
        input: TyId,
        output: TyId,
        value: Option<ValueId>,
        option: bool,
        n: NodeId,
    ) -> Option<ValueId> {
        let value = value?;
        let id = match self.rep(input) {
            Ok(Rep::Place(id)) => id,
            Err(what) => {
                self.gap(n, what);
                return None;
            }
            _ => return None,
        };
        let address = self.address(AddressBase::Value(value), &[], PlaceTy::Declared(id), n);
        let tag = self.value(Ty::Scalar(Scalar::U32));
        self.inst(
            Inst::Case {
                result: tag,
                address,
            },
            n,
        );
        let success = u32::from(option);
        let expected = self.konst(Ty::Scalar(Scalar::U32), u64::from(success), n)?;
        let on = self.value(Ty::Scalar(Scalar::Bool));
        self.inst(
            Inst::Compare {
                result: on,
                op: CompareOp::Eq,
                left: tag,
                right: expected,
            },
            n,
        );
        let yes = self.new_block(&[]);
        let no = self.new_block(&[]);
        let ye = self.edge(yes, &[]);
        let ne = self.edge(no, &[]);
        self.end(
            Terminator::Branch {
                condition: on,
                yes: ye,
                no: ne,
            },
            Some(n),
        );
        self.start(no);
        let cleanup = self.fail_to(n);
        self.end(
            Terminator::Raise {
                cause: hirsel_core_ir::RaiseCause::Unwrap,
                cleanup,
            },
            Some(n),
        );
        self.start(yes);
        let rep = match self.rep(output) {
            Ok(rep) => rep,
            Err(what) => {
                self.gap(n, what);
                return None;
            }
        };
        self.consume(Some(value), n);
        let payload = self.address(
            AddressBase::Address(address),
            &[Step::CaseField {
                case: success,
                field: 0,
            }],
            rep.stored()?,
            n,
        );
        match rep {
            Rep::Scalar(t) => Some(self.read_bits(payload, t, n)),
            Rep::View(_) => Some(self.read_bits(payload, Ty::Ptr, n)),
            Rep::Place(id) => {
                let source = self.pointer(payload, n);
                let slot = self.new_slot(PlaceTy::Declared(id), n);
                self.copy_to(slot, source, id, n);
                let value = self.pointer(slot, n);
                self.temp(output, Some(value));
                Some(value)
            }
            Rep::None => None,
        }
    }
}
