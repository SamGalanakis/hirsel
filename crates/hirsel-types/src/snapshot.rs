//! Canonical scalar/type-constructor tags, owned with the semantic enums.
use crate::{BuiltinType, Primitive};
use hirsel_codec::{DecodeError, Reader, Snapshot, Writer};

macro_rules! unit_enum {
    ($ty:ident { $($variant:ident),* $(,)? }) => {
        impl<C> Snapshot<C> for $ty {
            fn write(&self, w: &mut Writer<C>) {
                w.open(match self {
                    $(Self::$variant => stringify!($variant),)*
                });
                w.close();
            }
            fn read(r: &mut Reader<'_, C>) -> Result<Self, DecodeError> {
                let value = match r.open_any()? {
                    $(stringify!($variant) => Self::$variant,)*
                    other => return Err(r.error(format!(concat!("unknown ", stringify!($ty), " variant {}"), other))),
                };
                r.close()?;
                Ok(value)
            }
        }
    };
}

unit_enum!(Primitive {
    Bool,
    U8,
    U16,
    U32,
    U64,
    U128,
    I8,
    I16,
    I32,
    I64,
    I128,
    F32,
    F64,
    Char,
    String,
    Bytes,
    Never
});
unit_enum!(BuiltinType {
    Array,
    List,
    Map,
    Set,
    SortedMap,
    Inline,
    Simd,
    Packed,
    Shared,
    Slots,
    SlotId,
    Region,
    Iter,
    Task,
    Sender,
    Receiver,
    Mutex,
    Atomic,
    Ptr,
    FnPtr
});
