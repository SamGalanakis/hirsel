use super::*;
use crate::memory::{arc, errors, map, overflow, vector};
impl ViewTable {
    pub(super) fn table_control_bytes(&self) -> Option<u64> {
        let mut bytes = arc::<Self>()
            .checked_add(arc::<EnvironmentFacts>())?
            .checked_add(arc::<()>())?
            .checked_add(arc::<crate::FactGraph>())?
            .checked_add(vector(&self.views)?)?
            .checked_add(vector(&self.verdicts)?)?
            .checked_add(map::<DefId, u32>(self.index.capacity())?)?
            .checked_add(map::<DefId, Vec<u32>>(self.readers.capacity())?)?
            .checked_add(vector(&self.heap_readers)?)?
            .checked_add(errors(&self.errors.errors)?)?
            .checked_add(self.heap_evidence.retained_payload_bytes()?)?;
        for readers in self.readers.values() {
            bytes = bytes.checked_add(vector(readers)?)?;
        }
        if let Some(heap) = &self.heap_role {
            bytes = bytes.checked_add(heap_bytes(heap)?)?;
        }
        if let Some(installed) = &self.installed {
            bytes = bytes
                .checked_add((size_of_val(&*installed.parts) + 2 * size_of::<usize>()) as u64)?;
            for part in installed.parts.iter() {
                bytes = bytes
                    .checked_add(part.heap.retained_payload_bytes()?)?
                    .checked_add((size_of_val(&*part.errors.1) + 2 * size_of::<usize>()) as u64)?;
                for error in part.errors.1.iter() {
                    bytes = bytes.checked_add(crate::memory::violation(error)?)?;
                }
            }
        }
        Some(bytes)
    }
    pub fn retain_control(
        &self,
        memory: &Arc<dyn crate::ValidationMemory>,
    ) -> Result<(), ViewRefusal> {
        self._retention
            .retain(&**memory, || self.table_control_bytes())?;
        if let Some(installed) = &self.installed {
            installed.views.retain_control(memory)?;
        }
        for view in &self.views {
            view.input.retain_control(&**memory)?;
        }
        for verdict in &self.verdicts {
            verdict.retain_control(&**memory)?;
        }
        Ok(())
    }
}
impl<V> PreparedRows<V> {
    pub(super) fn layer_control_bytes(&self) -> Option<u64> {
        arc::<Self>()
            .checked_add(map::<DefId, V>(self.rows.capacity())?)?
            .checked_add(map::<DefId, ()>(self.removed.capacity())?)
    }
    fn retain_control(&self, memory: &dyn crate::ValidationMemory) -> Result<(), ViewRefusal> {
        let mut layer = Some(self);
        while let Some(rows) = layer {
            if rows.retention.get().is_none() {
                let bytes = rows.layer_control_bytes().ok_or_else(overflow)?;
                let _ = rows.retention.set(memory.retain(bytes)?);
            }
            layer = rows.parent.as_deref();
        }
        Ok(())
    }
}
impl PackageViews {
    pub(super) fn package_control_bytes(&self) -> Option<u64> {
        let mut bytes = arc::<Self>()
            .checked_add(arc::<EnvironmentFacts>())?
            .checked_add(arc::<()>())?
            .checked_add(arc::<crate::FactGraph>())?
            .checked_add(errors(&self.errors.errors)?)?
            .checked_add(self.heap_evidence.retained_payload_bytes()?)?;
        if let Some(heap) = &self.heap_role {
            bytes = bytes.checked_add(heap_bytes(heap)?)?;
        }
        Some(bytes)
    }
    pub fn retain_control(
        &self,
        memory: &Arc<dyn crate::ValidationMemory>,
    ) -> Result<(), ViewRefusal> {
        self.base.retain_control(memory)?;
        self._retention
            .retain(&**memory, || self.package_control_bytes())?;
        self.replaced.retain_control(&**memory)?;
        self.verdicts.retain_control(&**memory)?;
        let mut layer = Some(&*self.replaced);
        while let Some(rows) = layer {
            for view in rows.rows.values() {
                view.input.retain_control(&**memory)?;
            }
            layer = rows.parent.as_deref();
        }
        let mut layer = Some(&*self.verdicts);
        while let Some(rows) = layer {
            for verdict in rows.rows.values() {
                verdict.retain_control(&**memory)?;
            }
            layer = rows.parent.as_deref();
        }
        Ok(())
    }
}
