use super::{
    AdmissionError, AdmissionLimits, AdmissionStore, ControlConnection, Limit, NativeSemantics,
    ObjectKey, ObjectKind, admit_local,
};
use crate::code_store::CodeStore;
use hirsel_core_ids::{ContentHash, DefPath};
use hirsel_core_ir::address::{Binding, Group, Member};
use hirsel_core_ir::normal;
use hirsel_core_ir::release_recipe::{Backend, BodyRef, Launch, ObjectRef, Recipe};
use hirsel_core_ir::{FuncId, Identity};
use std::collections::BTreeMap;
use std::io::Write;
use std::os::unix::net::{UnixListener, UnixStream};
use std::thread;
use std::time::Duration;

const TARGET: &str = "x86_64-unknown-linux-gnu";
const U8_ERROR_DESCRIPTION: &[u8] = &[0, 0, 0, 0, 1, 6];

fn identity(path: &str) -> Identity {
    Identity::plain(DefPath::from_full(path).unwrap())
}

fn limits() -> AdmissionLimits {
    AdmissionLimits {
        recipe_bytes: 1 << 20,
        closure_objects: 64,
        object_bytes: 1 << 20,
        decoded_bytes: 1 << 20,
        members: 64,
        edges: 64,
        nesting: 32,
        outstanding: 64,
        timeout: Some(Duration::from_secs(10)),
    }
}

/// Build a complete single-group release recipe from parsed CoreIR modules where
/// index 0 is the entry body (`app@1/main/entry`) and every other module provides
/// an external callee bound in the same group.
struct ReleaseFixture {
    recipe: Recipe,
    objects: BTreeMap<ObjectKey, Vec<u8>>,
}

impl ReleaseFixture {
    fn from_modules(entry_path: &str, core_sources: &[(&str, &str)]) -> Self {
        let mut objects = BTreeMap::new();
        let mut put = |kind: ObjectKind, bytes: Vec<u8>| -> ObjectRef {
            let hash = kind.hash(&bytes);
            let len = bytes.len() as u64;
            objects.insert(ObjectKey { kind, hash }, bytes);
            ObjectRef { hash, len }
        };

        let mut bodies: Vec<BodyRef> = Vec::new();
        let mut slot_identities: Vec<Vec<Identity>> = Vec::new();
        let mut names: Vec<Identity> = Vec::new();
        let sources = hirsel_core_ir::source::SourceMap::default();
        let positions = sources.positions();

        for &(def_path, text) in core_sources {
            let module = hirsel_core_ir::text::parse(text).unwrap_or_else(|e| {
                panic!("{def_path}: parse line {}: {}", e.line, e.message);
            });
            let (body, mut annex) = normal::encode_with_annex(&module, FuncId(0), &positions);
            if def_path == entry_path {
                annex.1 = Some(vec![]);
                annex.2 = Some(U8_ERROR_DESCRIPTION.to_vec());
            }
            let decoded =
                normal::decode(&body.bytes, &hirsel_core_ir::decode::Debit::unlimited()).unwrap();
            let mut ids = Vec::new();
            for i in 0..decoded.slots.len() {
                ids.push(
                    decoded
                        .slot_identity(i, &hirsel_core_ir::decode::Debit::unlimited())
                        .unwrap(),
                );
            }
            slot_identities.push(ids);
            names.push(identity(def_path));
            let body_ref = BodyRef {
                body: put(ObjectKind::Body, body.bytes),
                annex: put(ObjectKind::Annex, annex.encode()),
            };
            bodies.push(body_ref);
        }

        // Bind each member's external slots to its sibling group or internal index.
        // Put each helper in its own leaf group so the entry group is a valid SCC.
        let mut groups = Vec::new();
        let mut member_addr: BTreeMap<Identity, ContentHash> = BTreeMap::new();
        for i in (1..bodies.len()).rev() {
            let bindings: Vec<Binding> = slot_identities[i]
                .iter()
                .map(|id| Binding::External(member_addr[id]))
                .collect();
            let group = Group {
                members: vec![Member {
                    body: bodies[i].body.hash,
                    slots: bindings,
                }],
            };
            let g_ref = put(ObjectKind::Group, group.encode());
            groups.push(g_ref);
            member_addr.insert(names[i].clone(), Group::member_address(g_ref.hash, 0));
        }
        let entry_bindings: Vec<Binding> = slot_identities[0]
            .iter()
            .map(|id| Binding::External(member_addr[id]))
            .collect();
        let entry_group = Group {
            members: vec![Member {
                body: bodies[0].body.hash,
                slots: entry_bindings,
            }],
        };
        let entry_group_ref = put(ObjectKind::Group, entry_group.encode());
        groups.push(entry_group_ref);
        groups.sort_by_key(|r| r.hash);

        let mut sorted_bodies = bodies.clone();
        sorted_bodies.sort_by_key(|r| r.body.hash);

        let recipe = Recipe {
            entry: identity(entry_path),
            address: Group::member_address(entry_group_ref.hash, 0),
            groups,
            bodies: sorted_bodies,
            foreign_modules: vec![],
            foreign_archives: vec![],
            toolchain: vec![],
            values: vec![],
            launch: Launch {
                backend: Backend::Cranelift,
                target: TARGET.into(),
                root_limit: 4096,
                n_place: 0,
                exit_status: false,
                grants: vec![],
            },
        };
        Self { recipe, objects }
    }

