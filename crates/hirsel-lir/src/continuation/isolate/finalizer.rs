//! Exact source identity for an opt-in entered finalizer. This is only the
//! descriptor/borrow seam: it grants no compact admission, retained address,
//! cleanup coverage or permission to return Pending.
use super::*;
use crate::abi_types::{FnAbiId, ParameterPassing};
use crate::stage::ValidatedUnit;
use hirsel_core_contract::Passing;
use hirsel_syntax::ContentHash;

#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub struct FinalizerSource {
    pub layout: LayoutId,
    pub body: ContentHash,
    pub digest: ContentHash,
    pub abi: FnAbiId,
    pub borrowed_self: ValueId,
}

impl FinalizerSource {
    /// Resolve the supplied program association against the actual resource
    /// descriptor and validated Ready body. A matching name alone is insufficient.
    pub fn derive(
        units: &[&ValidatedUnit<Ready>],
        table: &LayoutTable,
        layout: LayoutId,
        body: ContentHash,
    ) -> Result<Self, &'static str> {
        let validated = units
            .iter()
            .find(|u| u.unit().header.instance == body)
            .ok_or("missing finalizer source")?;
        let unit = validated.unit();
        let [borrowed_self] = unit.parameters.as_slice() else {
            return Err("finalizer source parameter count");
        };
        let source = Self {
            layout,
            body,
            digest: validated.digest(),
            abi: unit.header.abi,
            borrowed_self: *borrowed_self,
        };
        source.check(units, table)?;
        Ok(source)
    }

    /// Check the supplied association and signature against descriptor and
    /// Ready bytes, independently of any runtime resolver or continuation hash.
    pub fn check(
        &self,
        units: &[&ValidatedUnit<Ready>],
        table: &LayoutTable,
    ) -> Result<(), &'static str> {
        let record = table
            .layout(self.layout)
            .ok_or("unknown finalizer layout")?;
        let declared = record.finalizer.as_ref().ok_or("layout has no finalizer")?;
        let validated = units
            .iter()
            .find(|u| u.unit().header.instance == self.body)
            .ok_or("unavailable finalizer body")?;
        let unit = validated.unit();
        if unit.header.definition != declared.definition
            || validated.digest() != self.digest
            || unit.header.abi != self.abi
            || !unit.captures.is_empty()
        {
            return Err("finalizer source identity");
        }
        let abi = table
            .fn_abis
            .get(self.abi.0 as usize)
            .ok_or("missing finalizer ABI")?;
        let [parameter] = abi.parameters.as_slice() else {
            return Err("finalizer ABI parameter count");
        };
        let expected = if unit.header.origin == UnitOrigin::Library {
            (Passing::Mut, ParameterPassing::ExclusiveAddress)
        } else {
            (Passing::Borrow, ParameterPassing::ReadOnlyAddress)
        };
        if !abi.context
            || abi.environment
            || abi.destination
            || abi.diverges
            || abi.result.is_some()
            || abi.result_arena.is_some()
            || parameter.source != 0
            || parameter.layout != self.layout
            || (parameter.convention, parameter.passing) != expected
            || unit.parameters.as_slice() != [self.borrowed_self]
        {
            return Err("finalizer borrowed-self ABI");
        }
        let value = unit
            .values
            .get(self.borrowed_self.index())
            .ok_or("missing finalizer self")?;
        if unit
            .types
            .get(value.ty.index())
            .is_none_or(|ty| ty.layout != self.layout)
        {
            return Err("finalizer self layout");
        }
        Ok(())
    }
}

/// The finalizer body of each layout as `unit` runs it. A finalizer with a
/// charged instance (STORE-11 energy) has two units: a charged unit's frames
/// run the charged one, any other unit's frames the uncharged one, as the drop
/// wrapper picks them by the running context.
pub(crate) fn sources(
    units: &[&ValidatedUnit<Ready>],
    table: &LayoutTable,
    unit: ContentHash,
) -> std::collections::BTreeMap<LayoutId, ContentHash> {
    let instances: std::collections::BTreeSet<ContentHash> =
        units.iter().map(|u| u.unit().header.instance).collect();
    let charged: std::collections::BTreeSet<ContentHash> = instances
        .iter()
        .map(|&i| crate::energy::charged_digest(i))
        .filter(|c| instances.contains(c))
        .collect();
    let in_charged = charged.contains(&unit);
    let mut found = std::collections::BTreeMap::new();
    for (index, layout) in table.layouts.iter().enumerate() {
        let Some(finalizer) = &layout.finalizer else {
            continue;
        };
        let id = LayoutId(index as u32);
        let matching: Vec<_> = units
            .iter()
            .filter(|u| u.unit().header.definition == finalizer.definition)
            .filter(|u| charged.contains(&u.unit().header.instance) == in_charged)
            .filter_map(|u| {
                FinalizerSource::derive(units, table, id, u.unit().header.instance).ok()
            })
            .collect();
        if let [source] = matching.as_slice() {
            found.insert(id, source.body);
        }
    }
    found
}
