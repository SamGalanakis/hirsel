//! Callable values, using FORM's code pointers and ordinary call rows (LANG-13).
use super::calls::CallScratch;
use super::*;

impl<'c, 'a> Cx<'c, 'a> {
    pub(super) fn function_value(
        &mut self,
        f: usize,
        n: NodeId,
        want: Option<Want>,
    ) -> (TyId, Option<ValueId>) {
        let mut scratch = self.call_scratch.pop().unwrap_or_default();
        let result = self.function_value_inner(f, n, want, &mut scratch);
        self.call_scratch.push(scratch);
        result
    }

    fn function_value_inner(
        &mut self,
        f: usize,
        n: NodeId,
        want: Option<Want>,
        scratch: &mut CallScratch,
    ) -> (TyId, Option<ValueId>) {
        let g = &self.p.fns[f];
        if g.params.iter().any(|p| p.bytes) {
            return self.gap_ty(n, "function values with bytes parameters are not built");
        }
        if g.body.is_none() && g.path.package_is("core", "1") {
            return self.gap_ty(n, "core function values need callable wrappers");
        }
        let ty = g.callable;
        scratch.found.clear();
        scratch.found.resize(g.generics.len(), None);
        scratch.checked.clear();
        scratch.checked.resize(g.generics.len(), false);
        if let Some(w) = want.filter(|w| w.payload == 0) {
            if let TyKind::Fn(ps, r, _, _) = self.t.kind(w.ty) {
                let (len, r) = (ps.len(), *r);
                for (i, parameter) in g.params.iter().enumerate().take(len) {
                    let TyKind::Fn(ps, _, _, _) = self.t.kind(w.ty) else {
                        unreachable!()
                    };
                    let expected = ps[i].1;
                    self.unify(parameter.ty, expected, &mut scratch.found);
                }
                self.unify(g.result, r, &mut scratch.found);
            }
            self.determined(f, &scratch.found, &mut scratch.checked, n);
        }
        if scratch.found.iter().any(Option::is_none) {
            self.refuse(
                "LANG-12",
                n,
                "a function value's type parameters need an expected callable type",
            );
            return (ERROR, None);
        }
        scratch.tys.clear();
        scratch
            .tys
            .extend(scratch.found.iter().map(|t| t.unwrap_or(ERROR)));
        let ty = subst(&mut self.t, ty, &scratch.tys);
        // A direct rigid T uses an address ABI. Exposing its scalar/unit closed
        // instance as a normal callable requires an adapter, which is not emitted.
        let direct_result = matches!(self.t.kind(g.result), TyKind::Param(_));
        let direct_params = g
            .params
            .iter()
            .any(|p| p.mode != Mode::Mut && matches!(self.t.kind(p.ty), TyKind::Param(_)));
        if direct_result || direct_params {
            return self.gap_ty(n, "generic function-value ABI adapters are not built");
        }
        let (function, _) = match self.instance_row(f, &scratch.tys) {
            Ok(row) => row,
            Err(what) => return self.gap_ty(n, what),
        };
        let value = if self.live() {
            let result = self.value(Ty::Ptr);
            self.inst(Inst::FunctionRef { result, function }, n);
            Some(result)
        } else {
            None
        };
        (ty, value)
    }

    pub(super) fn call_value(
        &mut self,
        n: NodeId,
        callee: NodeId,
        args: &[NodeId],
        _want: Option<Want>,
        scratch: &mut CallScratch,
    ) -> (TyId, Option<ValueId>) {
        self.call_seq += 1;
        let call = self.call_seq;
        let holds = self.freezes.len();
        let (ty, value) = match self.place(callee) {
            Ok(Some(at)) => {
                self.use_place(at, Use::View(Hold::CallView(call, u32::MAX)), callee);
                (at.ty, self.emit_place(at, callee))
            }
            Ok(None) => self.expr(callee, None),
            Err(()) => return (ERROR, None),
        };
        let (result, waits, callable) = match self.t.kind(ty) {
            TyKind::Fn(params, result, waits, callable) => {
                scratch.params.clear();
                scratch.params.extend_from_slice(params);
                (*result, *waits, *callable)
            }
            TyKind::Error => return (ERROR, None),
            _ => {
                self.refuse("LANG-13", callee, "the value being called is not callable");
                return (ERROR, None);
            }
        };
        if matches!(self.t.kind(result), TyKind::Param(_))
            || scratch.params.iter().any(|(mode, ty)| {
                *mode != Mode::Mut && matches!(self.t.kind(*ty), TyKind::Param(_))
            })
        {
            return self.gap_ty(n, "indirect calls with opaque generic values are not built");
        }
        if args.len() < scratch.params.len() {
            self.refuse_at(
                "LANG-11",
                self.close_brace(n),
                "a callable needs one argument per parameter",
            );
            return (ERROR, None);
        }
        if args.len() > scratch.params.len() {
            self.refuse(
                "LANG-11",
                args[scratch.params.len()],
                "extra callable argument",
            );
            return (ERROR, None);
        }
        if self.patterns.guard && scratch.params.iter().any(|p| p.0 != Mode::View) {
            self.refuse(
                "LANG-17",
                n,
                "a match guard cannot call a take or mut parameter",
            );
            return (ERROR, None);
        }
        if waits {
            self.waited = true;
            if !self.fd.waits && !self.entry {
                self.refuse(
                    "LANG-8",
                    n,
                    "invokes a waits callable but is not declared waits",
                );
            }
        }
        if kind(&self.p.decls, &self.t, result, &self.params) == Kind::Scoped {
            self.gap(n, "scoped callable result anchors are not built");
        }
        scratch.vals.clear();
        scratch.vals.resize(args.len(), None);
        scratch.found.clear();
        scratch
            .found
            .extend(self.fd.generics.iter().enumerate().map(|(i, g)| {
                Some(self.t.mk(if g.region {
                    TyKind::Region(Root::Generic(i as u32))
                } else {
                    TyKind::Param(i as u32)
                }))
            }));
        for (i, &arg) in args.iter().enumerate() {
            if matches!(
                self.kind(arg),
                NodeKind::LabelledArgument | NodeKind::BytesArgument
            ) {
                self.gap(arg, "marked arguments to callable values are not built");
                continue;
            }
            let (mode, pt) = scratch.params[i];
            if !self.argument(
                mode,
                pt,
                call,
                i,
                arg,
                &mut scratch.found,
                &mut scratch.vals[i],
            ) {
                scratch.vals[i] = self
                    .expr(
                        arg,
                        Some(Want {
                            ty: pt,
                            view: mode == Mode::View,
                            payload: 0,
                        }),
                    )
                    .1;
            }
        }
        self.unfreeze(holds, |hold| match hold {
            Hold::CallView(c, _) | Hold::CallMut(c) if c == call => None,
            other => Some(other),
        });
        let (code, environment) = if callable == Callable::Plain {
            (value, None)
        } else {
            (
                self.callable_word(value, 0, n),
                self.callable_word(value, 8, n),
            )
        };
        let v = self.emit_value_call(code, environment, result, n, scratch);
        if result == NEVER {
            self.dead = true;
        }
        (result, v)
    }

