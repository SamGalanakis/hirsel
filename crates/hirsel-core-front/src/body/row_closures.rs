//! A closed type's row closure, built once per program and copied into each body.
//!
//! Bodies are lowered into modules of their own, so every body that first names a
//! closed row runs the row's layout and every hook layout gives it (its finalizer,
//! clone and codec makers, a tuple's codec), all again. A row's closure is the same in
//! every body (one name is one closure, FORM #109); only its numbering differs. This
//! cache keeps the closure as a fresh emitter builds it, from empty, and copies it into
//! a body whose module holds none of it, numbered after the body's own rows.
//!
//! The copy is exact when nothing in the module is reached: a fresh build of the type
//! only ever finds rows, instances and signatures it made itself. In a module holding
//! none of the closure's rows and none of its argument-free instances or row-free
//! signatures, the same build finds the same things, so it makes the same rows,
//! functions and signatures in the same order, each numbered past the module's own.
//! When the module holds any of them, the build runs as before.

use super::Emitter;
use crate::Program;
use crate::types::*;
use hirsel_core_ir::{FuncId, InstanceKey, Param, PlaceTy, SigId, Signature, TypeId};
use hirsel_core_layout::{FuncRef, Shape, TypeRef};
use std::sync::Arc;

/// The program's closed row closures, by row name: the name the module interns the
/// row by, so one key is one closure.
#[derive(Default)]
pub(crate) struct RowClosures(std::sync::Mutex<rustc_hash::FxHashMap<String, Entry>>);

enum Entry {
    /// Named once and built in place: a type most programs name in one body only is
    /// not built twice.
    Seen,
    /// The closure, or None when a fresh build is refused, reaches generic or body
    /// state, or names a finalizer (whose recipe depends on when it is bound).
    Built(Option<Arc<Closure>>),
}

/// What a fresh emitter holds after building one type's row, numbered from 0.
struct Closure {
    row: TypeRef,
    types: Vec<Shape>,
    signatures: Vec<Signature>,
    instances: Vec<InstanceKey>,
    functions: Vec<hirsel_core_ir::Function>,
    funcs: Vec<(InstanceKey, FuncId)>,
    rows: Vec<(usize, FuncId)>,
    program_type_rows: Vec<(TyId, TypeId)>,
    source_signatures: Vec<Option<SigId>>,
    interned: Vec<((Signature, super::scalar_abi::SignatureAbi), SigId)>,
    abis: Vec<(SigId, super::scalar_abi::SignatureAbi)>,
    /// How many of `types` had recipes when the build ended.
    recipes: usize,
    /// The argument-free instances and row-free signatures, which a module may hold
    /// already.
    open_instances: Vec<usize>,
    open_signatures: Vec<usize>,
}

impl Emitter {
    /// The row of closed `ty`, which no row of the module is named for yet.
    pub(super) fn closed_row(
        &mut self,
        p: &Program,
        t: &mut impl TyRead,
        ty: TyId,
    ) -> Option<TypeRef> {
        if has_param(t, ty) {
            return self.shaped(p, t, |s| s.shape(ty));
        }
        let key = match crate::decls::with_layout_name(&p.decls, &*t, ty, |name| {
            match self.types.get(name.as_ref()) {
                Some(&row) => Err(row),
                None => Ok(name.into_owned()),
            }
        }) {
            Ok(name) => name,
            Err(row) => return Some(row),
        };
        let built = match p.row_closures.0.lock().unwrap().entry(key.clone()) {
            std::collections::hash_map::Entry::Vacant(v) => {
                v.insert(Entry::Seen);
                None
            }
            std::collections::hash_map::Entry::Occupied(o) => match o.get() {
                Entry::Built(closure) => Some(closure.clone()),
                Entry::Seen => Some(None),
            },
        };
        // Built outside the lock: the build names other rows, whose closures it may
        // look up. Two bodies building one closure at once build the same one.
        let built = match built {
            Some(None) => {
                let closure = Closure::build(p, &mut *t, ty).map(Arc::new);
                p.row_closures
                    .0
                    .lock()
                    .unwrap()
                    .insert(key, Entry::Built(closure.clone()));
                Some(closure)
            }
            built => built,
        };
        match built.flatten() {
            Some(closure) if self.holds_none_of(&closure) && self.recipes_ready(p, &closure) => {
                Some(self.copy_closure(p, &closure))
            }
            _ => self.shaped(p, t, |s| s.shape(ty)),
        }
    }

    /// The module holds nothing the closure's fresh build would have found in it.
    fn holds_none_of(&self, c: &Closure) -> bool {
        c.types.iter().all(|s| !self.types.contains_key(&s.name))
            && c.rows.iter().all(|(f, _)| !self.rows.contains_key(f))
            && c.open_instances
                .iter()
                .all(|&i| !self.funcs.contains_key(&c.instances[i]))
            && c.open_signatures
                .iter()
                .all(|&i| !self.module.signatures.contains(&c.signatures[i]))
    }

    /// A build that reaches a recipe extends the module's pending recipes first, with
    /// the module's rows as they are: done here, once, before anything is copied. A
    /// refused extension leaves the build to run as before.
    fn recipes_ready(&mut self, p: &Program, c: &Closure) -> bool {
        let rows = self.module.types.len();
        if c.recipes == 0 || self.recipes.len() == rows {
            return true;
        }
        self.recipe(p, TypeId(rows as u32 - 1), rows).is_some()
    }

