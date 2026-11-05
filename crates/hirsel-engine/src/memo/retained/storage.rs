use super::Weight;
use hirsel_types::storage::{graph::*, *};

impl Weight<'_> {
    fn storage_place(&mut self, place: &StoragePlace) {
        self.path(&place.parts);
    }
    fn storage_selector(&mut self, selector: &StorageSelector) {
        self.path(&selector.parts);
    }
    fn storage_result(&mut self, result: &StorageResult) {
        self.path(&result.parts);
        self.storage_selector(&result.source);
    }
    fn storage_placement(&mut self, placement: &StoragePlacement) {
        match placement {
            StoragePlacement::Sources(sources) => self.vec(sources, Self::storage_place),
            StoragePlacement::Independent | StoragePlacement::Unknown => (),
        }
    }
    fn storage_value(&mut self, value: &StorageValue) {
        match value {
            StorageValue::ConstantReference { arguments, .. } => self.flat_vec(arguments),
            StorageValue::Reference { arguments, .. } => self.flat_vec(arguments),
            StorageValue::Constant { code, .. } => self.vec(code, |w, leaf| {
                w.path(&leaf.parts);
                w.flat_vec(&leaf.arguments);
            }),
            StorageValue::Copy { value, .. } | StorageValue::Unique { value, .. } => {
                self.storage_place(value)
            }
            StorageValue::Construct { fields, placement } => {
                self.vec(fields, |w, (path, place)| {
                    w.path(path);
                    w.storage_place(place);
                });
                self.storage_placement(placement);
            }
            StorageValue::Callable { captures, .. } => self.vec(captures, Self::storage_place),
            StorageValue::Fresh | StorageValue::Unknown => (),
        }
    }
    fn storage_callee(&mut self, callee: &StorageCallee) {
        match callee {
            StorageCallee::Apply { arguments, .. } => self.flat_vec(arguments),
            StorageCallee::Value(place) => self.storage_place(place),
            StorageCallee::Declared {
                allocations,
                results,
                ..
            } => {
                self.vec(allocations, Self::storage_selector);
                self.vec(results, Self::storage_result);
            }
            StorageCallee::MapGet { result_path, .. }
            | StorageCallee::MapRemove { result_path, .. }
            | StorageCallee::SortedMapGet { result_path, .. }
            | StorageCallee::SortedMapRemove { result_path, .. }
            | StorageCallee::ScopeCallback { result_path, .. } => self.path(result_path),
            StorageCallee::Definition(_)
            | StorageCallee::CopyTo { .. }
            | StorageCallee::MapInsert
            | StorageCallee::SortedMapInsert
            | StorageCallee::TaskAwait
            | StorageCallee::ReadRegion
            | StorageCallee::SealArena => (),
        }
    }
    pub(crate) fn storage_definition(&mut self, definition: &StorageDefinition) {
        self.vec(&definition.bodies, Self::storage_body);
    }
    pub(crate) fn storage_body(&mut self, body: &StorageBody) {
        let w = self;
        w.vec(&body.paths, |w, path| w.path(path));
        w.vec(&body.generics, Self::generic);
        w.vec(&body.inputs, |w, input| {
            w.storage_place(&input.place);
            w.vec(&input.callables, |w, path| w.path(path));
        });
        w.vec(&body.nodes, |w, node| {
            w.flat_vec(&node.successors);
            match &node.operation {
                StorageOp::Nop => (),
                StorageOp::Assign(assignments) => w.vec(assignments, |w, (place, value)| {
                    w.storage_place(place);
                    w.storage_value(value);
                }),
                StorageOp::Call {
                    callee,
                    arguments,
                    mutations,
                    destination,
                } => {
                    w.storage_callee(callee);
                    w.vec(arguments, |w, (_, place)| w.storage_place(place));
                    w.flat_vec(mutations);
                    if let Some(place) = destination {
                        w.storage_place(place);
                    }
                }
                StorageOp::Return(place) => w.storage_place(place),
                StorageOp::Kill(locals) => w.flat_vec(locals),
            }
        });
    }
    pub(crate) fn storage_profiles(&mut self, profiles: &Vec<CallableStorageProfile>) {
        self.vec(profiles, |w, profile| match &profile.profile.behavior {
            StorageBehavior::Known {
                allocations,
                results,
            } => {
                w.shared_values(allocations, Self::storage_selector);
                w.shared_values(results, Self::storage_result);
            }
            StorageBehavior::Unknown => (),
        });
    }
}
