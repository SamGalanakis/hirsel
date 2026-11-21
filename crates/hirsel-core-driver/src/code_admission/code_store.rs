use super::*;
use crate::code_store::CodeStore;
use hirsel_core_ids::DefPath;
use hirsel_core_ir::address::{Binding, Member};
use hirsel_core_ir::normal::Site;
use hirsel_core_ir::release_recipe::{BodyRef, Launch};
use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::net::Shutdown;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;
use std::thread;
use tempfile::TempDir;

const TARGET: &str = "x86_64-unknown-linux-gnu";
const LARGE: u64 = 1 << 20;

fn limits() -> AdmissionLimits {
    AdmissionLimits {
        recipe_bytes: LARGE as usize,
        closure_objects: 64,
        object_bytes: LARGE,
        decoded_bytes: LARGE as usize,
        members: 64,
        edges: 64,
        nesting: 32,
        outstanding: 64,
        timeout: Some(Duration::from_secs(10)),
    }
}

struct Harness {
    root: TempDir,
    control: UnixListener,
    store: AdmissionStore,
}

impl Harness {
    fn new(staging: u64) -> Self {
        let root = tempfile::tempdir().unwrap();
        let control = UnixListener::bind(root.path().join("control")).unwrap();
        let store = AdmissionStore::open(&root.path().join("code"), staging).unwrap();
        Self {
            root,
            control,
            store,
        }
    }

    fn root(&self) -> PathBuf {
        self.root.path().join("code")
    }

    fn run(
        &self,
        limits: AdmissionLimits,
        semantics: &(impl BackendSemantics + Sync),
        client: impl FnOnce(&mut UnixStream),
    ) -> Result<AdmittedRelease<'_>, AdmissionError> {
        thread::scope(|scope| {
            let host = scope.spawn(|| {
                let mut connection = ControlConnection::accept(&self.control).unwrap();
                self.store.admit(&mut connection, &limits, semantics)
            });
            client(&mut UnixStream::connect(self.root.path().join("control")).unwrap());
            host.join().unwrap()
        })
    }

    fn admit(&self, fixture: &Fixture) -> Result<AdmittedRelease<'_>, AdmissionError> {
        self.run(limits(), &NativeSemantics, |peer| fixture.send(peer))
    }

    fn cache(&self, fixture: &Fixture, kind: ObjectKind, object: ObjectRef) -> ObjectKey {
        let key = ObjectKey {
            kind,
            hash: object.hash,
        };
        assert_eq!(
            CodeStore::new(self.root())
                .put(kind, &fixture.objects[&key])
                .unwrap(),
            object
        );
        key
    }

    fn assert_released(&self, key: ObjectKey) {
        assert_staging_empty(&self.root());
        assert!(
            self.store.remove_unreserved(key).unwrap(),
            "collection can remove the former pin"
        );
    }
}

fn assert_staging_empty(root: &Path) {
    assert!(
        fs::read_dir(root.join("staging")).unwrap().next().is_none(),
        "staging is empty after the attempt ends"
    );
}

struct Fixture {
    recipe: Recipe,
    objects: BTreeMap<ObjectKey, Vec<u8>>,
}

impl Fixture {
    fn new(entry: &str) -> Self {
        Self {
            recipe: Recipe {
                entry: identity(entry),
                address: ContentHash::default(),
                groups: vec![],
                bodies: vec![],
                foreign_modules: vec![],
                foreign_archives: vec![],
                launch: Launch {
                    backend: Backend::Cranelift,
                    target: TARGET.into(),
                    root_limit: 1024,
                    n_place: 0,
                    exit_status: false,
                    grants: vec![],
                },
            },
            objects: BTreeMap::new(),
        }
    }

    fn leaf() -> Self {
        let mut fixture = Self::new("entry");
        let entry = fixture.body("entry", "()", "", "return");
        fixture.recipe.address = fixture.group(&[(entry, vec![])], 0);
        fixture
    }

    fn object(&mut self, kind: ObjectKind, bytes: Vec<u8>) -> ObjectRef {
        let hash = kind.hash(&bytes);
        let len = bytes.len() as u64;
        self.objects.insert(ObjectKey { kind, hash }, bytes);
        ObjectRef { hash, len }
    }

    fn body(&mut self, name: &str, params: &str, externs: &str, code: &str) -> BodyRef {
        let block_params = if params == "(u64)" { "(v0: u64)" } else { "" };
        let module = hirsel_core_ir::text::parse(&format!(
            "core v1\nsig g0 = {params} -> ()\ninst n0 = app@1/main/{name} [] []\n             fn f0 = n0 g0 {{\nb0{block_params}:\n{code}\n}}\n{externs}"
        ))
        .unwrap();
        let sources = hirsel_core_ir::source::SourceMap::default();
        let (body, mut annex) = normal::encode_with_annex(&module, FuncId(0), &sources.positions());
        if identity(name) == self.recipe.entry {
            annex.1 = Some(if params == "(u64)" {
                vec!["f".into()]
            } else {
                vec![]
            });
        }
        let row = BodyRef {
            body: self.object(ObjectKind::Body, body.bytes),
            annex: self.object(ObjectKind::Annex, annex.encode()),
        };
        self.recipe.bodies.push(row);
        self.recipe.bodies.sort_by_key(|row| row.body.hash);
        row
    }

