use super::*;

pub(super) fn fixed_path_bytes(path: &DefPath) -> Vec<u8> {
    fn text(bytes: &mut Vec<u8>, field: &str) {
        bytes.extend((field.len() as u64).to_le_bytes());
        bytes.extend(field.as_bytes());
    }

    let mut bytes = Vec::new();
    text(&mut bytes, &path.0.package.name);
    text(&mut bytes, &path.0.package.version);
    bytes.extend((path.0.modules.len() as u64).to_le_bytes());
    for module in &path.0.modules {
        text(&mut bytes, module);
    }
    text(&mut bytes, &path.0.name);
    bytes
}

#[test]
fn canonical_driver_encoding_visits_the_value_once() {
    struct Counted<'a>(&'a std::cell::Cell<usize>, &'a DefPath);
    impl Serialize for Counted<'_> {
        fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
            self.0.set(self.0.get() + 1);
            self.1.serialize(serializer)
        }
    }
    let path = DefPath(hirsel_core_ids::ItemPath {
        package: hirsel_core_ids::PackageId {
            name: "package".repeat(20),
            version: "1".into(),
        },
        modules: vec!["nested".repeat(20), "é\0".into()],
        name: "value".repeat(20),
    });
    let visits = std::cell::Cell::new(0);
    let encoded = encode(&Counted(&visits, &path)).unwrap();
    assert_eq!(encoded, fixed_path_bytes(&path));
    assert_eq!(decode::<DefPath>(&encoded), Some(path));
    assert!(decode::<DefPath>(&encoded[..encoded.len() - 1]).is_none());
    let mut trailing = encoded;
    trailing.push(0);
    assert!(decode::<DefPath>(&trailing).is_none());
    assert_eq!(
        visits.get(),
        1,
        "canonical encoding made a sizing traversal"
    );
}
