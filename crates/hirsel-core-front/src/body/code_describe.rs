use super::*;

impl<'c, 'a> Cx<'c, 'a, '_> {
    /// PROG-9 is a form: the argument names a definition and is never evaluated.
    pub(super) fn code_describe(
        &mut self,
        result: TyId,
        argument: NodeId,
        want: Option<TyId>,
        n: NodeId,
    ) -> (TyId, Option<ValueId>) {
        let Some(item) = self.named_callee(argument) else {
            self.refuse(
                "PROG-9",
                argument,
                "code.describe requires the name of a top-level function",
            );
            return (ERROR, None);
        };
        let fd = &self.p.fns[item];
        if mentions_error(&self.t, fd.callable) {
            self.inherited_refusal = true;
            self.halt();
            return (ERROR, None);
        }
        let expected = want.and_then(|ty| match self.t.kind(ty) {
            TyKind::Adt(d, args) if self.p.decls[d.index()].role == Some(Role::StaticFn) => {
                Some(args[0])
            }
            _ => None,
        });
        let mut found = vec![None; fd.generics.len()];
        for (i, g) in fd.generics.iter().enumerate() {
            if g.region {
                found[i] = Some(self.t.mk(generic_arg(fd, i)));
            }
        }
        let generic = fd.generics.iter().any(|g| !g.region);
        let mut fits = true;
        if generic && let Some(expected) = expected {
            match (
                self.t.kind(fd.callable).clone(),
                self.t.kind(expected).clone(),
            ) {
                (TyKind::Fn(ps, r, waits, _), TyKind::Fn(es, er, ew, _)) => {
                    fits = ps.len() == es.len() && (!waits || ew);
                    for ((mode, ty), (em, et)) in ps.into_iter().zip(es) {
                        fits &= self.unify(ty, et, &mut found) && mode == em;
                    }
                    fits &= self.unify(r, er, &mut found);
                }
                (_, TyKind::Hole) => {}
                _ => fits = false,
            }
        }
        for (g, arg) in fd.generics.iter().zip(&found) {
            if g.region {
                continue;
            }
            let name = self.p.index.symbols.text(g.name);
            let function = &fd.path.0.name;
            if let Some(ty) = arg {
                if names_region(&self.t, *ty) {
                    self.refuse("PROG-9", argument, format!(
                        "code.describe cannot name an instance whose type parameter {name} of {function} names a region"
                    ));
                    return (ERROR, None);
                }
            } else {
                self.refuse("PROG-9", argument, format!(
                    "code.describe leaves type parameter {name} of {function} open: the expected StaticFn type must fix it"
                ));
                return (ERROR, None);
            }
        }
        let before = self.out.refusals.len();
        self.determined(
            item,
            &found,
            &mut vec![false; found.len()],
            argument,
            argument,
        );
        if self.out.refusals.len() != before {
            return (ERROR, None);
        }
        let args = found.into_iter().map(Option::unwrap).collect::<Vec<_>>();
        let ty = subst(&mut self.t, fd.callable, &args);
        if !fits {
            self.refuse_compared(
                "LANG-13",
                argument,
                self.shown(ty),
                self.shown(expected.unwrap()),
                "the callable does not fit the written callable type",
            );
            return (ERROR, None);
        }
        let result = subst(&mut self.t, result, &[ty]);
        if !self.live() {
            return (result, None);
        }
        if fd
            .generics
            .iter()
            .zip(&args)
            .any(|(g, &ty)| !g.region && has_param(&self.t, ty))
        {
            return self.gap_ty(
                argument,
                "a descriptor depending on a generic body parameter is not built",
            );
        }
        let raw = match self.instance_row(item, &args) {
            Ok((raw, _)) => raw,
            Err(what) => return self.gap_ty(argument, what),
        };
        let Some(bytes) = self.code_identity_bytes(item, raw, &args) else {
            return self.gap_ty(argument, "StaticFn identity source is not built");
        };
        let Some(function) = self.item_adapter_closed(raw, item, &args, ty, n) else {
            return (result, None);
        };
        self.nested.remove(&function);
        let identity = self.e.static_bytes(&bytes);
        let descriptor = self.value(Ty::Ptr);
        self.inst(
            Inst::DescriptorRef {
                result: descriptor,
                function,
                identity,
            },
            n,
        );
        let word = self.reinterpret(descriptor, Ty::Ptr, Ty::Scalar(Scalar::U64), n);
        if let Some(word) = word {
            self.static_fns
                .insert(word, (function, identity, bytes.len() as u64));
        }
        self.temp(result, word);
        (result, word)
    }

    fn code_identity_bytes(&self, item: usize, raw: FuncId, args: &[TyId]) -> Option<Vec<u8>> {
        let key = &self.e.module.instances[self.e.module.functions[raw.index()].instance.index()];
        crate::identity::instance_identity(&self.p.decls, &self.t, &self.p.fns[item], key, args)
    }

    pub(super) fn reinterpret(
        &mut self,
        value: ValueId,
        from: Ty,
        to: Ty,
        n: NodeId,
    ) -> Option<ValueId> {
        if self.b.values.get(value.index()) != Some(&from) {
            return None;
        }
        let slot = self.new_slot(PlaceTy::Value(from), n);
        self.inst(
            Inst::Store {
                address: slot,
                value,
            },
            n,
        );
        let pointer = self.pointer(slot, n);
        let at = self.address(AddressBase::Value(pointer), &[], PlaceTy::Value(to), n);
        Some(self.read_bits(at, to, n))
    }
}

fn names_region(t: &impl TyRead, ty: TyId) -> bool {
    match t.kind(ty) {
        TyKind::Region(_) | TyKind::Anchored(..) => true,
        TyKind::Tuple(xs) | TyKind::Adt(_, xs) => xs.iter().any(|&x| names_region(t, x)),
        TyKind::View(x) | TyKind::Array(x, _) => names_region(t, *x),
        TyKind::Fn(ps, r, ..) => ps.iter().any(|&(_, x)| names_region(t, x)) || names_region(t, *r),
        _ => false,
    }
}
