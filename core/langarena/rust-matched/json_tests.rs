// Standalone laws: rustc --test json_tests.rs. Not part of the measured twin root.
include!("json.rs");

#[test]
fn exact_blob_and_offset_trailers() {
    let input = "[12,\"ab\",[true,30],[]]";
    let doc = parse(input);
    assert_eq!(doc.data.len(), 73);
    assert_eq!(doc.data.capacity(), 73);
    assert_eq!(doc.count(0), 4);
    assert_eq!(doc.word(5), 48);
    assert_eq!(
        (0..4).map(|i| doc.child(0, i)).collect::<Vec<_>>(),
        [9, 16, 23, 48]
    );
    assert_eq!(skip(&doc.data, 0), 73);
    let mut rendered = Vec::new();
    doc.write(0, &mut rendered);
    assert_eq!(rendered, input.as_bytes());
}

#[test]
fn decimal_grammar_sign_scale_and_rounding() {
    for (input, expected) in [
        ("0", 0.0f64),
        ("-0", -0.0),
        ("-0.0e+0", -0.0),
        ("0.1", 0.1),
        ("0.00000001", 1e-8),
        ("-12.34e+2", -1234.0),
        ("1e-22", 1e-22),
        ("1e22", 1e22),
        ("9007199254740992", 9007199254740992.0),
    ] {
        assert_eq!(
            decimal_small(input.as_bytes()).to_bits(),
            expected.to_bits(),
            "{input}"
        );
    }
    for input in [
        "", "+1", "01", ".1", "1.", "1e", "1e+", "--1", "NaN", "1e999",
    ] {
        assert!(
            std::panic::catch_unwind(|| decimal_small(input.as_bytes())).is_err(),
            "{input}"
        );
    }
}

#[test]
fn escaped_text_runs_and_surrogate_pair() {
    let doc = parse(r#"["plain","a\"b\\c\n","\u00e9\uD83D\uDE03"]"#);
    let mut rendered = Vec::new();
    doc.write(0, &mut rendered);
    assert_eq!(rendered, r#"["plain","a\"b\\c\n","é😃"]"#.as_bytes());
}

#[test]
fn first_coordinates_member_wins() {
    let doc = parse(r#"{"coordinates":[{"x":1,"y":0,"z":0}],"coordinates":[{"x":0,"y":1,"z":0}]}"#);
    for mapping in [false, true] {
        let c = average(&doc, mapping);
        assert_eq!((c.x, c.y, c.z), (1.0, 0.0, 0.0));
    }
}