    fn admit_local(&self, limits: &AdmissionLimits) -> Result<(), AdmissionError> {
        let dir = tempfile::tempdir().unwrap();
        let code = CodeStore::new(dir.path().to_path_buf());
        for (&key, bytes) in &self.objects {
            code.put(key.kind, bytes).unwrap();
        }
        let recipe_bytes = self.recipe.encode();
        let root = code.put(ObjectKind::Recipe, &recipe_bytes).unwrap().hash;
        admit_local(&code, root, limits, &NativeSemantics).map(|_| ())
    }

    fn admit_socket(&self, limits: &AdmissionLimits) -> Result<(), AdmissionError> {
        let dir = tempfile::tempdir().unwrap();
        let control = UnixListener::bind(dir.path().join("control")).unwrap();
        let store = AdmissionStore::open(&dir.path().join("code"), 1 << 20).unwrap();
        let recipe_bytes = self.recipe.encode();
        let root = Recipe::hash(&recipe_bytes);
        thread::scope(|scope| {
            let host = scope.spawn(|| {
                let mut conn = ControlConnection::accept(&control).unwrap();
                store.admit(&mut conn, limits, &NativeSemantics).map(|_| ())
            });
            let mut peer = UnixStream::connect(dir.path().join("control")).unwrap();
            let mut frame = root.0.to_vec();
            frame.extend_from_slice(&(recipe_bytes.len() as u64).to_le_bytes());
            frame.extend_from_slice(&recipe_bytes);
            let _ = peer.write_all(&frame);
            if let Some(need) = read_need(&mut peer) {
                for key in need {
                    let bytes = &self.objects[&key];
                    let tag = match key.kind {
                        ObjectKind::Group => 1u8,
                        ObjectKind::Body => 2,
                        ObjectKind::Annex => 3,
                        ObjectKind::ForeignArchive => 4,
                        ObjectKind::Value => 5,
                        ObjectKind::Recipe => unreachable!(),
                    };
                    let mut header = vec![tag];
                    header.extend_from_slice(&key.hash.0);
                    header.extend_from_slice(&(bytes.len() as u64).to_le_bytes());
                    let _ = peer.write_all(&header);
                    let _ = peer.write_all(bytes);
                }
                let _ = peer.write_all(&[0]);
            }
            host.join().unwrap()
        })
    }
}

fn read_need(peer: &mut UnixStream) -> Option<Vec<ObjectKey>> {
    use std::io::Read;
    let mut count = [0; 8];
    match peer.read(&mut count[..1]) {
        Ok(0) => return None,
        Err(_) => return None,
        Ok(_) => {}
    }
    peer.read_exact(&mut count[1..]).ok()?;
    let mut keys = Vec::new();
    for _ in 0..u64::from_le_bytes(count) {
        let mut tag = [0u8; 1];
        let mut hash = [0u8; 32];
        peer.read_exact(&mut tag).ok()?;
        peer.read_exact(&mut hash).ok()?;
        let kind = match tag[0] {
            1 => ObjectKind::Group,
            2 => ObjectKind::Body,
            3 => ObjectKind::Annex,
            4 => ObjectKind::ForeignArchive,
            5 => ObjectKind::Value,
            _ => return None,
        };
        keys.push(ObjectKey {
            kind,
            hash: ContentHash(hash),
        });
    }
    Some(keys)
}

