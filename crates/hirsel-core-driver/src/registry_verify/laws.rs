use super::notes::{Trust, Witnesses};
use super::status::{Disposition, StatusReplay};
use super::*;
use crate::call::base64;
use hirsel_core_net::key_wrap::{SignAlg, SigningRandom, ed25519_public, sign};
use serde::Deserialize;
use serde_json::json;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

const AUTHORITY: &str = "packages.example";
const NOW: u64 = 200_000;

#[derive(Clone, Copy)]
struct Signer {
    name: &'static str,
    seed: u8,
    kind: u8,
}

impl Signer {
    fn private(&self) -> Vec<u8> {
        let mut key = hirsel_core_ids::hex::decode("302e020100300506032b657004220420").unwrap();
        key.extend([self.seed; 32]);
        key
    }

    fn key(&self) -> (String, [u8; 4], String) {
        let public = ed25519_public(&self.private()).unwrap();
        let mut tagged_key = vec![self.kind];
        tagged_key.extend(public);
        let key_id: [u8; 4] = Sha256::new()
            .chain_update(self.name)
            .chain_update(b"\n")
            .chain_update(&tagged_key)
            .finalize()[..4]
            .try_into()
            .unwrap();
        let text = format!(
            "{}+{}+{}",
            self.name,
            hirsel_core_ids::hex::encode(&key_id),
            base64(&tagged_key)
        );
        (text, key_id, base64(&public))
    }

    fn line(&self, text: &str, time: u64) -> String {
        let (input, mut bytes) = if self.kind == 4 {
            (
                format!("cosignature/v1\ntime {time}\n{text}"),
                time.to_be_bytes().to_vec(),
            )
        } else {
            (text.to_owned(), Vec::new())
        };
        let mut signature = self.key().1.to_vec();
        signature.append(&mut bytes);
        signature.extend(
            sign(
                SignAlg::Ed25519,
                &self.private(),
                input.as_bytes(),
                SigningRandom::Virtual,
            )
            .unwrap(),
        );
        format!("— {} {}\n", self.name, base64(&signature))
    }
}

const RELEASE: Signer = Signer {
    name: "packages.example/log",
    seed: 1,
    kind: 1,
};
const STATUS: Signer = Signer {
    name: "packages.example/status",
    seed: 2,
    kind: 1,
};
const FIRST: Signer = Signer {
    name: "one.example/witness",
    seed: 3,
    kind: 4,
};
const SECOND: Signer = Signer {
    name: "two.example/witness",
    seed: 4,
    kind: 4,
};

fn policy(log: Log) -> Trust {
    let signer = match log {
        Log::Release => RELEASE,
        Log::Status => STATUS,
    };
    Trust::new(
        AUTHORITY,
        log,
        &signer.key().0,
        Witnesses::Pinned([
            ("operator-one", &FIRST.key().0),
            ("operator-two", &SECOND.key().0),
        ]),
    )
    .unwrap()
}

fn digest(prefix: u8, left: &[u8], right: &[u8]) -> Hash {
    Sha256::new()
        .chain_update([prefix])
        .chain_update(left)
        .chain_update(right)
        .finalize()
        .into()
}

// RFC 6962 section 2.1 defines MTH by splitting at the largest smaller power of two.
fn reference_root(entries: &[Vec<u8>]) -> Hash {
    match entries {
        [] => Sha256::digest([]).into(),
        [entry] => digest(0, entry, &[]),
        _ => {
            let split = (entries.len() - 1).ilog2();
            let (left, right) = entries.split_at(1 << split);
            digest(1, &reference_root(left), &reference_root(right))
        }
    }
}

fn signed(signer: Signer, size: u64, root: Hash, cosigners: &[(&Signer, u64)]) -> Vec<u8> {
    let text = format!("{}\n{size}\n{}\n", signer.name, base64(&root));
    let mut envelope = format!("{text}\n{}", signer.line(&text, 0));
    for (witness, time) in cosigners {
        envelope.push_str(&witness.line(&text, *time));
    }
    envelope.into_bytes()
}

fn note(signer: Signer, entries: &[Vec<u8>], time: u64) -> Vec<u8> {
    signed(
        signer,
        entries.len() as u64,
        reference_root(entries),
        &[(&FIRST, time), (&SECOND, time)],
    )
}

struct Objects<'a>(&'a [Vec<u8>]);

