//! PACK-14/16 closed-type witnesses and frontend checking for `core.code`.
//! PACK-16 source edges supplement the native type closure, which erases callable fields.
use super::*;
use crate::identity::closed_type_text;
use hirsel_core_ids::ContentHash;
use std::collections::BTreeMap;

#[cfg(test)]
#[path = "pack_witness_tests.rs"]
mod tests;

fn count(bytes: &mut Vec<u8>, n: usize) -> Option<()> {
    bytes.extend_from_slice(&u32::try_from(n).ok()?.to_le_bytes());
    Some(())
}

fn text(bytes: &mut Vec<u8>, s: &str) -> Option<()> {
    count(bytes, s.len())?;
    bytes.extend_from_slice(s.as_bytes());
    Some(())
}

impl Emitter {
    fn pack_native_hash(
        &mut self,
        p: &Program,
        t: &mut impl TyRead,
        ty: TyId,
    ) -> Option<ContentHash> {
        match t.kind(ty) {
            TyKind::View(inner) | TyKind::Anchored(inner, _) => {
                let inner = *inner;
                self.pack_native_hash(p, t, inner)
            }
            TyKind::Scalar(s) => Some(ContentHash::of_parts(
                "hirsel.core-closure.v1",
                [s.name().as_bytes()],
            )),
            TyKind::Unit => Some(ContentHash::of_parts(
                "hirsel.core-closure.v1",
                [b"()".as_slice()],
            )),
            TyKind::Adt(d, _) if p.decls[d.index()].role == Some(Role::Region) => Some(
                ContentHash::of_parts("hirsel.core-closure.v1", [b"core@1/core/Region".as_slice()]),
            ),
            _ => {
                let row = self.type_row(p, t, ty)?;
                Some(self.module.closure(row).hash)
            }
        }
    }

    pub fn held_type_hash(
        &mut self,
        program: &Program,
        types: &mut impl TyRead,
        root: TyId,
    ) -> Option<ContentHash> {
        let name = |types: &_, ty| closed_type_text(&program.decls, types, ty);
        let root_text = name(types, root)?;
        let native = self.pack_native_hash(program, types, root)?;
        let mut nodes = BTreeMap::new();
        let mut pending = vec![root];
        while let Some(ty) = pending.pop() {
            let spelling = name(types, ty)?;
            if nodes.contains_key(&spelling) {
                continue;
            }
            let mut edges: [Vec<TyId>; 4] = Default::default();
            match types.kind(ty).clone() {
                TyKind::Adt(id, args) => {
                    edges[3] = program.decls[id.index()]
                        .all_fields()
                        .map(|field| subst(types, field.ty, &args))
                        .collect();
                    edges[0] = args;
                }
                TyKind::Fn(params, result, ..) => {
                    edges[1].extend(params.into_iter().map(|(_, ty)| ty));
                    edges[2].push(result);
                }
                TyKind::Tuple(args) => edges[0] = args,
                TyKind::Array(arg, _) | TyKind::View(arg) => edges[0].push(arg),
                TyKind::Anchored(inner, region) => edges[0].extend([inner, region]),
                TyKind::Unit
                | TyKind::Scalar(_)
                | TyKind::Never
                | TyKind::Region(_)
                | TyKind::Vector(_) => {}
                TyKind::Param(_) | TyKind::Hole | TyKind::Error => return None,
            }
            let mut node = Vec::new();
            text(&mut node, &spelling)?;
            match types.kind(ty) {
                TyKind::Fn(.., Callable::Plain)
                | TyKind::View(_)
                | TyKind::Region(_)
                | TyKind::Anchored(..) => node.push(0),
                TyKind::Adt(d, _) if program.decls[d.index()].role == Some(Role::Region) => {
                    node.push(0);
                }
                _ => {
                    node.push(1);
                    let closure = if ty == root {
                        native
                    } else {
                        self.pack_native_hash(program, types, ty)?
                    };
                    node.extend_from_slice(&closure.0);
                }
            }
            for list in &edges {
                count(&mut node, list.len())?;
                for &child in list {
                    text(&mut node, &name(types, child)?)?;
                }
            }
            pending.extend(edges.into_iter().flatten().rev());
            nodes.insert(spelling, node);
        }
        let mut graph = Vec::new();
        count(&mut graph, nodes.len())?;
        for node in nodes.into_values() {
            graph.extend(node);
        }
        Some(ContentHash::of_parts(
            "hirsel.held-type.v1",
            [root_text.as_bytes(), &native.0, &graph],
        ))
    }

