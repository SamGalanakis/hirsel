//! A closed type's row closure, built once per program and copied into each body.
//!
//! Bodies are lowered into modules of their own, so every body that first names a
//! closed row runs the row's layout and every hook layout gives it (its finalizer,
//! clone and codec makers, a tuple's codec), all again. A row's closure is the same in
//! every body (one name is one closure, FORM #109); only its numbering differs. This
//! cache keeps the closure as a fresh emitter builds it, from empty, and copies it into
//! a body whose module holds none of it, numbered after the body's own rows.
//!
//! A build's output is a function of its types alone: one interner numbers every
//! signature it makes, a found instance keeps the signature it was made with, a hook
//! maker fails or not by its type, and the pending recipes are extended before a copy,
//! so the build's own extensions cover its own rows. A module that already holds some
//! of the closure's rows, instances or signatures (each found by the key the build looks
//! it up by) holds everything the build would make for them, since it made or copied
//! them the same way; the build in that module finds each held item where the fresh
//! build made it and makes every other item in the fresh build's order. So the copy maps
//! each held item to the module's and adds the rest in closure order, naming the
//! module's items: a module holding none of them takes the closure numbered past its
//! own. A held item whose content is not the closure's, mapped (a signature a body
//! installed, say), or a module that refused a layout, takes the build instead.

use super::Emitter;
use super::scalar_abi::SignatureAbi;
use crate::Program;
use crate::types::*;
use hirsel_core_ir::{FuncId, InstanceKey, Param, PlaceTy, SigId, Signature, TypeId};
use hirsel_core_layout::{FuncRef, Recipe, Row, Shape, TypeRef};
use std::sync::Arc;

/// The program's closed row closures, by row name: the name the module interns the
/// row by, so one key is one closure.
#[derive(Default)]
pub(crate) struct RowClosures {
    built: std::sync::Mutex<rustc_hash::FxHashMap<String, Entry>>,
    /// Every body builds every row it names (`Program::build_rows_fresh`).
    fresh: bool,
}

enum Entry {
    /// Named once and built in place: a type most programs name in one body only is
    /// not built twice.
    Seen,
    /// The closure, or None when a fresh build is refused, reaches generic or body
    /// state, or names a finalizer (whose recipe depends on when it is bound).
    Built(Option<Arc<Closure>>),
}

/// Which naming of a row's closure this is, by the program's cache.
#[derive(Clone, Copy, PartialEq)]
enum Naming {
    First,
    Second,
    Built,
}

/// What a module holds before a first naming builds a row in place.
struct Mark {
    rows: usize,
    signatures: usize,
    instances: usize,
    functions: usize,
    /// The sizes of the instance, definition-row, program-type-row and signature
    /// maps.
    maps: [usize; 4],
    body_state: [usize; 12],
}