    fn group(&mut self, rows: &[(BodyRef, Vec<Binding>)], ordinal: u32) -> ContentHash {
        let group = Group {
            members: rows
                .iter()
                .map(|(body, slots)| Member {
                    body: body.body.hash,
                    slots: slots.clone(),
                })
                .collect(),
        };
        let object = self.object(ObjectKind::Group, group.encode());
        self.recipe.groups.push(object);
        self.recipe.groups.sort_by_key(|row| row.hash);
        Group::member_address(object.hash, ordinal)
    }

    fn send(&self, peer: &mut UnixStream) {
        let bytes = self.recipe.encode();
        request(peer, Recipe::hash(&bytes), &bytes);
        if let Some(need) = read_need(peer) {
            self.send_objects(peer, &need);
        }
    }

    fn send_objects(&self, peer: &mut UnixStream, need: &[ObjectKey]) {
        for key in need {
            send_object(peer, *key, &self.objects[key]);
        }
        peer.write_all(&[0]).unwrap();
    }
}

fn identity(name: &str) -> Identity {
    Identity::plain(DefPath::from_full(&format!("app@1/main/{name}")).unwrap())
}

fn uses(name: &str) -> String {
    format!("inst n1 = app@1/main/{name} [] []\nfn f1 = n1 g0 extern\n")
}

fn request(peer: &mut UnixStream, root: ContentHash, recipe: &[u8]) {
    let mut frame = root.0.to_vec();
    frame.extend_from_slice(&(recipe.len() as u64).to_le_bytes());
    frame.extend_from_slice(recipe);
    peer.write_all(&frame).unwrap();
}

fn read_need(peer: &mut UnixStream) -> Option<Vec<ObjectKey>> {
    fn read<const N: usize>(peer: &mut UnixStream) -> [u8; N] {
        let mut bytes = [0; N];
        peer.read_exact(&mut bytes).unwrap();
        bytes
    }
    let mut count = [0; 8];
    match peer.read(&mut count[..1]) {
        Ok(0) => return None,
        Err(error) if error.kind() == io::ErrorKind::ConnectionReset => return None,
        result => {
            result.unwrap();
        }
    }
    peer.read_exact(&mut count[1..]).unwrap();
    Some(
        (0..u64::from_le_bytes(count))
            .map(|_| ObjectKey {
                kind: match read::<1>(peer)[0] {
                    1 => ObjectKind::Group,
                    2 => ObjectKind::Body,
                    3 => ObjectKind::Annex,
                    4 => ObjectKind::ForeignArchive,
                    tag => panic!("unexpected Need tag {tag}"),
                },
                hash: ContentHash(read(peer)),
            })
            .collect(),
    )
}

fn object_header(key: ObjectKey) -> Vec<u8> {
    let mut bytes = vec![match key.kind {
        ObjectKind::Group => 1,
        ObjectKind::Body => 2,
        ObjectKind::Annex => 3,
        ObjectKind::ForeignArchive => 4,
        ObjectKind::Recipe => panic!("recipe object frame"),
    }];
    bytes.extend_from_slice(&key.hash.0);
    bytes
}

fn send_object(peer: &mut UnixStream, key: ObjectKey, bytes: &[u8]) {
    let mut frame = object_header(key);
    frame.extend_from_slice(&(bytes.len() as u64).to_le_bytes());
    frame.extend_from_slice(bytes);
    peer.write_all(&frame).unwrap();
}

fn start(peer: &mut UnixStream, fixture: &Fixture) -> Vec<ObjectKey> {
    let raw = fixture.recipe.encode();
    request(peer, Recipe::hash(&raw), &raw);
    read_need(peer).expect("one Need")
}

#[track_caller]
fn failure(result: Result<AdmittedRelease<'_>, AdmissionError>) -> AdmissionError {
    result.err().expect("admission must refuse before assembly")
}