    /// PACK-15 uses the native ABI's implicit context and status. Parameters match
    /// CoreIR's sealed calling convention; every nonempty result uses a caller-owned
    /// destination place, including a scalar result.
    pub fn pack_export_hash(
        &mut self,
        p: &Program,
        t: &mut impl TyRead,
        callable: TyId,
    ) -> Option<ContentHash> {
        let TyKind::Fn(parameters, result, _, Callable::Plain) = t.kind(callable).clone() else {
            return None;
        };
        let spelling = closed_type_text(&p.decls, t, callable)?;
        let mut parts = Vec::with_capacity(parameters.len() + 2);
        let mut params = Vec::new();
        for (mode, ty) in parameters {
            parts.push(self.held_type_hash(p, t, ty)?);
            let rep = self.rep(p, t, ty).ok()?;
            match (mode, rep) {
                (Mode::Mut, Rep::Place(row)) => params.push(Param::Address {
                    place: scalar_abi::mut_place(&self.module.types, row),
                    loan: Loan::Mut,
                }),
                (Mode::Mut, _) => params.push(Param::Address {
                    place: self.mut_place(p, t, ty, rep)?,
                    loan: Loan::Mut,
                }),
                (_, Rep::None) => {}
                (_, Rep::Scalar(value)) => params.push(Param::Value(value)),
                (_, Rep::Place(row)) => match self.module.types[row.index()].carried {
                    Carried::Value(value) => params.push(Param::Value(value)),
                    Carried::Nothing => {}
                    Carried::Address => params.push(Param::Address {
                        place: PlaceTy::Declared(row),
                        loan: loan(mode),
                    }),
                },
            }
        }
        parts.push(self.held_type_hash(p, t, result)?);
        let dest = if self.rep(p, t, result).ok()? == Rep::None {
            None
        } else {
            Some(self.type_row(p, t, result)?)
        };
        let mut adapter = Signature::new(params, vec![], dest);
        adapter.never = result == NEVER;
        parts.push(hirsel_core_ir::normal::abi_signature(
            &self.module,
            &adapter,
        ));
        Some(ContentHash::of_parts(
            "hirsel.pack-export.v1",
            std::iter::once(spelling.as_bytes()).chain(parts.iter().map(|h| h.0.as_slice())),
        ))
    }
}

