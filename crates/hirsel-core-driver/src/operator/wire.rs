//! Value codec field types used by the generated operator bindings.
use super::EntryLine;
use crate::launcher::session::{Reader, Wire, Writer, get_roots, put_roots};
use std::io;

impl Wire for bool {
    fn put(&self, w: &mut Writer) {
        w.bool(*self);
    }

    fn get(r: &mut Reader<'_>) -> io::Result<Self> {
        r.bool()
    }
}

impl Wire for u64 {
    fn put(&self, w: &mut Writer) {
        w.u64(*self);
    }

    fn get(r: &mut Reader<'_>) -> io::Result<Self> {
        r.u64()
    }
}

impl Wire for String {
    fn put(&self, w: &mut Writer) {
        w.bytes(self.as_bytes());
    }

    fn get(r: &mut Reader<'_>) -> io::Result<Self> {
        r.string()
    }
}

impl Wire for Vec<u8> {
    fn put(&self, w: &mut Writer) {
        w.bytes(self);
    }

    fn get(r: &mut Reader<'_>) -> io::Result<Self> {
        r.bytes()
    }
}

impl Wire for Vec<String> {
    fn put(&self, w: &mut Writer) {
        w.strings(self);
    }

    fn get(r: &mut Reader<'_>) -> io::Result<Self> {
        r.strings()
    }
}

impl Wire for Vec<EntryLine> {
    fn put(&self, w: &mut Writer) {
        w.len(self.len());
        for entry in self {
            entry.put(w);
        }
    }

    fn get(r: &mut Reader<'_>) -> io::Result<Self> {
        (0..r.u32()?).map(|_| EntryLine::get(r)).collect()
    }
}

impl Wire for Vec<[u8; 32]> {
    fn put(&self, w: &mut Writer) {
        put_roots(w, self);
    }

    fn get(r: &mut Reader<'_>) -> io::Result<Self> {
        get_roots(r)
    }
}

impl<T: Wire> Wire for Option<T> {
    fn put(&self, w: &mut Writer) {
        w.option(self, |w, value| value.put(w));
    }

    fn get(r: &mut Reader<'_>) -> io::Result<Self> {
        r.option(T::get)
    }
}

impl<T: Wire> Wire for Box<T> {
    fn put(&self, w: &mut Writer) {
        self.as_ref().put(w);
    }

    fn get(r: &mut Reader<'_>) -> io::Result<Self> {
        T::get(r).map(Box::new)
    }
}