    fn callable_word(&mut self, value: Option<ValueId>, offset: u64, n: NodeId) -> Option<ValueId> {
        let base = value?;
        if !self.live() {
            return None;
        }
        let offset = self.konst(Ty::Scalar(Scalar::U64), offset, n)?;
        let address = self.address(
            AddressBase::Value(base),
            &[Step::Byte(offset)],
            PlaceTy::Value(Ty::Ptr),
            n,
        );
        let result = self.value(Ty::Ptr);
        self.inst(
            Inst::Read {
                result,
                address,
                mode: ReadMode::Bits,
            },
            n,
        );
        Some(result)
    }

    fn emit_value_call(
        &mut self,
        code: Option<ValueId>,
        environment: Option<ValueId>,
        result: TyId,
        n: NodeId,
        scratch: &mut CallScratch,
    ) -> Option<ValueId> {
        if !self.live() {
            return None;
        }
        let code = code?;
        scratch.ir_params.clear();
        scratch.ir_results.clear();
        scratch
            .ir_params
            .extend(environment.map(|_| Param::Value(Ty::Ptr)));
        for &(mode, ty) in &scratch.params {
            let rep = match self.rep(ty) {
                Ok(r) => r,
                Err(what) => {
                    self.gap(n, what);
                    return None;
                }
            };
            let param = if mode == Mode::Mut {
                Param::Address(self.e.type_row(self.p, &mut self.t, ty)?)
            } else {
                match rep {
                    Rep::None => continue,
                    Rep::Scalar(t) => Param::Value(t),
                    Rep::View(_) => Param::Value(Ty::Ptr),
                    Rep::Place(id) => Param::Address(id),
                }
            };
            scratch.ir_params.push(param);
        }
        let dest = match self.rep(result) {
            Ok(Rep::None) => None,
            Ok(Rep::Scalar(t)) => {
                scratch.ir_results.push(t);
                None
            }
            Ok(Rep::View(_)) => {
                scratch.ir_results.push(Ty::Ptr);
                None
            }
            Ok(Rep::Place(id)) => Some(id),
            Err(what) => {
                self.gap(n, what);
                return None;
            }
        };
        let signature = Signature {
            params: std::mem::take(&mut scratch.ir_params),
            results: std::mem::take(&mut scratch.ir_results),
            dest,
            never: result == NEVER,
        };
        let signature = if let Some(&id) = self.e.sigs.get(&signature) {
            scratch.ir_params = signature.params;
            scratch.ir_results = signature.results;
            id
        } else {
            self.e.sig(signature)
        };
        let dest = dest.map(|id| self.new_slot(PlaceTy::Declared(id), n));
        let start = self.b.pools.args.len();
        self.b.pools.args.extend(dest.map(Arg::Address));
        self.b.pools.args.extend(environment.map(Arg::Value));
        for (i, &(mode, ty)) in scratch.params.iter().enumerate() {
            if mode == Mode::Take {
                self.consume(scratch.vals[i], n);
            }
            let rep = self.rep(ty).ok()?;
            if rep == Rep::None && mode != Mode::Mut {
                continue;
            }
            let v = scratch.vals[i]?;
            let arg = if mode == Mode::Mut {
                let id = self.e.type_row(self.p, &mut self.t, ty)?;
                Arg::Address(self.address(AddressBase::Value(v), &[], PlaceTy::Declared(id), n))
            } else if let Rep::Place(id) = rep {
                Arg::Address(self.address(AddressBase::Value(v), &[], PlaceTy::Declared(id), n))
            } else {
                Arg::Value(v)
            };
            self.b.pools.args.push(arg);
        }
        let args = PoolRange::new(start as u32, (self.b.pools.args.len() - start) as u32);
        let first = self.e.module.signatures[signature.index()]
            .results
            .first()
            .copied()
            .map(|ty| self.value(ty));
        let results = PoolRange::push(&mut self.b.pools.values, first);
        let fail = self.fail_to(n);
        let call = CallId(self.b.calls.len() as u32);
        self.b.calls.push(CallSite {
            callee: Callee::Indirect(code),
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
        match dest {
            Some(a) => {
                let ptr = self.pointer(a, n);
                self.temp(result, Some(ptr));
                Some(ptr)
            }
            None => first,
        }
    }
}
