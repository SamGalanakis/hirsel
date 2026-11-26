//! Computed constants (LANG-20, PROG-10): each constant's initialiser runs on
//! the evaluator, once for each target, over that target's retained code. A
//! reader's body names the constant; its group binds the value, by hash.
use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use hirsel_core_ids::{ContentHash, DefPath};
use hirsel_core_ir::{Identity, StaticValue};
use hirsel_core_layout::{FieldTy, Target, Ty};

use crate::CheckProduct;
use crate::instances::{InstanceSet, Units};

/// A computed constant's value on one target, with its PROG-10 value hash.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub(crate) struct Value {
    pub value: StaticValue,
    pub hash: ContentHash,
}

/// An evaluation: the initialiser's code address on the target, when every
/// instance it reaches has one, and its value or the cause that refused it.
/// The cause names neither the constant nor the target: the caller adds both.
#[derive(Clone, Debug)]
pub(crate) struct Evaluation {
    pub address: Option<ContentHash>,
    pub value: Result<Arc<Value>, Arc<str>>,
    /// The initialiser reaches a body refused or not built already, or reads
    /// a constant that does: that report stands for the constant's.
    pub refused_elsewhere: bool,
    /// The complete initializer closure takes the same inputs on every target.
    target_independent: bool,
}

/// Each constant's evaluation for one product, by path and target, and each
/// run's result by the initialiser's code address.
#[derive(Default)]
pub(crate) struct Values {
    evaluations: Mutex<HashMap<(DefPath, Target), Evaluation>>,
    runs: Mutex<HashMap<ContentHash, Result<Arc<Value>, Arc<str>>>>,
}

impl Values {
    /// The values a library image stores: every run that returned, by address.
    pub(crate) fn published(&self) -> hirsel_core_ids::published::Map<ContentHash, Value> {
        let mut map = hirsel_core_ids::published::Map::default();
        for (address, run) in self.runs.lock().unwrap().iter() {
            if let Ok(value) = run {
                map.insert(*address, Value::clone(value));
            }
        }
        map
    }
}

thread_local! {
    /// The constants this thread is evaluating, outermost first.
    static ACTIVE: RefCell<Vec<(DefPath, Target)>> = const { RefCell::new(Vec::new()) };
}

impl CheckProduct {
    /// Constant `path`'s value on `target`, or why it is refused.
    pub(crate) fn computed(&self, path: &DefPath, target: Target) -> Evaluation {
        let key = (path.clone(), target);
        if let Some(done) = self.constant_values.evaluations.lock().unwrap().get(&key) {
            return done.clone();
        }
        let cycle = ACTIVE.with_borrow(|active| {
            let at = active.iter().position(|k| *k == key)?;
            let mut names: Vec<String> = active[at..].iter().map(|(p, _)| p.to_string()).collect();
            names.push(path.to_string());
            Some(names.join(" reads "))
        });
        if let Some(cycle) = cycle {
            // Not remembered: the cycle's outermost constant is refused with it.
            return Evaluation {
                address: None,
                value: Err(format!("its value depends on itself: {cycle}").into()),
                refused_elsewhere: false,
                target_independent: false,
            };
        }
        ACTIVE.with_borrow_mut(|active| active.push(key.clone()));
        let done = self.evaluate(path, target);
        ACTIVE.with_borrow_mut(|active| active.pop());
        let mut evaluations = self.constant_values.evaluations.lock().unwrap();
        if done.target_independent {
            for other in Target::ALL.into_iter().filter(|&other| other != target) {
                evaluations
                    .entry((path.clone(), other))
                    .or_insert_with(|| done.clone());
            }
        }
        evaluations.entry(key).or_insert(done).clone()
    }