#[test]
fn foreign_archive_objects_follow_grant_inventory_and_raw_pins() {
    use hirsel_core_ir::release_recipe::{
        ForeignFunction, ForeignLeaf, ForeignTarget, GrantBinding, GrantKind,
    };
    use sha2::{Digest, Sha256};

    let harness = Harness::new(LARGE);
    let local = tempfile::tempdir().unwrap();
    let store = CodeStore::new(local.path());
    let archive = store.put(ObjectKind::ForeignArchive, b"abc").unwrap();
    assert_eq!(
        crate::call::hex(&archive.hash.0),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
    let mut fixture = Fixture::new("entry");
    let body = fixture.body("entry", "(u64)", "", "return");
    fixture.recipe.address = fixture.group(&[(body, vec![])], 0);
    fixture.recipe.launch.grants.push(GrantBinding {
        ordinal: 0,
        kind: GrantKind::Foreign,
        values: vec!["ffi_laws@1/laws/Laws".into()],
        archive: Some(archive),
    });
    let admit = |fixture: &Fixture, archive_bytes: &[u8]| {
        harness.run(limits(), &NativeSemantics, |peer| {
            for key in start(peer, fixture) {
                let bytes = if key.kind == ObjectKind::ForeignArchive {
                    archive_bytes
                } else {
                    &fixture.objects[&key]
                };
                send_object(peer, key, bytes);
            }
            peer.write_all(&[0]).unwrap();
        })
    };
    assert!(
        matches!(admit(&fixture, b"abc"), Err(AdmissionError::Missing(hash)) if hash == archive.hash)
    );

    fixture.recipe.foreign_archives.push(archive);
    assert!(
        matches!(admit(&fixture, b"abd"), Err(AdmissionError::Hash(hash)) if hash == archive.hash)
    );
    assert!(admit(&fixture, b"abc").is_ok());

    fixture.recipe.launch.grants.clear();
    assert!(
        matches!(admit(&fixture, b"abc"), Err(AdmissionError::Extra(hash)) if hash == archive.hash)
    );

    // The address binds the full table even when the selected archive is unchanged.
    let grant = GrantBinding {
        ordinal: 0,
        kind: GrantKind::Foreign,
        values: vec!["app@1/main/I".into()],
        archive: Some(archive),
    };
    let header = "(core@1/core/Foreign[app@1/main/I])->()";
    let definition = DefPath::from_full("app@1/native/f").unwrap();
    let leaf = ForeignLeaf {
        module: DefPath::from_full("app@1/native").unwrap(),
        symbols: "hfl_".into(),
        targets: vec![ForeignTarget {
            target: TARGET.into(),
            archive,
        }],
        functions: vec![ForeignFunction {
            identity: Identity::plain(definition),
            header: header.into(),
        }],
    };
    let mut table = b"\x18x86_64-unknown-linux-gnu".to_vec();
    table.extend_from_slice(&archive.hash.0);
    table.push(3);
    let mut address_frame =
        b"\x0eapp@1/native/f\0\0\x27(core@1/core/Foreign[app@1/main/I])->()\x05hfl_f".to_vec();
    address_frame.extend_from_slice(&Sha256::digest(table));
    let address = ContentHash::of_parts("hirsel.code-foreign.v1", [address_frame.as_slice()]);

    fixture.recipe.launch.grants.push(grant.clone());
    fixture.recipe.foreign_modules.push(leaf.clone());
    assert!(matches!(admit(&fixture, b"abc"), Err(AdmissionError::Extra(hash)) if hash == address));

    let mut fixture = Fixture::new("entry");
    fixture.recipe.launch.grants.push(grant);
    fixture.recipe.foreign_modules.push(leaf.clone());
    fixture.recipe.foreign_archives.push(archive);
    let externs = "inst n1 = app@1/native/f [] []\nfn f1 = n1 g0 extern\n";
    let body = fixture.body("entry", "(u64)", externs, "call f1 g0 (v0)\nreturn");
    fixture.recipe.address = fixture.group(&[(body, vec![Binding::External(address)])], 0);
    let error = failure(admit(&fixture, b"abc")).to_string();
    assert!(error.ends_with("is not an indexed ar archive"), "{error}");

    // An exact FFI-9 address and matching identity cannot substitute for the
    // slot's machine signature. A recipe may otherwise make the shared bridge
    // return through a different register class than its caller expects.
    let mut wrong_abi = Fixture::new("entry");
    wrong_abi.recipe.launch.grants = fixture.recipe.launch.grants.clone();
    wrong_abi.recipe.foreign_modules.push(leaf);
    wrong_abi.recipe.foreign_archives.push(archive);
    let body = wrong_abi.body(
        "entry",
        "(u64)",
        "sig g1 = (u64) -> (f64)\ninst n1 = app@1/native/f [] []\nfn f1 = n1 g1 extern\n",
        "v1: f64 = call f1 g1 (v0)\nreturn",
    );
    wrong_abi.recipe.address = wrong_abi.group(&[(body, vec![Binding::External(address)])], 0);
    assert!(matches!(
        admit(&wrong_abi, b"abc"),
        Err(AdmissionError::Binding(
            "slot signature differs from its bound foreign header"
        ))
    ));

    fixture.recipe.foreign_modules[0].targets.insert(
        0,
        ForeignTarget {
            target: "aarch64-unknown-linux-gnu".into(),
            archive: ObjectRef {
                hash: ContentHash(Sha256::digest(b"").into()),
                len: 0,
            },
        },
    );
    assert!(
        matches!(admit(&fixture, b"abc"), Err(AdmissionError::Missing(hash)) if hash == address)
    );
}

// DESIGN §6.2 M1: the attested hash covers the exact bytes in every object kind.
#[test]
fn m1_hashes_every_frame() {
    let harness = Harness::new(LARGE);
    let fixture = Fixture::leaf();
    let mut raw = fixture.recipe.encode();
    let root = Recipe::hash(&raw);
    *raw.last_mut().unwrap() ^= 1;
    let error = failure(harness.run(limits(), &NativeSemantics, |peer| {
        request(peer, root, &raw);
    }));
    assert!(
        matches!(error, AdmissionError::Hash(hash) if hash == root),
        "M1 recipe hash: {error:?}"
    );

    for &changed in fixture.objects.keys() {
        let error = failure(harness.run(limits(), &NativeSemantics, |peer| {
            let need = start(peer, &fixture);
            for key in need.into_iter().take_while(|key| *key != changed) {
                send_object(peer, key, &fixture.objects[&key]);
            }
            let mut bytes = fixture.objects[&changed].clone();
            *bytes.last_mut().unwrap() ^= 1;
            send_object(peer, changed, &bytes);
        }));
        assert!(
            matches!(error, AdmissionError::Hash(hash) if hash == changed.hash),
            "M1 object hash: {error:?}"
        );
    }

    let body = fixture.recipe.bodies[0].body;
    let mut bad = fixture.objects[&ObjectKey {
        kind: ObjectKind::Body,
        hash: body.hash,
    }]
        .clone();
    *bad.last_mut().unwrap() ^= 1;
    let store = CodeStore::new(harness.root());
    let path = store.path(ObjectKind::Body, body.hash);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, bad).unwrap();
    let error = failure(harness.admit(&fixture));
    assert!(
        matches!(error, AdmissionError::Hash(hash) if hash == body.hash),
        "M1 cached bytes hash: {error:?}"
    );
}

