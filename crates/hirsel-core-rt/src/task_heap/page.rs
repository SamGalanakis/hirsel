use super::*;

impl Page {
    pub(super) fn with_backing(
        heap: &Arc<Heap>,
        class: Class,
        dedicated_payer: Option<Payer>,
        backing: Backing,
        start: NonNull<u8>,
    ) -> PagePointer {
        let slab = class.fits_slab();
        let data = Self {
            heap: Arc::clone(heap),
            returned: returns::Inbox::new(),
            backing,
            dedicated_payer: dedicated_payer
                .map_or_else(MaybeUninit::uninit, MaybeUninit::new)
                .into(),
            payer_delta: (class.payer_offset as usize).into(),
            class: class.into(),
            slots: Self::slots(start, class).into(),
            spread: Spread::default().into(),
        };
        if slab {
            let page: PagePointer = start.cast();
            // SAFETY: the start of a new slab has room for its aligned descriptor.
            unsafe { page.as_ptr().write(data) };
            page
        } else {
            let page = NonNull::from(Box::leak(Box::new(data)));
            // SAFETY: the boxed descriptor has a stable address and has
            // yet to publish the mapping's sole payload.
            unsafe {
                let header = page.as_ref();
                let payer_address = header.dedicated_payer.get().addr();
                header
                    .payer_delta
                    .get()
                    .write(payer_address.wrapping_sub(start.addr().get()));
            }
            dedicated_index(start)
                .lock()
                .unwrap()
                .get_or_insert_with(|| {
                    HashMap::with_hasher(foldhash::fast::FixedState::with_seed(0))
                })
                .insert(start.addr().get(), IndexedPage(page));
            page
        }
    }
}
