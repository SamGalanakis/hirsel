//! Select the recording-only hooks from the checked identities and layouts.

use crate::{BlockId, Body, Callee, Function, Module, Param, PlaceTy, Terminator, TypeId, ValueId};
use hirsel_core_ids::DefPath;
use hirsel_core_layout::{Body as ShapeBody, FieldTy, Fixed, Layout, RecipeRow, law};

fn prop_name(definition: &DefPath) -> Option<&str> {
    (definition.package_is("std", "1") && definition.0.modules == ["prop"])
        .then_some(definition.0.name.as_str())
}

pub fn callee(definition: &DefPath) -> Option<u64> {
    if definition.package_is("std", "1") && definition.0.modules == ["model"] {
        return match definition.0.name.as_str() {
            "explore" => Some(law::MODEL_EXPLORE),
            "check" => Some(law::MODEL_CHECK),
            "conforms" => Some(law::MODEL_CONFORMS),
            "conforms_check" => Some(law::MODEL_CONFORMS_CHECK),
            _ => None,
        };
    }
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
            let model = law::model_engine(callee);
            let (config_name, fields) = if model.is_some() {
                (
                    "std@1/model/Bounds",
                    ["depth", "states", "step_steps", "step_bytes"],
                )
            } else {
                (
                    "std@1/prop/Config",
                    ["cases", "max_choices", "max_rejects", "gen_steps"],
                )
            };
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
                    (module.types[ty.index()].name == config_name).then_some((i, ty))
                })
                .expect("an observed engine takes bounds");
            let entry = Hook {
                operation: law::ENTER,
                value: params[usize::from(signature.dest.is_some()) + i],
                predicate: None,
                words: fields
                    .map(|name| field(module, layouts, config, name))
                    .to_vec(),
            };
            let result = matches!(
                callee,
                law::ALL | law::SEARCH | law::MODEL_EXPLORE | law::MODEL_CONFORMS
            )
            .then(|| Hook {
                operation: law::OUTCOME,
                value: params[0],
                predicate: None,
                words: {
                    let ty = signature.dest.expect("an engine outcome has a destination");
                    match callee {
                        law::MODEL_EXPLORE => model_outcome(module, layouts, ty),
                        law::MODEL_CONFORMS => conformance_outcome(module, layouts, ty),
                        _ => outcome(module, layouts, ty),
                    }
                },
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
        let Callee::Direct(target) = target else {
            return false;
        };
        let instance = module.functions[target.index()].instance;
        let expected = match self.callee {
            Some(law::CHECK_CORPUS) => law::CHECK,
            Some(law::CHECK_TASKS_CORPUS) => law::CHECK_TASKS,
            Some(law::MODEL_CHECK) => law::MODEL_EXPLORE,
            Some(law::MODEL_CONFORMS_CHECK) => law::MODEL_CONFORMS,
            _ => return false,
        };
        callee(&module.instances[instance.index()].definition) == Some(expected)
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

fn inline(ty: &FieldTy) -> TypeId {
    let FieldTy::Inline(ty) = ty else {
        panic!("expected inline model data")
    };
    TypeId(ty.0)
}

fn model_outcome(module: &Module, layouts: &[RecipeRow], ty: TypeId) -> Vec<u64> {
    let layout = fixed(layouts, ty);
    let ShapeBody::Variant(cases) = &module.types[ty.index()].body else {
        panic!("expected a model outcome variant")
    };
    let stats = inline(&cases[0].fields[0].ty);
    let failure = inline(&cases[2].fields[0].ty);
    let limit = inline(&cases[1].fields[1].ty);
    let ShapeBody::Record(stats_fields) = &module.types[stats.index()].body else {
        unreachable!()
    };
    let FieldTy::Seq(pair) = &stats_fields[field_index(module, stats, "witnessed")].ty else {
        panic!("expected a list of model witnesses")
    };
    let pair = fixed(layouts, inline(pair));
    let ShapeBody::Record(failure_fields) = &module.types[failure.index()].body else {
        unreachable!()
    };
    let reason = inline(&failure_fields[field_index(module, failure, "reason")].ty);

    let mut words = law::tag_words(layout.tag.expect("model outcome tag")).to_vec();
    words.extend([
        layout.offsets[0][0],
        layout.offsets[1][0],
        layout.offsets[2][0],
    ]);
    words.extend(
        ["states", "edges", "depth", "witnessed"].map(|name| field(module, layouts, stats, name)),
    );
    words.extend([pair.size, pair.offsets[0][0], pair.offsets[0][1]]);
    words.push(layout.offsets[1][1]);
    words.extend(law::tag_words(
        fixed(layouts, limit).tag.expect("model limit tag"),
    ));
    words.extend(["depth", "reason"].map(|name| field(module, layouts, failure, name)));
    words.extend(law::tag_words(
        fixed(layouts, reason).tag.expect("model failure kind tag"),
    ));
    words
}

fn conformance_outcome(module: &Module, layouts: &[RecipeRow], ty: TypeId) -> Vec<u64> {
    let layout = fixed(layouts, ty);
    let ShapeBody::Variant(cases) = &module.types[ty.index()].body else {
        panic!("expected a conformance outcome variant")
    };
    let limit = inline(&cases[2].fields[1].ty);
    let reason = inline(&cases[1].fields[2].ty);
    let mut words = law::tag_words(layout.tag.expect("conformance outcome tag")).to_vec();
    words.extend([
        layout.offsets[0][0],
        layout.offsets[1][0],
        layout.offsets[2][0],
    ]);
    words.push(layout.offsets[2][1]);
    words.extend(law::tag_words(
        fixed(layouts, limit).tag.expect("conformance limit tag"),
    ));
    words.push(layout.offsets[1][2]);
    words.extend(law::tag_words(
        fixed(layouts, reason).tag.expect("conformance reason tag"),
    ));
    words
}
