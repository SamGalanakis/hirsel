//! Callable payloads use typed addresses, so a rigid generic call and its closed
//! scalar instance have the same convention. Adapters retain FORM's task/status ABI.
use super::calls::CallScratch;
use super::*;

/// The loan a by-address parameter of this source mode carries (crate docs of
/// hirsel-core-ir, "Loans"). `bytes` parameters are `u64` values, never places.
pub(super) fn loan(mode: Mode) -> Loan {
    match mode {
        Mode::Mut => Loan::Mut,
        Mode::Take => Loan::Take,
        Mode::View | Mode::Bytes | Mode::Steps => Loan::View,
    }
}

#[derive(Clone, Copy)]
pub(super) struct CallableAbi {
    pub(super) signature: SigId,
    pub(super) params: PoolRange<Rep>,
    pub(super) result: Rep,
    pub(super) regions: PoolRange<u32>,
    pub(super) scoped_result: bool,
}

impl<'c, 'a> Cx<'c, 'a, '_> {
    pub(super) fn indirect_signature(
        &mut self,
        params: &[(Mode, TyId)],
        result: TyId,
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
        buffer.push(Param::Value(Ty::Ptr));
        abi.params.push(false);
        for &(mode, ty) in params {
            let rep = self.rep(ty)?;
            let p = if mode == Mode::Mut {
                Param::Address {
                    place: self
                        .e
                        .mut_place(self.p, &mut self.t, ty, rep)
                        .ok_or("callable payload layouts are not built")?,
                    loan: Loan::Mut,
                }
            } else {
                match rep {
                    Rep::None => continue,
                    Rep::Scalar(t) => Param::Value(t),
                    Rep::Place(id) => Param::Address {
                        place: PlaceTy::Declared(id),
                        loan: loan(mode),
                    },
                }
            };
            abi.params
                .push(mode != Mode::Mut && matches!(p, Param::Address { .. }));
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
            anchors: vec![],
            never: result == NEVER,
        };
        if let Some(&row) = self
            .e
            .source_signatures
            .signatures
            .get(&(signature.clone(), abi.clone()))
        {
            *buffer = signature.params;
            Ok(row)
        } else {
            Ok(self.e.sig_abi(signature, abi))
        }
    }

    pub(super) fn region_handle(&self, ty: TyId) -> bool {
        matches!(self.t.kind(ty), TyKind::Adt(d, _) if self.p.decls[d.index()].role == Some(Role::Region))
    }

    // MEM-1 / OWN-3: a marked Region argument still names the same region.
    // Only root substitution strips the mark; argument checking sees the original
    // expression and enforces its mode, place and loan rules as usual.
    pub(super) fn region_argument_path(&self, mut n: NodeId) -> Option<NodeId> {
        loop {
            match self.kind(n) {
                NodeKind::Group | NodeKind::Ascription => n = self.kids(n)[0],
                NodeKind::Unary if matches!(self.token_text(n), "take" | "mut") => {
                    n = self.kids(n)[0];
                }
                _ => return self.binder_path(n),
            }
        }
    }

    pub(super) fn call_region_roots(
        &mut self,
        params: impl IntoIterator<Item = TyId>,
        args: &[NodeId],
        roots: &mut Vec<Option<TyId>>,
    ) {
        roots.clear();
        for (i, ty) in params.into_iter().enumerate() {
            if matches!(self.t.kind(ty), TyKind::Adt(d, _) if self.p.decls[d.index()].role == Some(Role::Region))
                && let Some(&arg) = args.get(i)
                && let Some(path) = self.region_argument_path(arg)
            {
                roots.resize(i + 1, None);
                roots[i] = Some(self.body_root(path, true));
            }
        }
    }

    /// Call the callable `value` of type `ty` from a built-in form with argument values
    /// already lowered (MEM-4 writers); its result value.
    pub(super) fn call_lowered(
        &mut self,
        ty: TyId,
        value: ValueId,
        vals: &[Option<ValueId>],
        n: NodeId,
    ) -> Option<ValueId> {
        self.call_lowered_with_fail(
            ty,
            value,
            vals,
            n,
            closures::CallFailure::Cleanup,
            closures::Entry::Step,
        )
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
    pub(super) fn checked_function_item(&mut self, f: usize, n: NodeId) -> Option<TyId> {
        let g = &self.p.fns[f];
        if g.path.0.modules == ["code"]
            && g.path.package_is("core", "1")
            && matches!(
                g.path.0.name.as_str(),
                "bind" | "load" | "slot" | "activate" | "share" | "dispatch" | "hold" | "take_as"
            )
        {
            self.refuse("PACK-14", n, "this form checks a witness at its direct use");
            return None;
        }
        if mentions_error(&self.t, g.callable) {
            self.inherited_refusal = true;
            self.halt();
            return None;
        }
        if g.params.iter().any(|p| self.vector_shape(p.ty).is_some())
            || self.vector_shape(g.result).is_some()
        {
            self.refuse(
                "SEM-11",
                n,
                "a function with a vector parameter or result cannot be a callable value",
            );
            return None;
        }
        if g.anchors
            .iter()
            .any(|&i| !self.region_handle(g.params[i as usize].ty))
        {
            self.refuse(
                "LANG-13",
                n,
                "a result anchored to a view parameter fits no callable type",
            );
            return None;
        }
        let ty = g.callable;
        if let TyKind::Fn(_, result, _, _) = self.t.kind(ty)
            && !crate::decls::scoped_result_rooted(&self.p.decls, &self.t, *result, false)
        {
            self.gap(
                n,
                "scoped callable result region anchors are not represented",
            );
            return None;
        }
        Some(ty)
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
        self.callable_adapter(raw, Some((item, types)), ty, n, None)
    }
    pub(super) fn item_descriptor_adapter(
        &mut self,
        raw: FuncId,
        item: usize,
        types: &[TyId],
        ty: TyId,
        n: NodeId,
        depends_on_enclosing: bool,
    ) -> Option<FuncId> {
        self.callable_adapter(raw, Some((item, types)), ty, n, Some(depends_on_enclosing))
    }
    pub(super) fn row_adapter(&mut self, raw: FuncId, ty: TyId, n: NodeId) -> Option<FuncId> {
        self.callable_adapter(raw, None, ty, n, None)
    }
    fn callable_adapter(
        &mut self,
        raw: FuncId,
        item: Option<(usize, &[TyId])>,
        ty: TyId,
        n: NodeId,
        descriptor_depends_on_enclosing: Option<bool>,
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
        let signature = match self.indirect_signature(&params, result, &mut vec![]) {
            Ok(signature) => signature,
            Err(what) => {
                self.gap(n, what);
                return None;
            }
        };
        let descriptor_instance = descriptor_depends_on_enclosing.is_some()
            && item.is_some_and(|(f, _)| self.p.fns[f].generics.iter().any(|g| !g.region));
        if !descriptor_instance
            && self.e.module.signatures[self.e.source_signature(raw).index()]
                == self.e.module.signatures[signature.index()]
            && item.map_or(
                self.e.module.functions[raw.index()].body.is_some()
                    || self.nested.contains_key(&raw),
                |(f, _)| self.p.fns[f].body.is_some(),
            )
        {
            return Some(raw);
        }
        let descriptor_key = descriptor_instance.then(|| {
            let mut key = self.e.module.instances
                [self.e.module.functions[raw.index()].instance.index()]
            .clone();
            if descriptor_depends_on_enclosing == Some(true) {
                // Each rigid adapter belongs to one template. Its closed key is
                // chosen from the specialized target during materialization.
                let enclosing = self.func.expect("a live body has a function row");
                key.definition
                    .0
                    .name
                    .push_str(&format!("$descriptor-template[{}]", enclosing.0));
            } else {
                key.definition.0.name.push_str("$callable[instance]");
            }
            key
        });
        match &descriptor_key {
            Some(key) => {
                if let Some(&row) = self.e.funcs.get(key) {
                    return Some(row);
                }
            }
            None => {
                // Closures are built in their own bodies. A second closure that
                // reuses a generic adapter needs its template, or its instance
                // names a row that no instance fills.
                if let Some((row, template)) = self.e.callable_adapters.get(&(raw, signature)) {
                    if let Some(template) = template {
                        self.nested.insert(*row, template.clone());
                    }
                    return Some(*row);
                }
            }
        }
        let mut child = Cx::new(self.p, self.definition, self.e);
        let mut fd = self.fd.as_ref().clone();
        fd.params.clear();
        // An item adapter closes the source function's generic arguments. Keeping
        // the enclosing generic declaration here leaves a body-less generic row;
        // a StaticFn described in a generic body then names no native body.
        if descriptor_depends_on_enclosing == Some(false) {
            fd.generics.clear();
            fd.has_region_parameters = false;
        }
        fd.result = result;
        fd.drop_of = None;
        fd.clone_of = None;
        fd.body = None;
        let target =
            &child.e.module.instances[child.e.module.functions[raw.index()].instance.index()];
        fd.path = target.definition.clone();
        fd.path.0.name = format!(
            "{}$callable[{}]",
            fd.path.0.name,
            crate::decls::canonical(&self.p.decls, &self.t, ty)
        );
        if let Some(key) = &descriptor_key {
            fd.path = key.definition.clone();
        }
        child.fd = std::borrow::Cow::Owned(fd);
        child.entry = false;
        child.generated = true;
        if let Some(template) = self.nested.get(&raw) {
            child.nested.insert(raw, template.clone());
        }
        std::mem::swap(&mut child.t, &mut self.t);
        let sig = child.e.module.signatures[signature.index()].clone();
        let row = match descriptor_key {
            Some(key) => child.e.add_function(key, sig),
            None => child.e.function(&child.fd.path, sig),
        };
        child.e.install_source_signature(row, signature);
        child.func = Some(row);
        let sig = &child.e.module.signatures[signature.index()];
        let entry = sig
            .dest
            .map(|_| Ty::Ptr)
            .into_iter()
            .chain(sig.params.iter().map(|p| match p {
                Param::Value(t) => *t,
                Param::Address { .. } => Ty::Ptr,
            }))
            .collect::<Vec<_>>();
        let block = child.new_block(&entry);
        child.start(block);
        let incoming = child.block_params(block);
        let has_dest = child.e.module.signatures[signature.index()].dest.is_some();
        let mut k = usize::from(has_dest);
        let env = incoming[k];
        k += 1;
        let mut values = Vec::with_capacity(params.len());
        let mut parameter = 1;
        for &(mode, ty) in &params {
            let rep = child.rep_at(ty, n)?;
            if mode != Mode::Mut && matches!(rep, Rep::None) {
                values.push(None);
                continue;
            }
            let param = child.e.module.signatures[signature.index()].params[parameter];
            parameter += 1;
            let value = match param {
                Param::Value(_) => Some(incoming[k]),
                Param::Address { .. } if mode == Mode::Mut => Some(incoming[k]),
                Param::Address { place, .. } => {
                    let at = child.address(AddressBase::Value(incoming[k]), &[], place, n);
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
            child.adapter_call(raw, Some(env), &params, &values, result, n)
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
        if let Some(template) = &out.template {
            self.nested.insert(row, template.clone());
        }
        self.out.refusals.extend(out.refusals);
        self.out.gaps.extend(out.gaps);
        self.out.counts += out.counts;
        self.out.computed.extend(out.computed);
        if out.function.is_none() {
            self.halt();
        } else if !descriptor_instance {
            self.e
                .callable_adapters
                .insert((raw, signature), (row, out.template));
        }
        out.function
    }
    /// The input a task of `fn(take x) -> r` takes: the function, then `x`;
    /// with the function's type.
    pub(super) fn task_input_ty(&mut self, x: TyId, r: TyId) -> (TyId, TyId) {
        let callback = self
            .t
            .mk(TyKind::Fn(vec![(Mode::Take, x)], r, true, Callable::Plain));
        (self.t.mk(TyKind::Tuple(vec![callback, x])), callback)
    }

    /// A task's entry into user code carries its own Step. The runtime and both
    /// native task thunks only invoke this ordinary front-produced callable.
    pub(super) fn task_entry_adapter(
        &mut self,
        x: TyId,
        r: TyId,
        function: ValueId,
        value: Option<ValueId>,
        n: NodeId,
    ) -> Option<(TypeId, AddressId, ValueId)> {
        let (wrapped, callback) = self.task_input_ty(x, r);
        let id = self.runtime_type(wrapped, n)?;
        let input = self.new_slot(PlaceTy::Declared(id), n);
        let code = self.address(
            AddressBase::Address(input),
            &[Step::Field(0)],
            PlaceTy::Value(Ty::Ptr),
            n,
        );
        self.install(code, Rep::Scalar(Ty::Ptr), Some(function), n);
        let xrep = self.rep_at(x, n)?;
        if let Some(place) = xrep.stored() {
            let at = self.address(AddressBase::Address(input), &[Step::Field(1)], place, n);
            match (xrep, value) {
                // The caller still owns the input while spawn admits the task.
                (Rep::Place(id), Some(from)) => self.copy_bytes_to(at, from, id, n),
                _ => self.install(at, xrep, value, n),
            }
        }
        let signature = match self.indirect_signature(&[(Mode::Take, wrapped)], r, &mut vec![]) {
            Ok(s) => s,
            Err(what) => {
                self.gap(n, what);
                return None;
            }
        };
        let mut fd = self.fd.as_ref().clone();
        fd.path.0.name = self.generated_name(GeneratedKind::TaskEntry, n);
        fd.params.clear();
        fd.result = r;
        fd.drop_of = None;
        fd.clone_of = None;
        fd.body = None;
        let mut child = Cx::new(self.p, self.definition, self.e);
        child.fd = std::borrow::Cow::Owned(fd);
        child.entry = false;
        child.generated = false;
        std::mem::swap(&mut child.t, &mut self.t);
        let row = child.e.function(
            &child.fd.path,
            child.e.module.signatures[signature.index()].clone(),
        );
        child.e.install_source_signature(row, signature);
        child.func = Some(row);
        let sig = child.e.module.signatures[signature.index()].clone();
        let has_dest = sig.dest.is_some();
        let params = sig
            .dest
            .map(|_| Ty::Ptr)
            .into_iter()
            .chain(sig.params.iter().map(|p| match p {
                Param::Value(t) => *t,
                Param::Address { .. } => Ty::Ptr,
            }))
            .collect::<Vec<_>>();
        let block = child.new_block(&params);
        child.start(block);
        let incoming = child.block_params(block);
        if let Some(dest) = sig.dest {
            child.dest = Some((incoming[0], dest));
        }
        let child_input = incoming[usize::from(has_dest) + 1];
        let root = child.fields_of(AddressBase::Value(child_input), id, n);
        let code = child.address(
            AddressBase::Address(root),
            &[Step::Field(0)],
            PlaceTy::Value(Ty::Ptr),
            n,
        );
        let code = child.read_bits(code, Ty::Ptr, n);
        let value = if let Some(place) = xrep.stored() {
            let at = child.address(AddressBase::Address(root), &[Step::Field(1)], place, n);
            child.read_carried_rep(at, xrep, n)
        } else {
            None
        };
        child.temp(x, value);
        let returned = child.call_lowered(callback, code, &[value], n);
        child.ret(returned, n);
        std::mem::swap(&mut child.t, &mut self.t);
        let out = child.finish();
        if let Some(template) = out.template {
            self.nested.insert(row, template);
        }
        self.out.refusals.extend(out.refusals);
        self.out.gaps.extend(out.gaps);
        self.out.counts += out.counts;
        self.out.computed.extend(out.computed);
        if out.function.is_none() {
            self.halt();
            return None;
        }
        let code = self.value(Ty::Ptr);
        self.inst(
            Inst::FunctionRef {
                result: code,
                function: row,
            },
            n,
        );
        Some((id, input, code))
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
        let signature = self.e.source_signature(target);
        let dest = self.e.module.signatures[signature.index()]
            .dest
            .map(|id| self.new_slot(PlaceTy::Declared(id), n));
        let mut args = Vec::with_capacity(params.len() + 2);
        args.extend(dest.map(Arg::Address));
        args.extend(env.map(Arg::Value));
        let mut parameter = usize::from(env.is_some());
        for (i, &(mode, ty)) in params.iter().enumerate() {
            let rep = match self.rep(ty) {
                Ok(rep) => rep,
                Err(what) => {
                    self.gap(n, what);
                    return None;
                }
            };
            if rep == Rep::None && mode != Mode::Mut {
                continue;
            }
            // #88: the callee signature already names the borrowed place, including
            // scalar and Region value places. No declared payload row is derived here.
            let param = self.e.module.signatures[signature.index()].params[parameter];
            parameter += 1;
            match param {
                Param::Address { place, .. } => args.push(Arg::Address(self.address(
                    AddressBase::Value(values[i]?),
                    &[],
                    place,
                    n,
                ))),
                Param::Value(_) => args.extend(values[i].map(Arg::Value)),
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
            cold: false,
        });
        self.inst(Inst::Call { call }, n);
        if result == NEVER {
            self.end(Terminator::Unreachable, Some(n));
            return None;
        }
        dest.map(|at| self.pointer(at, n)).or(value)
    }
}

impl<'c, 'a> Cx<'c, 'a, '_> {
    pub(super) fn value_call_abi(
        &mut self,
        callable: TyId,
        result: TyId,
        n: NodeId,
        scratch: &mut CallScratch,
    ) -> Option<CallableAbi> {
        let abi = match self.callable_signatures.get(&callable).copied() {
            Some(abi) => abi,
            None => {
                let signature = match self.indirect_signature(
                    &scratch.params,
                    result,
                    &mut scratch.ir_params,
                ) {
                    Ok(signature) => signature,
                    Err(what) => {
                        self.gap(n, what);
                        return None;
                    }
                };
                let start = self.callable_reps.len();
                for &(_, ty) in &scratch.params {
                    let rep = match self.rep(ty) {
                        Ok(rep) => rep,
                        Err(what) => {
                            self.gap(n, what);
                            return None;
                        }
                    };
                    self.callable_reps.push(rep);
                }
                let result_ty = result;
                let result = match self.rep(result) {
                    Ok(rep) => rep,
                    Err(what) => {
                        self.gap(n, what);
                        return None;
                    }
                };
                let roots = self.callable_regions.len();
                for (i, &(_, ty)) in scratch.params.iter().enumerate() {
                    if self.region_handle(ty)
                        && (self.parameter_region_result(result_ty, i as u32)
                            || scratch
                                .params
                                .iter()
                                .any(|&(_, ty)| self.parameter_region_result(ty, i as u32)))
                    {
                        self.callable_regions.push(i as u32);
                    }
                }
                let abi = CallableAbi {
                    signature,
                    params: PoolRange::new(start as u32, scratch.params.len() as u32),
                    result,
                    scoped_result: matches!(
                        self.type_kind(result_ty),
                        Kind::Scoped | Kind::Generic
                    ),
                    regions: PoolRange::new(
                        roots as u32,
                        (self.callable_regions.len() - roots) as u32,
                    ),
                };
                self.callable_signatures.insert(callable, abi);
                abi
            }
        };
        Some(abi)
    }
    pub(super) fn scoped_result_regions_only(
        &self,
        ty: TyId,
        params: &[crate::decls::ParamDecl],
    ) -> bool {
        match self.t.kind(ty) {
            TyKind::Region(Root::Parameter(i)) => self
                .fd
                .params
                .get(*i as usize)
                .is_some_and(|p| self.region_handle(p.ty)),
            TyKind::Region(Root::CallableParameter { depth: 0, index: i }) => params
                .get(*i as usize)
                .is_some_and(|p| self.region_handle(p.ty)),
            TyKind::Region(Root::Local(slot)) => self.vars[*slot as usize]
                .map(|v| v.ty)
                .or_else(|| self.captured_root_ty(*slot))
                .is_some_and(|ty| self.region_handle(ty)),
            TyKind::Anchored(x, root) => {
                self.scoped_result_regions_only(*x, params)
                    && self.scoped_result_regions_only(*root, params)
            }
            TyKind::Tuple(xs) | TyKind::Adt(_, xs) => xs
                .iter()
                .all(|&x| self.scoped_result_regions_only(x, params)),
            TyKind::View(x) | TyKind::Array(x, _) => self.scoped_result_regions_only(*x, params),
            _ => true,
        }
    }

    pub(super) fn parameter_region_result(&self, ty: TyId, parameter: u32) -> bool {
        self.parameter_region_result_at(ty, parameter, 0)
    }
    pub(super) fn parameter_region_result_at(&self, ty: TyId, parameter: u32, depth: u32) -> bool {
        match self.t.kind(ty) {
            TyKind::Region(Root::CallableParameter {
                depth: bound,
                index: i,
            }) => *i == parameter && *bound == depth,
            TyKind::Anchored(inner, root) => {
                matches!(self.t.kind(*root), TyKind::Region(Root::CallableParameter {depth: bound, index: i}) if *i == parameter && *bound == depth)
                    || self.parameter_region_result_at(*inner, parameter, depth)
            }
            TyKind::Tuple(xs) | TyKind::Adt(_, xs) => xs
                .iter()
                .any(|&x| self.parameter_region_result_at(x, parameter, depth)),
            TyKind::Array(x, _) | TyKind::View(x) => {
                self.parameter_region_result_at(*x, parameter, depth)
            }
            TyKind::Fn(ps, r, _, _) => {
                ps.iter()
                    .any(|&(_, p)| self.parameter_region_result_at(p, parameter, depth + 1))
                    || self.parameter_region_result_at(*r, parameter, depth + 1)
            }
            _ => false,
        }
    }
}
