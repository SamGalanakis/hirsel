use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};

pub(super) struct OpenFile {
    pub(super) file: Arc<Mutex<ModelFile>>,
    pub(super) at: u64,
    pub(super) lock: Option<FileLock>,
    pub(super) mode: u8,
    pub(super) read_only_dir: bool,
}
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum FileLock {
    Shared,
    Exclusive,
}
const FILE_PAGE: u64 = 4096;

#[derive(Default)]
pub(super) struct ModelFile {
    pub(super) len: u64,
    pub(super) modified_ns: u64,
    pub(super) pages: BTreeMap<u64, Box<[u8; FILE_PAGE as usize]>>,
}

impl ModelFile {
    pub(super) fn read(&self, at: u64, count: usize) -> Vec<u8> {
        let mut bytes = vec![0; count];
        Self::visit(at, count, |page, from, to, pos| {
            if let Some(page) = self.pages.get(&page) {
                bytes[pos..pos + to - from].copy_from_slice(&page[from..to]);
            }
        });
        bytes
    }

    pub(super) fn zero_at(&mut self, at: u64, len: u64) {
        if len == 0 {
            return;
        }
        let end = at + len;
        for (&page, bytes) in self.pages.range_mut(at / FILE_PAGE..=(end - 1) / FILE_PAGE) {
            let start = page * FILE_PAGE;
            let low = at.saturating_sub(start) as usize;
            let high = (end - start).min(FILE_PAGE) as usize;
            bytes[low..high].fill(0);
        }
        self.len = self.len.max(end);
    }

    pub(super) fn write(&mut self, at: u64, bytes: &[u8]) {
        Self::visit(at, bytes.len(), |page, from, to, pos| {
            let page = self
                .pages
                .entry(page)
                .or_insert_with(|| Box::new([0; FILE_PAGE as usize]));
            page[from..to].copy_from_slice(&bytes[pos..pos + to - from]);
        });
        self.len = self.len.max(at + bytes.len() as u64);
    }

    fn visit(at: u64, count: usize, mut f: impl FnMut(u64, usize, usize, usize)) {
        let mut pos = 0;
        while pos < count {
            let offset = at + pos as u64;
            let from = (offset % FILE_PAGE) as usize;
            let n = (count - pos).min(FILE_PAGE as usize - from);
            f(offset / FILE_PAGE, from, from + n, pos);
            pos += n;
        }
    }
}