/// What a fresh emitter holds after building one type's row, numbered from 0. Function
/// `i` is instance `i`, and every function and signature is interned by its key.
struct Closure {
    row: TypeRef,
    types: Vec<Row>,
    signatures: Vec<Signature>,
    /// Each signature's interner convention, by signature.
    conventions: Vec<SignatureAbi>,
    instances: Vec<InstanceKey>,
    functions: Vec<hirsel_core_ir::Function>,
    rows: Vec<(usize, FuncId)>,
    program_type_rows: Vec<(TyId, TypeId)>,
    source_signatures: Vec<SigId>,
    /// The recipes of the first rows of `types`, as the build ended with them.
    recipes: Vec<hirsel_core_layout::RecipeRow>,
    /// The argument-free instances and row-free signatures, which a module may hold
    /// without holding a row of the closure.
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
        if has_param(t, ty) || p.row_closures.fresh {
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
        let (naming, closure) = match p.row_closures.built.lock().unwrap().entry(key.clone()) {
            std::collections::hash_map::Entry::Vacant(v) => {
                v.insert(Entry::Seen);
                (Naming::First, None)
            }
            std::collections::hash_map::Entry::Occupied(o) => match o.get() {
                Entry::Built(closure) => (Naming::Built, closure.clone()),
                Entry::Seen => (Naming::Second, None),
            },
        };
        if naming == Naming::First {
            // Built in place. When that build found nothing the module held, what it
            // added is the fresh build numbered past the module's own: the closure,
            // with no second build at the next naming.
            let mark = self.mark();
            let row = self.shaped(p, t, |s| s.shape(ty))?;
            if let Some(closure) = self.added_closure(&mark, row)
                && let Some(entry @ Entry::Seen) =
                    p.row_closures.built.lock().unwrap().get_mut(&key)
            {
                *entry = Entry::Built(Some(Arc::new(closure)));
            }
            return Some(row);
        }
        // Built outside the lock: the build names other rows, whose closures it may
        // look up. Two bodies building one closure at once build the same one. A
        // refused build stays refused, as the same types refuse it again.
        let closure = if naming == Naming::Second {
            let closure = Closure::build(p, &mut *t, ty).map(Arc::new);
            p.row_closures
                .built
                .lock()
                .unwrap()
                .insert(key, Entry::Built(closure.clone()));
            closure
        } else {
            closure
        };
        match closure {
            Some(closure) if self.holds_none_of(&closure) => {
                if self.recipes_ready(p, &closure) {
                    return Some(self.copy_closure(&closure));
                }
            }
            Some(closure) => {
                if let Some(row) = self.copy_over(p, &closure) {
                    return Some(row);
                }
            }
            None => {}
        }
        self.shaped(p, t, |s| s.shape(ty))
    }

    /// The module holds nothing the closure's fresh build would have found in it.
    fn holds_none_of(&self, c: &Closure) -> bool {
        c.types.iter().all(|s| !self.types.contains_key(&s.name))
            && c.rows.iter().all(|(f, _)| !self.rows.contains_key(f))
            && c.open_instances
                .iter()
                .all(|&i| !self.funcs.contains_key(&c.instances[i]))
            && c.open_signatures.iter().all(|&i| {
                !self
                    .source_signatures
                    .signatures
                    .contains_key(&(c.signatures[i].clone(), c.conventions[i].clone()))
            })
    }

    fn mark(&self) -> Mark {
        Mark {
            rows: self.module.types.len(),
            signatures: self.module.signatures.len(),
            instances: self.module.instances.len(),
            functions: self.module.functions.len(),
            maps: self.maps(),
            body_state: self.body_state(),
        }
    }

    fn maps(&self) -> [usize; 4] {
        [
            self.funcs.len(),
            self.rows.len(),
            self.program_type_rows.len(),
            self.source_signatures.signatures.len(),
        ]
    }

    /// The sizes of the state `Closure::build` refuses a closure for reaching.
    fn body_state(&self) -> [usize; 12] {
        [
            self.generic_rows.len(),
            self.generic_names.len(),
            self.generic_codecs.len(),
            self.generic_calls.len(),
            self.items.len(),
            self.statics.len(),
            self.constant_statics.len(),
            self.callable_adapters.len(),
            self.module.items.len(),
            self.module.statics.len(),
            self.module.code_variants.len(),
            self.module.step_free.len(),
        ]
    }

