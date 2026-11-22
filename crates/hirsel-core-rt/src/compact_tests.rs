//! ABI fixtures for FORM v1b's ordinary producer code. Writers and compact
//! checks are emitted code; this runtime owns identity, storage and refunds.

use crate::{Capacity, Context, abi::*};
use hirsel_core_layout::charge;
use std::ptr::NonNull;

struct OpenRegion(*mut crate::region::Region);
impl OpenRegion {
    fn new(context: &Context) -> Self {
        // SAFETY: the current context is live through open.
        Self(unsafe { hirsel_core_region_open(context) })
    }
    fn allocate(
        &mut self,
        ctx: &Context,
        bytes: usize,
        charge: Option<u64>,
    ) -> Result<NonNull<u8>, Capacity> {
        // SAFETY: the fixture owns its region and borrows a live context; sizes
        // describe raw region storage, including any uncharged engine header.
        let result = unsafe {
            hirsel_core_region_alloc(
                ctx,
                self.0,
                bytes,
                8,
                charge.unwrap_or(u64::MAX),
                charge.is_none(),
            )
        };
        NonNull::new(result.pointer).ok_or(result.capacity)
    }
}
impl Drop for OpenRegion {
    fn drop(&mut self) {
        // SAFETY: the fixture ends every view before closing its unique region.
        unsafe {
            hirsel_core_region_close(self.0);
        }
    }
}

#[test]
fn destination_identity_needs_no_context_status_or_admission() {
    let operation: extern "C" fn() -> u64 = hirsel_core_dest_id;
    let ctx = Context::new(0);
    ctx.current.inject(1, true);
    let mut previous = operation();
    for _ in 0..1000 {
        let current = operation();
        assert_ne!(current, previous);
        previous = current;
    }
    assert_eq!(ctx.available(), 0);
    assert_eq!(ctx.current.attempts(), 0);
}

#[test]
fn destination_identity_is_unique_across_workers_and_released_storage() {
    let workers: Vec<_> = (0..4)
        .map(|_| {
            std::thread::spawn(|| (0..1000).map(|_| hirsel_core_dest_id()).collect::<Vec<_>>())
        })
        .collect();
    let mut ids: std::collections::BTreeSet<_> = workers
        .into_iter()
        .flat_map(|w| w.join().unwrap())
        .collect();
    assert_eq!(ids.len(), 4000);
    for _ in 0..16 {
        let ctx = Context::new(8);
        let mut region = OpenRegion::new(&ctx);
        let _allocation = region.allocate(&ctx, 24, Some(8)).unwrap();
        assert!(ids.insert(hirsel_core_dest_id()));
        drop(region);
        assert_eq!(ctx.available(), 8);
    }
}