impl Cx<'_, '_, '_> {
    pub(super) fn pack_call_overload(&self, f: usize, want: Option<Want>) -> (usize, bool) {
        let path = &self.p.fns[f].path;
        let reflect = if path.is_core("code.identity") {
            Some("code.loaded_identity")
        } else if path.is_core("code.revision") {
            Some("code.loaded_revision")
        } else {
            None
        };
        let send_held = path.is_core("code.hold") && want.is_some_and(|w| {
            matches!(
                self.t.kind(w.ty),
                TyKind::Adt(id, _) if self.p.decls[id.index()].path.is_core("code.TransferableHeld")
            )
        });
        let overload = if send_held {
            Some("code.hold_send")
        } else {
            reflect
        };
        let target = overload
            .and_then(|path| self.p.function_id(&DefPath::core(path)))
            .unwrap_or(f);
        (target, reflect.is_some())
    }

    pub(super) fn code_bind(
        &mut self,
        f: usize,
        argument: NodeId,
        want: Option<Want>,
        n: NodeId,
    ) -> (TyId, Option<ValueId>) {
        let Some(item) = self.named_callee(argument) else {
            if matches!(self.call_target(argument), Some(Target::Missing)) {
                self.halt();
            } else {
                self.refuse(
                    "PACK-14",
                    argument,
                    "code.bind requires a named top-level entry",
                );
            }
            return (ERROR, None);
        };
        if !self.p.fns[item].outlives.is_empty() {
            self.refuse(
                "PACK-14",
                argument,
                "the callable cannot express this entry's outlives constraints",
            );
            return (ERROR, None);
        }
        let Some(callable) = self.checked_function_item(item, argument) else {
            return (ERROR, None);
        };
        let expected = want.and_then(|w| match self.t.kind(w.ty) {
            TyKind::Adt(d, a) if self.p.decls[d.index()].path.is_core("core.Code") => Some(a[0]),
            _ => None,
        });
        let fd = &self.p.fns[item];
        let mut found = vec![None; fd.generics.len()];
        for (i, g) in fd.generics.iter().enumerate() {
            if g.region {
                found[i] = Some(self.t.mk(generic_arg(fd, i)));
            }
        }
        if let Some(expected) = expected
            && let TyKind::Fn(ps, r, _, _) = self.t.kind(callable).clone()
            && let TyKind::Fn(es, er, _, _) = self.t.kind(expected).clone()
        {
            for ((_, ty), (_, et)) in ps.into_iter().zip(es) {
                self.unify(ty, et, &mut found);
            }
            self.unify(r, er, &mut found);
        }
        for (g, arg) in fd.generics.iter().zip(&found) {
            if g.region {
                continue;
            }
            let name = self.p.index.symbols.text(g.name);
            let function = &fd.path.0.name;
            if let Some(ty) = arg {
                if has_hole(&self.t, *ty) {
                    self.refuse(
                        "PACK-14",
                        argument,
                        format!("code.bind leaves type parameter {name} of {function} open"),
                    );
                    return (ERROR, None);
                }
                if super::code_describe::names_region(&self.t, *ty) {
                    self.refuse(
                        "PACK-14",
                        argument,
                        format!(
                            "code.bind cannot name an instance whose type parameter {name} of {function} names a region"
                        ),
                    );
                    return (ERROR, None);
                }
            } else {
                self.refuse(
                    "PACK-14",
                    argument,
                    format!(
                        "code.bind leaves type parameter {name} of {function} open: the expected Code type must fix it"
                    ),
                );
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
        let args: Vec<_> = found.into_iter().map(Option::unwrap).collect();
        let ty = subst(&mut self.t, callable, &args);
        let result = subst(&mut self.t, self.p.fns[f].result, &[ty]);
        self.builtin_use(f, n);
        (result, self.pack_witness_call(ty, true, result, n))
    }

    pub(super) fn pack_named_call(
        &mut self,
        f: usize,
        tys: &[TyId],
        args: &[NodeId],
        result: TyId,
        n: NodeId,
    ) -> Option<(TyId, Option<ValueId>)> {
        let g = &self.p.fns[f];
        if !(g.path.0.modules == ["code"] && g.path.package_is("core", "1")) {
            return None;
        }
        match g.path.0.name.as_str() {
            "load" | "slot" | "activate" | "share" => {
                self.builtin_use(f, n);
                Some((result, self.pack_witness_call(tys[0], true, result, n)))
            }
            "hold" | "take_as" | "hold_send" => {
                if g.path.0.name == "take_as"
                    && !matches!(
                        self.t.kind(tys[1]),
                        TyKind::Adt(id, _)
                            if self.p.decls[id.index()].path.is_core("core.Held")
                                || self.p.decls[id.index()].path.is_core("code.TransferableHeld")
                    )
                {
                    self.refuse("PACK-16", args[0], "take_as consumes a Held value");
                    return Some((ERROR, None));
                }
                self.builtin_use(f, n);
                Some((result, self.pack_witness_call(tys[0], false, result, n)))
            }
            _ => None,
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn pack_reflect_call(
        &mut self,
        source_function: usize,
        f: usize,
        tys: &[TyId],
        vals: &[Option<ValueId>],
        arg: NodeId,
        result: TyId,
        n: NodeId,
    ) -> Option<ValueId> {
        match self.t.kind(self.bare(tys[0])).clone() {
            TyKind::Adt(id, a) if self.p.decls[id.index()].role == Some(Role::StaticFn) => {
                self.emit_call(source_function, &a, vals, result, n)
            }
            TyKind::Adt(id, _) if self.p.decls[id.index()].path.is_core("core.Code") => {
                self.emit_call(f, tys, vals, result, n)
            }
            TyKind::Error => None,
            _ => {
                self.refuse(
                    "PACK-14",
                    arg,
                    "code reflection requires a StaticFn or Code handle",
                );
                None
            }
        }
    }

    pub(super) fn pack_callee(
        &mut self,
        ty: TyId,
        dispatch: bool,
        callee: NodeId,
    ) -> Result<Option<TyId>, ()> {
        let entry = match self.t.kind(ty) {
            TyKind::Adt(id, args)
                if self.p.decls[id.index()].path.is_core(if dispatch {
                    "core.CodeSlot"
                } else {
                    "core.Code"
                }) =>
            {
                Some(args[0])
            }
            _ => None,
        };
        if dispatch && entry.is_none() {
            self.refuse("PACK-14", callee, "dispatch requires a typed code slot");
            return Err(());
        }
        Ok(entry)
    }

    pub(super) fn pack_waits_call_refused(
        &mut self,
        waits: bool,
        entry: Option<TyId>,
        dispatch: bool,
        n: NodeId,
    ) -> bool {
        if waits && entry.is_some() && !dispatch {
            self.refuse(
                "PACK-14",
                n,
                "a synchronous Call cannot invoke a waits entry",
            );
            true
        } else {
            false
        }
    }

    pub(super) fn finish_pack_call(
        &mut self,
        entry: TyId,
        dispatch: bool,
        result: TyId,
        n: NodeId,
    ) -> (TyId, Option<ValueId>) {
        let result = if dispatch {
            let Some(Def::Type(error)) = self.p.def(&DefPath::core("code.DispatchError")) else {
                return self.gap_ty(n, "code dispatch error type is unavailable");
            };
            let error = self.t.mk(TyKind::Adt(error, vec![]));
            self.t.mk(TyKind::Adt(
                self.p.core_decl("Result").unwrap(),
                vec![result, error],
            ))
        } else {
            result
        };
        let value = self.pack_witness_call(entry, true, result, n);
        if result == NEVER {
            self.dead = true;
        }
        (result, value)
    }

    pub(super) fn pack_witness_call(
        &mut self,
        ty: TyId,
        export: bool,
        result: TyId,
        at: NodeId,
    ) -> Option<ValueId> {
        if ty == ERROR {
            return None;
        }
        let (hash, helper) = if export {
            (
                self.e.pack_export_hash(self.p, &mut self.t, ty),
                "code.export_unbuilt",
            )
        } else {
            if !self.holdable(ty) {
                self.refuse(
                    "PACK-16",
                    at,
                    "Held requires an owned-lifetime payload without an uncounted code reference",
                );
                return None;
            }
            (
                self.e.held_type_hash(self.p, &mut self.t, ty),
                "code.held_unbuilt",
            )
        };
        let Some(hash) = hash else {
            self.refuse(
                if export { "PACK-14" } else { "PACK-16" },
                at,
                "the operation requires a closed type with a compiled witness",
            );
            return None;
        };
        if self.live() {
            let data = self.e.static_bytes(&hash.0);
            let ptr = self.value(Ty::Ptr);
            self.inst(Inst::ReadStatic { result: ptr, data }, at);
            let helper = self
                .p
                .function_id(&DefPath::core(helper))
                .expect("core.code witness operation");
            return self.emit_call(helper, &[result], &[Some(ptr)], result, at);
        }
        None
    }

    fn holdable(&mut self, root: TyId) -> bool {
        let mut pending = vec![root];
        let mut visited = HashSet::default();
        while let Some(ty) = pending.pop() {
            if !visited.insert(ty) {
                continue;
            }
            if matches!(
                self.type_kind(ty),
                Kind::Scoped | Kind::Region | Kind::Generic
            ) {
                return false;
            }
            match self.t.kind(ty).clone() {
                TyKind::Adt(d, args) => {
                    let decl = &self.p.decls[d.index()];
                    if matches!(decl.role, Some(Role::StaticFn | Role::ForeignHandle)) {
                        return false;
                    }
                    if !decl.pack_callable_carrier() {
                        pending.extend(decl.all_fields().map(|f| subst(&mut self.t, f.ty, &args)));
                        if decl.role.is_some() {
                            pending.extend(args);
                        }
                    }
                }
                TyKind::Tuple(xs) => pending.extend(xs),
                TyKind::Array(x, _) => pending.push(x),
                TyKind::Unit | TyKind::Scalar(_) | TyKind::Vector(_) | TyKind::Never => {}
                _ => return false,
            }
        }
        true
    }
}
