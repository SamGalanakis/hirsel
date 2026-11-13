//! Interned identity for ordinary closed type and size actuals.
//! Type spellings omit modes and callable parameter names; sizes are decimal.
use crate::*;
use hirsel_intern::Symbol;
use std::sync::Arc;

/// Separates actual spellings; no canonical type spelling contains it.
const SEPARATOR: char = '\u{0}';

/// Interned closed generic actuals. Equality and hashing compare ids; ordering
/// compares their canonical spellings.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ClosedActualKey(Symbol);

impl ClosedActualKey {
    /// Each actual's spelling, in argument order.
    pub fn spellings(self) -> Vec<&'static str> {
        let text = self.0.as_str();
        if text.is_empty() {
            return vec![];
        }
        text[1..].split(SEPARATOR).collect()
    }
}
impl std::fmt::Debug for ClosedActualKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("ClosedActualKey")
            .field(&self.spellings())
            .finish()
    }
}

/// The key of closed `arguments`; None when an argument still names a
/// parameter. Each type's spelling is memoized per TypeId.
pub fn closed_actual_key(
    t: &TypeInterner,
    arguments: &[GenericArgument],
) -> Option<ClosedActualKey> {
    let mut key = String::new();
    for argument in arguments {
        key.push(SEPARATOR);
        match argument {
            GenericArgument::Type(ty) => key.push_str(&closed_spelling(t, *ty)?),
            GenericArgument::Size(Size::Constant(n)) => key.push_str(&n.to_string()),
            GenericArgument::Size(Size::Parameter(_)) => return None,
        }
    }
    Some(ClosedActualKey(Symbol::intern(&key)))
}

/// Closed spelling of `ty`, or None if it names a parameter or its
/// graph is invalid (a recovery row or a cycle has no instance).
fn closed_spelling(t: &TypeInterner, ty: TypeId) -> Option<Arc<str>> {
    let compute = || {
        let _retained = t.retained_owner(ty).enter();
        (t.valid(ty) && !mentions_parameter(t, ty))
            .then(|| Arc::from(CanonicalSpelling::closed(t, ty)))
    };
    match t.entry(ty) {
        Some(entry) => entry.actual_key.get_or_init(compute).clone(),
        None => compute(),
    }
}

/// Persisted as the spellings, never as a session id.
impl serde::Serialize for ClosedActualKey {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeSeq;
        let spellings = self.0.as_str().split(SEPARATOR).skip(1);
        let mut sequence = s.serialize_seq(Some(spellings.clone().count()))?;
        for spelling in spellings {
            sequence.serialize_element(spelling)?;
        }
        sequence.end()
    }
}
impl<'de> serde::Deserialize<'de> for ClosedActualKey {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let spellings: Vec<String> = serde::Deserialize::deserialize(d)?;
        let mut key = String::new();
        for spelling in spellings {
            key.push(SEPARATOR);
            key.push_str(&spelling);
        }
        Ok(ClosedActualKey(Symbol::intern(&key)))
    }
}
