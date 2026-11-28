//! The reviewed decoder cohort `registry`: a checkpoint note, a status entry
//! bundle and a release entry bundle, each under the laws' fixed trust and time.

use super::*;

// tools/decoder-fuzz builds the driver's test configuration with the libFuzzer
// callback for nightly discovery. No other build compiles it.
#[cfg(fuzzing)]
#[path = "../../../../../tools/decoder-fuzz/host_target.rs"]
mod host_target;

const SURFACES: [&str; 3] = ["note", "status", "inclusion"];

/// The reference reading of an entry bundle: each row is a big-endian u16
/// length and that many bytes.
fn framed(mut bytes: &[u8]) -> Option<Vec<Vec<u8>>> {
    let mut rows = Vec::new();
    while let [high, low, rest @ ..] = bytes {
        let (row, rest) = rest.split_at_checked(usize::from(u16::from_be_bytes([*high, *low])))?;
        rows.push(row.to_vec());
        bytes = rest;
    }
    bytes.is_empty().then_some(rows)
}

fn refusal(error: Error) -> String {
    let text = format!("{error:?}");
    text.split([' ', '{', '(']).next().unwrap().to_owned()
}

/// A checkpoint note against the release log's pinned trust. An accepted
/// note is retained as given and is its own successor.
fn observe_note(bytes: &[u8]) -> Result<String, String> {
    let trust = policy(Log::Release);
    let head = trust
        .accept(bytes, None, NOW, |_| Err(Error::MissingObject))
        .map_err(refusal)?;
    assert_eq!(head.note(), bytes);
    let again = trust.accept(bytes, Some(&head), NOW, |_| Err(Error::MissingObject));
    assert_eq!(
        again.as_ref(),
        Ok(&head),
        "an accepted note does not follow itself"
    );
    let [first, second] = head.witness_times().expect("pinned witnesses");
    Ok(format!(
        "size={} root={} witnessed={first},{second}",
        head.size(),
        hirsel_core_ids::hex::encode(&head.root())
    ))
}

/// A log whose one entry bundle is `bytes`, under a head the fixed log key
/// signs over the reference reading of its rows. Bytes with no such reading
/// stand as one row, which no reader may accept as a bundle.
struct Served<'a> {
    bytes: &'a [u8],
    rows: Option<Vec<Vec<u8>>>,
    head: Checkpoint,
}

impl<'a> Served<'a> {
    fn new(log: Log, signer: Signer, bytes: &'a [u8]) -> Self {
        let rows = framed(bytes);
        let entries = rows.clone().unwrap_or_else(|| vec![bytes.to_vec()]);
        let head = policy(log)
            .accept(&note(signer, &entries, NOW), None, NOW, |_| panic!())
            .unwrap();
        Self { bytes, rows, head }
    }

    fn read(&self, path: &str) -> Result<Vec<u8>, Error> {
        match &self.rows {
            Some(rows) => Objects(rows).read(path),
            None => Ok(self.bytes.to_vec()),
        }
    }

    fn rows(&self) -> &[Vec<u8>] {
        self.rows
            .as_deref()
            .expect("a reader accepted bytes that are no entry bundle")
    }
}

/// A status entry bundle replayed from an empty state. A refusal leaves the
/// state empty. An accepted replay projects each release's disposition.
fn observe_status(bytes: &[u8]) -> Result<String, String> {
    let served = Served::new(Log::Status, STATUS, bytes);
    let legal_key = ed25519_public(&FIRST.private()).unwrap();
    let mut replay = StatusReplay::default();
    let advanced = replay.advance::<&Release>(
        &served.head,
        NOW,
        |path| served.read(path),
        legal_key,
        |_, _| Err(Error::MissingEvidence),
    );
    if let Err(error) = advanced {
        assert_eq!(
            replay,
            StatusReplay::default(),
            "a refusal changed the state"
        );
        return Err(refusal(error));
    }
    assert_eq!(replay.checkpoint(), Some(&served.head));
    let mut text = format!("entries={}", served.rows().len());
    let mut seen = Vec::new();
    for row in served.rows() {
        let row: Value = serde_json::from_slice(row).unwrap();
        let root = Root(parse_digest(row[1].as_str().unwrap()).unwrap());
        if seen.contains(&root) {
            continue;
        }
        seen.push(root);
        let state = match &replay.state(root).expect("a replayed release").disposition {
            Disposition::Active => "active".to_owned(),
            Disposition::Retired { reason, .. } => format!("retired:{reason}"),
            Disposition::TakenDown { reason, .. } => format!("taken-down:{reason}"),
        };
        text.push_str(&format!(" {}={state}", &row[1].as_str().unwrap()[..8]));
    }
    Ok(text)
}