// DESIGN §6.2 M2: a recipe has exactly the entry's typed closure.
#[test]
fn m2_requires_exact_closure() {
    let harness = Harness::new(LARGE);
    let fixture = Fixture::leaf();
    for kind in [ObjectKind::Body, ObjectKind::Group] {
        let error = failure(harness.run(limits(), &NativeSemantics, |peer| {
            let need = start(peer, &fixture);
            let need: Vec<_> = need.into_iter().filter(|key| key.kind != kind).collect();
            fixture.send_objects(peer, &need);
        }));
        let missing = fixture
            .objects
            .keys()
            .find(|key| key.kind == kind)
            .unwrap()
            .hash;
        assert!(
            matches!(error, AdmissionError::Missing(hash) if hash == missing),
            "M2 withheld object: {error:?}"
        );
    }

    let good = harness.admit(&fixture).unwrap();
    assert_eq!(good.release().groups().len(), 1);
    assert_eq!(good.release().bodies().len(), 1);
    assert_eq!(good.staged_objects().count(), 3);
    drop(good);

    let mut fixture = Fixture::leaf();
    let extra_body = fixture.body("spare", "()", "", "return");
    let error = failure(harness.admit(&fixture));
    assert!(
        matches!(error, AdmissionError::Extra(hash) if hash == extra_body.body.hash),
        "M2 unreachable body refused: {error:?}"
    );
    fixture.group(&[(extra_body, vec![])], 0);
    let error = failure(harness.admit(&fixture));
    assert!(
        matches!(error, AdmissionError::Extra(_)),
        "M2 unreachable group refused: {error:?}"
    );

    let mut fixture = Fixture::new("entry");
    let root = fixture.body("entry", "()", &uses("f"), "call f1 g0 ()\nreturn");
    let missing = ContentHash([7; 32]);
    fixture.recipe.address = fixture.group(&[(root, vec![Binding::External(missing)])], 0);
    let error = failure(harness.admit(&fixture));
    assert!(
        matches!(error, AdmissionError::Missing(hash) if hash == missing),
        "M2 unlisted reachable address: {error:?}"
    );
}

// DESIGN §6.2 M3: content hashes cannot attest an identity binding.
#[test]
fn m3_checks_bound_identity() {
    let harness = Harness::new(LARGE);
    let mut fixture = Fixture::new("entry");
    let wrong = fixture.body("g", "()", "", "return");
    let wrong = fixture.group(&[(wrong, vec![])], 0);
    let entry = fixture.body("entry", "()", &uses("f"), "call f1 g0 ()\nreturn");
    fixture.recipe.address = fixture.group(&[(entry, vec![Binding::External(wrong)])], 0);
    let error = failure(harness.admit(&fixture));
    assert!(
        matches!(
            error,
            AdmissionError::Binding("slot identity differs from its bound member")
        ),
        "M3 slot identity agrees with target: {error:?}"
    );
}

// DESIGN §6.2 M4: group order and membership follow the canonical SCC partition.
#[test]
fn m4_requires_canonical_groups() {
    let harness = Harness::new(LARGE);
    let mut fixture = Fixture::new("g");
    let f = fixture.body("f", "()", &uses("g"), "call f1 g0 ()\nreturn");
    let g = fixture.body("g", "()", &uses("f"), "call f1 g0 ()\nreturn");
    fixture.recipe.address = fixture.group(
        &[
            (g, vec![Binding::Internal(1)]),
            (f, vec![Binding::Internal(0)]),
        ],
        0,
    );
    let error = failure(harness.admit(&fixture));
    assert!(
        matches!(
            error,
            AdmissionError::Binding("group members are not in ascending identity order")
        ),
        "M4 ascending member identities: {error:?}"
    );

    let mut fixture = Fixture::new("f");
    let f = fixture.body("f", "()", &uses("g"), "call f1 g0 ()\nreturn");
    let g = fixture.body("g", "()", "", "return");
    fixture.recipe.address = fixture.group(&[(f, vec![Binding::Internal(1)]), (g, vec![])], 0);
    let error = failure(harness.admit(&fixture));
    assert!(
        matches!(
            error,
            AdmissionError::Binding("group is not strongly connected")
        ),
        "M4 group is one SCC: {error:?}"
    );

    let mut fixture = Fixture::new("f");
    let f = fixture.body("f", "()", &uses("g"), "call f1 g0 ()\nreturn");
    let g = fixture.body("g", "()", &uses("f"), "call f1 g0 ()\nreturn");
    fixture.recipe.address = fixture.group(
        &[
            (f, vec![Binding::Internal(1)]),
            (g, vec![Binding::Internal(0)]),
        ],
        0,
    );
    assert_eq!(
        harness.admit(&fixture).unwrap().release().groups()[0]
            .members
            .len(),
        2
    );
}

