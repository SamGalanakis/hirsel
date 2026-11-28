const PARTS: usize = 4;

fn run_part(part: usize) {
    let seqs = sequences();
    let names: BTreeSet<&str> = seqs.iter().map(|s| s.name).collect();
    assert_eq!(names.len(), seqs.len(), "sequence names are distinct");
    let stale: Vec<&str> = GATES
        .iter()
        .map(|g| g.0)
        .filter(|n| !names.contains(n))
        .collect();
    assert!(stale.is_empty(), "gate classes of no sequence: {stale:?}");
    let selected = seqs.into_iter().skip(part).step_by(PARTS).collect();
    run_sequences(selected, None, UNEQUAL_ROOTS);
}

#[test]
fn request_sequences_answer_as_an_empty_store_0() {
    run_part(0);
}

#[test]
fn request_sequences_answer_as_an_empty_store_1() {
    run_part(1);
}

#[test]
fn request_sequences_answer_as_an_empty_store_2() {
    run_part(2);
}

#[test]
fn request_sequences_answer_as_an_empty_store_3() {
    run_part(3);
}
