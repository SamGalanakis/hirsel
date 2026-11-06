//! Allocation-free preflight for demanded installed products. The walk follows
//! bincode 1.3.3's fixed-int structs, enum tags and sequence lengths
//! (de/mod.rs:293-350,402-411), without constructing semantic values.
use crate::{EnvelopeLayout, TypeRecords};
use hirsel_artifact_store::Bytes;
use hirsel_engine_admission::MemoryReservation;
use hirsel_syntax as ast;
use hirsel_types as ty;
use std::{marker::PhantomData, sync::Arc};

pub struct InstalledHeader;
pub struct InstalledSyntax;
pub struct RunContent;

/// Exact immutable backing, consumer schema and type owner. This value cannot
/// be deserialized, and decoding never accepts a producer's allocation claim.
pub struct DecodeRequirement<S> {
    bytes: Bytes,
    required: u64,
    retained: u64,
    records: Option<Arc<TypeRecords>>,
    schema: PhantomData<S>,
}
impl<S> DecodeRequirement<S> {
    pub fn required_bytes(&self) -> u64 {
        self.required
    }
    /// Conservative decoded allocation bound after the input buffer and
    /// per-record binding directories disappear. Thread-local scope capacity
    /// stays charged. Session interner rows keep their existing owner.
    pub fn retained_bytes(&self) -> u64 {
        self.retained
    }
}
pub struct Scan<'a> {
    bytes: &'a [u8],
    required: u64,
    anchors: u32,
    types: u32,
}
impl<'a> Scan<'a> {
    pub(crate) fn record(bytes: &'a [u8]) -> Option<Self> {
        let layout = EnvelopeLayout::open(bytes)?;
        let mut scan = Self {
            bytes: &bytes[layout.payload_start..],
            required: bytes.len().try_into().ok()?,
            anchors: layout.anchors,
            types: layout.types,
        };
        scan.required = scan
            .required
            .checked_add(hirsel_types::collection_scope_working_bytes(1)?)?
            .checked_add(ast::def_path_scope_working_bytes(1)?)?
            .checked_add(hirsel_intern::persist::symbol_scope_working_bytes(1)?)?;
        // Temporary binding directories, source paths and ProductTypes owner.
        scan.add(size_of::<crate::ProductTypes>() + 4 * size_of::<usize>())?;
        scan.add(
            (layout.types as usize)
                .checked_mul(2)?
                .max(4)
                .checked_mul(size_of::<crate::TypeRef>())?,
        )?;
        scan.add(
            (layout.anchors as usize)
                .checked_mul(2)?
                .max(4)
                .checked_mul(size_of::<hirsel_span::Anchor>())?,
        )?;
        for n in 0..layout.anchors {
            let owner = layout.anchor(bytes, n)?;
            scan.add(size_of::<ast::DefPath>())?;
            scan.add(
                owner
                    .package
                    .len()
                    .checked_add(owner.version.len())?
                    .checked_add(owner.name.len())?,
            )?;
            scan.add(
                owner
                    .modules()
                    .len()
                    .checked_mul(2)?
                    .max(4)
                    .checked_mul(size_of::<String>())?,
            )?;
            for module in owner.modules() {
                scan.add(module.len())?;
            }
        }
        Some(scan)
    }
    pub fn raw_reads(bytes: &'a [u8]) -> Self {
        Self::raw(bytes, 0, 0)
    }
    pub(crate) fn raw(bytes: &'a [u8], anchors: u32, types: u32) -> Self {
        Self {
            bytes,
            required: 0,
            anchors,
            types,
        }
    }
    pub(crate) fn required_bytes(&self) -> u64 {
        self.required
    }
    pub fn finish(self) -> Option<u64> {
        self.bytes.is_empty().then_some(self.required)
    }
    pub fn read_u8(&mut self) -> Option<u8> {
        Some(self.take(1)?[0])
    }
    pub fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let (head, tail) = self.bytes.split_at_checked(n)?;
        self.bytes = tail;
        Some(head)
    }
    pub fn word(&mut self) -> Option<u32> {
        Some(u32::from_le_bytes(self.take(4)?.try_into().ok()?))
    }
    pub fn length(&mut self) -> Option<usize> {
        u64::from_le_bytes(self.take(8)?.try_into().ok()?)
            .try_into()
            .ok()
    }
    pub fn add(&mut self, n: usize) -> Option<()> {
        self.required = self.required.checked_add(n.try_into().ok()?)?;
        Some(())
    }
    pub fn vector<T: Walk>(&mut self) -> Option<usize> {
        let n = self.length()?;
        // These installed schemas have no zero-byte sequence element.
        if n > self.bytes.len() {
            return None;
        }
        if n != 0 {
            // Serde 1.0.229 de/impls.rs:1161-1165 reserves the complete
            // bincode size hint through private/size_hint.rs:12-24's 1 MiB
            // preallocation threshold. Only larger vectors can reallocate.
            let slots = if n <= (1024 * 1024) / size_of::<T>().max(1) {
                n
            } else {
                n.checked_mul(3)?.max(4)
            };
            self.add(slots.checked_mul(size_of::<T>())?)?;
        }
        for _ in 0..n {
            T::walk(self)?;
        }
        Some(n)
    }
}
pub trait Walk {
    fn walk(scan: &mut Scan<'_>) -> Option<()>;
}
pub fn field<T, F: Walk>(_: impl FnOnce(&T) -> &F, scan: &mut Scan<'_>) -> Option<()> {
    F::walk(scan)
}
#[macro_export]
macro_rules! decode_structure {
    ($ty:ty { $($field:ident),* $(,)? } $(skip $($skip:ident),*)?) => {
        impl $crate::decode_schema::Walk for $ty { fn walk(scan: &mut $crate::decode_schema::Scan<'_>) -> Option<()> {
            // Exhaustive destructuring makes added schema fields a compile error.
            let _: fn(&$ty) = |value| { let Self { $($field: _,)* $($($skip: _,)*)? } = value; };
            $($crate::decode_schema::field(|value: &$ty| &value.$field, scan)?;)*
            Some(())
        }}
    };
}
#[doc(hidden)]
#[macro_export]
macro_rules! decode_ignore {
    ($name:ident) => {
        _
    };
}
#[doc(hidden)]
#[macro_export]
macro_rules! decode_tuple_fields {
    ($scan:ident, $variant:ident, $pattern:tt; $head:ident $(,$tail:ident)*) => {
        $crate::decode_schema::field(#[allow(unused_variables)] |value: &Self| match value {
            Self::$variant $pattern => $head, _ => unreachable!()
        }, $scan)?;
        $crate::decode_tuple_fields!($scan, $variant, $pattern; $($tail),*);
    };
    ($scan:ident, $variant:ident, $pattern:tt;) => {};
}
#[macro_export]
macro_rules! decode_variants {
    ($ty:ty { $($variant:ident $(($($tuple:ident),*))? $({$($named:ident),*})?),* $(,)? }) => {
        impl $crate::decode_schema::Walk for $ty { fn walk(scan: &mut $crate::decode_schema::Scan<'_>) -> Option<()> {
            let _: fn(&Self) = |value| { match value { $(Self::$variant $(($($crate::decode_ignore!($tuple)),*))? $({$($named: _),*})? => (),)* }};
            let readers: &[fn(&mut $crate::decode_schema::Scan<'_>) -> Option<()>] = &[$(|scan| {
                $($crate::decode_tuple_fields!(scan, $variant, ($($tuple),*); $($tuple),*);)?
                $($($crate::decode_schema::field(|value: &Self| match value { Self::$variant { $named, .. } => $named, _ => unreachable!() }, scan)?;)*)?
                let _ = scan;
                Some(())
            }),*];
            readers.get(scan.word()? as usize)?(scan)
        }}
    };
}