    fn evaluate(&self, path: &DefPath, target: Target) -> Evaluation {
        let root = Identity::plain(path.clone());
        let mut set = InstanceSet::for_reach(false).for_target(Some(target));
        let reached = match self.instances_into(
            [root.clone()],
            crate::native::DEFAULT_INSTANCE_LIMIT,
            &mut set,
        ) {
            Ok(reached) => reached,
            Err(stop) => {
                let refused_elsewhere = match &stop {
                    crate::instances::InstanceStop::Incomplete { definition, .. } => {
                        self.demand_body(definition).is_ok_and(|p| {
                            !p.check.refusals.is_empty()
                                || !p.incomplete.is_empty()
                                || p.check.function.is_none()
                                || p.check
                                    .computed
                                    .iter()
                                    .any(|c| self.computed(c, target).refused_elsewhere)
                        })
                    }
                    _ => false,
                };
                let cause = match stop {
                    crate::instances::InstanceStop::Incomplete {
                        definition,
                        message,
                    } => {
                        format!("{message}, in {definition}")
                    }
                    stop => stop.to_string(),
                };
                return Evaluation {
                    address: None,
                    value: Err(cause.into()),
                    refused_elsewhere,
                    target_independent: false,
                };
            }
        };
        if !set.foreign.is_empty() && self.bind_foreign_addresses(&mut set).is_err() {
            set.foreign_addresses.clear();
        }
        let address = set
            .addresses(&reached, None)
            .and_then(|addresses| addresses.get(&root).copied());
        let target_independent = address.is_some() && set.is_target_independent();
        // The run reads only the code its address names: a target whose code
        // agrees with one run already takes its result.
        let known = address.and_then(|a| {
            let run = self.constant_values.runs.lock().unwrap().get(&a).cloned();
            run.or_else(|| {
                let image = self.image.as_ref()?.constant_value(&a).ok()??;
                Some(Ok(Arc::new(image)))
            })
        });
        let value = known.unwrap_or_else(|| {
            let value = crate::resident::CONSTANTS
                .get_or(address, || {
                    Ok::<_, std::convert::Infallible>(run(&Units::new(reached, set), &root))
                })
                .unwrap_or_else(|never| match never {});
            if let Some(a) = address {
                self.constant_values
                    .runs
                    .lock()
                    .unwrap()
                    .insert(a, value.clone());
            }
            value
        });
        Evaluation {
            address,
            value,
            refused_elsewhere: false,
            target_independent,
        }
    }
}

/// Run the initialiser `root` names over `units`' joined module.
fn run(units: &Units, root: &Identity) -> Result<Arc<Value>, Arc<str>> {
    let modules = units.modules().map_err(|e| e.to_string())?;
    let (module, entry) = hirsel_core_materialize::join(&modules, root)?;
    let pure = hirsel_core_eval::PureModule::new(&module);
    crate::meter::count(crate::meter::Count::ConstantRuns, 1);
    let mut run =
        hirsel_core_eval::run_pure(&pure, entry, &[], hirsel_core_eval::PureInput::constant());
    let signature = &module.signatures[module.functions[entry.index()].signature.index()];
    let ty = match (signature.dest, signature.results.first()) {
        (Some(t), _) => Some(FieldTy::Inline(hirsel_core_layout::TypeRef(t.0))),
        (None, Some(Ty::Scalar(s))) => Some(FieldTy::Scalar(*s)),
        _ => None,
    };
    let name = |f: hirsel_core_ir::FuncId| {
        let function = &module.functions[f.index()];
        module.instances[function.instance.index()]
            .definition
            .to_string()
    };
    let cause = match &run.outcome {
        hirsel_core_eval::Outcome::Returned(bits) => {
            let recipes =
                hirsel_core_layout::compute(&module.types).map_err(|e| format!("{e:?}"))?;
            let value = match (&ty, &run.destination) {
                // A String or FrozenBytes: its bytes, stored static (LANG-20).
                (Some(FieldTy::Inline(_)), Some(_)) if run.held_bytes.is_some() => {
                    StaticValue::Bytes(run.held_bytes.take().unwrap_or_default())
                }
                (Some(ty @ FieldTy::Inline(_)), Some(bytes)) => {
                    hirsel_core_ir::static_data::read_value(ty, bytes, &module.types, &recipes)
                }
                (Some(_), _) => StaticValue::Bits(bits.first().copied().unwrap_or(0)),
                (None, _) => StaticValue::Fields {
                    case: None,
                    fields: vec![],
                },
            };
            use hirsel_core_ir::static_data as data;
            let hash = match ty
                .as_ref()
                .and_then(|ty| Some((ty, data::value_closure(&module, ty)?)))
            {
                Some((ty, closure)) => {
                    data::value_hash(closure, ty, &value, &module.types, &recipes)
                }
                // `()` reaches no row and has no value bytes.
                None => ContentHash::of_parts(
                    data::VALUE_HASH_DOMAIN,
                    [
                        ContentHash::of_parts(data::SCALAR_CLOSURE_DOMAIN, [b"()".as_slice()])
                            .0
                            .as_slice(),
                        &[],
                    ],
                ),
            };
            return Ok(Arc::new(Value { value, hash }));
        }
        hirsel_core_eval::Outcome::Failed(chain) => {
            format!(
                "it fails with {}",
                chain.first().map_or("a failure", String::as_str)
            )
        }
        hirsel_core_eval::Outcome::Stopped("Closed") => format!(
            "it reaches {}, on the closed list",
            run.reached.unwrap_or("a closed function")
        ),
        hirsel_core_eval::Outcome::Stopped("Foreign") => format!(
            "it reaches foreign function {}",
            run.foreign.map_or_else(|| "?".to_owned(), name)
        ),
        hirsel_core_eval::Outcome::Stopped(budget) => {
            format!("it exceeds its budget ({budget}, MEM-34)")
        }
    };
    let mut text = cause;
    if !run.calls.is_empty() {
        text.push_str(", in ");
        text.push_str(&call_chain(run.calls.iter().map(|&f| name(f))));
    }
    Err(text.into())
}

