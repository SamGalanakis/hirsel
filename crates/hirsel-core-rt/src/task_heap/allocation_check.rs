use super::NonNull;
use crate::abi_layout::own_prefix;
use hirsel_stack::valgrind_request;

fn start(pointer: NonNull<u8>, kind: u8) -> usize {
    pointer.addr().get() - own_prefix(kind)
}

pub(super) fn allocated(pointer: NonNull<u8>, bytes: usize, kind: u8) {
    const MALLOCLIKE_BLOCK: u64 = 0x1301;
    valgrind_request(
        0,
        MALLOCLIKE_BLOCK,
        [start(pointer, kind), bytes.max(1) + own_prefix(kind), 0, 0],
    );
}

pub(super) fn resized(pointer: NonNull<u8>, old: usize, new: usize, kind: u8) {
    const RESIZEINPLACE_BLOCK: u64 = 0x130b;
    let prefix = own_prefix(kind);
    valgrind_request(
        0,
        RESIZEINPLACE_BLOCK,
        [
            start(pointer, kind),
            old.max(1) + prefix,
            new.max(1) + prefix,
            0,
        ],
    );
}

pub(super) fn freed(pointer: NonNull<u8>, kind: u8) {
    const FREELIKE_BLOCK: u64 = 0x1302;
    valgrind_request(0, FREELIKE_BLOCK, [start(pointer, kind), 0, 0, 0]);
}

pub(super) fn link_access(pointer: NonNull<u8>, readable: bool) {
    storage_access(pointer, size_of::<usize>(), readable);
}

pub(super) fn storage_access(pointer: NonNull<u8>, bytes: usize, readable: bool) {
    const MAKE_MEM_NOACCESS: u64 = 0x4d43_0000;
    const MAKE_MEM_DEFINED: u64 = 0x4d43_0002;
    let request = if readable {
        MAKE_MEM_DEFINED
    } else {
        MAKE_MEM_NOACCESS
    };
    valgrind_request(0, request, [pointer.addr().get(), bytes, 0, 0]);
}