use crate::{decode_structure as structure, decode_variants as variants};
macro_rules! scalar { ($($ty:ty => $n:expr),* $(,)?) => {$(impl Walk for $ty { fn walk(scan: &mut Scan<'_>) -> Option<()> { scan.take($n)?; Some(()) } })*}; }
scalar!(usize=>8, isize=>8, u8=>1, u16=>2, u32=>4, u64=>8, u128=>16, i8=>1, i16=>2, i32=>4, i64=>8, i128=>16, f32=>4, f64=>8);
impl Walk for bool {
    fn walk(scan: &mut Scan<'_>) -> Option<()> {
        (*scan.take(1)?.first()? <= 1).then_some(())
    }
}
impl<T: Walk> Walk for Option<T> {
    fn walk(scan: &mut Scan<'_>) -> Option<()> {
        match scan.take(1)?[0] {
            0 => Some(()),
            1 => T::walk(scan),
            _ => None,
        }
    }
}
impl<T: Walk> Walk for Vec<T> {
    fn walk(scan: &mut Scan<'_>) -> Option<()> {
        scan.vector::<T>()?;
        Some(())
    }
}
impl<T: Walk> Walk for Box<T> {
    fn walk(scan: &mut Scan<'_>) -> Option<()> {
        scan.add(size_of::<T>())?;
        T::walk(scan)
    }
}
impl<T: Walk> Walk for Arc<T> {
    fn walk(scan: &mut Scan<'_>) -> Option<()> {
        scan.add(
            size_of::<T>()
                .checked_mul(2)?
                .checked_add(2 * size_of::<usize>())?,
        )?;
        T::walk(scan)
    }
}
impl<T: Walk> Walk for ty::SharedSlice<T> {
    fn walk(scan: &mut Scan<'_>) -> Option<()> {
        let n = scan.vector::<T>()?;
        if n != 0 {
            scan.add(
                n.checked_mul(size_of::<T>())?
                    .checked_add(2 * size_of::<usize>())?,
            )?;
        }
        Some(())
    }
}
impl Walk for String {
    fn walk(scan: &mut Scan<'_>) -> Option<()> {
        let n = scan.length()?;
        std::str::from_utf8(scan.take(n)?).ok()?;
        scan.add(n)
    }
}
impl<A: Walk, B: Walk> Walk for (A, B) {
    fn walk(scan: &mut Scan<'_>) -> Option<()> {
        A::walk(scan)?;
        B::walk(scan)
    }
}
impl<T: Walk> Walk for Box<[T]> {
    fn walk(scan: &mut Scan<'_>) -> Option<()> {
        let n = scan.vector::<T>()?;
        scan.add(n.checked_mul(size_of::<T>())?)
    }
}
impl<T: Walk> Walk for Arc<[T]> {
    fn walk(scan: &mut Scan<'_>) -> Option<()> {
        let n = scan.vector::<T>()?;
        scan.add(
            n.checked_mul(size_of::<T>())?
                .checked_add(2 * size_of::<usize>())?,
        )
    }
}
impl<K: Walk, V: Walk> Walk for std::collections::BTreeMap<K, V> {
    fn walk(scan: &mut Scan<'_>) -> Option<()> {
        let n = scan.length()?;
        if n > scan.bytes.len() {
            return None;
        }
        scan.add(n.checked_mul(11 * (size_of::<K>() + size_of::<V>()) + 16 * size_of::<usize>())?)?;
        for _ in 0..n {
            K::walk(scan)?;
            V::walk(scan)?;
        }
        Some(())
    }
}
impl<K: Walk> Walk for std::collections::BTreeSet<K> {
    fn walk(scan: &mut Scan<'_>) -> Option<()> {
        std::collections::BTreeMap::<K, ()>::walk(scan)
    }
}
impl Walk for () {
    fn walk(_: &mut Scan<'_>) -> Option<()> {
        Some(())
    }
}
macro_rules! tuples { ($($name:ident),+) => {
    impl<$($name: Walk),+> Walk for ($($name,)+) {
        fn walk(scan: &mut Scan<'_>) -> Option<()> { $($name::walk(scan)?;)+ Some(()) }
    }
}; }
tuples!(A, B, C);
tuples!(A, B, C, D);
tuples!(A, B, C, D, E, F, G);
tuples!(A, B, C, D, E, F, G, H, I, J);
impl<T: Walk, const N: usize> Walk for [T; N] {
    fn walk(scan: &mut Scan<'_>) -> Option<()> {
        for _ in 0..N {
            T::walk(scan)?;
        }
        Some(())
    }
}
impl Walk for hirsel_intern::Symbol {
    fn walk(scan: &mut Scan<'_>) -> Option<()> {
        String::walk(scan)
    }
}
impl Walk for ast::DefId {
    fn walk(scan: &mut Scan<'_>) -> Option<()> {
        ast::DefPath::walk(scan)
    }
}
impl Walk for ty::TypeId {
    fn walk(scan: &mut Scan<'_>) -> Option<()> {
        (scan.word()? < scan.types).then_some(())
    }
}
impl Walk for hirsel_span::Anchor {
    fn walk(scan: &mut Scan<'_>) -> Option<()> {
        (scan.word()? <= scan.anchors).then_some(())
    }
}
impl Walk for hirsel_span::Span {
    fn walk(scan: &mut Scan<'_>) -> Option<()> {
        hirsel_span::Anchor::walk(scan)?;
        scan.take(8)?;
        Some(())
    }
}
impl<P: Walk> Walk for ty::BinderRef<P> {
    fn walk(scan: &mut Scan<'_>) -> Option<()> {
        match scan.word()? {
            0 => {
                u32::walk(scan)?;
                P::walk(scan)
            }
            1 => {
                ast::SiteRef::walk(scan)?;
                P::walk(scan)
            }
            _ => None,
        }
    }
}
impl<T: Walk> Walk for ty::SharedPath<T> {
    fn walk(scan: &mut Scan<'_>) -> Option<()> {
        let n = scan.vector::<T>()?;
        {
            struct Node<T> {
                _prefix: ty::SharedPath<T>,
                _value: T,
                _len: usize,
            }
            let bytes = std::alloc::Layout::new::<[usize; 2]>()
                .extend(std::alloc::Layout::new::<Node<T>>())
                .ok()?
                .0
                .pad_to_align()
                .size();
            scan.add(n.checked_mul(bytes)?)
        }
    }
}
scalar!(ast::TokenId=>4, ast::ExprId=>4, ast::PatternId=>4, ast::TypeExprId=>4, ast::StmtId=>4, ty::ParamId=>4, ty::FieldId=>4, ty::TypeParamId=>4, ty::SizeParamId=>4);
impl Walk for hirsel_diag::RuleId {
    fn walk(scan: &mut Scan<'_>) -> Option<()> {
        String::walk(scan)
    }
}
structure!(ast::LexicalOwner { def, occurrence });
structure!(ast::SiteRef { owner, site });
variants!(ast::NameSite {Expression(f0), Pattern(f0), Type(f0), Statement(f0), Item});
mod ordinary;
mod schema;

pub fn record_decode_requirement<T: Walk>(bytes: &[u8]) -> Option<u64> {
    let mut scan = Scan::record(bytes)?;
    T::walk(&mut scan)?;
    scan.finish()
}
impl TypeRecords {
    fn installed_requirement<S, T: Walk>(
        self: &Arc<Self>,
        bytes: Bytes,
    ) -> Option<DecodeRequirement<S>> {
        let mut scan = Scan::record(&bytes)?;
        let scopes = hirsel_types::collection_scope_working_bytes(1)?
            .checked_add(ast::def_path_scope_working_bytes(1)?)?
            .checked_add(hirsel_intern::persist::symbol_scope_working_bytes(1)?)?;
        let transient = scan.required.checked_sub(scopes)?;
        T::walk(&mut scan)?;
        if !scan.bytes.is_empty() {
            return None;
        }
        let required = scan.required;
        Some(DecodeRequirement {
            bytes,
            required,
            retained: required.checked_sub(transient)?,
            records: Some(self.clone()),
            schema: PhantomData,
        })
    }
    pub fn record_requirement<T: Walk>(
        self: &Arc<Self>,
        bytes: Bytes,
    ) -> Option<DecodeRequirement<T>> {
        self.installed_requirement::<T, T>(bytes)
    }
    pub fn installed_header(
        self: &Arc<Self>,
        bytes: Bytes,
    ) -> Option<DecodeRequirement<InstalledHeader>> {
        self.installed_requirement::<InstalledHeader, (ty::SignatureRef, ty::SignatureRef)>(bytes)
    }
    pub fn installed_syntax(
        self: &Arc<Self>,
        bytes: Bytes,
    ) -> Option<DecodeRequirement<InstalledSyntax>> {
        self.installed_requirement::<InstalledSyntax, Arc<ast::ItemSyntax>>(bytes)
    }
}
impl<S> DecodeRequirement<S> {
    fn decode_record<T: serde::de::DeserializeOwned>(
        self,
        reservation: &MemoryReservation,
        anchor: impl FnMut(&ast::DefPath, u32) -> Option<hirsel_span::Anchor>,
    ) -> Option<T> {
        if self.required > reservation.bytes() {
            return None;
        }
        self.records.as_ref()?.decode_record(&self.bytes, anchor)
    }
}
impl<T: Walk + serde::de::DeserializeOwned> DecodeRequirement<T> {
    pub fn plain(bytes: Bytes) -> Option<Self> {
        let mut scan = Scan::raw(&bytes, 0, 0);
        T::walk(&mut scan)?;
        let required = scan.finish()?.checked_add(bytes.len() as u64)?;
        Some(Self {
            retained: required.checked_sub(bytes.len() as u64)?,
            bytes,
            required,
            records: None,
            schema: PhantomData,
        })
    }
    /// Hold the consumer's reservation through semantic deserialization.
    pub fn decode_reserved<R>(
        self,
        reserve: impl FnOnce(u64) -> Option<R>,
        anchor: impl FnMut(&ast::DefPath, u32) -> Option<hirsel_span::Anchor>,
    ) -> Option<T> {
        let _reservation = reserve(self.required)?;
        match &self.records {
            Some(records) => records.decode_record(&self.bytes, anchor),
            None => crate::options(self.bytes.len())
                .deserialize(&self.bytes)
                .ok(),
        }
    }
}
impl DecodeRequirement<InstalledHeader> {
    pub fn decode(
        self,
        reservation: &MemoryReservation,
        anchor: impl FnMut(&ast::DefPath, u32) -> Option<hirsel_span::Anchor>,
    ) -> Option<(ty::SignatureRef, ty::SignatureRef)> {
        self.decode_record::<(ty::SignatureRef, ty::SignatureRef)>(reservation, anchor)
    }
}
impl DecodeRequirement<InstalledSyntax> {
    pub fn decode(
        self,
        reservation: &MemoryReservation,
        anchor: impl FnMut(&ast::DefPath, u32) -> Option<hirsel_span::Anchor>,
    ) -> Option<Arc<ast::ItemSyntax>> {
        self.decode_record::<Arc<ast::ItemSyntax>>(reservation, anchor)
    }
}
impl DecodeRequirement<RunContent> {
    pub fn open(bytes: Bytes) -> Option<Self> {
        let mut scan = Scan {
            bytes: &bytes,
            required: bytes.len().try_into().ok()?,
            anchors: 0,
            types: 0,
        };
        Vec::<Vec<u8>>::walk(&mut scan)?;
        if !scan.bytes.is_empty() {
            return None;
        }
        let required = scan.required;
        Some(Self {
            retained: required.checked_sub(bytes.len() as u64)?,
            bytes,
            required,
            records: None,
            schema: PhantomData,
        })
    }
    pub fn decode_content(self, reservation: &MemoryReservation) -> Option<Vec<Vec<u8>>> {
        if self.required > reservation.bytes() {
            return None;
        }
        crate::options(self.bytes.len())
            .deserialize(&self.bytes)
            .ok()
    }
}