// DESIGN §6.2 M5: the whole program has one address and one ABI per identity.
#[test]
fn m5_requires_coherent_identities_and_signatures() {
    let harness = Harness::new(LARGE);
    let mut fixture = Fixture::new("entry");
    let f1 = fixture.body("f", "()", "", "v0: u64 = const 1\nreturn");
    let f2 = fixture.body("f", "()", "", "v0: u64 = const 2\nreturn");
    let f1 = fixture.group(&[(f1, vec![])], 0);
    let f2 = fixture.group(&[(f2, vec![])], 0);
    let g = fixture.body("g", "()", &uses("f"), "call f1 g0 ()\nreturn");
    let g = fixture.group(&[(g, vec![Binding::External(f2)])], 0);
    let entry = fixture.body(
        "entry",
        "()",
        &(uses("f") + "inst n2 = app@1/main/g [] []\nfn f2 = n2 g0 extern\n"),
        "call f1 g0 ()\ncall f2 g0 ()\nreturn",
    );
    fixture.recipe.address = fixture.group(
        &[(entry, vec![Binding::External(f1), Binding::External(g)])],
        0,
    );
    let error = failure(harness.admit(&fixture));
    assert!(
        matches!(error, AdmissionError::Conflict(ref id) if **id == identity("f")),
        "M5 one address per full identity: {error:?}"
    );

    let mut fixture = Fixture::new("entry");
    let f = fixture.body("f", "(u64)", "", "return");
    let f = fixture.group(&[(f, vec![])], 0);
    let entry = fixture.body("entry", "()", &uses("f"), "call f1 g0 ()\nreturn");
    fixture.recipe.address = fixture.group(&[(entry, vec![Binding::External(f)])], 0);
    let error = failure(harness.admit(&fixture));
    assert!(
        matches!(error, AdmissionError::Conflict(ref id) if **id == identity("f")),
        "M5 slot ABI agrees with target: {error:?}"
    );
}

fn annex(fixture: &Fixture, row: BodyRef) -> Annex {
    let mut budget = DecodeBudget::unlimited();
    let raw = &fixture.objects[&ObjectKey {
        kind: ObjectKind::Body,
        hash: row.body.hash,
    }];
    let decoded = normal::decode(raw, &mut budget).unwrap();
    let raw = &fixture.objects[&ObjectKey {
        kind: ObjectKind::Annex,
        hash: row.annex.hash,
    }];
    Annex::decode(raw, &decoded.module, &mut budget).unwrap()
}

// DESIGN §6.2 M6: malformed bytes are refused before map normalization.
#[test]
fn m6_requires_canonical_object_frames() {
    let harness = Harness::new(LARGE);
    let mut fixture = Fixture::leaf();
    let row = fixture.recipe.bodies[0];
    let mut other = annex(&fixture, row);
    other.0[0].as_mut().unwrap().terminators[0] = Some(Site {
        file: "other".into(),
        line: 1,
        column: 1,
    });
    let second = fixture.object(ObjectKind::Annex, other.encode());
    let mut duplicate = fixture.recipe.clone();
    duplicate.bodies.push(BodyRef {
        body: row.body,
        annex: second,
    });
    let mut uleb = fixture.recipe.encode();
    // The first identity text starts immediately after the recipe headers.
    let len = b"HIRRECIP\0\x02\x00\x02\x00".len();
    uleb.splice(len..=len, [uleb[len] | 128, 0]);
    let mut trailing = fixture.recipe.encode();
    trailing.push(0);
    for (label, raw) in [
        ("duplicate body row with a second annex", duplicate.encode()),
        ("non-minimal ULEB", uleb),
        ("trailing recipe byte", trailing),
    ] {
        let error = failure(harness.run(limits(), &NativeSemantics, |peer| {
            request(peer, Recipe::hash(&raw), &raw);
            peer.shutdown(Shutdown::Write).unwrap();
        }));
        assert!(
            matches!(error, AdmissionError::Malformed(_)),
            "M6 {label}: {error:?}"
        );
    }
    other.0.push(None);
    fixture.recipe.bodies[0].annex = fixture.object(ObjectKind::Annex, other.encode());
    let error = failure(harness.admit(&fixture));
    assert!(
        matches!(error, AdmissionError::Malformed(_)),
        "M6 annex entries match body: {error:?}"
    );
}

struct TestSemantics {
    version: u32,
    disabled: Option<EngineTag>,
}

impl BackendSemantics for TestSemantics {
    fn supports_target(&self, backend: Backend, target: &str) -> bool {
        NativeSemantics.supports_target(backend, target)
    }

