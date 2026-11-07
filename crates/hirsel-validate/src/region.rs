//! Chunk ownership follows bumpalo 3.20.3 src/lib.rs:488-505,2516-2528:
//! collections drop their elements; the last allocator owner frees the chunks.
//! Unlike bumpalo, admission precedes each physical chunk allocation, and a
//! mutex permits immutable collections to be shared across compiler threads.
use crate::{ValidationMemory, ViewRefusal};
use allocator_api2::alloc::{AllocError, Allocator, Global, Layout};
use std::{
    ptr::NonNull,
    sync::{Arc, Mutex},
};

pub(crate) type Vector<T> = allocator_api2::vec::Vec<T, Region>;
pub(crate) type Map<K, V> = hashbrown::HashMap<K, V, hirsel_intern::IdHasher, Region>;

#[derive(Clone, Default)]
pub(crate) struct Region(Arc<Mutex<State>>);
#[derive(Default)]
struct State {
    head: Option<NonNull<Chunk>>,
    memory: Option<Arc<dyn ValidationMemory>>,
    owner: Option<Arc<dyn Send + Sync>>,
    refused: Option<ViewRefusal>,
}
struct Chunk {
    previous: Option<NonNull<Chunk>>,
    layout: Layout,
    used: usize,
    owner: Option<Arc<dyn Send + Sync>>,
}
// SAFETY: every chunk link, cursor and admission owner is accessed under the
// region mutex. Allocated disjoint ranges belong to the calling collection.
unsafe impl Send for State {}
impl Drop for State {
    fn drop(&mut self) {
        while let Some(head) = self.head {
            // SAFETY: links are initialized once and owned exclusively by this
            // last region owner. All collections have already dropped elements.
            let chunk = unsafe { head.as_ptr().read() };
            self.head = chunk.previous;
            // SAFETY: this is the original Global allocation and layout.
            unsafe { Global.deallocate(head.cast(), chunk.layout) };
            drop(chunk.owner);
        }
    }
}
impl Region {
    pub(crate) fn vector<T>(&self) -> Vector<T> {
        Vector::new_in(self.clone())
    }
    pub(crate) fn map<K, V>(&self) -> Map<K, V> {
        Map::with_hasher_in(Default::default(), self.clone())
    }
    pub(crate) fn collect<T>(
        &self,
        values: impl IntoIterator<Item = T>,
    ) -> Result<Vector<T>, ViewRefusal> {
        let values = values.into_iter();
        let mut result = self.vector();
        result
            .try_reserve(values.size_hint().0)
            .map_err(|_| self.refusal())?;
        for value in values {
            self.push(&mut result, value)?;
        }
        Ok(result)
    }
    pub(crate) fn push<T>(&self, values: &mut Vector<T>, value: T) -> Result<(), ViewRefusal> {
        values.try_reserve(1).map_err(|_| self.refusal())?;
        values.push(value);
        Ok(())
    }
    pub(crate) fn insert<K: Eq + std::hash::Hash, V>(
        &self,
        map: &mut Map<K, V>,
        key: K,
        value: V,
    ) -> Result<(), ViewRefusal> {
        map.try_reserve(1).map_err(|_| self.refusal())?;
        map.insert(key, value);
        Ok(())
    }
    pub(crate) fn refusal(&self) -> ViewRefusal {
        self.0
            .lock()
            .unwrap()
            .refused
            .take()
            .unwrap_or_else(crate::memory::overflow)
    }
    pub(crate) fn memory(&self) -> Option<Arc<dyn ValidationMemory>> {
        self.0.lock().unwrap().memory.clone()
    }
    pub(crate) fn bytes(&self) -> Option<u64> {
        let state = self.0.lock().unwrap();
        let mut bytes = crate::memory::arc::<Mutex<State>>();
        let mut next = state.head;
        while let Some(chunk) = next {
            // SAFETY: the region mutex keeps this initialized link live.
            let chunk = unsafe { chunk.as_ref() };
            bytes = bytes.checked_add(chunk.layout.size() as u64)?;
            next = chunk.previous;
        }
        Some(bytes)
    }
    pub(crate) fn retain(&self, memory: &Arc<dyn ValidationMemory>) -> Result<(), ViewRefusal> {
        let mut state = self.0.lock().unwrap();
        if state.memory.is_some() {
            return Ok(());
        }
        if state.owner.is_none() {
            state.owner = Some(memory.retain(crate::memory::arc::<Mutex<State>>())?);
        }
        let mut next = state.head;
        while let Some(mut chunk) = next {
            // SAFETY: links and owners are exclusively accessed under this lock.
            let chunk = unsafe { chunk.as_mut() };
            if chunk.owner.is_none() {
                chunk.owner = Some(memory.retain(chunk.layout.size() as u64)?);
            }
            next = chunk.previous;
        }
        state.memory = Some(memory.clone());
        Ok(())
    }
}
// SAFETY: the mutex serializes allocation. Every returned range is aligned,
// disjoint and remains live until the last Region clone drops. Clone preserves
// allocation identity. Deallocation is deferred; collection destructors still
// drop every element. Default grow/shrink copies to a new disjoint range.
unsafe impl Allocator for Region {
    fn allocate(&self, layout: Layout) -> Result<NonNull<[u8]>, AllocError> {
        if layout.size() == 0 {
            return Ok(NonNull::slice_from_raw_parts(
                NonNull::without_provenance(std::num::NonZeroUsize::new(layout.align()).unwrap()),
                0,
            ));
        }
        let mut state = self.0.lock().unwrap();
        if let Some(mut head) = state.head {
            // SAFETY: the mutex owns the chunk cursor and link lifetime.
            let chunk = unsafe { head.as_mut() };
            if chunk.layout.align() >= layout.align() {
                let start = chunk
                    .used
                    .checked_add(layout.align() - 1)
                    .ok_or(AllocError)?
                    & !(layout.align() - 1);
                let end = start.checked_add(layout.size()).ok_or(AllocError)?;
                if end <= chunk.layout.size() {
                    chunk.used = end;
                    // SAFETY: this aligned, unused range is inside the chunk.
                    let ptr = unsafe { head.cast::<u8>().add(start) };
                    return Ok(NonNull::slice_from_raw_parts(ptr, layout.size()));
                }
            }
        }
        let (minimum, offset) = Layout::new::<Chunk>()
            .extend(layout)
            .map_err(|_| AllocError)?;
        let chunk_layout = Layout::from_size_align(minimum.size().max(16 * 1024), minimum.align())
            .map_err(|_| AllocError)?;
        let owner = match &state.memory {
            Some(memory) => match memory.retain(chunk_layout.size() as u64) {
                Ok(owner) => Some(owner),
                Err(refused) => {
                    state.refused = Some(refused);
                    return Err(AllocError);
                }
            },
            None => None,
        };
        let block = Global.allocate(chunk_layout)?;
        let head = block.cast::<Chunk>();
        // SAFETY: Global returned enough correctly aligned storage. The header
        // and returned range are disjoint, and both stay owned by the region.
        unsafe {
            head.as_ptr().write(Chunk {
                previous: state.head,
                layout: chunk_layout,
                used: offset + layout.size(),
                owner,
            });
        }
        state.head = Some(head);
        // SAFETY: offset comes from Layout::extend and fits chunk_layout.
        Ok(NonNull::slice_from_raw_parts(
            unsafe { head.cast::<u8>().add(offset) },
            layout.size(),
        ))
    }
    unsafe fn deallocate(&self, _ptr: NonNull<u8>, _layout: Layout) {}
}

