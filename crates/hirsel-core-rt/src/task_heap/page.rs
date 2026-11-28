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
    pub(super) fn slots(start: NonNull<u8>, class: Class) -> Slots {
        let (first, limit) = if class.fits_slab() {
            let offset = (size_of::<Self>() + class.offset()).next_multiple_of(class.align);
            // SAFETY: size class guarantees at least one slot in the mapping.
            (
                unsafe { start.add(offset) },
                (SLAB - offset + class.offset()) / class.stride,
            )
        } else {
            (start, 1)
        };
        Slots {
            first,
            limit,
            bump: 0,
            live: 0,
            free: ptr::null_mut(),
            discarded: 0,
            watch: 1,
            #[cfg(feature = "memcheck")]
            checked_bytes: 0,
            head: false,
            previous: None,
            next: None,
        }
    }

    #[inline(never)]
    pub(super) unsafe fn reuse(page: PagePointer, class: Class, dedicated_payer: Option<Payer>) {
        // SAFETY: the caller owns the empty page and verified it fits.
        unsafe {
            let header = page.as_ref();
            if let Some(payer) = dedicated_payer {
                header.dedicated_payer.get().write(MaybeUninit::new(payer));
            } else {
                header.payer_delta.get().write(class.payer_offset as usize);
            }
            header.class.get().write(class);
            let slots = page.as_ref().slots.get();
            let start = if class.slab {
                page.cast()
            } else {
                (*slots).first
            };
            slots.write(Self::slots(start, class));
        }
    }
    /// # Safety
    /// The custodian owns the page's list state.
    pub(super) unsafe fn set_head(page: PagePointer, head: bool) {
        // SAFETY: only the custodian accesses these fields while open.
        unsafe {
            let slots = &mut *page.as_ref().slots.get();
            slots.head = head;
            if !(*page.as_ref().spread.get()).spread {
                // An unspread head can become its own empty spare.
                slots.watch = usize::from(!head);
            }
        }
    }
}
