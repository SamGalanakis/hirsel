use hirsel_lir::operation::{NativeType, OperationArgument};
use hirsel_query_products::retained::MetadataWeight as Weight;
pub fn argument(w: &mut Weight, value: &OperationArgument) {
    if let OperationArgument::Type(value) = value {
        native_type(w, value);
    }
}
pub fn native_type(w: &mut Weight, value: &NativeType) {
    match value {
        NativeType::Primitive(_) | NativeType::Function(_) => (),
        NativeType::Tuple(values) => w.vec(values, native_type),
        NativeType::Named(path, values) => {
            w.path(path);
            w.vec(values, argument);
        }
        NativeType::Record(values) => w.vec(values, |w, (_, value)| native_type(w, value)),
        NativeType::Variant(values) => w.vec(values, |w, values| w.vec(values, native_type)),
        NativeType::Box(value) => {
            w.add(size_of::<NativeType>());
            native_type(w, value);
        }
        NativeType::Builtin(_, values) => w.vec(values, argument),
    }
}