    /// What the build in place of `row` added after `m`, numbered from 0: the closure
    /// a fresh build makes, as the module held none of it. None when an added item
    /// names one the module held before `m` (the module held part of the closure), or
    /// when `Closure::build` would refuse the closure.
    fn added_closure(&self, m: &Mark, row: TypeRef) -> Option<Closure> {
        let (rows, signatures) = (m.rows as u32, m.signatures as u32);
        let (instances, functions) = (m.instances as u32, m.functions as u32);
        let n = self.module.functions.len() - m.functions;
        if self.refused
            || (row.0 as usize) < m.rows
            || self.body_state() != m.body_state
            || self.module.instances.len() - m.instances != n
        {
            return None;
        }
        let held = std::cell::Cell::new(false);
        let below = |x: u32, base: u32| {
            held.set(held.get() || x < base);
            x.wrapping_sub(base)
        };
        let unbound = |f: &Option<FuncRef>| f.is_some_and(|f| f.0 == u32::MAX);
        let mut types = Vec::with_capacity(self.module.types.len() - m.rows);
        for s in &self.module.types[m.rows..] {
            if s.finalizer.is_some()
                || unbound(&s.clone)
                || unbound(&s.codec)
                || unbound(&s.key_codec)
            {
                return None;
            }
            if !s.numbered() {
                types.push(Row::clone(s));
                continue;
            }
            let mut s = Shape::clone(s);
            s.map_refs(&mut |r| TypeRef(below(r.0, rows)));
            for f in [&mut s.clone, &mut s.codec, &mut s.key_codec]
                .into_iter()
                .flatten()
            {
                f.0 = below(f.0, functions);
            }
            types.push(Row::new(s));
        }
        let signatures_added: Vec<Signature> = self.module.signatures[m.signatures..]
            .iter()
            .map(|s| map_rows(s, |r| TypeId(below(r.0, rows))))
            .collect();
        let instances_added: Vec<InstanceKey> = self.module.instances[m.instances..]
            .iter()
            .map(|k| InstanceKey {
                definition: k.definition.clone(),
                arguments: k
                    .arguments
                    .iter()
                    .map(|a| TypeId(below(a.0, rows)))
                    .collect(),
                dictionaries: k.dictionaries.clone(),
            })
            .collect();
        let mut functions_added = Vec::with_capacity(n);
        let mut source_signatures = Vec::with_capacity(n);
        for (i, f) in self.module.functions[m.functions..].iter().enumerate() {
            let id = FuncId(functions + i as u32);
            if f.body.is_some() || f.instance.0 != instances + i as u32 {
                return None;
            }
            source_signatures.push(SigId(below(self.source_signatures.get(id)?.0, signatures)));
            functions_added.push(hirsel_core_ir::Function {
                instance: hirsel_core_ir::InstanceId(i as u32),
                signature: SigId(below(f.signature.0, signatures)),
                body: None,
                versioned: f.versioned,
            });
        }
        // The maps only gain entries, each added function's under its instance: every
        // added function and signature is interned, as a fresh build leaves them. A
        // map that gained nothing is not searched.
        let [funcs, defined, typed, interned] = m.maps;
        let now = self.maps();
        if now[0] - funcs != n || now[3] - interned != signatures_added.len() {
            return None;
        }
        let mut conventions = vec![None; signatures_added.len()];
        if now[3] > interned {
            for ((_, abi), id) in &self.source_signatures.signatures {
                if id.0 >= signatures {
                    conventions[(id.0 - signatures) as usize] = Some(abi.clone());
                }
            }
        }
        // An added entry naming a row or function held before `m` is the module's.
        let rows_added: Vec<(usize, FuncId)> = if now[1] > defined {
            self.rows
                .iter()
                .filter(|(_, f)| f.0 >= functions)
                .map(|(&d, f)| (d, FuncId(f.0 - functions)))
                .collect()
        } else {
            vec![]
        };
        let program_type_rows: Vec<(TyId, TypeId)> = if now[2] > typed {
            self.program_type_rows
                .iter()
                .filter(|(_, r)| r.0 >= rows)
                .map(|(&ty, r)| (ty, TypeId(r.0 - rows)))
                .collect()
        } else {
            vec![]
        };
        if rows_added.len() != now[1] - defined || program_type_rows.len() != now[2] - typed {
            return None;
        }
        let conventions: Option<Vec<SignatureAbi>> = conventions.into_iter().collect();
        let recipes = self
            .recipes
            .get(m.rows..)
            .unwrap_or_default()
            .iter()
            .map(|r| {
                if !r.names_row() {
                    return Arc::clone(r);
                }
                let mut r = Recipe::clone(r);
                r.map_refs(|x| TypeRef(below(x.0, rows)));
                Arc::new(r)
            })
            .collect();
        if held.get() {
            return None;
        }
        Some(Closure {
            row: TypeRef(row.0 - rows),
            types,
            open_signatures: (0..signatures_added.len())
                .filter(|&i| names_no_row(&signatures_added[i]))
                .collect(),
            signatures: signatures_added,
            conventions: conventions?,
            open_instances: (0..instances_added.len())
                .filter(|&i| instances_added[i].arguments.is_empty())
                .collect(),
            instances: instances_added,
            functions: functions_added,
            rows: rows_added,
            program_type_rows,
            source_signatures,
            recipes,
        })
    }

