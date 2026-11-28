use super::*;

/// Whether `target` is compact data a [`FieldTy::Locator`] may name.
fn locates(shapes: &[Row], target: &FieldTy) -> bool {
    match target {
        FieldTy::Text | FieldTy::FixedSpan(_) | FieldTy::SeqSpan(_) | FieldTy::IndexedSpan(_) => {
            true
        }
        FieldTy::Inline(c) => matches!(shapes[c.index()].repr, Repr::Compact(_)),
        _ => false,
    }
}

#[derive(Debug)]
pub(super) enum MeasureError {
    Field(&'static str),
    NativeRange(u128),
}
impl MeasureError {
    pub(super) fn at(self, shape: &Shape, field: &Field) -> LayoutError {
        match self {
            Self::Field(why) => err(shape, field, why),
            Self::NativeRange(bytes) => LayoutError::NativeRange {
                ty: shape.name.clone(),
                field: field.name.clone(),
                bytes,
            },
        }
    }
}

/// Native size, native alignment and logical size (MEM-24) of a field of type `ty`
/// in a fixed layout whose holder has representation `holder`, or why the holder
/// refuses it.
pub(super) fn measure<'r>(
    shapes: &[Row],
    holder: Repr,
    ty: &FieldTy,
    get: &dyn Fn(TypeRef) -> &'r Recipe,
) -> Result<(u64, u64, u64), MeasureError> {
    let stable = holder == Repr::Region;
    Ok(match ty {
        FieldTy::Scalar(s) => (s.size(), s.size(), s.size()),
        FieldTy::Inline(t) => {
            let inner = &shapes[t.index()];
            if matches!(inner.repr, Repr::Compact(_)) {
                return Err(MeasureError::Field("a compact value lives only in a blob"));
            }
            if stable && inner.repr != Repr::Region && !get(*t).plain {
                return Err(MeasureError::Field(
                    "a region block holds only plain or region data",
                ));
            }
            if inner.repr == Repr::Resource {
                // A resource value is a pointer to its block, which carries the
                // cause cell and grant; it counts 8 in its holder (MEM-24).
                return Ok((POINTER, POINTER, 8));
            }
            let Layout::Fixed(l) = &get(*t).layout else {
                unreachable!("non-compact shapes have fixed layouts")
            };
            (l.size, l.align, get(*t).logical_size.expect("fixed"))
        }
        FieldTy::Array(e, n) => {
            if !array_element(shapes, e) {
                return Err(MeasureError::Field(
                    "an inline array holds plain or own values (SEM-9)",
                ));
            }
            let (size, align, logical) = measure(shapes, holder, e, get)?;
            let native = size
                .checked_mul(*n)
                .ok_or_else(|| MeasureError::NativeRange(u128::from(size) * u128::from(*n)))?;
            let logical = logical
                .checked_mul(*n)
                .ok_or(MeasureError::Field("logical size exceeds 2^64 - 1 bytes"))?;
            (native, align, logical)
        }
        FieldTy::RegionRef(t) if matches!(shapes[t.index()].repr, Repr::Compact(_)) => {
            return Err(MeasureError::Field(
                "a reference to compact data is a locator",
            ));
        }
        FieldTy::RegionRef(_) => (POINTER, POINTER, 16),
        FieldTy::Locator(target) if locates(shapes, target) => (2 * POINTER, POINTER, 16),
        FieldTy::Locator(_) => {
            return Err(MeasureError::Field(
                "a locator names compact data: Text, a span or a compact type",
            ));
        }
        FieldTy::Fn => (POINTER, POINTER, 8),
        FieldTy::Borrow if holder != Repr::Scoped => {
            return Err(MeasureError::Field(
                "views and scoped callables live only in scoped types",
            ));
        }
        FieldTy::Borrow => (CLOSURE_CARRIER, POINTER, 16),
        FieldTy::View(_) if holder != Repr::Scoped => {
            return Err(MeasureError::Field(
                "views and scoped callables live only in scoped types",
            ));
        }
        // A compact value has no native field form; its view is its locator. So a
        // template's `View[$i]` is right for every argument by substitution alone.
        FieldTy::View(t) => match **t {
            FieldTy::Inline(c) if matches!(shapes[c.index()].repr, Repr::Compact(_)) => {
                (2 * POINTER, POINTER, 16)
            }
            _ => measure(shapes, Repr::Own, t, get)?,
        },
        FieldTy::Handle(Handle::Capability) => (POINTER, POINTER, 8),
        _ if stable => {
            return Err(MeasureError::Field(
                "a region block holds only plain or region data",
            ));
        }
        FieldTy::Handle(Handle::Sender | Handle::Receiver) => (POINTER, POINTER, 24),
        FieldTy::Box(_)
        | FieldTy::Rc(_)
        | FieldTy::Arc(_)
        | FieldTy::Shared(_)
        | FieldTy::Handle(_) => (POINTER, POINTER, 8),
        FieldTy::Seq(_) | FieldTy::Columns(_) | FieldTy::Map(..) | FieldTy::FrozenBytes => {
            (SEQ_CARRIER, POINTER, 24)
        }
        FieldTy::OwnFn => (CLOSURE_CARRIER, POINTER, 16),
        FieldTy::Text
        | FieldTy::FixedSpan(_)
        | FieldTy::SeqSpan(_)
        | FieldTy::IndexedSpan(_)
        | FieldTy::Link(_) => {
            return Err(MeasureError::Field(
                "compact field types need a compact declaration",
            ));
        }
    })
}

/// SEM-9: an inline array's element is plain or own, never a resource or scoped.
fn array_element(shapes: &[Row], ty: &FieldTy) -> bool {
    match ty {
        FieldTy::Inline(t) => matches!(shapes[t.index()].repr, Repr::Own | Repr::Region),
        FieldTy::Array(e, _) => array_element(shapes, e),
        FieldTy::Handle(Handle::Capability | Handle::Causes) => true,
        FieldTy::Borrow | FieldTy::View(_) | FieldTy::Handle(_) => false,
        _ => true,
    }
}