impl Objects<'_> {
    fn read(&self, path: &str) -> Result<Vec<u8>, Error> {
        let fields: Vec<_> = path.split('/').collect();
        let level = if fields[1] == "entries" {
            None
        } else {
            Some(fields[1].parse::<u32>().unwrap())
        };
        let mut index = 0;
        for group in &fields[2..] {
            let (digits, partial) = group.split_once(".p").unwrap_or((group, ""));
            index = index * 1000 + digits.trim_start_matches('x').parse::<usize>().unwrap();
            if digits != *group || (!digits.starts_with('x') && partial.is_empty()) {
                break;
            }
        }
        let width = if path.contains(".p/") {
            fields.last().unwrap().parse::<usize>().unwrap()
        } else {
            256
        };
        let stride = 1usize << (8 * level.unwrap_or(0));
        let start = index * 256 * stride;
        let entries = self
            .0
            .get(start..start + width * stride)
            .ok_or(Error::MissingObject)?;
        let mut bytes = Vec::new();
        for group in entries.chunks_exact(stride) {
            if level.is_some() {
                bytes.extend(reference_root(group));
            } else {
                bytes.extend((group[0].len() as u16).to_be_bytes());
                bytes.extend(&group[0]);
            }
        }
        Ok(bytes)
    }
}

fn auth() -> Value {
    json!([
        ["ed25519", FIRST.key().2],
        "ab".repeat(32),
        null,
        base64(&[7; 64])
    ])
}

fn release_entry(identity: &str, root: Root, facts: Hash) -> Vec<u8> {
    serde_json::to_vec(&json!([
        "release",
        identity,
        "1.2.3",
        hirsel_core_ids::hex::encode(&root.0),
        "Release",
        null,
        "100",
        "1",
        "ab".repeat(32),
        hirsel_core_ids::hex::encode(&facts),
        auth()
    ]))
    .unwrap()
}

#[test]
fn g6_signed_vectors() {
    release_hint_bundle_and_tile_membership();
    consistency_and_split_view_preserve_original_notes();
    witness_quorum_freshness_and_heartbeats_are_per_log();
    full_and_incremental_status_replay_refuse_omission();
    status_advisory_is_bound_to_release_and_withdrawal_keeps_other_advisories();
}

#[derive(Deserialize)]
struct EntryBundleCorpus {
    schema_version: u64,
    objects: BTreeMap<String, String>,
    cases: Vec<EntryBundleCase>,
}

#[derive(Deserialize)]
struct EntryBundleCase {
    id: String,
    digest: String,
    generator_version: u64,
    setup: EntryBundleSetup,
    expected: EntryBundleExpected,
}

