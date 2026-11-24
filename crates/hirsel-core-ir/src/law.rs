//! Select the recording-only hooks from the checked identities and layouts.

use crate::{BlockId, Body, Callee, Function, Module, Param, PlaceTy, Terminator, TypeId, ValueId};
use hirsel_core_ids::DefPath;
use hirsel_core_layout::{Body as ShapeBody, FieldTy, Fixed, Layout, RecipeRow, law};

fn prop_name(definition: &DefPath) -> Option<&str> {
    (definition.package_is("std", "1") && definition.0.modules == ["prop"])
        .then_some(definition.0.name.as_str())
}

pub fn callee(definition: &DefPath) -> Option<u64> {
    let name = prop_name(definition)?;
    law::CALLEES
        .iter()
        .position(|&item| item == name)
        .map(|i| i as u64)
}

pub fn observes(definition: &DefPath) -> bool {
    callee(definition).is_some()
        || matches!(
            prop_name(definition),
            Some("finish_check" | "corpus_report" | "sometimes")
        )
}

pub struct Hook {
    pub operation: u64,
    pub value: ValueId,
    pub predicate: Option<ValueId>,
    pub words: Vec<u64>,
}

pub struct Hooks {
    pub callee: Option<u64>,
    pub start: BlockId,
    pub entry: Hook,
    pub result: Option<Hook>,
}

impl Hooks {
    pub fn pick(
        module: &Module,
        function: &Function,
        body: &Body,
        layouts: &[RecipeRow],
    ) -> Option<Self> {
        if !module.law_observation {
            return None;
        }
        let definition = &module.instances[function.instance.index()].definition;
        let signature = &module.signatures[function.signature.index()];
        let params = body.blocks[0].params.get(&body.pools.values).unwrap();
        let start = if body.parked_returns.is_empty() {
            BlockId(0)
        } else {
            let Terminator::Switch { otherwise, .. } = body.terminators[0] else {
                panic!("a returnable body starts with a resume dispatch")
            };
            body.edges[otherwise.index()].target
        };
        if let Some(callee) = callee(definition) {
            let (i, config) = signature
                .params
                .iter()
                .enumerate()
                .find_map(|(i, p)| {
                    let Param::Address {
                        place: PlaceTy::Declared(ty),
                        ..
                    } = *p
                    else {
                        return None;
                    };
                    (module.types[ty.index()].name == "std@1/prop/Config").then_some((i, ty))
                })
                .expect("a prop engine takes Config");
            let entry = Hook {
                operation: law::ENTER,
                value: params[usize::from(signature.dest.is_some()) + i],
                predicate: None,
                words: ["cases", "max_choices", "max_rejects", "gen_steps"]
                    .map(|name| field(module, layouts, config, name))
                    .to_vec(),
            };
            let result = matches!(callee, law::ALL | law::SEARCH).then(|| Hook {
                operation: law::OUTCOME,
                value: params[0],
                predicate: None,
                words: outcome(
                    module,
                    layouts,
                    signature.dest.expect("an engine outcome has a destination"),
                ),
            });
            return Some(Self {
                callee: Some(callee),
                start,
                entry,
                result,
            });
        }
        let (operation, parameter) = match prop_name(definition)? {
            "sometimes" => {
                return Some(Self {
                    callee: None,
                    start,
                    result: None,
                    entry: Hook {
                        operation: law::WITNESS,
                        value: params[0],
                        predicate: Some(params[1]),
                        words: Vec::new(),
                    },
                });
            }
            "finish_check" => (law::SETTLED_CHECK, 0),
            "corpus_report" => (law::CORPUS_FAILURE, 2),
            _ => return None,
        };
        let Param::Address {
            place: PlaceTy::Declared(ty),
            ..
        } = signature.params[parameter]
        else {
            panic!("a prop check settles with an addressed outcome")
        };
        let words = if operation == law::SETTLED_CHECK {
            outcome(module, layouts, ty)
        } else {
            found(module, layouts, ty)
        };
        Some(Self {
            callee: None,
            start,
            entry: Hook {
                operation,
                value: params[usize::from(signature.dest.is_some()) + parameter],
                predicate: None,
                words,
            },
            result: None,
        })
    }

    pub fn delegates(&self, module: &Module, target: Callee) -> bool {
        let name = match self.callee {
            Some(law::CHECK_CORPUS) => "check",
            Some(law::CHECK_TASKS_CORPUS) => "check_tasks",
            _ => return false,
        };
        let Callee::Direct(target) = target else {
            return false;
        };
        let instance = module.functions[target.index()].instance;
        prop_name(&module.instances[instance.index()].definition) == Some(name)
    }
}

fn fixed(layouts: &[RecipeRow], ty: TypeId) -> &Fixed {
    let Layout::Fixed(layout) = &layouts[ty.index()].layout else {
        panic!("prop obs layout is fixed")
    };
    layout
}

fn field_index(module: &Module, ty: TypeId, name: &str) -> usize {
    let ShapeBody::Record(fields) = &module.types[ty.index()].body else {
        panic!("expected a record")
    };
    fields
        .iter()
        .position(|field| field.name == name)
        .expect("prop field")
}

fn field(module: &Module, layouts: &[RecipeRow], ty: TypeId, name: &str) -> u64 {
    fixed(layouts, ty).offsets[0][field_index(module, ty, name)]
}

fn found(module: &Module, layouts: &[RecipeRow], ty: TypeId) -> Vec<u64> {
    let i = field_index(module, ty, "class");
    let ShapeBody::Record(fields) = &module.types[ty.index()].body else {
        unreachable!()
    };
    let FieldTy::Inline(class) = fields[i].ty else {
        panic!("expected an inline failure class")
    };
    let class_layout = fixed(layouts, TypeId(class.0));
    let mut words = vec![
        field(module, layouts, ty, "cases"),
        fixed(layouts, ty).offsets[0][i],
    ];
    words.extend(law::tag_words(class_layout.tag.expect("failure class tag")));
    words.push(class_layout.offsets[2][0]);
    words
}

fn outcome(module: &Module, layouts: &[RecipeRow], ty: TypeId) -> Vec<u64> {
    let layout = fixed(layouts, ty);
    let ShapeBody::Variant(cases) = &module.types[ty.index()].body else {
        panic!("expected a variant")
    };
    let mut words = law::tag_words(layout.tag.expect("prop outcome tag")).to_vec();
    for offsets in &layout.offsets {
        words.push(offsets.first().copied().unwrap_or(law::ABSENT));
        words.push(offsets.get(1).copied().unwrap_or(law::ABSENT));
    }
    let FieldTy::Inline(found_ty) = cases.last().unwrap().fields[0].ty else {
        panic!("expected Found")
    };
    words.extend(found(module, layouts, TypeId(found_ty.0)));
    words
}
