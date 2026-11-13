//! Callable payloads use typed addresses, so a rigid generic call and its closed
//! scalar instance have the same convention. Adapters retain FORM's task/status ABI.
use super::*;

impl<'c, 'a> Cx<'c, 'a, '_> {
    pub(super) fn indirect_signature(
        &mut self,
        params: &[(Mode, TyId)],
        result: TyId,
        environment: bool,
        buffer: &mut Vec<Param>,
    ) -> Result<SigId, &'static str> {
        #[cfg(test)]
        {
            self.e.callable_signature_builds += 1;
        }
        buffer.clear();
        let mut abi = scalar_abi::SignatureAbi {
            params: vec![],
            result: true,
        };
        if environment {
            buffer.push(Param::Value(Ty::Ptr));
            abi.params.push(false);
        }
        for &(mode, ty) in params {
            let rep = self.rep(ty)?;
            let p = if mode == Mode::Mut {
                Param::Address(
                    self.e
                        .type_row(self.p, &mut self.t, ty)
                        .ok_or("callable payload layouts are not built")?,
                )
            } else {
                match rep {
                    Rep::None => continue,
                    Rep::Scalar(t) => Param::Value(t),
                    Rep::Place(id) => Param::Address(id),
                }
            };
            abi.params
                .push(mode != Mode::Mut && matches!(p, Param::Address(_)));
            buffer.push(p);
        }
        let (results, dest) = match self.rep(result)? {
            Rep::None => (vec![], None),
            Rep::Scalar(t) => (vec![t], None),
            Rep::Place(id) => (vec![], Some(id)),
        };
        let signature = Signature {
            params: std::mem::take(buffer),
            results,
            dest,
            never: result == NEVER,
        };
        if let Some(&row) = self.e.sigs.get(&(signature.clone(), abi.clone())) {
            *buffer = signature.params;
            Ok(row)
        } else {
            Ok(self.e.sig_abi(signature, abi))
        }
    }

    pub(super) fn region_handle(&self, ty: TyId) -> bool {
        matches!(self.t.kind(ty), TyKind::Adt(d, _) if self.p.decls[d.index()].role == Some(Role::Region))
    }

    /// Read a source value from its callable argument/result place.
    pub(super) fn read_carried(&mut self, at: AddressId, ty: TyId, n: NodeId) -> Option<ValueId> {
        let rep = match self.rep(ty) {
            Ok(rep) => rep,
            Err(what) => {
                self.gap(n, what);
                return None;
            }
        };
        self.read_carried_rep(at, rep, n)
    }
    pub(super) fn read_carried_rep(
        &mut self,
        at: AddressId,
        rep: Rep,
        n: NodeId,
    ) -> Option<ValueId> {
        match rep {
            Rep::None => None,
            Rep::Place(_) => Some(self.pointer(at, n)),
            Rep::Scalar(t) => {
                let at = self.address(AddressBase::Address(at), &[], PlaceTy::Value(t), n);
                Some(self.read_bits(at, t, n))
            }
        }
    }
    pub(super) fn item_adapter(
        &mut self,
        item: usize,
        types: &[TyId],
        ty: TyId,
        n: NodeId,
    ) -> Option<FuncId> {
        let (raw, _) = match self.instance_row(item, types) {
            Ok(row) => row,
            Err(what) => {
                self.gap(n, what);
                return None;
            }
        };
        let environment =
            matches!(self.t.kind(ty), TyKind::Fn(_, _, _, k) if *k != Callable::Plain);
        self.callable_adapter(raw, Some((item, types)), ty, environment, n)
    }
    pub(super) fn row_adapter(
        &mut self,
        raw: FuncId,
        ty: TyId,
        environment: bool,
        n: NodeId,
    ) -> Option<FuncId> {
        self.callable_adapter(raw, None, ty, environment, n)
    }
    fn callable_adapter(
        &mut self,
        raw: FuncId,
        item: Option<(usize, &[TyId])>,
        ty: TyId,
        environment: bool,
        n: NodeId,
    ) -> Option<FuncId> {
        if !self.live() {
            return None;
        }
        let TyKind::Fn(params, result, _, _) = self.t.kind(ty).clone() else {
            unreachable!()
        };
        for ty in params.iter().map(|p| p.1).chain(std::iter::once(result)) {
            if let Err(what) = self.rep(ty) {
                self.gap(n, what);
                return None;
            }
        }
        let signature = match self.indirect_signature(&params, result, environment, &mut vec![]) {
            Ok(signature) => signature,
            Err(what) => {
                self.gap(n, what);
                return None;
            }
        };
        if self.e.module.signatures[self.e.module.functions[raw.index()].signature.index()]
            == self.e.module.signatures[signature.index()]
            && item.map_or(
                self.e.module.functions[raw.index()].body.is_some()
                    || self.nested.contains_key(&raw),
                |(f, _)| self.p.fns[f].body.is_some(),
            )
        {
            return Some(raw);
        }
        if let Some(&row) = self.e.callable_adapters.get(&(raw, signature)) {
            return Some(row);
        }
        let mut child = Cx::new(self.p, self.definition, self.e);
        let mut fd = self.fd.as_ref().clone();
        fd.params.clear();
        fd.result = result;
        fd.drop_of = None;
        fd.body = None;
        let target =
            &child.e.module.instances[child.e.module.functions[raw.index()].instance.index()];
        fd.path = target.definition.clone();
        fd.path.0.name = format!(
            "{}$callable[{}]",
            fd.path.0.name,
            crate::decls::canonical(&self.p.decls, &self.t, ty)
        );
        child.fd = std::borrow::Cow::Owned(fd);
        child.entry = false;
        if let Some(template) = self.nested.get(&raw) {
            child.nested.insert(raw, template.clone());
        }
        std::mem::swap(&mut child.t, &mut self.t);
        let row = child.e.function(
            &child.fd.path,
            child.e.module.signatures[signature.index()].clone(),
        );
        child.e.module.functions[row.index()].signature = signature;
        child.func = Some(row);
        let sig = &child.e.module.signatures[signature.index()];
        let entry = sig
            .dest
            .map(|_| Ty::Ptr)
            .into_iter()
            .chain(sig.params.iter().map(|p| match p {
                Param::Value(t) => *t,
                Param::Address(_) => Ty::Ptr,
            }))
            .collect::<Vec<_>>();
        let block = child.new_block(&entry);
        child.start(block);
        let incoming = child.block_params(block);
        let has_dest = child.e.module.signatures[signature.index()].dest.is_some();
        let mut k = usize::from(has_dest);
        let env = if environment {
            let env = incoming[k];
            k += 1;
            Some(env)
        } else {
            None
        };
        let mut values = Vec::with_capacity(params.len());
        let mut parameter = usize::from(environment);
        for &(mode, ty) in &params {
            let rep = child.rep(ty).ok()?;
            if mode != Mode::Mut && matches!(rep, Rep::None) {
                values.push(None);
                continue;
            }
            let param = child.e.module.signatures[signature.index()].params[parameter];
            parameter += 1;
            let value = match param {
                Param::Value(_) => Some(incoming[k]),
                Param::Address(_) if mode == Mode::Mut => Some(incoming[k]),
                Param::Address(id) => {
                    let at = child.address(
                        AddressBase::Value(incoming[k]),
                        &[],
                        PlaceTy::Declared(id),
                        n,
                    );
                    child.read_carried(at, ty, n)
                }
            };
            k += 1;
            values.push(value);
        }
        let value = if let Some((f, types)) = item {
            let g = &child.p.fns[f];
            if g.path.package_is("core", "1")
                && g.body.is_none()
                && (g.path.0.modules == ["int"] || g.path.0.modules == ["float"])
            {
                child.intrinsic(f, types, &values, n)
            } else {
                child.emit_call(f, types, &values, result, n)
            }
        } else {
            child.adapter_call(raw, env, &params, &values, result, n)
        };
        if child.live() {
            if result == NEVER {
                child.end(Terminator::Unreachable, Some(n));
            } else {
                if has_dest
                    && let Ok(rep) = child.rep(result)
                    && let Some(place) = rep.stored()
                {
                    let at = child.address(AddressBase::Value(incoming[0]), &[], place, n);
                    child.install(at, rep, value, n);
                    child.consume(value, n);
                }
                let values = if has_dest {
                    PoolRange::EMPTY
                } else {
                    PoolRange::push(&mut child.b.pools.values, value)
                };
                child.end(Terminator::Return(values), Some(n));
            }
        }
        std::mem::swap(&mut child.t, &mut self.t);
        let out = child.finish();
        if let Some(template) = out.template {
            self.nested.insert(row, template);
        }
        self.out.refusals.extend(out.refusals);
        self.out.gaps.extend(out.gaps);
        self.out.counts += out.counts;
        if out.function.is_none() {
            self.halt();
        } else {
            self.e.callable_adapters.insert((raw, signature), row);
        }
        out.function
    }
    fn adapter_call(
        &mut self,
        target: FuncId,
        env: Option<ValueId>,
        params: &[(Mode, TyId)],
        values: &[Option<ValueId>],
        result: TyId,
        n: NodeId,
    ) -> Option<ValueId> {
        let signature = self.e.module.functions[target.index()].signature;
        let dest = self.e.module.signatures[signature.index()]
            .dest
            .map(|id| self.new_slot(PlaceTy::Declared(id), n));
        let mut args = Vec::with_capacity(params.len() + 2);
        args.extend(dest.map(Arg::Address));
        args.extend(env.map(Arg::Value));
        for (i, &(mode, ty)) in params.iter().enumerate() {
            let rep = match self.rep(ty) {
                Ok(rep) => rep,
                Err(what) => {
                    self.gap(n, what);
                    return None;
                }
            };
            if mode == Mode::Mut || matches!(rep, Rep::Place(_)) {
                let Some(id) = self.e.type_row(self.p, &mut self.t, ty) else {
                    self.gap(n, "callable payload layouts are not built");
                    return None;
                };
                args.push(Arg::Address(self.address(
                    AddressBase::Value(values[i]?),
                    &[],
                    PlaceTy::Declared(id),
                    n,
                )));
            } else if let Some(value) = values[i] {
                args.push(Arg::Value(value));
            }
        }
        let args = PoolRange::push(&mut self.b.pools.args, args);
        let value = self.e.module.signatures[signature.index()]
            .results
            .first()
            .copied()
            .map(|ty| self.value(ty));
        let results = PoolRange::push(&mut self.b.pools.values, value);
        let fail = self.fail_to(n);
        let call = CallId(self.b.calls.len() as u32);
        self.b.calls.push(CallSite {
            callee: Callee::Direct(target),
            signature,
            args,
            results,
            fail,
        });
        self.inst(Inst::Call { call }, n);
        if result == NEVER {
            self.end(Terminator::Unreachable, Some(n));
            return None;
        }
        dest.map(|at| self.pointer(at, n)).or(value)
    }
}