    fn copy_closure(&mut self, p: &Program, c: &Closure) -> TypeRef {
        let rows = self.module.types.len() as u32;
        let functions = self.module.functions.len() as u32;
        let instances = self.module.instances.len() as u32;
        let signatures = self.module.signatures.len() as u32;
        let row = |r: TypeId| TypeId(r.0 + rows);
        let function = |f: FuncId| FuncId(f.0 + functions);
        let signature = |s: SigId| SigId(s.0 + signatures);
        let key = |k: &InstanceKey| InstanceKey {
            definition: k.definition.clone(),
            arguments: k.arguments.iter().map(|&a| row(a)).collect(),
            dictionaries: k.dictionaries.clone(),
        };
        let shape_at = |i: usize| {
            let mut s = c.types[i].clone();
            s.map_refs(&mut |r| TypeRef(r.0 + rows));
            for hook in [&mut s.finalizer, &mut s.clone, &mut s.codec] {
                if let Some(f) = hook {
                    f.0 += functions;
                }
            }
            s
        };
        self.module
            .signatures
            .extend(c.signatures.iter().map(|s| renumber(s, rows)));
        self.module.instances.extend(c.instances.iter().map(key));
        self.module
            .functions
            .extend(c.functions.iter().map(|f| hirsel_core_ir::Function {
                instance: hirsel_core_ir::InstanceId(f.instance.0 + instances),
                signature: signature(f.signature),
                body: None,
                versioned: f.versioned,
            }));
        // The fresh build's recipes covered its first `c.recipes` rows, each made from
        // those rows alone: extend over exactly them, then add the rest.
        for i in 0..c.recipes {
            let s = shape_at(i);
            self.types.insert(s.name.clone(), TypeRef(rows + i as u32));
            self.module.types.push(s);
        }
        if c.recipes > 0 {
            let last = self.module.types.len();
            let extended = self.recipe(p, TypeId(last as u32 - 1), last).is_some();
            debug_assert!(extended, "the fresh build extended these recipes");
        }
        for i in c.recipes..c.types.len() {
            let s = shape_at(i);
            self.types.insert(s.name.clone(), TypeRef(rows + i as u32));
            self.module.types.push(s);
        }
        for (k, f) in &c.funcs {
            self.funcs.insert(key(k), function(*f));
        }
        for &(f, func) in &c.rows {
            self.rows.insert(f, function(func));
        }
        for &(ty, r) in &c.program_type_rows {
            self.program_type_rows.insert(ty, row(r));
        }
        for (i, s) in c.source_signatures.iter().enumerate() {
            if let Some(s) = s {
                self.source_signatures
                    .remember(FuncId(functions + i as u32), signature(*s));
            }
        }
        for ((s, abi), id) in &c.interned {
            self.source_signatures
                .signatures
                .insert((renumber(s, rows), abi.clone()), signature(*id));
        }
        for (id, abi) in &c.abis {
            self.source_signatures
                .abis
                .insert(signature(*id), abi.clone());
        }
        TypeRef(c.row.0 + rows)
    }
}

impl Closure {
    /// A fresh emitter's row of `ty`, or None when the copy would not be the build.
    fn build(p: &Program, t: &mut impl TyRead, ty: TyId) -> Option<Closure> {
        let mut e = Emitter::new();
        let row = e.shaped(p, t, |s| s.shape(ty))?;
        let reached_body_state = e.refused
            || !e.generic_rows.is_empty()
            || !e.generic_names.is_empty()
            || !e.generic_codecs.is_empty()
            || !e.generic_calls.is_empty()
            || !e.items.is_empty()
            || !e.statics.is_empty()
            || !e.constant_statics.is_empty()
            || !e.callable_adapters.is_empty()
            || !e.module.items.is_empty()
            || !e.module.statics.is_empty()
            || !e.module.code_variants.is_empty()
            || !e.module.step_free.is_empty()
            || e.module.functions.iter().any(|f| f.body.is_some());
        let unbound = |f: &Option<FuncRef>| f.is_some_and(|f| f.0 == u32::MAX);
        if reached_body_state
            || e.module
                .types
                .iter()
                .any(|s| s.finalizer.is_some() || unbound(&s.clone) || unbound(&s.codec))
        {
            return None;
        }
        let open_instances = (0..e.module.instances.len())
            .filter(|&i| e.module.instances[i].arguments.is_empty())
            .collect();
        let open_signatures = (0..e.module.signatures.len())
            .filter(|&i| names_no_row(&e.module.signatures[i]))
            .collect();
        let functions = e.module.functions.len();
        Some(Closure {
            row,
            recipes: e.recipes.len(),
            source_signatures: (0..functions)
                .map(|f| e.source_signatures.get(FuncId(f as u32)))
                .collect(),
            interned: e.source_signatures.signatures.into_iter().collect(),
            abis: e.source_signatures.abis.into_iter().collect(),
            funcs: e.funcs.into_iter().collect(),
            rows: e.rows.into_iter().collect(),
            program_type_rows: e.program_type_rows.into_iter().collect(),
            types: e.module.types,
            signatures: e.module.signatures,
            instances: e.module.instances,
            functions: e.module.functions,
            open_instances,
            open_signatures,
        })
    }
}

fn places(s: &Signature) -> impl Iterator<Item = TypeId> + '_ {
    s.params
        .iter()
        .filter_map(|p| match p {
            Param::Address {
                place: PlaceTy::Declared(r),
                ..
            } => Some(*r),
            _ => None,
        })
        .chain(s.dest)
}

fn names_no_row(s: &Signature) -> bool {
    places(s).next().is_none()
}

fn renumber(s: &Signature, rows: u32) -> Signature {
    let mut s = s.clone();
    for p in &mut s.params {
        if let Param::Address {
            place: PlaceTy::Declared(r),
            ..
        } = p
        {
            r.0 += rows;
        }
    }
    if let Some(d) = &mut s.dest {
        d.0 += rows;
    }
    s
}
