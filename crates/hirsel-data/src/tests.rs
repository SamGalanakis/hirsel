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

#[test]
fn proposal_host_and_guest_values_and_error_spans_agree() {
    let grants = include_str!("../../hirsel-config-schema/schemas/grants.hirsel");
    let package = include_str!("../../hirsel-config-schema/schemas/pkg_v1.hirsel");
    let host = include_str!("../../hirsel-config-schema/schemas/host_v1.hirsel");
    let package = format!("{package}\n{grants}");
    let host = format!("{host}\n{grants}");
    let schemas = [
        ("pkg_v1", "Package", package.as_str()),
        ("pkg_v1", "Workspace", package.as_str()),
        (
            "user_v1",
            "Config",
            include_str!("../../hirsel-config-schema/schemas/user_v1.hirsel"),
        ),
        ("host_v1", "Group", host.as_str()),
        (
            "orders_v1",
            "Site",
            include_str!("../../hirsel-config-schema/fixtures/orders_v1.hirsel"),
        ),
    ]
    .map(|(module, root, source)| Descriptor::lower(module, source, root).unwrap());
    let docs: [&[u8]; 5] = [
        include_bytes!("../../hirsel-config-schema/fixtures/app/hirsel.hcfg"),
        include_bytes!("../../hirsel-config-schema/fixtures/hirsel.hcfg"),
        include_bytes!("../../hirsel-config-schema/fixtures/config.hcfg"),
        include_bytes!("../../hirsel-config-schema/fixtures/group.hcfg"),
        include_bytes!("../../hirsel-config-schema/fixtures/site.hcfg"),
    ];
    for (i, doc) in docs.into_iter().enumerate() {
        for (j, schema) in schemas.iter().enumerate() {
            let limits = Limits::default();
            let host = decode(doc, &[schema], limits).map(|d| d.value);
            let guest = read::guest_with_budget(doc, schema, limits, &mut |_| Ok(()))
                .map(Decoded::projection);
            assert_eq!(guest, host, "document {i}, schema {j}");
            assert_eq!(host.is_ok(), i == j);
        }
    }
}

// tools/decoder-fuzz builds this crate's test configuration with the libFuzzer
// callback for nightly discovery. No other build compiles it.
#[cfg(fuzzing)]
#[path = "../../../tools/decoder-fuzz/host_target.rs"]
mod host_target;
#[path = "../../hirsel-core-driver/src/property_corpus/reviewed.rs"]
mod reviewed;

/// The limits every reviewed and discovered `.hcfg` document is read under.
const REVIEWED_LIMITS: Limits = Limits {
    input_bytes: 4096,
    depth: 16,
    nodes: 256,
    collection: 8,
    decoded_bytes: 1024,
    comparison_bytes: 4096,
    diagnostics: 1,
};

#[derive(Debug, PartialEq, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
enum Observed {
    Decoded(serde_json::Value),
    /// The document is not a value of the schema.
    Invalid(String),
    /// The document is past one of the limits.
    Limit(String),
}

fn orders() -> Descriptor {
    let source = include_str!("../../hirsel-config-schema/fixtures/orders_v1.hirsel");
    Descriptor::lower("orders_v1", source, "Site").unwrap()
}

/// The owner predicate of the `.hcfg` reader: the host's value or typed
/// refusal, which the guest projection of the same bytes must equal.
fn observe(schema: &Descriptor, bytes: &[u8]) -> Observed {
    let host = decode(bytes, &[schema], REVIEWED_LIMITS).map(|document| document.value);
    let guest = read::guest_with_budget(bytes, schema, REVIEWED_LIMITS, &mut |_| Ok(()))
        .map(Decoded::projection);
    assert_eq!(guest, host, "guest and host readings");
    match host {
        Ok(value) => Observed::Decoded(value),
        Err(error) => match error.kind {
            ErrorKind::Invalid(reason) => Observed::Invalid(reason.into()),
            ErrorKind::Limit(limit) => Observed::Limit(limit.into()),
            resource => panic!("the reader ran out of {resource:?}"),
        },
    }
}

/// Discovery's entry to the owner predicate. `expect` is a declared status
/// that the observation must also meet. Returns the observation.
#[cfg(fuzzing)]
fn discovered(input: &[u8], _schedule: usize, expect: Option<&str>) -> String {
    let observation = match observe(&orders(), input) {
        Observed::Decoded(value) => format!("decoded {value}"),
        Observed::Invalid(reason) => format!("invalid {reason}"),
        Observed::Limit(limit) => format!("limit {limit}"),
    };
    if let Some(expect) = expect {
        let status = observation.split(' ').next().unwrap();
        assert_eq!(status, expect, "discovered: declared status");
    }
    observation
}

#[test]
fn reviewed_hcfg() {
    #[derive(serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Setup {
        schema: String,
    }
    #[derive(serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Schedule {
        kind: String,
    }
    #[derive(serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Budget {
        input_bytes: u64,
        depth: u64,
        nodes: u64,
        collection: u64,
        decoded_bytes: u64,
        comparison_bytes: u64,
        diagnostics: u64,
    }
    type Case = reviewed::Case<Budget, Setup, Observed, Schedule>;
    let cases: Vec<(Case, Vec<u8>)> = reviewed::read(
        include_bytes!("../../../core/tests/decoder-corpus/hcfg.json"),
        "hcfg",
        "reviewed_hcfg",
        "hcfg-document",
        "hirsel-data-projection",
    )
    .unwrap();
    let mut statuses = [0; 3];
    for (case, bytes) in cases {
        let id = &case.id;
        let budget = &case.limits;
        let limits = Limits::from([
            budget.input_bytes,
            budget.depth,
            budget.nodes,
            budget.collection,
            budget.decoded_bytes,
            budget.comparison_bytes,
            budget.diagnostics,
        ]);
        assert_eq!(
            format!("{limits:?}"),
            format!("{REVIEWED_LIMITS:?}"),
            "{id}"
        );
        assert_eq!(case.setup.schema, "orders_v1.Site", "{id}");
        assert!(
            matches!(case.schedules.as_slice(), [one] if one.kind == "contiguous"),
            "{id}"
        );
        // Two replays, each from a newly lowered schema.
        let first = observe(&orders(), &bytes);
        assert_eq!(observe(&orders(), &bytes), first, "{id}: replay");
        assert_eq!(first, case.expected, "{id}");
        let status = match first {
            Observed::Decoded(_) => 0,
            Observed::Invalid(_) => 1,
            Observed::Limit(_) => 2,
        };
        statuses[status] += 1;
        eprintln!("{id}: {} twice", ["decoded", "invalid", "limit"][status]);
    }
    // A value, a semantic refusal and a budget refusal are separate cases.
    assert!(statuses.iter().all(|&count| count > 0), "{statuses:?}");
}