    /// The closure in a module holding some of it: each held row, instance and
    /// signature is the module's, found by the key the build finds it by, and the rest
    /// are added in closure order. None, leaving the build to run, when a held item's
    /// content is not the closure's mapped onto the module, or the module refused a
    /// layout (whose rows it took back, leaving what named them).
    fn copy_over(&mut self, p: &Program, c: &Closure) -> Option<TypeRef> {
        crate::cost_scope!("phase", "row closure overlap copy");
        if self.refused || !self.recipes_ready(p, c) {
            return None;
        }
        let rows = self.module.types.len() as u32;
        let signatures = self.module.signatures.len() as u32;
        let functions = self.module.functions.len() as u32;
        let instances = self.module.instances.len() as u32;
        let (mut next, mut fresh_rows) = (rows, vec![]);
        let at: Vec<TypeRef> = c
            .types
            .iter()
            .enumerate()
            .map(|(i, s)| {
                self.types.get(&s.name).copied().unwrap_or_else(|| {
                    fresh_rows.push(i);
                    next += 1;
                    TypeRef(next - 1)
                })
            })
            .collect();
        let row = |r: TypeId| TypeId(at[r.index()].0);
        let held = |r: TypeId| at[r.index()].0 < rows;
        // An instance or signature naming an added row is not the module's: only one
        // naming held rows alone is looked up.
        let (mut next, mut fresh_signatures) = (signatures, vec![]);
        let sig_at: Vec<SigId> = c
            .signatures
            .iter()
            .zip(&c.conventions)
            .map(|(s, abi)| {
                let key = (map_rows(s, row), abi.clone());
                let found = places(s)
                    .all(held)
                    .then(|| self.source_signatures.signatures.get(&key))
                    .flatten();
                match found {
                    Some(&id) => id,
                    None => {
                        fresh_signatures.push(key);
                        next += 1;
                        SigId(next - 1)
                    }
                }
            })
            .collect();
        let (mut next, mut fresh_functions) = (functions, vec![]);
        let func_at: Vec<FuncId> = c
            .instances
            .iter()
            .enumerate()
            .map(|(i, k)| {
                let key = InstanceKey {
                    definition: k.definition.clone(),
                    arguments: k.arguments.iter().map(|&a| row(a)).collect(),
                    dictionaries: k.dictionaries.clone(),
                };
                let found = k
                    .arguments
                    .iter()
                    .all(|&a| held(a))
                    .then(|| self.funcs.get(&key))
                    .flatten();
                match found {
                    Some(&f) => f,
                    None => {
                        fresh_functions.push((i, key));
                        next += 1;
                        FuncId(next - 1)
                    }
                }
            })
            .collect();
        let shape = |i: usize| {
            if !c.types[i].numbered() {
                return Row::clone(&c.types[i]);
            }
            let mut s = Shape::clone(&c.types[i]);
            s.map_refs(&mut |r| at[r.index()]);
            for hook in [
                &mut s.finalizer,
                &mut s.clone,
                &mut s.codec,
                &mut s.key_codec,
            ] {
                if let Some(f) = hook {
                    *f = FuncRef(func_at[f.0 as usize].0);
                }
            }
            Row::new(s)
        };
        // A held row is the closure's by its name: in a module that took no row back
        // (`refused`), the rows it names are held under the closure's names. Its hooks
        // are the functions the closure's map to.
        let hook = |f: Option<FuncRef>| f.map(|f| FuncRef(func_at[f.0 as usize].0));
        let held_rows_agree = (0..c.types.len()).filter(|&i| at[i].0 < rows).all(|i| {
            let (s, own) = (&c.types[i], &self.module.types[at[i].index()]);
            hook(s.finalizer) == own.finalizer
                && hook(s.clone) == own.clone
                && hook(s.codec) == own.codec
                && hook(s.key_codec) == own.key_codec
        });
        let held_functions_agree = (0..c.functions.len())
            .filter(|&i| func_at[i].0 < functions)
            .all(|i| {
                let (f, held) = (&c.functions[i], func_at[i]);
                let own = &self.module.functions[held.index()];
                own.signature == sig_at[f.signature.index()]
                    && own.versioned == f.versioned
                    && self.source_signatures.get(held)
                        == Some(sig_at[c.source_signatures[i].index()])
            });
        let held_rows_map_agrees = c
            .rows
            .iter()
            .all(|(f, func)| self.rows.get(f).is_none_or(|&g| g == func_at[func.index()]));
        if !(held_rows_agree && held_functions_agree && held_rows_map_agrees) {
            return None;
        }
        // The module's recipes reach its last row (`recipes_ready`); the fresh build's
        // covered its first rows, resolved by row name, and the added rows among them
        // come first in closure order.
        if !c.recipes.is_empty() {
            debug_assert_eq!(self.recipes.len(), rows as usize);
            for &i in fresh_rows.iter().take_while(|&&i| i < c.recipes.len()) {
                let r = &c.recipes[i];
                if !r.names_row() {
                    self.recipes.push(Arc::clone(r));
                    continue;
                }
                let mut r = Recipe::clone(r);
                r.map_refs(|x| at[x.index()]);
                self.recipes.push(Arc::new(r));
            }
        }
        for (key, id) in fresh_signatures.into_iter().zip(signatures..) {
            let id = SigId(id);
            self.module.signatures.push(key.0.clone());
            if key.1.result || key.1.params.iter().any(|x| *x) {
                self.source_signatures.abis.insert(id, key.1.clone());
            }
            self.source_signatures.signatures.insert(key, id);
        }
        for ((i, key), n) in fresh_functions.into_iter().zip(0..) {
            let f = &c.functions[i];
            let id = FuncId(functions + n);
            self.module.instances.push(key.clone());
            self.module.functions.push(hirsel_core_ir::Function {
                instance: hirsel_core_ir::InstanceId(instances + n),
                signature: sig_at[f.signature.index()],
                body: None,
                versioned: f.versioned,
            });
            self.source_signatures
                .remember(id, sig_at[c.source_signatures[i].index()]);
            self.funcs.insert(key, id);
        }
        for &i in &fresh_rows {
            let s = shape(i);
            self.types.insert(s.name.clone(), at[i]);
            self.module.types.push(s);
        }
        for &(f, func) in &c.rows {
            self.rows.entry(f).or_insert(func_at[func.index()]);
        }
        for &(ty, r) in &c.program_type_rows {
            self.program_type_rows.insert(ty, row(r));
        }
        Some(at[c.row.index()])
    }