    fn supports_builtin(&self, backend: Backend, id: &Identity, version: u32) -> bool {
        backend == Backend::Cranelift
            && id == &Identity::plain(DefPath::core("float.widen"))
            && version == self.version
    }

    fn supports_tag(&self, backend: Backend, tag: EngineTag) -> bool {
        self.disabled != Some(tag) && NativeSemantics.supports_tag(backend, tag)
    }
}

// DESIGN §6.2 M7: a backend running a newer implementation must refuse the old number.
#[test]
fn m7_requires_exact_backend_semantics() {
    let harness = Harness::new(LARGE);
    let semantics = TestSemantics {
        version: 2,
        disabled: None,
    };
    for version in [1, 2] {
        let mut fixture = Fixture::new("entry");
        let body = fixture.body("entry", "()", "", "builtin_use core@1/float/widen\nreturn");
        fixture.recipe.address = fixture.group(&[(body, vec![Binding::Builtin(version)])], 0);
        let result = harness.run(limits(), &semantics, |peer| fixture.send(peer));
        if version == 1 {
            let error = failure(result);
            assert!(
                matches!(
                    error,
                    AdmissionError::Unsupported("builtin identity and version")
                ),
                "M7 exact builtin version required: {error:?}"
            );
        } else {
            assert_eq!(result.unwrap().release().groups().len(), 1);
        }
    }

    let fixture = Fixture::leaf();
    let semantics = TestSemantics {
        version: 1,
        disabled: Some(EngineTag::Terminator(3)),
    };
    let error = failure(harness.run(limits(), &semantics, |peer| fixture.send(peer)));
    assert!(
        matches!(error, AdmissionError::Unsupported("engine operation tag")),
        "M7 exact engine tag required: {error:?}"
    );

    for backend in [Backend::Cranelift, Backend::Llvm] {
        let mut fixture = Fixture::new("entry");
        fixture.recipe.launch.backend = backend;
        let body = fixture.body(
            "entry",
            "()",
            "sig g1 = (u64) -> (u64)",
            "v0: u64 = const 7\nv1: u64 = call rt.map_hash_word g1 (v0)\nreturn",
        );
        fixture.recipe.address = fixture.group(&[(body, vec![])], 0);
        assert_eq!(fixture.recipe.bodies.len(), 1);
        let release = harness.admit(&fixture).unwrap();
        assert_eq!(release.release().recipe(), &fixture.recipe);
    }
}

#[test]
fn a_current_recipe_cannot_admit_an_old_normal_form_body() {
    let harness = Harness::new(LARGE);
    let mut fixture = Fixture::new("entry");
    let callee = fixture.body("old", "()", "", "return");
    let mut old = fixture
        .objects
        .remove(&ObjectKey {
            kind: ObjectKind::Body,
            hash: callee.body.hash,
        })
        .unwrap();
    old[..10].copy_from_slice(b"HIRBODY\0\x01\0");
    fixture.recipe.bodies.clear();
    let old = BodyRef {
        body: fixture.object(ObjectKind::Body, old),
        annex: callee.annex,
    };
    fixture.recipe.bodies.push(old);
    let callee = fixture.group(&[(old, vec![])], 0);
    let entry = fixture.body("entry", "()", &uses("old"), "call f1 g0 ()\nreturn");
    fixture.recipe.address = fixture.group(&[(entry, vec![Binding::External(callee)])], 0);
    assert!(matches!(
        failure(harness.admit(&fixture)),
        AdmissionError::Unsupported("code frame version")
    ));

    // A retained executable can still be launched with its admitted envelope.
    // The same old recipe version is refused at a new admission.
    let mut prior = fixture.recipe.encode();
    prior[11..13].copy_from_slice(&1u16.to_le_bytes());
    let error = failure(harness.run(limits(), &NativeSemantics, |peer| {
        request(peer, Recipe::hash(&prior), &prior);
        assert!(read_need(peer).is_none());
    }));
    assert!(matches!(error, AdmissionError::Unsupported(reason) if reason.contains("Republish")));
    assert_eq!(
        Recipe::decode_retained(&prior, &mut DecodeBudget::unlimited()).unwrap(),
        fixture.recipe
    );
}

fn refused_before_need(
    harness: &Harness,
    fixture: &Fixture,
    limits: AdmissionLimits,
    expected: Limit,
) {
    let error = failure(harness.run(limits, &NativeSemantics, |peer| {
        let recipe = fixture.recipe.encode();
        request(peer, Recipe::hash(&recipe), &recipe);
        assert!(read_need(peer).is_none(), "M8 bounds enforced before Need");
    }));
    assert!(
        matches!(error, AdmissionError::Limit(limit) if limit == expected),
        "M8 named limit: {error:?}"
    );
}

