//! A linked object: the instance it was emitted for, the record key it is
//! stored under and its blob in the content store.
use crate::Store;
use crate::store::Bare;
use hirsel_core_ids::ContentHash;
use hirsel_core_ir::Identity;
use std::path::PathBuf;

pub struct ObjectProduct {
    pub private_variants: usize,
    pub instance: Identity,
    /// The key of the object's record; its coverage fragment is named from it.
    pub(crate) key: ContentHash,
    pub identity: ContentHash,
    pub path: PathBuf,
    /// The store that holds the object and, for a coverage object, its map
    /// fragment, which the link joins from here; none for an object held
    /// outside every store.
    pub(crate) store: Option<Store>,
    /// The object's read check (`Bare::check`).
    pub(crate) check: u64,
    /// The bytes this request checked or wrote, which the link hands the
    /// linker. None: a stored object named by a record, which the link reads
    /// and checks.
    pub(crate) bytes: Option<std::sync::Arc<[u8]>>,
    pub debug: Option<std::sync::Arc<hirsel_core_ir::source::NativeDebug>>,
    /// Stored debug rows: their store and blob. A link with debug lines
    /// reads them when `debug` is not in hand.
    pub(crate) debug_record: Option<(Store, Bare)>,
    pub compiled: bool,
}
impl ObjectProduct {
    /// A stored object named by a record this request did not write.
    pub(super) fn stored(
        instance: Identity,
        key: ContentHash,
        store: &Store,
        object: Bare,
        debug: Option<Bare>,
    ) -> Self {
        Self {
            private_variants: 0,
            instance,
            key,
            identity: object.address,
            path: crate::native_object::path(store, object.address),
            store: Some(store.clone()),
            check: object.check,
            bytes: None,
            debug: None,
            debug_record: debug.map(|d| (store.clone(), d)),
            compiled: false,
        }
    }
    /// An object this request read and checked, or emitted and wrote.
    pub(super) fn held(
        (instance, key): (Identity, ContentHash),
        store: &Store,
        object: Bare,
        native: hirsel_core_ir::source::NativeObject,
        debug: Option<Bare>,
        compiled: bool,
    ) -> Self {
        let mut product = Self::shared(
            (instance, key),
            store,
            (object, native.bytes.into()),
            native.debug.map(std::sync::Arc::new),
            compiled,
        );
        product.debug_record = debug.map(|d| (store.clone(), d));
        product
    }
    /// An object whose checked `bytes` this request holds, its debug rows
    /// `debug` in hand.
    pub(crate) fn shared(
        (instance, key): (Identity, ContentHash),
        store: &Store,
        (object, bytes): (Bare, std::sync::Arc<[u8]>),
        debug: Option<std::sync::Arc<hirsel_core_ir::source::NativeDebug>>,
        compiled: bool,
    ) -> Self {
        Self {
            private_variants: 0,
            instance,
            key,
            identity: object.address,
            path: crate::native_object::path(store, object.address),
            store: Some(store.clone()),
            check: object.check,
            bytes: Some(bytes),
            debug,
            debug_record: None,
            compiled,
        }
    }
    pub(super) fn bare(&self) -> Bare {
        Bare {
            address: self.identity,
            check: self.check,
        }
    }
}