/// Immutable lookup rows stored in allocation-backed validation chunks.
pub struct ValidationRows<K, V>(Vector<(K, V)>);
impl<K: Ord, V> ValidationRows<K, V> {
    pub fn new(rows: impl IntoIterator<Item = (K, V)>) -> Result<Self, ViewRefusal> {
        let storage = Region::default();
        let mut rows = storage.collect(rows)?;
        rows.sort_unstable_by(|a, b| a.0.cmp(&b.0));
        Ok(Self(rows))
    }
    pub fn get(&self, key: &K) -> Option<&V> {
        self.0
            .binary_search_by(|row| row.0.cmp(key))
            .ok()
            .map(|i| &self.0[i].1)
    }
    pub fn contains_key(&self, key: &K) -> bool {
        self.get(key).is_some()
    }
    pub fn values(&self) -> impl Iterator<Item = &V> {
        self.0.iter().map(|(_, v)| v)
    }
    pub fn iter(&self) -> impl Iterator<Item = (&K, &V)> {
        self.0.iter().map(|(k, v)| (k, v))
    }
    pub fn retain(&self, memory: &Arc<dyn ValidationMemory>) -> Result<(), ViewRefusal> {
        self.0.allocator().retain(memory)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    struct Allowance {
        live: Arc<AtomicU64>,
        limit: u64,
    }
    struct Debit {
        live: Arc<AtomicU64>,
        bytes: u64,
    }
    impl Drop for Debit {
        fn drop(&mut self) {
            self.live.fetch_sub(self.bytes, Ordering::SeqCst);
        }
    }
    impl ValidationMemory for Allowance {
        fn retain(&self, bytes: u64) -> Result<Arc<dyn Send + Sync>, ViewRefusal> {
            self.live
                .try_update(Ordering::SeqCst, Ordering::SeqCst, |live| {
                    live.checked_add(bytes).filter(|sum| *sum <= self.limit)
                })
                .map_err(|_| crate::memory::overflow())?;
            Ok(Arc::new(Debit {
                live: self.live.clone(),
                bytes,
            }))
        }
        fn retain_signature(
            &self,
            _: &hirsel_types::SignatureRef,
        ) -> Result<Arc<dyn Send + Sync>, ViewRefusal> {
            unreachable!()
        }
    }
    #[repr(align(4096))]
    struct Aligned(u64);
    #[test]
    fn chunks_refuse_growth_preserve_alignment_and_refund_after_the_last_collection() {
        let live = Arc::new(AtomicU64::new(0));
        let memory: Arc<dyn ValidationMemory> = Arc::new(Allowance {
            live: live.clone(),
            limit: 64 * 1024,
        });
        let region = Region::default();
        let mut values = region.collect(0u64..1024).unwrap();
        region.retain(&memory).unwrap();
        let aligned = region.collect([Aligned(23), Aligned(41)]).unwrap();
        assert_eq!(aligned.as_ptr().addr() % 4096, 0);
        assert_eq!([aligned[0].0, aligned[1].0], [23, 41]);
        let charged = live.load(Ordering::SeqCst);
        assert!(charged > 0 && charged <= 64 * 1024);
        assert!(values.try_reserve(1 << 20).is_err());
        assert_eq!(live.load(Ordering::SeqCst), charged);
        assert!(values.iter().copied().eq(0..1024));
        drop(region);
        drop(aligned);
        assert_eq!(live.load(Ordering::SeqCst), charged);
        std::thread::spawn(move || drop(values)).join().unwrap();
        assert_eq!(live.load(Ordering::SeqCst), 0);
    }
}