// DESIGN §6.2 M8: bounded transfer and release after refusal, disconnect and restart.
#[test]
fn m8_bounds_and_releases_every_reservation() {
    let fixture = Fixture::leaf();
    let body = fixture.recipe.bodies[0].body;
    let harness = Harness::new(LARGE);
    let pin = harness.cache(&fixture, ObjectKind::Body, body);
    for (limits, expected) in [
        (
            AdmissionLimits {
                recipe_bytes: 1,
                ..limits()
            },
            Limit::RecipeBytes,
        ),
        (
            AdmissionLimits {
                closure_objects: 2,
                ..limits()
            },
            Limit::ClosureObjects,
        ),
        (
            AdmissionLimits {
                object_bytes: body.len,
                ..limits()
            },
            Limit::ObjectBytes,
        ),
        (
            AdmissionLimits {
                outstanding: 1,
                ..limits()
            },
            Limit::Outstanding,
        ),
        (
            AdmissionLimits {
                decoded_bytes: 0,
                ..limits()
            },
            Limit::DecodeBytes,
        ),
    ] {
        refused_before_need(&harness, &fixture, limits, expected);
    }
    harness.assert_released(pin);

    let harness = Harness::new(1);
    let pin = harness.cache(&fixture, ObjectKind::Body, body);
    refused_before_need(&harness, &fixture, limits(), Limit::StagingBytes);
    harness.assert_released(pin);

    let bytes: u64 = fixture
        .objects
        .values()
        .map(|object| object.len() as u64)
        .sum();
    let harness = Harness::new(bytes);
    let active = harness
        .run(limits(), &NativeSemantics, |peer| {
            let needed = start(peer, &fixture);
            refused_before_need(&harness, &fixture, limits(), Limit::StagingBytes);
            fixture.send_objects(peer, &needed);
        })
        .unwrap();
    drop(active);
    assert_staging_empty(&harness.root());

    let pin = harness.cache(&fixture, ObjectKind::Body, body);
    let error = failure(harness.run(
        AdmissionLimits {
            timeout: Some(Duration::from_secs(1)),
            ..limits()
        },
        &NativeSemantics,
        |peer| {
            assert!(!start(peer, &fixture).contains(&pin));
            assert!(
                !harness.store.remove_unreserved(pin).unwrap(),
                "live Need pins stored objects"
            );
            assert_eq!(peer.read(&mut [0]).unwrap(), 0);
        },
    ));
    assert!(
        matches!(error, AdmissionError::Limit(Limit::Deadline)),
        "M8 stalled peer refused: {error:?}"
    );
    harness.assert_released(pin);

    for duplicate in [false, true] {
        let pin = harness.cache(&fixture, ObjectKind::Body, body);
        let mut rejected = pin;
        let error = failure(harness.run(limits(), &NativeSemantics, |peer| {
            let needed = start(peer, &fixture);
            if duplicate {
                rejected = needed[0];
                send_object(peer, rejected, &fixture.objects[&rejected]);
            }
            // The peer has sent only a key and remains connected. A refusal
            // must precede any read of this object's length or payload.
            peer.write_all(&object_header(rejected)).unwrap();
            assert_eq!(peer.read(&mut [0]).unwrap(), 0);
        }));
        assert!(
            matches!(error, AdmissionError::Unsolicited(key) if key == rejected),
            "M8 duplicate or unsolicited key refused: {error:?}"
        );
        harness.assert_released(pin);
    }

    let pin = harness.cache(&fixture, ObjectKind::Body, body);
    let error = failure(harness.run(limits(), &NativeSemantics, |peer| {
        let key = start(peer, &fixture)[0];
        let bytes = &fixture.objects[&key];
        let mut frame = object_header(key);
        frame.extend_from_slice(&(bytes.len() as u64).to_le_bytes());
        frame.extend_from_slice(&bytes[..1]);
        peer.write_all(&frame).unwrap();
        peer.shutdown(Shutdown::Write).unwrap();
    }));
    assert!(
        matches!(error, AdmissionError::Disconnected),
        "M8 partial frame disconnect: {error:?}"
    );
    harness.assert_released(pin);

    let (root, pin) = {
        let root = tempfile::tempdir().unwrap();
        let store = AdmissionStore::open(root.path(), bytes).unwrap();
        let pin = ObjectKey {
            hash: body.hash,
            kind: ObjectKind::Body,
        };
        CodeStore::new(root.path())
            .put(pin.kind, &fixture.objects[&pin])
            .unwrap();
        let mut listing = Listing::new();
        listing.insert(pin, ListedObject::new(body.len));
        let (&staged, content) = fixture
            .objects
            .iter()
            .find(|(key, _)| **key != pin)
            .unwrap();
        listing.insert(staged, ListedObject::new(content.len() as u64));
        let reservation = store
            .reserve(
                ContentHash::default(),
                listing,
                1,
                &mut DecodeBudget::unlimited(),
            )
            .unwrap();
        fs::write(reservation.staged_path(staged).unwrap(), &content[..1]).unwrap();
        assert!(!store.remove_unreserved(pin).unwrap());
        assert!(
            AdmissionStore::open(root.path(), bytes).is_err(),
            "a second live owner cannot clear staging"
        );
        // Simulate a process exit that skips the attempt's destructor. The OS
        // releases the owner lock and the only records left are the files.
        std::mem::forget(reservation);
        drop(store);
        (root, pin)
    };
    let restarted = AdmissionStore::open(root.path(), bytes).unwrap();
    assert_staging_empty(root.path());
    assert!(
        restarted.remove_unreserved(pin).unwrap(),
        "collection can remove the pre-restart pin"
    );
    let new_key = ObjectKey {
        kind: ObjectKind::Group,
        hash: ContentHash([1; 32]),
    };
    let mut listing = Listing::new();
    listing.insert(new_key, ListedObject::new(bytes));
    assert!(
        restarted
            .reserve(
                ContentHash::default(),
                listing,
                1,
                &mut DecodeBudget::unlimited()
            )
            .is_ok(),
        "staging bytes can be reserved again"
    );
    assert_staging_empty(root.path());

    let root = tempfile::tempdir().unwrap();
    let socket = root.path().join("partial");
    let control = UnixListener::bind(&socket).unwrap();
    let mut peer = UnixStream::connect(&socket).unwrap();
    let mut connection = ControlConnection::accept(&control).unwrap();
    let mut exchange = Exchange::new(&mut connection, deadline(&limits()).unwrap());
    peer.write_all(b"half").unwrap();
    peer.shutdown(Shutdown::Write).unwrap();
    let mut staged = Vec::new();
    let outcome = exchange.object(new_key, 8, 8, &mut staged);
    assert!(matches!(outcome, Err(AdmissionError::Disconnected)));
    assert_eq!(
        exchange.received_bytes(),
        4,
        "M8 charges actual bytes in a partial frame"
    );
    assert_eq!(staged, b"half");
}