const HELPER_DROP: &str = r#"core v1
sig g0 = (ptr) -> ()
inst n0 = app@1/main/drop_r [] []
fn f0 = n0 g0 {
b0(v0: ptr):
  return
}
"#;

const HELPER_MAKE: &str = r#"core v1
type t0 = resource "app@1/main/R" drop f1 { id: u64 }
sig g0 = (u64) -> () dest t0
sig g1 = (ptr) -> ()
inst n0 = app@1/main/mk [] []
inst n1 = app@1/main/drop_r [] []
fn f0 = n0 g0 {
b0(v0: ptr, v1: u64):
  return
}
fn f1 = n1 g1 extern
"#;

const HELPER_VIEW: &str = r#"core v1
type t0 = resource "app@1/main/R" drop f1 { id: u64 }
sig g0 = (&view t0) -> () dest view t0 in (0)
sig g1 = (ptr) -> ()
inst n0 = app@1/main/view_of [] []
inst n1 = app@1/main/drop_r [] []
fn f0 = n0 g0 {
b0(v0: ptr, v1: ptr):
  return
}
fn f1 = n1 g1 extern
"#;

const HELPER_ID_OF: &str = r#"core v1
type t0 = resource "app@1/main/R" drop f1 { id: u64 }
sig g0 = (&view t0) -> (u64)
sig g1 = (ptr) -> ()
inst n0 = app@1/main/id_of [] []
inst n1 = app@1/main/drop_r [] []
fn f0 = n0 g0 {
b0(v0: ptr):
  v1: u64 = const 0
  return v1
}
fn f1 = n1 g1 extern
"#;

#[test]
fn admission_rejects_double_drop_before_codegen() {
    let valid_entry = r#"core v1
type t0 = resource "app@1/main/R" drop f2 { id: u64 }
sig g0 = () -> ()
sig g1 = (u64) -> () dest t0
sig g2 = (ptr) -> ()
inst n0 = app@1/main/entry [] []
inst n1 = app@1/main/mk [] []
inst n2 = app@1/main/drop_r [] []
fn f0 = n0 g0 {
  slot s0 = t0
  addr a0 = s0 : t0
b0:
  v0: u64 = const 1
  live s0
  address a0
  call f1 g1 (&a0, v0)
  drop a0 t0
  return
}
fn f1 = n1 g1 extern
fn f2 = n2 g2 extern
"#;
    let double_drop_entry = valid_entry.replace("  drop a0 t0\n", "  drop a0 t0\n  drop a0 t0\n");

    let good = ReleaseFixture::from_modules(
        "app@1/main/entry",
        &[
            ("app@1/main/entry", valid_entry),
            ("app@1/main/mk", HELPER_MAKE),
            ("app@1/main/drop_r", HELPER_DROP),
        ],
    );
    good.admit_local(&limits()).expect("valid release admits");
    good.admit_socket(&limits())
        .expect("valid release admits over socket");

    let bad = ReleaseFixture::from_modules(
        "app@1/main/entry",
        &[
            ("app@1/main/entry", &double_drop_entry),
            ("app@1/main/mk", HELPER_MAKE),
            ("app@1/main/drop_r", HELPER_DROP),
        ],
    );
    match bad.admit_local(&limits()) {
        Err(AdmissionError::InvalidCoreIr(reason)) => {
            assert!(reason.contains("may be dropped already"), "{reason}");
        }
        other => panic!("expected InvalidCoreIr double-drop refusal, got {other:?}"),
    }
    match bad.admit_socket(&limits()) {
        Err(AdmissionError::InvalidCoreIr(reason)) => {
            assert!(reason.contains("may be dropped already"), "{reason}");
        }
        other => panic!("expected socket InvalidCoreIr double-drop refusal, got {other:?}"),
    }
}

