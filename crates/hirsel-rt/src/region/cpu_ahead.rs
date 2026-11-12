//! Idle-side regional allocation retains the ordinary arena's segment rules.
use super::*;

pub(crate) struct Plan {
    pub(crate) region: *mut Region,
    index: u32,
    extent: u64,
    align: u64,
    flags: u32,
    need: Option<u64>,
    schedule: bool,
}

impl Plan {
    pub(crate) fn charge(&self) -> u64 {
        self.need.map_or(0, canonical_size)
    }

    /// The exact saved mapper still owns this active region. Nothing mutates
    /// the region or pool until backing is admitted; refusal leaves it intact.
    pub(crate) unsafe fn allocate(
        &self,
        ctx: *mut TaskCtx,
    ) -> Result<*mut Header, crate::pool::cpu_scoped::ledger::BackingFull> {
        unsafe {
            if let Some(need) = self.need {
                (*ctx).pool().reserve_cpu_backing(need)?;
            }
            Ok(arena_alloc(
                ctx,
                self.region,
                self.index,
                self.extent,
                self.align,
                self.flags,
                true,
                self.schedule,
            ))
        }
    }
}

/// Only an active region in the saved task's bounded region chain is accepted.
/// This checks addresses before reading the caller-supplied region pointer.
pub(crate) unsafe fn plan(
    ctx: *mut TaskCtx,
    region: *mut Region,
    index: u32,
    extent: u64,
    align: u64,
    flags: u32,
    schedule: bool,
) -> Option<Plan> {
    unsafe {
        if region.is_null()
            || index >= (*ctx).heap_ref().layout_count()
            || extent > crate::alloc::EXTENT_MAX
            || align == 0
            || !align.is_power_of_two()
            || align > 1 << 30
            || flags & !ALLOC_TRACKED != 0
        {
            return None;
        }
        let mut active = (*ctx).region;
        let mut found = false;
        for _ in 0..crate::budget::stretch::RETAINED_MAX {
            if active.is_null() {
                break;
            }
            if active == region {
                found = true;
                break;
            }
            active = (*active).parent;
        }
        if !found || (*region).state != REGION_ACTIVE {
            return None;
        }
        let layout = &*(*ctx).descriptor(index);
        let bare = layout.flags & LAYOUT_NO_EXTENT != 0;
        if layout.size > extent || (bare && (extent != layout.size || align != layout.align)) {
            return None;
        }
        let physical = align.max(8);
        let prefix = if flags & ALLOC_TRACKED != 0 {
            TRACKED_PREFIX_BYTES
        } else if bare {
            8
        } else {
            24
        };
        let reserved = prefix.checked_add(extent)?.checked_add(physical - 8)?;
        let schedule = schedule && layout.step_count != 0;
        if reserved > crate::alloc::EXTENT_MAX {
            return None;
        }
        let (need, remaining) = match reservation(&*region, reserved, 8, true) {
            Ok(start) => (
                None,
                (segment_end(&*region).addr() - start.addr()) as u64 - reserved,
            ),
            Err(need) => (
                Some(need),
                canonical_size(need) - SEGMENT_HEADER as u64 - reserved,
            ),
        };
        if need.is_some_and(|need| need > crate::alloc::EXTENT_MAX) {
            return None;
        }
        // Entry's size and the segment end are word aligned, so this is also
        // the fit test after aligning the post-block cursor for record().
        let need = if schedule && remaining < size_of::<Entry>() as u64 {
            // Regrowth of an existing segment leaves geometric slack for its
            // cleanup. Only a block that fits the old segment can need a new
            // standard segment for record(), so one preflight still suffices.
            if need.is_some() {
                return None;
            }
            Some(SEGMENT_HEADER as u64 + size_of::<Entry>() as u64 + align_of::<Entry>() as u64 - 1)
        } else {
            need
        };
        Some(Plan {
            region,
            index,
            extent,
            align,
            flags,
            need,
            schedule,
        })
    }
}

/// A compiler-local reservation has no Header and no counted generation.
/// Its active region and the saved frame retain the raw storage.
pub(crate) struct RawPlan {
    region: *mut Region,
    extent: u64,
    align: u64,
    need: Option<u64>,
}

impl RawPlan {
    pub(crate) fn charge(&self) -> u64 {
        self.need.map_or(0, canonical_size)
    }

    /// # Safety
    /// The checked saved frame still owns the region accepted by raw_plan.
    pub(crate) unsafe fn allocate(
        &self,
        ctx: *mut TaskCtx,
    ) -> Result<*mut u8, crate::pool::cpu_scoped::ledger::BackingFull> {
        // SAFETY: the idle service prevents resumption and owns this region.
        unsafe {
            if let Some(need) = self.need {
                (*ctx).pool().reserve_cpu_backing(need)?;
            }
            Ok(reserve_in_mode(
                &mut *ctx,
                self.region,
                self.extent,
                self.align,
                false,
                true,
            ))
        }
    }
}

/// # Safety
/// ctx is the saving task's live context; the region address is untrusted.
pub(crate) unsafe fn raw_plan(
    ctx: *mut TaskCtx,
    region: *mut Region,
    extent: u64,
    align: u64,
) -> Option<RawPlan> {
    if region.is_null()
        || extent > crate::alloc::EXTENT_MAX
        || align == 0
        || !align.is_power_of_two()
        || align > 1 << 30
    {
        return None;
    }
    // SAFETY: only the trusted context chain is read until address membership
    // is proved. The bound is the existing regional custody cap.
    unsafe {
        let mut active = (*ctx).region;
        let mut found = false;
        for _ in 0..crate::budget::stretch::RETAINED_MAX {
            if active.is_null() {
                break;
            }
            if active == region {
                found = true;
                break;
            }
            active = (*active).parent;
        }
        if !found || (*region).state != REGION_ACTIVE {
            return None;
        }
        let need = reservation(&*region, extent, align, false).err();
        if need.is_some_and(|need| need > crate::alloc::EXTENT_MAX) {
            return None;
        }
        Some(RawPlan {
            region,
            extent,
            align,
            need,
        })
    }
}
