//! comptime Phase 0 (throwaway, never lands): the computed constants' values
//! table, keyed by the initializer's code address (COMPTIME-DIRECTION §3.3).
use crate::{CheckProduct, DefPath};
use hirsel_core_ids::ContentHash;
use hirsel_core_ir::{Identity, Static};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, Mutex};

/// One computed value and its hash.
pub(crate) struct Value {
    pub bytes: Arc<[u8]>,
    pub hash: ContentHash,
}

static VALUES: LazyLock<Mutex<rustc_hash::FxHashMap<ContentHash, Arc<Value>>>> =
    LazyLock::new(Default::default);

/// Initializer evaluations this process ran (G2, G5 to G7).
pub static EVALUATIONS: AtomicU64 = AtomicU64::new(0);

/// The step limit for one initializer.
const LIMIT: u64 = 1 << 32;

pub fn evaluations() -> u64 {
    EVALUATIONS.load(Ordering::Relaxed)
}

/// The value of the constant `init` computes: from the table by the
/// initializer's code address, else evaluated once on the pure core.
pub(crate) fn value(product: &CheckProduct, init: &DefPath) -> Result<Arc<Value>, String> {
    let identity = Identity::plain(init.clone());
    let units = product
        .units([identity.clone()], 1 << 20)
        .map_err(|e| format!("{e:?}"))?;
    let address = units
        .address(&identity)
        .ok_or_else(|| format!("no code address for {init}"))?;
    if let Some(v) = VALUES.lock().unwrap().get(&address) {
        return Ok(v.clone());
    }
    let modules = units.modules().map_err(|e| e.to_string())?;
    let (module, entry) = hirsel_core_materialize::join(&modules, &identity)?;
    let (bytes, _) = hirsel_core_ir::eval::run_constant(&module, entry, LIMIT)
        .map_err(|e| format!("evaluating {init}: {e}"))?;
    EVALUATIONS.fetch_add(1, Ordering::Relaxed);
    // G2, G5 to G7: one line per evaluation, outside cli.rs and the metrics schema.
    if let Some(log) = std::env::var_os("HIRSEL_COMPTIME_LOG") {
        use std::io::Write;
        if let Ok(mut f) = std::fs::OpenOptions::new().append(true).create(true).open(log) {
            let _ = writeln!(f, "{init} {address:?} {}", bytes.len());
        }
    }
    let value = Arc::new(Value {
        hash: ContentHash::of_bytes(&bytes),
        bytes: bytes.into(),
    });
    VALUES.lock().unwrap().insert(address, value.clone());
    Ok(value)
}

/// A definition's record folded with the value hash of each computed constant
/// its body reads: what reach and instance records key on. Unchanged when the
/// body reads none.
pub(crate) fn effective_own(product: &CheckProduct, path: &DefPath) -> Option<ContentHash> {
    let d = product.definitions.get(path)?;
    Some(fold(product, d.record, &d.check.computed))
}

pub(crate) fn fold(product: &CheckProduct, record: ContentHash, computed: &[DefPath]) -> ContentHash {
    if computed.is_empty() {
        return record;
    }
    let mut bytes = record.0.to_vec();
    for init in computed {
        match value(product, init) {
            Ok(v) => bytes.extend_from_slice(&v.hash.0),
            Err(e) => bytes.extend_from_slice(e.as_bytes()),
        }
    }
    ContentHash::of_bytes(&bytes)
}

/// Replace each `Static::Computed` in `module` with its value's bytes.
pub(crate) fn substitute(
    product: &CheckProduct,
    module: &mut hirsel_core_ir::Module,
) -> Result<(), String> {
    for data in &mut module.statics {
        if let Static::Computed { init } = data {
            let v = value(product, init)?;
            *data = Static::Bytes {
                align: 16,
                bytes: v.bytes.to_vec(),
            };
        }
    }
    Ok(())
}