#[test]
fn blob_header_is_uncharged_trim_keeps_payer_and_closed_copy_keeps_actual_width() {
    for warm in [false, true] {
        let mut ctx = Context::new(128);
        if warm {
            let mut previous = OpenRegion::new(&ctx);
            previous.allocate(&ctx, 80, Some(0)).unwrap();
        }
        let mut source = OpenRegion::new(&ctx);
        let mut target = OpenRegion::new(&ctx);
        ctx.limit_enter(64).unwrap();
        let blob = source.allocate(&ctx, 16 + 64, charge::region(64)).unwrap();
        assert_eq!(
            ctx.available(),
            0,
            "the 16-byte engine header is not charged"
        );
        // FORM sequences.core f0(3,64): Arr header 9 + three Null tags = 12.
        // SAFETY: the producer writes exactly the initialized blob header and
        // its completed 12-byte packed prefix, within the reserved 80 native bytes.
        unsafe {
            blob.as_ptr().cast::<u64>().write(12);
            blob.as_ptr().add(8).cast::<u64>().write(4);
            blob.as_ptr().add(16).write(1);
            blob.as_ptr()
                .add(17)
                .cast::<u32>()
                .write_unaligned(3u32.to_le());
            blob.as_ptr()
                .add(21)
                .cast::<u32>()
                .write_unaligned(3u32.to_le());
            blob.as_ptr().add(25).write_bytes(0, 3);
        }
        ctx.limit_exit();
        ctx.limit_enter(16).unwrap();
        // SAFETY: finish_trim retains round8(12), preserving every completed byte.
        unsafe {
            hirsel_core_region_lower_charge(
                source.0,
                (*source.0).entry,
                64,
                charge::region(12).unwrap(),
            );
        }
        assert_eq!(
            ctx.available(),
            16,
            "trim refunds the blob's original payer"
        );
        let closed = target.allocate(&ctx, 16 + 1, charge::region(1)).unwrap();
        // SAFETY: Null is a closed link-free one-byte subtree; copy exactly that
        // extent and retain its actual W=4, regardless of the child declaration.
        unsafe {
            closed.as_ptr().cast::<u64>().write(1);
            closed
                .as_ptr()
                .add(8)
                .cast::<u64>()
                .write(blob.as_ptr().add(8).cast::<u64>().read());
            std::ptr::copy_nonoverlapping(blob.as_ptr().add(25), closed.as_ptr().add(16), 1);
        }
        assert_eq!(ctx.available(), 8);
        ctx.limit_exit();
        assert_eq!(ctx.available(), 104); // retained source 16 and closed copy 8.
        drop(source);
        assert_eq!(ctx.available(), 120);
        // SAFETY: the target owns the copied bytes after the source region ends.
        unsafe {
            assert_eq!(closed.as_ptr().add(8).cast::<u64>().read(), 4);
            assert_eq!(closed.as_ptr().add(16).read(), 0);
        }
        drop(target);
        assert_eq!(ctx.available(), 128);
    }
}

#[test]
fn failed_stream_keeps_its_full_reservation_and_completed_bytes_do_not_move() {
    for warm in [false, true] {
        let ctx = Context::new(128);
        if warm {
            let mut previous = OpenRegion::new(&ctx);
            previous.allocate(&ctx, 26, Some(0)).unwrap();
        }
        let mut region = OpenRegion::new(&ctx);
        // FORM f0(3,10): tag, reserved sequence header, one complete Null;
        // the next Null cannot fit. No finish or trim occurs on this path.
        let blob = region.allocate(&ctx, 16 + 10, charge::region(10)).unwrap();
        // SAFETY: initialize the completed child and metadata before exposing its view.
        unsafe {
            blob.as_ptr().add(8).cast::<u64>().write(4);
            blob.as_ptr().add(16).write(1);
            blob.as_ptr().add(25).write(0);
        }
        let child = blob.as_ptr().wrapping_add(25);
        let _later = region.allocate(&ctx, 16 + 64, charge::region(64)).unwrap();
        assert_eq!(
            ctx.available(),
            48,
            "failed build keeps round8(10), plus later 64"
        );
        assert_eq!(
            region.allocate(&ctx, 16 + 64, charge::region(64)).err(),
            Some(Capacity {
                requested: 64,
                available: 48
            })
        );
        // SAFETY: completed-child views stay live until region close, including
        // after a failed parent and after later chunks are allocated.
        unsafe {
            assert_eq!(child.read(), 0);
        }
        drop(region);
        assert_eq!(ctx.available(), 128);
    }
}

#[test]
fn compact_reservations_closed_copies_and_indexes_fail_at_each_and_all_later_admissions() {
    let mut runs = 0;
    for warm in [false, true] {
        for later in [false, true] {
            for k in 1..=4 {
                let ctx = Context::new(128);
                if warm {
                    let mut previous = OpenRegion::new(&ctx);
                    previous.allocate(&ctx, 80, Some(0)).unwrap();
                }
                ctx.current.inject(k, later);
                let mut region = OpenRegion::new(&ctx);
                let mut retained = 0;
                if let Ok(_blob) = region.allocate(&ctx, 16 + 64, charge::region(64)) {
                    // SAFETY: this fixture's completed source uses twelve bytes.
                    unsafe {
                        hirsel_core_region_lower_charge(
                            region.0,
                            (*region.0).entry,
                            64,
                            charge::region(12).unwrap(),
                        );
                    }
                    retained += 16;
                    if region.allocate(&ctx, 16 + 1, charge::region(1)).is_ok() {
                        retained += 8;
                    }
                    // An index of the source is independent of whether copying
                    // its subtree succeeded: Nth-only may resume, all-later may not.
                    if region.allocate(&ctx, 48, charge::seq_index(3)).is_ok() {
                        retained += 48;
                    }
                }
                assert_eq!(ctx.available(), 128 - retained);
                drop(region);
                assert_eq!(ctx.available(), 128);
                runs += 1;
            }
        }
    }
    assert_eq!(runs, 16);
}

