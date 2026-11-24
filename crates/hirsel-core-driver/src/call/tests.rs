use super::*;

#[test]
fn call_wrong_typed_flags_refuse_before_any_connection() {
    // Bug: encoding a flag without checking it against the Describe schema.
    // Oracle: docs/spec/sdk.md, "Calling an entry from the terminal": a flag
    // that does not fit its type is refused before anything is sent.
    // Args = record R { n: u64, label: String }; the server is a stub that
    // only answers Describe, and --url names a port nothing listens on, so
    // an unchecked flag would fail later with a probe or connect error.
    let dir = tempfile::tempdir().unwrap();
    let mut args = vec![3u8, 1, 0, 0, 0, 1, 0, 0, 0, b'R', 0, 0, 0, 0, 0, 2, 0, 0, 0];
    args.extend([0, 1, 0, 0, 0, b'n', 6]);
    args.extend([0, 5, 0, 0, 0]);
    args.extend(b"label");
    args.extend([13, 19, 0, 0, 0, 0]);
    let identity = b"\x01app@1/m/record(take app@1/m/R)->u64";
    let mut describe = Vec::new();
    let blob = |out: &mut Vec<u8>, b: &[u8]| {
        out.extend((b.len() as u32).to_le_bytes());
        out.extend(b);
    };
    blob(&mut describe, &[]);
    blob(&mut describe, b"t");
    describe.extend(17u64.to_le_bytes());
    describe.extend(1u32.to_le_bytes());
    describe.extend(0u32.to_le_bytes());
    blob(&mut describe, identity);
    blob(&mut describe, &args);
    blob(&mut describe, &[3, 0, 0, 0, 0, 6]);
    describe.extend(1u64.to_le_bytes());
    describe.extend(2u64.to_le_bytes());
    blob(&mut describe, &[]);
    let server = dir.path().join("server");
    std::fs::write(&server, []).unwrap();
    let attempt = |flags: &[&str]| {
        let mut call = vec!["m/record".to_owned()];
        call.extend(flags.iter().map(|f| (*f).to_owned()));
        let options = Options {
            server: server.clone(),
            url: Some("ws://127.0.0.1:1/".into()),
            n_place: 17,
            place_timeout: Duration::from_secs(1),
            timeout: Duration::from_secs(1),
            stream: None,
            call,
        };
        run_with(options, |_, _, _| Ok(describe.clone()))
            .unwrap_err()
            .to_string()
    };
    assert_eq!(
        attempt(&["--n", "seven", "--label", "x"]),
        "--n expects u64: `seven` is not a decimal integer"
    );
    assert_eq!(
        attempt(&["--n", "18446744073709551616", "--label", "x"]),
        "--n expects u64: 18446744073709551616 is outside 0..=18446744073709551615"
    );
    assert_eq!(attempt(&["--label", "x"]), "missing --n u64");
}
