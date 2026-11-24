use super::*;

include!("cases.rs");

/// NET-18 `public_class_table`: every row's first and last address and the
/// public address just outside it, and the named embedded cases. Expected
/// classes and CPython 3.14.0's `is_global` come from `oracle.py`, which checks
/// the table against the IANA special-purpose registries.
#[test]
fn public_class_table() {
    for &(text, expected, _cpython_global) in CASES {
        let ip: IpAddr = text.parse().unwrap();
        assert_eq!(class(ip), expected, "{text}");
    }
}