#[test]
fn index_charge_overflow_is_a_capacity_value_before_any_storage_is_written() {
    let ctx = Context::new(128);
    let mut region = OpenRegion::new(&ctx);
    assert_eq!(charge::seq_index(3), Some(48));
    assert_eq!(
        region.allocate(&ctx, 48, charge::seq_index(u64::MAX)).err(),
        Some(Capacity {
            requested: u64::MAX,
            available: 128
        })
    );
    assert_eq!(ctx.available(), 128);
    drop(region);
    assert_eq!(ctx.available(), 128);
}

/// SEM-7: text orders by unsigned bytes, a proper prefix first. The bugs: a
/// memcmp over the shorter length that stops there (prefix compares equal), and a
/// signed-byte compare (0x80 before 0x7f). Empty ranges are never read.
#[test]
fn byte_order_is_unsigned_with_a_proper_prefix_first() {
    let cmp = |a: &[u8], b: &[u8]| {
        // SAFETY: both ranges are live slices.
        unsafe { hirsel_core_compare_bytes(a.as_ptr(), a.len() as u64, b.as_ptr(), b.len() as u64) }
    };
    assert_eq!(cmp(b"ab", b"abc"), -1);
    assert_eq!(cmp(b"abd", b"abc"), 1);
    assert_eq!(cmp(b"abc", b"abc"), 0);
    assert_eq!(cmp(&[0x80], &[0x7f]), 1);
    // SAFETY: length 0 is never read.
    assert_eq!(
        unsafe { hirsel_core_compare_bytes(std::ptr::null(), 0, std::ptr::null(), 0) },
        0
    );
}

/// Fixed-key words from siphasher 1.0.4 over the structural byte stream.
/// Offsets exercise unaligned words and the full tail range without an overread.
#[test]
fn map_hash_uses_the_siphash_13_vectors() {
    let mut ctx = Context::new(0);
    let bytes = |ctx: &Context, b: &[u8]| {
        // SAFETY: the context and byte range are live.
        unsafe { hirsel_core_map_hash_bytes(ctx, b.as_ptr(), b.len() as u64) }
    };
    // SAFETY: a zero length permits a null byte range.
    assert_eq!(
        unsafe { hirsel_core_map_hash_bytes(&ctx, std::ptr::null(), 0) },
        0xbd60acb658c79e45
    );
    for (n, want) in [
        (0, 0xbd60acb658c79e45),
        (1, 0x8934f5f0a51fbffa),
        (2, 0x02448a6b1c466c6f),
        (3, 0x4577ab078b9dbddc),
        (4, 0x59ebb5f15eeeb94a),
        (5, 0xa314a10555783c3c),
        (6, 0xa270c64d572974bf),
        (7, 0x88d7b10d83238a46),
        (8, 0x128eb5bf6a9b4604),
        (9, 0xf5103240d47b2e9e),
        (10, 0x8906c76a3a487ce0),
        (11, 0x8cdc41e1889d00b7),
        (12, 0x2f577626683bdcd0),
        (13, 0x2d3a1aa9c3f86b1c),
        (14, 0xcf3d96a5eee2ee1b),
        (15, 0x940dedebc0559b50),
        (16, 0x5026d4b5372d6206),
        (17, 0x48b5a13e58996335),
        (18, 0xf30650b163a36d39),
        (19, 0x183ff6b555ad300c),
        (20, 0x0f52c59cc0dc95d9),
        (21, 0x2604d0d2cb21c46c),
        (22, 0x35b4eba9b0ca0e76),
        (23, 0xff8a2af253c0d053),
        (24, 0x750feda696e350b3),
        (25, 0x1572102aa3513ef5),
        (26, 0x75953c1eb8043ead),
        (27, 0x5f712c5602f5d393),
        (28, 0x177af755cdbdf9fa),
        (29, 0x76e82dedc44d97f8),
        (30, 0x869b134d4fbd584e),
        (31, 0xf5daff99b18b0ea3),
        (32, 0x6a9d14ef1a0f4b98),
        (33, 0xfc090f8af269999a),
        (63, 0x2c3c5d16bd12a718),
        (64, 0x47ee413eb5b62e62),
        (65, 0x32c0c1aabb2e5dfd),
        (257, 0x7f744cd6ade23b6e),
    ] {
        for prefix in 0..8 {
            let mut data = vec![0xe7; prefix];
            data.extend((0..n).map(|i| i as u8));
            assert_eq!(
                bytes(&ctx, &data[prefix..]),
                want,
                "length {n}, prefix {prefix}"
            );
        }
    }
    ctx.map_hash_key =
        hirsel_core_layout::map_hash::PreparedKey::new(&std::array::from_fn(|i| i as u8));
    let mut scratch = std::mem::MaybeUninit::uninit();
    let state = scratch.as_mut_ptr();
    // SAFETY: ctx is live and state is aligned exclusive scratch initialized
    // by start. The byte range is a disjoint two-byte literal.
    unsafe {
        let word = u64::from_le_bytes([0, 1, 2, 3, 4, 5, 6, 7]);
        assert_eq!(hirsel_core_map_hash_word(&ctx, word), 0x369095118d299a8e);
        hirsel_core_map_hash_start(&ctx, state);
        hirsel_core_map_hash_add_bytes(state, b"ab".as_ptr(), 2);
        hirsel_core_map_hash_add_word(state, 7);
        assert_eq!(hirsel_core_map_hash_finish(state), 0xdb92b956a12fc30b);
    }
}