#[test]
fn admission_rejects_use_after_drop_before_codegen() {
    let uad_entry = r#"core v1
type t0 = resource "app@1/main/R" drop f3 { id: u64 }
sig g0 = () -> ()
sig g1 = (u64) -> () dest t0
sig g2 = (&view t0) -> (u64)
sig g3 = (ptr) -> ()
inst n0 = app@1/main/entry [] []
inst n1 = app@1/main/mk [] []
inst n2 = app@1/main/id_of [] []
inst n3 = app@1/main/drop_r [] []
fn f0 = n0 g0 {
  slot s0 = t0
  addr a0 = s0 : t0
b0:
  v0: u64 = const 1
  live s0
  address a0
  call f1 g1 (&a0, v0)
  drop a0 t0
  v1: u64 = call f2 g2 (&a0)
  return
}
fn f1 = n1 g1 extern
fn f2 = n2 g2 extern
fn f3 = n3 g3 extern
"#;
    let bad = ReleaseFixture::from_modules(
        "app@1/main/entry",
        &[
            ("app@1/main/entry", uad_entry),
            ("app@1/main/mk", HELPER_MAKE),
            ("app@1/main/id_of", HELPER_ID_OF),
            ("app@1/main/drop_r", HELPER_DROP),
        ],
    );
    match bad.admit_local(&limits()) {
        Err(AdmissionError::InvalidCoreIr(reason)) => {
            assert!(
                reason.contains("is used where it may be dropped"),
                "{reason}"
            );
        }
        other => panic!("expected InvalidCoreIr use-after-drop refusal, got {other:?}"),
    }
    match bad.admit_socket(&limits()) {
        Err(AdmissionError::InvalidCoreIr(reason)) => {
            assert!(
                reason.contains("is used where it may be dropped"),
                "{reason}"
            );
        }
        other => panic!("expected socket InvalidCoreIr use-after-drop refusal, got {other:?}"),
    }
}