    /// A build that reaches a recipe extends the module's pending recipes first, with
    /// the module's rows as they are: done here, once, before anything is copied. A
    /// refused extension leaves the build to run as before.
    fn recipes_ready(&mut self, p: &Program, c: &Closure) -> bool {
        let rows = self.module.types.len();
        if c.recipes.is_empty() || self.recipes.len() == rows {
            return true;
        }
        self.recipe(p, TypeId(rows as u32 - 1), rows).is_some()
    }

    fn copy_closure(&mut self, c: &Closure) -> TypeRef {
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
            if !c.types[i].numbered() {
                return Row::clone(&c.types[i]);
            }
            let mut s = Shape::clone(&c.types[i]);
            s.map_refs(&mut |r| TypeRef(r.0 + rows));
            for f in [
                &mut s.finalizer,
                &mut s.clone,
                &mut s.codec,
                &mut s.key_codec,
            ]
            .into_iter()
            .flatten()
            {
                f.0 += functions;
            }
            Row::new(s)
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
        // The fresh build's recipes covered its first rows, each made from those rows
        // alone and resolved by row name, as an extension over them here resolves
        // them: the module's recipes reach its last row (`recipes_ready`), so they are
        // the closure's, numbered past the module's.
        if !c.recipes.is_empty() {
            debug_assert_eq!(self.recipes.len(), rows as usize);
            self.recipes.extend(c.recipes.iter().map(|r| {
                if !r.names_row() {
                    return Arc::clone(r);
                }
                let mut r = Recipe::clone(r);
                r.map_refs(|x| TypeRef(x.0 + rows));
                Arc::new(r)
            }));
        }
        for i in 0..c.types.len() {
            let s = shape_at(i);
            self.types.insert(s.name.clone(), TypeRef(rows + i as u32));
            self.module.types.push(s);
        }
        for &(f, func) in &c.rows {
            self.rows.insert(f, function(func));
        }
        for &(ty, r) in &c.program_type_rows {
            self.program_type_rows.insert(ty, row(r));
        }
        for (i, k) in c.instances.iter().enumerate() {
            let f = function(FuncId(i as u32));
            self.funcs.insert(key(k), f);
            self.source_signatures
                .remember(f, signature(c.source_signatures[i]));
        }
        for (i, (s, abi)) in c.signatures.iter().zip(&c.conventions).enumerate() {
            let id = signature(SigId(i as u32));
            if abi.result || abi.params.iter().any(|x| *x) {
                self.source_signatures.abis.insert(id, abi.clone());
            }
            self.source_signatures
                .signatures
                .insert((renumber(s, rows), abi.clone()), id);
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
            || e.module.types.iter().any(|s| {
                s.finalizer.is_some()
                    || unbound(&s.clone)
                    || unbound(&s.codec)
                    || unbound(&s.key_codec)
            })
        {
            return None;
        }
        let functions = e.module.functions.len();
        let mut conventions = vec![None; e.module.signatures.len()];
        for ((_, abi), id) in std::mem::take(&mut e.source_signatures.signatures) {
            conventions[id.index()] = Some(abi);
        }
        let interned = conventions.iter().all(Option::is_some)
            && e.funcs.len() == functions
            && e.funcs
                .values()
                .all(|f| e.module.functions[f.index()].instance.index() == f.index());
        let source_signatures: Option<Vec<SigId>> = (0..functions)
            .map(|f| e.source_signatures.get(FuncId(f as u32)))
            .collect();
        let (true, Some(source_signatures)) = (interned, source_signatures) else {
            return None;
        };
        let open_instances = (0..e.module.instances.len())
            .filter(|&i| e.module.instances[i].arguments.is_empty())
            .collect();
        let open_signatures = (0..e.module.signatures.len())
            .filter(|&i| names_no_row(&e.module.signatures[i]))
            .collect();
        Some(Closure {
            row,
            recipes: e.recipes,
            source_signatures,
            conventions: conventions.into_iter().flatten().collect(),
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
    map_rows(s, |r| TypeId(r.0 + rows))
}

fn map_rows(s: &Signature, row: impl Fn(TypeId) -> TypeId) -> Signature {
    let mut s = s.clone();
    for p in &mut s.params {
        if let Param::Address {
            place: PlaceTy::Declared(r),
            ..
        } = p
        {
            *r = row(*r);
        }
    }
    if let Some(d) = &mut s.dest {
        *d = row(*d);
    }
    s
}

impl Program<'_> {
    /// Build every row in each body that names it, copying no closure: the fresh build
    /// a copy stands for.
    pub fn build_rows_fresh(&mut self) {
        self.row_closures.fresh = true;
    }
}