/// A release entry bundle listed under its head. Each listed release is
/// then included at its own index under the same head.
fn observe_inclusion(bytes: &[u8]) -> Result<String, String> {
    let served = Served::new(Log::Release, RELEASE, bytes);
    let releases = served
        .head
        .bundle_releases(|path| served.read(path))
        .map_err(refusal)?;
    let mut listed = Vec::new();
    for release in &releases {
        let identity = format!("{}@{}", release.id().name, release.id().major);
        let version = release.version().to_string();
        let claim = ReleaseClaim {
            identity: &identity,
            version: &version,
            root: release.root(),
            facts: Some(release.facts()),
        };
        let included = served
            .head
            .release(release.index(), claim, |path| served.read(path))
            .expect("a listed release is not included");
        assert_eq!(included.time(), release.time());
        listed.push(format!("{identity}:{version}#{}", release.index()));
    }
    Ok(format!(
        "entries={} releases={}",
        served.rows().len(),
        listed.join(",")
    ))
}

fn observe(surface: &str, bytes: &[u8]) -> Result<String, String> {
    match surface {
        "note" => observe_note(bytes),
        "status" => observe_status(bytes),
        "inclusion" => observe_inclusion(bytes),
        other => panic!("no surface {other}"),
    }
}

/// Discovery's entry to the owner predicates. The first byte selects the
/// surface, so an edited input keeps its parent's. `expect` is a declared
/// status that the observation must also meet. Returns the observation.
#[cfg(fuzzing)]
fn discovered(input: &[u8], _schedule: usize, expect: Option<&str>) -> String {
    let surface = input
        .split_first()
        .and_then(|(&index, bytes)| Some((*SURFACES.get(usize::from(index))?, bytes)));
    let observation = match surface {
        Some((surface, bytes)) => match observe(surface, bytes) {
            Ok(projection) => format!("accepted {surface} {projection}"),
            Err(reason) => format!("refused {surface} {reason}"),
        },
        None => "refused surface unknown".into(),
    };
    if let Some(expect) = expect {
        let status = observation.split(' ').next().unwrap();
        assert_eq!(status, expect, "discovered: declared status");
    }
    observation
}

#[test]
fn reviewed_registry() {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Limits {
        bytes: usize,
    }
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Setup {
        surface: String,
        authority: String,
        now: u64,
    }
    #[derive(Debug, PartialEq, Deserialize)]
    #[serde(rename_all = "snake_case")]
    enum Expected {
        Accepted(String),
        Refused(String),
    }
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Schedule {
        kind: String,
    }
    type Case = crate::property_corpus::reviewed::Case<Limits, Setup, Expected, Schedule>;
    let cases: Vec<(Case, Vec<u8>)> = crate::property_corpus::reviewed::read(
        include_bytes!("../../../../../core/tests/decoder-corpus/registry.json"),
        "registry",
        "reviewed_registry",
        "registry-log-object",
        "c2sp-signed-note",
    )
    .unwrap();
    // Accepted and refused cases of each surface.
    let mut seen = [[0; 2]; SURFACES.len()];
    for (case, bytes) in cases {
        let id = &case.id;
        let surface = case.setup.surface.as_str();
        let index = SURFACES.iter().position(|name| *name == surface).unwrap();
        assert_eq!(
            (case.setup.authority.as_str(), case.setup.now),
            (AUTHORITY, NOW),
            "{id}"
        );
        assert!(bytes.len() <= case.limits.bytes, "{id}");
        assert!(
            matches!(case.schedules.as_slice(), [one] if one.kind == "contiguous"),
            "{id}"
        );
        // Two replays, each from a new trust policy and an empty state.
        let first = observe(surface, &bytes);
        assert_eq!(observe(surface, &bytes), first, "{id}: replay");
        let observed = match first {
            Ok(projection) => Expected::Accepted(projection),
            Err(reason) => Expected::Refused(reason),
        };
        assert_eq!(observed, case.expected, "{id}");
        seen[index][usize::from(matches!(observed, Expected::Refused(_)))] += 1;
        eprintln!("{id}: {surface} {observed:?} twice");
    }
    assert!(seen.iter().flatten().all(|&count| count > 0), "{seen:?}");
}
