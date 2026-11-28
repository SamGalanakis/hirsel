use super::*;

impl Cx<'_, '_, '_> {
    /// A call of `std.codec.of` (SEM-10, EXPANSION 9.1) whose T is not a parameter: a
    /// call of T's codec maker, std's for a scalar, String, List, Map, Option or Result. At a type parameter it is None, and the call stays a
    /// call of `of`'s instance row, which substitution replaces by the instance's maker.
    pub(super) fn codec_of(
        &mut self,
        path: &DefPath,
        types: &[TyId],
        vals: &[Option<ValueId>],
        result: TyId,
        n: NodeId,
    ) -> Option<Option<ValueId>> {
        let mut ty = types[0];
        while let TyKind::Anchored(x, _) | TyKind::View(x) = *self.t.kind(ty) {
            ty = x;
        }
        if let TyKind::Param(_) = self.t.kind(ty) {
            return None;
        }
        // A declared codec type's maker is the derived `<Name>$codec` at its arguments.
        if let TyKind::Adt(d, args) = self.t.kind(ty).clone()
            && let Some(maker) = self.p.decls[d.index()].codec().and_then(|c| c.maker)
        {
            return Some(self.emit_call(maker as usize, &args, vals, result, n));
        }
        // A core List, Map, Option or Result's maker is std's `of_list` (`of_bytes` at u8) and the rest at
        // its arguments, when theirs are built.
        if let TyKind::Adt(d, args) = self.t.kind(ty).clone()
            && let Some((maker, args)) = self.p.structural_codecs().maker(&self.t, d, &args)
        {
            if !self.codec_built(ty) {
                self.gap(n, "the codec of this type is not built");
                return Some(None);
            }
            return Some(self.emit_call(maker as usize, &args, vals, result, n));
        }
        // A tuple's maker is the one the front derives, its row's codec hook.
        if let TyKind::Tuple(_) = self.t.kind(ty) {
            let Some(row) = self.e.type_row(self.p, &mut self.t, ty) else {
                self.gap(n, "codec type layout is not built");
                return Some(None);
            };
            self.e.tuple_codec(self.p, &mut self.t, ty, row);
            let maker = self.e.module.types[row.index()].codec;
            return Some(match maker {
                Some(maker) if self.codec_built(ty) => self.call_maker(FuncId(maker.0), result, n),
                _ => {
                    self.gap(n, "the codec of this type is not built");
                    None
                }
            });
        }
        // A sibling read of `of` (`crate::SIBLING_READS`).
        let name = crate::decls::layout_name(&self.p.decls, &self.t, ty);
        let maker = templates::builtin_codec(&name).and_then(|maker| {
            let mut path = path.clone();
            path.0.name = maker.into();
            self.p.function_id(&path)
        });
        let Some(maker) = maker else {
            self.gap(n, "the codec of this type is not built");
            return Some(None);
        };
        Some(self.emit_call(maker, &[], vals, result, n))
    }

    /// Whether `of` at `ty` reaches a maker: a parameter's is its instance's; a tuple's,
    /// or a List, Map, Option or Result's, is built when its arguments' are; a declared
    /// codec type has its derived one.
    fn codec_built(&mut self, ty: TyId) -> bool {
        let mut ty = ty;
        while let TyKind::Anchored(x, _) | TyKind::View(x) = *self.t.kind(ty) {
            ty = x;
        }
        match self.t.kind(ty).clone() {
            TyKind::Param(_) => true,
            TyKind::Tuple(xs) => xs.into_iter().all(|x| self.codec_built(x)),
            TyKind::Adt(d, _)
                if self.p.decls[d.index()]
                    .codec()
                    .and_then(|c| c.maker)
                    .is_some() =>
            {
                true
            }
            TyKind::Adt(d, args) if self.p.structural_codecs().has(d) => {
                args.into_iter().all(|x| self.codec_built(x))
            }
            _ => {
                let name = crate::decls::layout_name(&self.p.decls, &self.t, ty);
                templates::builtin_codec(&name).is_some()
            }
        }
    }
    /// A call of the maker row `maker`, `() -> Codec[T]`, whose result has type `result`.
    pub(super) fn call_maker(&mut self, maker: FuncId, result: TyId, n: NodeId) -> Option<ValueId> {
        if !self.live() {
            return None;
        }
        let signature = self.e.module.functions[maker.index()].signature;
        let dest = self.e.module.signatures[signature.index()].dest?;
        let at = self.new_slot(PlaceTy::Declared(dest), n);
        let args = PoolRange::push(&mut self.b.pools.args, [Arg::Address(at)]);
        let results = PoolRange::push(&mut self.b.pools.values, []);
        let fail = self.fail_to(n);
        self.b.calls.push(CallSite {
            callee: Callee::Direct(maker),
            signature,
            args,
            results,
            fail,
            cold: false,
        });
        let call = CallId(self.b.calls.len() as u32 - 1);
        self.inst(Inst::Call { call }, n);
        let p = self.pointer(at, n);
        self.temp(result, Some(p));
        Some(p)
    }
}