#[derive(Deserialize)]
struct EntryBundleSetup {
    width: u64,
    rust_source: BundleSource,
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum BundleSource {
    Partial,
    Full,
}

#[derive(Deserialize)]
struct EntryBundleExpected {
    rust: RustBundleExpected,
}

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum RustBundleExpected {
    Entries { entries_hex: Vec<String> },
    Bundle,
    MissingObject,
}

#[test]
fn reviewed_entry_bundles() {
    use hirsel_core_ids::hex::{decode, encode};

    let reviewed = include_str!("../../../../core/tests/decoder-corpus/c2sp-bundle.json");
    let projection = include_str!("../../../../core/programs/registry/entry_bundle_fixture.hirsel");
    let literal = projection
        .lines()
        .find_map(|line| line.strip_prefix("const C2SP_BUNDLE_JSON: String = "))
        .expect("C2SP corpus projection contains its JSON literal");
    let projected: String = serde_json::from_str(literal).unwrap();
    assert_eq!(
        projected.as_bytes(),
        reviewed.as_bytes(),
        "stale C2SP corpus projection"
    );
    let corpus: EntryBundleCorpus = serde_json::from_str(reviewed).unwrap();
    assert_eq!(corpus.schema_version, 1, "unsupported C2SP corpus version");
    assert_eq!(corpus.cases.len(), 5);
    assert!(corpus.cases.windows(2).all(|pair| pair[0].id < pair[1].id));
    let objects: BTreeMap<_, _> = corpus
        .objects
        .into_iter()
        .map(|(digest, hex)| {
            let bytes = decode(&hex).unwrap();
            assert_eq!(
                encode(&Sha256::digest(&bytes)),
                digest,
                "C2SP object digest"
            );
            (digest, bytes)
        })
        .collect();
    for case in corpus.cases {
        let id = &case.id;
        assert_eq!(
            case.generator_version, 1,
            "{id}: unsupported generator version"
        );
        let width = case.setup.width;
        assert!((1..tiles::WIDTH).contains(&width), "{id}: partial width");
        let partial = format!("tile/entries/000.p/{width}");
        let stored = match case.setup.rust_source {
            BundleSource::Partial => &partial,
            BundleSource::Full => "tile/entries/000",
        };
        let bytes = &objects[&case.digest];
        let expected = match case.expected.rust {
            RustBundleExpected::Entries { entries_hex } => Ok(entries_hex
                .iter()
                .map(|hex| decode(hex).unwrap())
                .collect::<Vec<_>>()),
            RustBundleExpected::Bundle => Err(Error::Bundle),
            RustBundleExpected::MissingObject => Err(Error::MissingObject),
        };
        let entries = expected.as_deref().unwrap_or(&[]);
        let mut requests = Vec::new();
        let included = tiles::included_bundle(width, reference_root(entries), 0, &mut |path| {
            requests.push(path.to_owned());
            if path == stored {
                Ok(bytes.clone())
            } else {
                Err(Error::MissingObject)
            }
        });
        let actual = included.and_then(|body| {
            tiles::bundle_entries(&body, width, 0)
                .map(|entries| entries.into_iter().map(<[u8]>::to_vec).collect())
        });
        assert_eq!(actual, expected, "{id}");
        assert_eq!(requests, [partial], "{id}: Rust reads only the exact key");
    }
}

fn release_hint_bundle_and_tile_membership() {
    let identity = "packages.example/json@1";
    let root = Root::of(b"library content");
    let facts = [11; 32];
    let entry = release_entry(identity, root, facts);
    let rows = vec![entry; 257];
    let objects = Objects(&rows);
    let trust = policy(Log::Release);
    let head = trust
        .accept(&note(RELEASE, &rows, NOW), None, NOW, |_| panic!())
        .unwrap();
    let claim = ReleaseClaim {
        identity,
        version: "1.2.3",
        root,
        facts: Some(facts),
    };
    for index in [0, 255, 256] {
        assert_eq!(
            head.release(index, claim, |path| objects.read(path))
                .unwrap()
                .root(),
            root
        );
    }
    for claim in [
        ReleaseClaim {
            identity: "packages.example/other@1",
            ..claim
        },
        ReleaseClaim {
            root: Root::of(b"mislabelled content"),
            ..claim
        },
        ReleaseClaim {
            version: "1.2.4",
            ..claim
        },
        ReleaseClaim {
            facts: Some([12; 32]),
            ..claim
        },
    ] {
        assert_eq!(
            head.release(256, claim, |path| objects.read(path))
                .unwrap_err(),
            Error::ReleaseClaim
        );
    }
    assert_eq!(
        head.release(0, claim, |path| {
            let mut bytes = objects.read(path)?;
            if path == "tile/entries/000" {
                // Corrupt a different member of the fetched bundle.
                *bytes.last_mut().unwrap() ^= 1;
            }
            Ok(bytes)
        })
        .unwrap_err(),
        Error::MerkleRoot
    );
    assert_eq!(
        head.release(256, claim, |path| {
            let mut bytes = objects.read(path)?;
            if path == "tile/1/000.p/1" {
                bytes[0] ^= 1;
            }
            Ok(bytes)
        })
        .unwrap_err(),
        Error::MerkleRoot
    );
}

fn consistency_and_split_view_preserve_original_notes() {
    let rows: Vec<_> = (0u64..513)
        .map(|index| index.to_be_bytes().to_vec())
        .collect();
    let trust = policy(Log::Release);
    let next_note = note(RELEASE, &rows, NOW);
    for size in [0, 1, 3, 255, 256, 257, 511, 512, 513] {
        let old_note = note(RELEASE, &rows[..size], NOW);
        let old = trust.accept(&old_note, None, NOW, |_| panic!()).unwrap();
        let head = trust
            .accept(&next_note, Some(&old), NOW, |path| {
                Objects(&rows).read(path)
            })
            .unwrap();
        assert_eq!(head.size(), 513);
    }
    let old = trust.accept(&next_note, None, NOW, |_| panic!()).unwrap();
    let fork = signed(RELEASE, 513, [22; 32], &[(&FIRST, NOW), (&SECOND, NOW)]);
    assert_eq!(
        trust
            .accept(&fork, Some(&old), NOW, |_| panic!())
            .unwrap_err(),
        Error::Conflict {
            prior: next_note.clone(),
            candidate: fork
        }
    );
    let smaller = note(RELEASE, &rows[..512], NOW);
    assert_eq!(
        trust
            .accept(&smaller, Some(&old), NOW, |_| panic!())
            .unwrap_err(),
        Error::Conflict {
            prior: next_note.clone(),
            candidate: smaller
        }
    );
    let old_note = note(RELEASE, &rows[..257], NOW);
    let old = trust.accept(&old_note, None, NOW, |_| panic!()).unwrap();
    assert_eq!(
        trust
            .accept(&next_note, Some(&old), NOW, |path| {
                let mut data = Objects(&rows).read(path)?;
                data[0] ^= 1;
                Ok(data)
            })
            .unwrap_err(),
        Error::Conflict {
            prior: old_note,
            candidate: next_note
        }
    );
}

#[test]
fn unavailable_consistency_tile_preserves_fetch_error() {
    let rows = [vec![1], vec![2]];
    let trust = policy(Log::Release);
    let old = trust.authenticate(&note(RELEASE, &rows[..1], NOW)).unwrap();
    let next = trust.authenticate(&note(RELEASE, &rows, NOW)).unwrap();
    assert_eq!(
        next.extends(&old, |_| Err(Error::MissingObject)),
        Err(Error::MissingObject)
    );
}

fn witness_quorum_freshness_and_heartbeats_are_per_log() {
    for (log, signer) in [(Log::Release, RELEASE), (Log::Status, STATUS)] {
        let trust = policy(log);
        let root = reference_root(&[]);
        let with_one = signed(signer, 0, root, &[(&FIRST, NOW)]);
        assert_eq!(
            trust
                .accept(&with_one, None, NOW, |_| panic!())
                .unwrap_err(),
            Error::WitnessQuorum
        );
        let preview = Trust::new(
            AUTHORITY,
            log,
            &match log {
                Log::Release => RELEASE,
                Log::Status => STATUS,
            }
            .key()
            .0,
            Witnesses::Preview,
        )
        .unwrap();
        let preview_head = preview.accept(&with_one, None, NOW, |_| panic!()).unwrap();
        assert!(preview_head.is_preview());
        let signer = match log {
            Log::Release => RELEASE,
            Log::Status => STATUS,
        };
        let earliest = note(signer, &[], NOW - 86_400);
        let old = trust.accept(&earliest, None, NOW, |_| panic!()).unwrap();
        assert_eq!(
            trust
                .accept(&earliest, Some(&old), NOW + 1, |_| panic!())
                .unwrap_err(),
            Error::WitnessTime
        );
        let signer = match log {
            Log::Release => RELEASE,
            Log::Status => STATUS,
        };
        let heartbeat = note(signer, &[], NOW + 1);
        assert_eq!(
            trust
                .accept(&heartbeat, Some(&preview_head), NOW + 1, |_| panic!())
                .unwrap_err(),
            Error::Trust
        );
        let fresh = trust
            .accept(&heartbeat, Some(&old), NOW + 1, |_| panic!())
            .unwrap();
        assert_eq!(fresh.size(), old.size());
        assert_eq!(fresh.witness_times(), Some([NOW + 1; 2]));
        let other = match log {
            Log::Release => STATUS,
            Log::Status => RELEASE,
        };
        assert_eq!(
            trust
                .accept(
                    &note(other, &[], NOW + 1),
                    Some(&old),
                    NOW + 1,
                    |_| panic!()
                )
                .unwrap_err(),
            Error::Origin
        );
        let signer = match log {
            Log::Release => RELEASE,
            Log::Status => STATUS,
        };
        let text = format!("{}\n0\n{}\n", signer.name, base64(&root));
        let wrong_head = format!("{}\n1\n{}\n", signer.name, base64(&[1; 32]));
        let bad_heartbeat = format!(
            "{text}\n{}{}{}",
            signer.line(&text, 0),
            FIRST.line(&text, NOW + 1),
            SECOND.line(&wrong_head, NOW + 1)
        );
        assert_eq!(
            trust
                .accept(bad_heartbeat.as_bytes(), Some(&old), NOW + 1, |_| panic!())
                .unwrap_err(),
            Error::Signature
        );
    }
    assert!(matches!(
        Trust::new(
            AUTHORITY,
            Log::Release,
            &RELEASE.key().0,
            Witnesses::Pinned([("shared", &FIRST.key().0), ("shared", &SECOND.key().0)])
        ),
        Err(Error::Trust)
    ));
    let same_key = Signer {
        name: "two.example/renamed",
        seed: FIRST.seed,
        kind: 4,
    };
    assert!(matches!(
        Trust::new(
            AUTHORITY,
            Log::Release,
            &RELEASE.key().0,
            Witnesses::Pinned([("one", &FIRST.key().0), ("two", &same_key.key().0)])
        ),
        Err(Error::Trust)
    ));
}

fn status_row(action: &str, root: Root, previous: Option<&Vec<u8>>, fields: &[Value]) -> Vec<u8> {
    let mut row = vec![
        json!(action),
        json!(hirsel_core_ids::hex::encode(&root.0)),
        json!(previous.map(|entry| hirsel_core_ids::hex::encode(&digest(0, entry, &[])))),
    ];
    row.extend_from_slice(fields);
    row.extend([json!("100"), auth()]);
    serde_json::to_vec(&row).unwrap()
}

fn full_and_incremental_status_replay_refuse_omission() {
    let root = Root::of(b"release A");
    let other = Root::of(b"release B");
    let mut rows = Vec::new();
    for _ in 0..128 {
        rows.push(status_row(
            "retire",
            root,
            rows.last(),
            &[json!("security"), json!("fix pending")],
        ));
        rows.push(status_row(
            "reinstate",
            root,
            rows.last(),
            &[json!("fixed")],
        ));
    }
    rows.push(status_row(
        "retire",
        root,
        rows.last(),
        &[json!("other"), json!("final retirement")],
    ));
    let (row, intent) = crate::registry_route::laws::signed_takedown(
        AUTHORITY,
        "fixture.test",
        other,
        Value::Null,
        "removed",
        FIRST.seed,
    );
    rows.push(serde_json::to_vec(&row).unwrap());
    let legal_key = ed25519_public(&FIRST.private()).unwrap();
    let releases = [release_entry("fixture.test/json@1", other, [3; 32])];
    let release_head = policy(Log::Release)
        .authenticate(&note(RELEASE, &releases, NOW))
        .unwrap();
    let release = release_head
        .locate_release(0, "fixture.test/json@1", "1.2.3", |path| {
            Objects(&releases).read(path)
        })
        .unwrap();
    let evidence = |root, digest| {
        assert_eq!(root, other);
        assert_eq!(digest, Hash::from(Sha256::digest(&intent)));
        Ok((&release, intent.clone()))
    };
    let trust = policy(Log::Status);
    let head = trust
        .accept(&note(STATUS, &rows, NOW), None, NOW, |_| panic!())
        .unwrap();
    let old = trust
        .accept(&note(STATUS, &rows[..255], NOW), None, NOW, |_| panic!())
        .unwrap();
    let full = Objects(&rows);
    let mut contiguous = StatusReplay::default();
    contiguous
        .advance::<&Release>(&head, NOW, |path| full.read(path), legal_key, evidence)
        .unwrap();
    assert_eq!(
        contiguous.state(root).unwrap().disposition,
        Disposition::Retired {
            reason: "other".into(),
            message: "final retirement".into()
        }
    );
    assert_eq!(
        contiguous.state(other).unwrap().disposition,
        Disposition::TakenDown {
            reason: "legal".into(),
            message: "removed".into()
        }
    );
    let mut incremental = StatusReplay::default();
    incremental
        .advance::<&Release>(
            &old,
            NOW,
            |path| {
                if path == "tile/entries/000.p/255" {
                    return full.read("tile/entries/000");
                }
                panic!("unexpected prefix path {path}");
            },
            legal_key,
            evidence,
        )
        .unwrap();
    let next = trust
        .accept(&note(STATUS, &rows, NOW), Some(&old), NOW, |path| {
            full.read(path)
        })
        .unwrap();
    for (change, expected) in [
        ("omit bundle", Error::MissingObject),
        ("remove retirement", Error::Bundle),
        ("truncate bundle", Error::Bundle),
        ("edit message", Error::MerkleRoot),
    ] {
        let result = incremental.advance::<&Release>(
            &next,
            NOW,
            |path| {
                let mut bytes = full.read(path)?;
                if path == "tile/entries/001.p/2" {
                    match change {
                        "omit bundle" => return Err(Error::MissingObject),
                        "remove retirement" => {
                            bytes.drain(..2 + rows[256].len());
                        }
                        "truncate bundle" => bytes.truncate(bytes.len() - 1),
                        "edit message" => {
                            let offset = bytes
                                .windows(b"final retirement".len())
                                .position(|window| window == b"final retirement")
                                .unwrap();
                            bytes[offset] = b'F';
                        }
                        _ => unreachable!(),
                    }
                }
                Ok(bytes)
            },
            legal_key,
            evidence,
        );
        assert_eq!(result.unwrap_err(), expected, "{change}");
        assert_eq!(incremental.checkpoint().unwrap().size(), 255);
        assert!(incremental.state(other).is_none());
    }
    incremental
        .advance::<&Release>(&next, NOW, |path| full.read(path), legal_key, evidence)
        .unwrap();
    assert_eq!(incremental, contiguous);
}

fn status_advisory_is_bound_to_release_and_withdrawal_keeps_other_advisories() {
    let identity = "packages.example/json@1";
    let root = Root::of(b"OSV release");
    let release_rows = [release_entry(identity, root, [3; 32])];
    let log = policy(Log::Release);
    let head = log
        .accept(&note(RELEASE, &release_rows, NOW), None, NOW, |_| panic!())
        .unwrap();
    let release = head
        .release(
            0,
            ReleaseClaim {
                identity,
                version: "1.2.3",
                root,
                facts: None,
            },
            |path| Objects(&release_rows).read(path),
        )
        .unwrap();
    let first = br#"{"affected":[{"ranges":[{"events":[{"introduced":"0"}],"repo":"https://example.com/repo","type":"GIT"}]},{"package":{"ecosystem":"Hirsel","name":"packages.example/json"},"ranges":[{"events":[{"limit":"0.1.0"},{"fixed":"1.3.0+release"},{"introduced":"1.0.0"},{"limit":"*"}],"type":"SEMVER"}]}],"aliases":["CVE-1"],"id":"OSV-1","modified":"2026-01-01T00:00:00Z"}"#;
    let second = br#"{"affected":[{"package":{"ecosystem":"Hirsel","name":"packages.example/json"},"versions":["1.2.3"]}],"id":"OSV-2","modified":"2026-01-01T00:00:00Z"}"#;
    let withdrawn = br#"{"affected":[{"package":{"ecosystem":"Hirsel","name":"packages.example/json"},"versions":["1.2.3"]}],"id":"OSV-1","modified":"2026-01-02T00:00:00Z","withdrawn":"2026-01-02T00:00:00Z"}"#;
    let docs: BTreeMap<Hash, &[u8]> = [first.as_slice(), second.as_slice(), withdrawn.as_slice()]
        .into_iter()
        .map(|bytes| (Sha256::digest(bytes).into(), bytes))
        .collect();
    let mut rows = Vec::new();
    for (id, bytes) in [
        ("OSV-1", first.as_slice()),
        ("OSV-2", second),
        ("OSV-1", withdrawn),
    ] {
        rows.push(status_row(
            "advise",
            root,
            rows.last(),
            &[
                json!(id),
                json!(hirsel_core_ids::hex::encode(&Sha256::digest(bytes))),
            ],
        ));
    }
    let trust = policy(Log::Status);
    let status = trust
        .accept(&note(STATUS, &rows, NOW), None, NOW, |_| panic!())
        .unwrap();
    let mut replay = StatusReplay::default();
    assert_eq!(
        replay
            .advance::<&Release>(
                &status,
                NOW,
                |path| Objects(&rows).read(path),
                [0; 32],
                |_, _| Err(Error::MissingEvidence)
            )
            .unwrap_err(),
        Error::MissingEvidence
    );
    assert!(replay.checkpoint().is_none());
    replay
        .advance::<&Release>(
            &status,
            NOW,
            |path| Objects(&rows).read(path),
            [0; 32],
            |target, hash| {
                assert_eq!(target, root);
                Ok((&release, docs[&hash].to_vec()))
            },
        )
        .unwrap();
    let state = replay.state(root).unwrap();
    assert_eq!(state.disposition, Disposition::Active);
    assert_eq!(state.advisories.keys().collect::<Vec<_>>(), &["OSV-2"]);
}

// The reviewed decoder cohort `registry` over the fixtures above.
mod reviewed;
