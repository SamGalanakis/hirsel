//! Extern dispatch (S-13): each extern's `hirsel_rt_v2_foreign_enter` dispatch word, from its
//! package's trusted `threads` and its `@blocking`, and the build check that a callback which
//! can reach a `home` package is handed only to a `home` package's extern.
use super::root_source::LinkSource;
use super::stages::{NativeError, ReadyProgram, StageName};
use hirsel_lir::runtime_abi::{
    FOREIGN_BLOCKING, FOREIGN_PACKAGE_SHIFT, FOREIGN_THREADS_ANY, FOREIGN_THREADS_HOME,
    FOREIGN_THREADS_MASK, FOREIGN_THREADS_SERIAL,
};
use hirsel_syntax::DefPath;
use hirsel_types::{
    BuiltinType, ForeignThreads, FunctionType, GenericArgument, SignatureKind, Type, TypeId,
    TypeInterner, TypeKind,
};
use std::collections::{BTreeMap, BTreeSet, HashMap};

/// Each extern's dispatch word: its package's `threads`, its `@blocking` and its package's
/// ordinal among the program's `unsafe ffi` packages, which names the package's `serial`
/// gate in the run.
pub(crate) fn dispatch(input: &LinkSource<'_>) -> BTreeMap<DefPath, u64> {
    let ordinals: BTreeMap<_, u64> = input
        .foreign_threads
        .keys()
        .enumerate()
        .map(|(ordinal, package)| (package, ordinal as u64))
        .collect();
    input
        .signatures
        .iter()
        .filter_map(|(path, signature)| {
            let SignatureKind::Function(function) = &signature.kind else {
                return None;
            };
            let contract = function.foreign.as_ref()?;
            let package = &path.0.package;
            let threads = match threads(input, path) {
                ForeignThreads::Home => FOREIGN_THREADS_HOME,
                ForeignThreads::Serial => FOREIGN_THREADS_SERIAL,
                ForeignThreads::Any => FOREIGN_THREADS_ANY,
            };
            let blocking = if contract.blocking {
                FOREIGN_BLOCKING
            } else {
                0
            };
            let ordinal = ordinals.get(package).copied().unwrap_or(0);
            Some((
                path.clone(),
                threads | blocking | ordinal << FOREIGN_PACKAGE_SHIFT,
            ))
        })
        .collect()
}

fn threads(input: &LinkSource<'_>, path: &DefPath) -> ForeignThreads {
    input
        .foreign_threads
        .get(&path.0.package)
        .copied()
        .unwrap_or_default()
}

/// S-13's nesting check: a callback runs on the thread of the extern call it was handed to,
/// and a `home` package's externs run only on the home thread, so a callback that can reach
/// a `home` package's extern may be handed only to a `home` package's extern. A callback may
/// be handed to an extern when its function type is the callback type of one of the
/// extern's parameters (a `FnPtr`, also inside an `Option` or other type argument):
/// callback values are opaque and only ever passed back to C, so their type is the whole of
/// their flow. Types compare by their nominal shape, since layouts do not tell two
/// packages' records apart.
pub(crate) fn check_callbacks(
    program: &ReadyProgram,
    input: &LinkSource<'_>,
) -> Result<(), NativeError> {
    let snapshot = input;
    // Each callback type a parameter of a non-home extern carries, with the first such
    // extern.
    let mut handed: HashMap<CallbackType, DefPath> = HashMap::new();
    for (path, signature) in snapshot.signatures {
        match program.foreign_dispatch.get(path) {
            Some(word) if word & FOREIGN_THREADS_MASK != FOREIGN_THREADS_HOME => {}
            _ => continue,
        }
        let SignatureKind::Function(function) = &signature.kind else {
            continue;
        };
        for parameter in function.parameters.iter() {
            callback_types(&snapshot.types, parameter.ty, &mut |shape| {
                handed.entry(shape).or_insert_with(|| path.clone());
            });
        }
    }
    if handed.is_empty() {
        return Ok(());
    }
    // Compact summaries survive both object hits and whole-plan assembly, where
    // no Ready bodies remain. Copied callbacks retain their independent owner.
    let callbacks: BTreeSet<_> = program
        .summaries
        .iter()
        .flat_map(|unit| unit.glue.callbacks.iter().map(|(path, _)| path))
        .collect();
    for callback in callbacks {
        let Some(signature) = snapshot.signatures.get(callback) else {
            continue;
        };
        let SignatureKind::Function(function) = &signature.kind else {
            continue;
        };
        let Some(extern_) = handed.get(&callback_type(&snapshot.types, function)) else {
            continue;
        };
        if let Some(reach) = input.semantic.home_reach(callback) {
            let home = reach.path.last().unwrap_or(callback);
            return Err(NativeError {
                definition: Some(callback.clone()),
                ..NativeError::new(
                    StageName::Import,
                    format!(
                        "PROG-4: the callback {callback} can reach {home}, an extern of the \
                     home package {}@{} (through {}), and its callback type is a \
                     parameter of {extern_}, an extern of the {} package {}@{}, \
                     whose calls do not run on the home thread; only a home \
                     package's extern may take such a callback (S-13)",
                        home.0.package.name,
                        home.0.package.version,
                        reach.spelled().join(" -> "),
                        threads(snapshot, extern_).name(),
                        extern_.0.package.name,
                        extern_.0.package.version,
                    ),
                )
            });
        }
    }
    Ok(())
}

/// Visit the shape of each callback type `ty` carries: a `FnPtr`'s function type, also as a
/// type argument or a tuple element.
fn callback_types(types: &TypeInterner, ty: TypeId, visit: &mut impl FnMut(CallbackType)) {
    let Some(row) = types.get(ty) else {
        return;
    };
    let arguments = |arguments: &[GenericArgument], visit: &mut dyn FnMut(TypeId)| {
        for argument in arguments {
            if let GenericArgument::Type(ty) = argument {
                visit(*ty);
            }
        }
    };
    match &row.kind {
        TypeKind::Builtin {
            kind: BuiltinType::FnPtr,
            arguments: fn_arguments,
        } => {
            for argument in fn_arguments.iter() {
                if let GenericArgument::Type(target) = argument
                    && let Some(Type {
                        kind: TypeKind::Function(function),
                        ..
                    }) = types.get(*target)
                {
                    visit(callback_type(types, function));
                }
            }
        }
        TypeKind::Builtin { arguments: a, .. } | TypeKind::Named { arguments: a, .. } => {
            arguments(a, &mut |ty| callback_types(types, ty, visit))
        }
        TypeKind::Tuple(elements) => {
            for element in elements.iter() {
                callback_types(types, *element, visit);
            }
        }
        TypeKind::Box(inner) => callback_types(types, *inner, visit),
        _ => {}
    }
}

/// Exact nominal callback identity in one source interner. Names, passing modes and
/// uniqueness differ between definitions and `FnPtr` declarations; flow depends on the
/// parameter and result types. Their interned shape IDs retain every nested constructor
/// and nominal argument without a depth cutoff.
#[derive(PartialEq, Eq, Hash)]
struct CallbackType {
    parameters: Vec<TypeId>,
    result: TypeId,
}

fn callback_type(types: &TypeInterner, function: &FunctionType) -> CallbackType {
    CallbackType {
        parameters: function
            .parameters
            .iter()
            .map(|parameter| types.shape(parameter.ty))
            .collect(),
        result: types.shape(function.result),
    }
}