#[test]
fn sender_streams_only_need_in_dependency_order() {
    let mut fixture = Fixture::new("entry");
    let callee = fixture.body("callee", "()", "", "return");
    let callee_address = fixture.group(&[(callee, vec![])], 0);
    let callee_group = fixture.recipe.groups[0];
    let entry = fixture.body("entry", "()", &uses("callee"), "call f1 g0 ()\nreturn");
    fixture.recipe.address = fixture.group(&[(entry, vec![Binding::External(callee_address)])], 0);
    let entry_group = *fixture
        .recipe
        .groups
        .iter()
        .find(|&&group| group != callee_group)
        .unwrap();

    let temp = tempfile::tempdir().unwrap();
    let store = CodeStore::new(temp.path());
    for (&key, bytes) in &fixture.objects {
        store.put(key.kind, bytes).unwrap();
    }
    let raw = fixture.recipe.encode();
    let root = store.put(ObjectKind::Recipe, &raw).unwrap().hash;
    let (mut client, mut server) = UnixStream::pair().unwrap();
    thread::scope(|scope| {
        let sender = scope.spawn(|| send_release(&mut client, &store, root, &limits()));
        let mut request = vec![0; 40 + raw.len()];
        server.read_exact(&mut request).unwrap();
        assert_eq!(&request[..32], &root.0);

        let entry_group = ObjectKey {
            kind: ObjectKind::Group,
            hash: entry_group.hash,
        };
        let callee_group = ObjectKey {
            kind: ObjectKind::Group,
            hash: callee_group.hash,
        };
        let callee_body = ObjectKey {
            kind: ObjectKind::Body,
            hash: callee.body.hash,
        };
        server.write_all(&3_u64.to_le_bytes()).unwrap();
        for key in [entry_group, callee_group, callee_body] {
            server.write_all(&object_header(key)).unwrap();
        }
        for key in [callee_body, callee_group, entry_group] {
            let mut frame = [0; 41];
            server.read_exact(&mut frame).unwrap();
            assert_eq!(&frame[..33], object_header(key));
            let mut body = vec![0; u64::from_le_bytes(frame[33..].try_into().unwrap()) as usize];
            server.read_exact(&mut body).unwrap();
            assert_eq!(body, fixture.objects[&key]);
        }
        let mut end = [1];
        server.read_exact(&mut end).unwrap();
        assert_eq!(end, [0], "only requested keys cross the socket");
        sender.join().unwrap().unwrap();
    });

    let local = admit_local(
        &store,
        root,
        &crate::request::local_admission_limits(),
        &NativeSemantics,
    )
    .unwrap();
    let (verified_root, recipe, groups, bodies) = local.into_parts();
    assert_eq!(verified_root, root);
    assert_eq!(recipe.address, fixture.recipe.address);
    assert_eq!(groups.len(), 2);
    assert_eq!(bodies.len(), 2);

    let harness = Harness::new(LARGE);
    harness.cache(&fixture, ObjectKind::Body, callee.body);
    let admitted = harness
        .run(limits(), &NativeSemantics, |peer| {
            send_release(peer, &store, root, &limits()).unwrap();
        })
        .unwrap();
    let (release, lease) = admitted.into_parts();
    assert_eq!(release.root(), root);
    assert_eq!(release.groups().len(), 2);
    assert!(
        !lease
            .staged_objects()
            .any(|(key, _)| key.hash == callee.body.hash)
    );

    let host_store = CodeStore::new(harness.root());
    host_store
        .put(ObjectKind::Recipe, release.recipe_bytes())
        .unwrap();
    let recipe = ObjectKey {
        kind: ObjectKind::Recipe,
        hash: root,
    };
    assert!(
        !harness.store.remove_unreserved(recipe).unwrap(),
        "publication keeps the recipe pinned"
    );
    drop(lease);
    assert!(
        harness.store.remove_unreserved(recipe).unwrap(),
        "collection can remove the recipe when its lease ends"
    );
}
