use crate::causes;

#[test]
fn small_release_owners_share_read_only_pages_until_last_release() {
    const CHILD: &str = "HIRSEL_TEST_LITERAL_ARENA";
    if std::env::var_os(CHILD).is_none() {
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "host::linux::literal_tests::small_release_owners_share_read_only_pages_until_last_release",
            ])
            .env(CHILD, "1")
            .status()
            .unwrap();
        assert!(status.success());
        return;
    }
    let owners: Vec<_> = (0..128u8)
        .map(|i| causes::CauseData::new(&[i; 16], &[]).unwrap())
        .collect();
    let maps = std::fs::read_to_string("/proc/self/maps").unwrap();
    let mut mappings = std::collections::BTreeSet::new();
    let mut pages = std::collections::BTreeSet::new();
    let page_size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) } as usize;
    for (i, owner) in owners.iter().enumerate() {
        let (data, len) = owner.literals();
        // SAFETY: the owner pins these bytes for the comparison.
        assert_eq!(
            unsafe { std::slice::from_raw_parts(data, len) },
            &[i as u8; 16]
        );
        let entry = maps
            .lines()
            .find(|line| {
                let (lo, hi) = line
                    .split_whitespace()
                    .next()
                    .unwrap()
                    .split_once('-')
                    .unwrap();
                (usize::from_str_radix(lo, 16).unwrap()..usize::from_str_radix(hi, 16).unwrap())
                    .contains(&data.addr())
            })
            .unwrap();
        assert!(entry.split_whitespace().nth(1).unwrap().starts_with("r--"));
        mappings.insert(entry);
        pages.insert(data.addr() / page_size);
    }
    assert_eq!(mappings.len(), 1, "128 small releases fit in one chunk");
    assert_eq!(
        pages.len(),
        1,
        "128 aligned 24-byte records fit in one page"
    );
    let last = owners[51].clone();
    let address = last.literals().0.addr();
    drop(owners);
    assert!(crate::host::mapping_perms(address).is_some());
    drop(last);
    assert_eq!(crate::host::mapping_perms(address), None);
}
