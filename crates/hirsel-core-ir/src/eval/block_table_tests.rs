use super::*;

fn block() -> Blk {
    Blk {
        bytes: vec![42; 8],
        align: 8,
        live: true,
        limit: None,
        charge: 0,
        lent_size: None,
        res: None,
        counted: None,
        class: None,
        fixed: false,
        marks: BTreeMap::new(),
    }
}

#[test]
fn block_storage_follows_liveness_and_never_reuses_an_identity() {
    let mut blocks = Blocks::default();
    let pinned = blocks.insert(block());
    let mut previous = pinned;
    for _ in 0..10 * BLOCK_CHUNK {
        let id = blocks.insert(block());
        assert!(id > previous);
        previous = id;
        assert_eq!(blocks[id].bytes, [42; 8]);
        blocks.remove(id);
        assert!(!blocks[id].live);
    }
    assert_eq!(blocks.entries.len(), 1);
    assert!(blocks.entries.capacity() <= 256);
    assert_eq!(blocks.chunks.iter().filter(|c| c.is_some()).count(), 2);
    assert_eq!(blocks[pinned].bytes, [42; 8]);
    blocks.remove(pinned);
    assert!(blocks.chunks[0].is_none());
    assert!(!blocks[pinned].live);
}

#[test]
fn removing_a_record_preserves_the_identity_of_the_moved_record() {
    let mut blocks = Blocks::default();
    let ids: Vec<_> = (0..2 * BLOCK_CHUNK)
        .map(|_| blocks.insert(block()))
        .collect();
    for &id in &ids[..BLOCK_CHUNK] {
        blocks.remove(id);
    }
    assert!(blocks.chunks[0].is_none());
    for &id in &ids[BLOCK_CHUNK..] {
        assert_eq!(blocks[id].bytes, [42; 8]);
    }
    for &id in &ids[BLOCK_CHUNK..] {
        blocks.remove(id);
    }
    assert!(blocks.chunks.iter().all(Option::is_none));
    assert!(blocks.entries.capacity() <= 256);
}

#[test]
fn block_identity_exhaustion_stops_instead_of_wrapping() {
    let mut blocks = Blocks {
        next: u32::MAX as usize,
        ..Blocks::default()
    };
    let fault = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| blocks.insert(block())))
        .unwrap_err();
    assert_eq!(
        fault.downcast_ref::<&str>().copied(),
        Some("evaluator block identity space exhausted (a run stop, not Capacity)")
    );
    assert_eq!(blocks.next, u32::MAX as usize);
    assert!(blocks.entries.is_empty());
    let last = ptr(u32::MAX as usize - 1, u32::MAX as u64);
    assert_eq!(split(last), (u32::MAX as usize - 1, u32::MAX as usize));
}
