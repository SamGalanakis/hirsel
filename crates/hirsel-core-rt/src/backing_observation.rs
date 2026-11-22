// Expand at the transition so replay stack capture keeps the caller's frame.
// Each consumer evaluates only the operands it needs in that feature build.
macro_rules! observe {
    (allocate, $pointer:expr, $bytes:expr, $backing:ident $(,)?) => {{
        #[cfg(any(test, feature = "charge-observation"))]
        $crate::peak::allocated($pointer, $bytes);
        #[cfg(feature = "prof")]
        $crate::mem_prof::alloc($pointer, $bytes, $crate::mem_prof::Backing::$backing);
    }};
    (resize, $pointer:expr, $old_bytes:expr, $bytes:expr $(,)?) => {{
        #[cfg(any(test, feature = "charge-observation"))]
        $crate::peak::resized($pointer, $bytes);
        #[cfg(feature = "prof")]
        $crate::mem_prof::resize($pointer, $old_bytes, $bytes);
    }};
    (free, $pointer:expr, $bytes:expr $(,)?) => {{
        #[cfg(any(test, feature = "charge-observation"))]
        $crate::peak::deallocated($pointer);
        #[cfg(feature = "prof")]
        $crate::mem_prof::free($pointer, $bytes);
    }};
}

pub(crate) use observe;