/// PROG-11's independent vectors cover the two sides of an 8-byte tail
/// and a 16-byte chunk, the signed-word width and all four derivation labels.
#[test]
fn map_placement_matches_prog_11_vectors() {
    use hirsel_core_layout::{map_hash, map_index};

    let key = std::array::from_fn(|i| i as u8);
    let (s0, s1, strong) = map_index::derive(&key);
    assert_eq!(s0, 0xc93577b642b675d3);
    assert_eq!(s1, 0x216bdc5358d9c4d5);
    assert_eq!(
        strong,
        [
            0x1e, 0x7e, 0xe6, 0x05, 0xb3, 0x3f, 0x70, 0x78, 0xd1, 0x71, 0xd0, 0xad, 0xe9, 0x94,
            0x21, 0x0e,
        ]
    );
    for (word, fast, keyed) in [
        (0, 0x175d5dfbd09e75c7, 0xe8b70260e432308d),
        (1, 0xb08ad43c5024c9bd, 0x5cd87fd53c86d83a),
        (u64::MAX, 0xd71c61fbd21d7517, 0x78ee0e4e0cad36d2),
        (
            u64::from(-1i8 as u8),
            0x488dc5e5cae947bd,
            0xfbe4122ec2e54220,
        ),
    ] {
        assert_eq!(map_index::fast_word(word, s0), fast);
        assert_eq!(map_hash::word(&strong, word), keyed);
    }
    for (bytes, expected) in [
        (&b""[..], 0xc601c2084ab71e17),
        (&b"abcdefgh"[..], 0x3251c98ae37d4dbf),
        (&b"abcdefghi"[..], 0xdd8ba4778c150f32),
        (&b"abcdefghijklmnop"[..], 0x441cb7420c58fe33),
        (&b"abcdefghijklmnopq"[..], 0x8d14bb88cfa906fe),
    ] {
        assert_eq!(map_index::fast_bytes(bytes, s0, s1), expected);
    }
    for (bytes, expected) in [
        (&b"a"[..], 0x9b4f3fea3a32bb74),
        (&b"abcdefghi"[..], 0x687d6af59de6e421),
        (&b"abcdefghijklmnopq"[..], 0x4427af8d97cca0dd),
    ] {
        assert_eq!(map_hash::bytes(&strong, bytes), expected);
    }
}