/// The calls of a stopped run, innermost first: a run of calls to one
/// function is named once with its count, and past [`CHAIN_NAMES`] names the
/// rest are counted.
fn call_chain(calls: impl Iterator<Item = String>) -> String {
    let mut runs: Vec<(String, usize)> = Vec::new();
    for call in calls {
        match runs.last_mut() {
            Some((last, n)) if *last == call => *n += 1,
            _ => runs.push((call, 1)),
        }
    }
    let mut names: Vec<String> = runs
        .iter()
        .take(CHAIN_NAMES)
        .map(|(f, n)| {
            if *n == 1 {
                f.clone()
            } else {
                format!("{f} ({n} calls)")
            }
        })
        .collect();
    if runs.len() > CHAIN_NAMES {
        names.push(format!("{} more", runs.len() - CHAIN_NAMES));
    }
    names.join(" from ")
}

const CHAIN_NAMES: usize = 16;

/// The refusal text of constant `path`'s `cause`, with its target when the
/// constant depends on it.
pub(crate) fn refusal(path: &DefPath, cause: &str, target: Option<Target>) -> String {
    match target {
        Some(target) => format!(
            "constant {path} is refused: {cause}, for target {}",
            target.triple()
        ),
        None => format!("constant {path} is refused: {cause}"),
    }
}

impl CheckProduct {
    /// The refusal of each computed constant in `constants` (LANG-20): each is
    /// evaluated on every target; one that depends on the target, its code
    /// address differing between targets, names the first target that fails.
    pub(crate) fn constant_refusals(
        &self,
        constants: &[(DefPath, hirsel_core_resolve::Site)],
    ) -> Vec<hirsel_core_resolve::Refusal> {
        let mut out = Vec::new();
        for (path, site) in constants {
            let runs: Vec<(Target, Evaluation)> = Target::ALL
                .iter()
                .map(|&t| (t, self.computed(path, t)))
                .collect();
            let depends = runs.windows(2).any(|w| w[0].1.address != w[1].1.address);
            if runs.iter().any(|(_, e)| e.refused_elsewhere) {
                continue;
            }
            let Some((target, cause)) = runs
                .iter()
                .find_map(|(t, e)| Some((*t, e.value.clone().err()?)))
            else {
                continue;
            };
            out.push(hirsel_core_resolve::Refusal {
                rule: "LANG-20".into(),
                primary: *site,
                causal: vec![],
                declared: None,
                required: None,
                message: refusal(path, &cause, depends.then_some(target)),
                unresolved: None,
                detail: None,
            });
        }
        out
    }
}

impl CheckProduct {
    /// The value on `target` of each constant in `computed`, which
    /// `definition`'s body reads; the first refused one stops its instance.
    pub(crate) fn constant_values(
        &self,
        definition: &DefPath,
        computed: &std::collections::BTreeSet<DefPath>,
        target: Target,
    ) -> Result<Vec<(DefPath, Arc<Value>)>, crate::instances::InstanceStop> {
        computed
            .iter()
            .map(|path| match self.computed(path, target).value {
                Ok(value) => Ok((path.clone(), value)),
                Err(cause) => Err(crate::instances::InstanceStop::Incomplete {
                    definition: definition.clone(),
                    // A cycle names its constants already.
                    message: if ACTIVE.with_borrow(|a| a.iter().any(|(p, _)| p == path)) {
                        cause.to_string()
                    } else {
                        refusal(path, &cause, None)
                    },
                }),
            })
            .collect()
    }
}

/// An instance's own record with the hashes of the values its body reads
/// folded in, so a changed value misses every record and memo keyed by it.
pub(crate) fn own_with_values(
    record: ContentHash,
    values: &[(DefPath, Arc<Value>)],
) -> ContentHash {
    if values.is_empty() {
        return record;
    }
    let mut parts: Vec<&[u8]> = vec![record.0.as_slice()];
    parts.extend(values.iter().map(|(_, v)| v.hash.0.as_slice()));
    ContentHash::of_parts("hirsel.core-own-values.v1", parts)
}

#[cfg(test)]
mod tests;