#[test]
fn admission_rejects_escaping_borrow_and_dangling_view_before_codegen() {
    // Case A: anchored view used after its anchor is dropped.
    let dangling_view_entry = r#"core v1
type t0 = resource "app@1/main/R" drop f4 { id: u64 }
sig g0 = () -> ()
sig g1 = (u64) -> () dest t0
sig g2 = (&view t0) -> () dest view t0 in (0)
sig g3 = (&view t0) -> (u64)
sig g4 = (ptr) -> ()
inst n0 = app@1/main/entry [] []
inst n1 = app@1/main/mk [] []
inst n2 = app@1/main/view_of [] []
inst n3 = app@1/main/id_of [] []
inst n4 = app@1/main/drop_r [] []
fn f0 = n0 g0 {
  slot s0 = t0
  slot s1 = t0
  addr a0 = s0 : t0
  addr a1 = s1 : t0
b0:
  v0: u64 = const 1
  live s0
  address a0
  call f1 g1 (&a0, v0)
  live s1
  address a1
  call f2 g2 (&a1, &a0)
  drop a0 t0
  v1: u64 = call f3 g3 (&a1)
  return
}
fn f1 = n1 g1 extern
fn f2 = n2 g2 extern
fn f3 = n3 g3 extern
fn f4 = n4 g4 extern
"#;
    let bad_view = ReleaseFixture::from_modules(
        "app@1/main/entry",
        &[
            ("app@1/main/entry", dangling_view_entry),
            ("app@1/main/mk", HELPER_MAKE),
            ("app@1/main/view_of", HELPER_VIEW),
            ("app@1/main/id_of", HELPER_ID_OF),
            ("app@1/main/drop_r", HELPER_DROP),
        ],
    );
    match bad_view.admit_local(&limits()) {
        Err(AdmissionError::InvalidCoreIr(reason)) => {
            assert!(
                reason.contains("s1 is used where it may be dropped"),
                "{reason}"
            );
        }
        other => panic!("expected InvalidCoreIr dangling-view refusal, got {other:?}"),
    }

    // Case B: helper returning a raw pointer into its own local stack slot.
    let leak_ptr_helper = r#"core v1
sig g0 = () -> (ptr)
inst n0 = app@1/main/leak_ptr [] []
fn f0 = n0 g0 {
  slot s0 = u64
  addr a0 = s0 : u64
b0:
  live s0
  address a0
  v0: u64 = const 7
  store a0, v0
  v1: ptr = pointer a0
  return v1
}
"#;
    let caller_leak_ptr = r#"core v1
sig g0 = () -> ()
sig g1 = () -> (ptr)
inst n0 = app@1/main/entry [] []
inst n1 = app@1/main/leak_ptr [] []
fn f0 = n0 g0 {
b0:
  v0: ptr = call f1 g1 ()
  return
}
fn f1 = n1 g1 extern
"#;
    let bad_ptr = ReleaseFixture::from_modules(
        "app@1/main/entry",
        &[
            ("app@1/main/entry", caller_leak_ptr),
            ("app@1/main/leak_ptr", leak_ptr_helper),
        ],
    );
    match bad_ptr.admit_local(&limits()) {
        Err(AdmissionError::InvalidCoreIr(reason)) => {
            assert!(
                reason.contains("returns a pointer into local slot"),
                "{reason}"
            );
        }
        other => panic!("expected InvalidCoreIr escaping-pointer refusal, got {other:?}"),
    }

    // Case C: helper returning a dest view anchored on a local slot.
    let leak_dest_helper = r#"core v1
type t0 = resource "app@1/main/R" drop f3 { id: u64 }
sig g0 = () -> () dest t0
sig g1 = (u64) -> () dest t0
sig g2 = (&view t0) -> () dest view t0 in (0)
sig g3 = (ptr) -> ()
inst n0 = app@1/main/leak_view [] []
inst n1 = app@1/main/mk [] []
inst n2 = app@1/main/view_of [] []
inst n3 = app@1/main/drop_r [] []
fn f0 = n0 g0 {
  slot s0 = t0
  addr a0 = v0 : t0
  addr a1 = s0 : t0
b0(v0: ptr):
  v1: u64 = const 9
  live s0
  address a1
  call f1 g1 (&a1, v1)
  address a0
  call f2 g2 (&a0, &a1)
  return
}
fn f1 = n1 g1 extern
fn f2 = n2 g2 extern
fn f3 = n3 g3 extern
"#;
    let caller_leak_dest = r#"core v1
type t0 = resource "app@1/main/R" drop f2 { id: u64 }
sig g0 = () -> ()
sig g1 = () -> () dest t0
sig g2 = (ptr) -> ()
inst n0 = app@1/main/entry [] []
inst n1 = app@1/main/leak_view [] []
inst n2 = app@1/main/drop_r [] []
fn f0 = n0 g0 {
  slot s0 = t0
  addr a0 = s0 : t0
b0:
  live s0
  address a0
  call f1 g1 (&a0)
  return
}
fn f1 = n1 g1 extern
fn f2 = n2 g2 extern
"#;
    let bad_dest = ReleaseFixture::from_modules(
        "app@1/main/entry",
        &[
            ("app@1/main/entry", caller_leak_dest),
            ("app@1/main/leak_view", leak_dest_helper),
            ("app@1/main/mk", HELPER_MAKE),
            ("app@1/main/view_of", HELPER_VIEW),
            ("app@1/main/drop_r", HELPER_DROP),
        ],
    );
    match bad_dest.admit_local(&limits()) {
        Err(AdmissionError::InvalidCoreIr(reason)) => {
            assert!(
                reason.contains("returns a view anchored on a local slot"),
                "{reason}"
            );
        }
        other => panic!("expected InvalidCoreIr escaping-dest-view refusal, got {other:?}"),
    }
}

#[test]
fn admission_wf_respects_decode_byte_budget() {
    let valid_entry = r#"core v1
type t0 = resource "app@1/main/R" drop f2 { id: u64 }
sig g0 = () -> ()
sig g1 = (u64) -> () dest t0
sig g2 = (ptr) -> ()
inst n0 = app@1/main/entry [] []
inst n1 = app@1/main/mk [] []
inst n2 = app@1/main/drop_r [] []
fn f0 = n0 g0 {
  slot s0 = t0
  addr a0 = s0 : t0
b0:
  v0: u64 = const 1
  live s0
  address a0
  call f1 g1 (&a0, v0)
  drop a0 t0
  return
}
fn f1 = n1 g1 extern
fn f2 = n2 g2 extern
"#;
    let good = ReleaseFixture::from_modules(
        "app@1/main/entry",
        &[
            ("app@1/main/entry", valid_entry),
            ("app@1/main/mk", HELPER_MAKE),
            ("app@1/main/drop_r", HELPER_DROP),
        ],
    );
    let mut tight = limits();
    tight.decoded_bytes = 64;
    assert!(matches!(
        good.admit_local(&tight),
        Err(AdmissionError::Limit(Limit::DecodeBytes))
    ));
}
