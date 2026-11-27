use super::*;
use serde_json::json;

const SCHEMA: &str = r#"
pub type Site: codec {
    banner: String,
    labels: Map[String, u64],
    attempts: u8 = 3,
    credential: Option[String],
}
"#;

#[test]
fn checked_data_laws() {
    let schema = Descriptor::lower("orders_v1", SCHEMA, "Site").unwrap();
    let read = |text: &str| decode(text.as_bytes(), &[&schema], Limits::default());
    let doc = read(
        r#"orders_v1.Site(labels: [("k", 18446744073709551615)], // stays
        banner: "hello")"#,
    )
    .unwrap();
    assert_eq!(
        doc.value,
        json!({"labels":[["k", "18446744073709551615"]],"banner":"hello","attempts":3,"credential":{"case":"None","fields":[]}})
    );
    for bad in [
        "other.Site(labels: [], banner: \"secret\")",
        "orders_v1.Site(labels: [])",
        "orders_v1.Site(labels: [], banner: include(\"secret\"))",
        "orders_v1.Site(labels: [], banner: \"secret\" + \"rest\")",
        "orders_v1.Site(labels: [], banner: \"secret\", banner: \"again\")",
    ] {
        let error = read(bad).unwrap_err();
        assert!(!format!("{error:?}").contains("secret"));
    }
    let duplicate = r#"orders_v1.Site(banner: "", labels: [("a", 1), ("\u{61}", 2)])"#;
    let error = read(duplicate).unwrap_err();
    assert_eq!(
        &duplicate[error.span.start as usize..error.span.end as usize],
        r#""\u{61}""#
    );
    let first = error.related.unwrap();
    assert_eq!(
        &duplicate[first.start as usize..first.end as usize],
        r#""a""#
    );
    assert!(Descriptor::lower("orders_v1", &SCHEMA.replace("= 3", "= 256"), "Site").is_err());
    let limits = Limits {
        input_bytes: 1,
        ..Limits::default()
    };
    assert_eq!(
        decode(b"not utf8\xff", &[&schema], limits)
            .unwrap_err()
            .kind,
        ErrorKind::Limit("input bytes")
    );
}
