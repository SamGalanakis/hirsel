//! One dedicated page per independently charged file buffer. The issuer
//! retains the underlying mappings until all SQEs naming its slots complete.
use super::*;
use hirsel_core_net::file_ring::{FileBuffer, FileBuffers};

pub(super) struct Backing {
    payload: FileBuffer,
    spare: Mutex<Option<FileBuffer>>,
}

fn lookup<T>(addr: usize, visit: impl FnOnce(&Backing) -> T) -> Option<T> {
    let pointer = NonNull::new(ptr::without_provenance_mut(addr))?;
    if !addr.is_multiple_of(SLAB) {
        return None;
    }
    let index = dedicated_index(pointer).lock().unwrap();
    let page = index.as_ref()?.get(&addr)?.0;
    // SAFETY: destruction must remove this entry under the same lock.
    // A live page's backing cannot move, including during cross-heap returns.
    match &unsafe { page.as_ref() }.backing {
        super::Backing::File(buffer) => Some(visit(buffer)),
        _ => None,
    }
}

pub(crate) fn registration(data: &[u8]) -> Option<Arc<FileBuffers>> {
    lookup(data.as_ptr().addr(), |backing| {
        backing.payload.pair().clone()
    })
}

pub(crate) fn allocate(
    context: &crate::Context,
    head: Option<usize>,
    size: usize,
) -> Option<NonNull<u8>> {
    // Checked runs use their normal aligned allocations and quarantine.
    #[cfg(any(test, feature = "memcheck"))]
    if crate::allocator::checked::enabled() || cfg!(feature = "memcheck") {
        return None;
    }
    let (mut payload, spare) = if let Some(addr) = head {
        let mut partner = lookup(addr, |buffer| buffer.spare.lock().unwrap().take())??;
        if partner.bytes().len() != size.max(size_of::<usize>()) {
            return None;
        }
        (partner, None)
    } else {
        // CPU alignment is for page lookup. The Direct write's alignment is
        // smaller. Neither changes the logical capacity or charged bytes.
        let [first, second] = FileBuffers::new_aligned(size.max(size_of::<usize>()), SLAB)
            .ok()?
            .split();
        (first, Some(second))
    };
    let start = NonNull::new(payload.bytes().as_mut_ptr())?;
    let heap = context.heap.get_or_init(Arc::default);
    let class = Class::dedicated(size, 4096, crate::block::PLAIN, false);
    let backing = Backing {
        payload,
        spare: Mutex::new(spare),
    };
    let page = Page::with_backing(
        heap,
        class,
        Some(context.payer()),
        super::Backing::File(backing),
        start,
    );
    // SAFETY: the single slot is newly allocated and unpublished. Its payer
    // is installed before the carrier can reach application code.
    unsafe {
        let slots = &mut *page.as_ref().slots.get();
        slots.live = 1;
        slots.bump = 1;
    }
    #[cfg(any(test, feature = "memcheck"))]
    crate::allocator::checked::allocated(start, size);
    Some(finish_allocation(start, class))
}
