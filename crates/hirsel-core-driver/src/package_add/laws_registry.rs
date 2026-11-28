use crate::native::tests::process::{request_command_at, tape_request_output};
use crate::package_content::{Binding, Content, Id, Root, Source};
use crate::package_manifest::name::Version;
use crate::registry_route::laws::{
    Server, auth, bundle, compress, frame_path, hex, merkle, note, route,
};
use crate::registry_verify::notes::Log;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

const HOST: &str = "alpha.test";
const NAME: &str = "fixture.test/json";
const NOW: u64 = 1_000_000;
const WAIT: u64 = 259_200;
const APP: &str = "pub fn main() -> Result[(), Exit] { Ok(()) }\n";
const LIB: &str = "pub fn value() -> u64 { 7u64 }\n";

struct Catalog<'a> {
    server: &'a Server,
    rows: Vec<Vec<u8>>,
    hints: BTreeMap<String, Vec<Value>>,
}

impl Catalog<'_> {
    fn publish(
        &mut self,
        name: &str,
        version: &str,
        source: &str,
        time: u64,
        bindings: BTreeMap<String, (Id, Binding)>,
    ) -> Root {
        let version = Version::parse(version).unwrap();
        let id = Id {
            name: name.into(),
            major: version.major,
        };
        let module = name.rsplit('/').next().unwrap();
        let content = Content {
            id: id.clone(),
            version: version.clone(),
            bindings,
            files: [(
                format!("{module}.hirsel"),
                Source {
                    module: vec![module.into()],
                    bytes: source.as_bytes().to_vec().into(),
                },
            )]
            .into(),
            foreign: BTreeMap::new(),
        };
        let (root, bytes) = content.inventory().unwrap().encode().unwrap();
        self.server
            .serve(HOST, &frame_path(root), compress(&bytes, 1, false));
        let row = json!([
            "release",
            id.to_string(),
            version.to_string(),
            hex(&root.0),
            "fixture",
            null,
            "1",
            "1",
            "aa".repeat(32),
            "bb".repeat(32),
            auth()
        ]);
        let index = self.rows.len();
        self.rows.push(serde_json::to_vec(&row).unwrap());
        let size = self.rows.len();
        self.hints.entry(name.into()).or_default().push(json!([
            version.to_string(),
            index.to_string(),
            size.to_string()
        ]));
        self.server.serve(
            HOST,
            &format!("/versions/{name}"),
            serde_json::to_vec(&self.hints[name]).unwrap(),
        );
        self.server.serve(
            HOST,
            &format!("/log/heads/{size}"),
            note(
                HOST,
                Log::Release,
                size as u64,
                merkle(&self.rows),
                Some(time),
            ),
        );
        serve_log(self.server, Log::Release, &self.rows, true);
        root
    }
}

fn serve_log(server: &Server, log: Log, rows: &[Vec<u8>], entries: bool) {
    let size = rows.len();
    let prefix = log.prefix();
    server.serve(
        HOST,
        &format!("/{prefix}/checkpoint"),
        note(HOST, log, size as u64, merkle(rows), Some(NOW)),
    );
    if size != 0 {
        let hashes = rows
            .iter()
            .flat_map(|row| merkle(std::slice::from_ref(row)))
            .collect();
        server.serve(HOST, &format!("/{prefix}/tile/0/000.p/{size}"), hashes);
        if entries {
            server.serve(
                HOST,
                &format!("/{prefix}/tile/entries/000.p/{size}"),
                bundle(rows),
            );
        }
    }
}

fn invoke(server: &Server, dir: &Path, now: u64, args: &[&str], expected: i32) -> String {
    let mut command: Vec<String> = args.iter().map(ToString::to_string).collect();
    command.extend([
        "--package-store".into(),
        dir.join("packages").display().to_string(),
    ]);
    let mut child = request_command_at(&std::env::current_exe().unwrap(), &command, dir);
    server.route(&mut child, dir);
    child
        .env("XDG_CONFIG_HOME", dir.join("config"))
        .env("XDG_CACHE_HOME", dir.join("cache"))
        .env("HIRSEL_TEST_REGISTRY_SECONDS", now.to_string());
    let (status, out, err) = tape_request_output(child);
    assert_eq!(status, Some(expected), "{args:?}\n{out}\n{err}");
    format!("{out}\n{err}")
}

fn config(dir: &Path, preview: bool) {
    fs::create_dir_all(dir.join("config/hirsel")).unwrap();
    fs::write(
        dir.join("config/hirsel/config.hcfg"),
        route("*", HOST, preview, 3),
    )
    .unwrap();
}

fn snapshot(dir: &Path) -> [Option<Vec<u8>>; 2] {
    ["hirsel.hcfg", "hirsel.lock"].map(|name| fs::read(dir.join(name)).ok())
}

#[test]
fn g6_cli_named_add_update_audit() {
    let server = Server::start();
    let scratch = tempfile::tempdir().unwrap();
    let dir = scratch.path();
    config(dir, false);
    fs::write(dir.join("hirsel.hcfg"),"pkg_v1.Package(name: \"local/app\", version: \"1.0.0\", charge: ChargeV1, sources: [(\"main.hirsel\", \"main\")], dependencies: [])\n").unwrap();
    fs::write(dir.join("main.hirsel"), APP).unwrap();
    let mut catalog = Catalog {
        server: &server,
        rows: vec![],
        hints: BTreeMap::new(),
    };
    let old = catalog.publish(NAME, "0.3.0", LIB, NOW - WAIT + 1, BTreeMap::new());
    let mut status = Vec::new();
    serve_log(&server, Log::Status, &status, true);
    let run = |args: &[&str], status| invoke(&server, dir, NOW, args, status);
    let pristine = snapshot(dir);
    assert!(run(&["add", NAME], 2).contains("no eligible"));
    assert_eq!(snapshot(dir), pristine);
    let add = invoke(&server, dir, NOW + 1, &["add", NAME, "--as", "j"], 0);
    assert!(add.contains(&old.to_string()), "{add}");
    let pinned = snapshot(dir);

    let frozen = ["audit", "--frozen"];
    server.take();
    invoke(&server, dir, NOW + 86_401, &frozen, 2);
    assert_eq!(server.take(), (vec![], 0));
    run(&frozen, 0);
    assert_eq!(server.take(), (vec![], 0));
    let cache = dir.join("packages/registry");
    fs::rename(&cache, dir.join("verified")).unwrap();
    run(&frozen, 2);
    assert_eq!(server.take(), (vec![], 0));
    fs::rename(dir.join("verified"), cache).unwrap();

    let fix = catalog.publish(NAME, "0.3.1", LIB, NOW - 7200, BTreeMap::new());
    // A valid locator and a matching bundle cannot authorize a frame under a
    // signed fork or an expired head, even when their HTTP reads race.
    server.take();
    server.serve(
        HOST,
        "/log/checkpoint",
        note(
            HOST,
            Log::Release,
            1,
            merkle(&catalog.rows[1..2]),
            Some(NOW),
        ),
    );
    assert!(run(&["update", "j@0.3.1"], 2).contains("does not extend"));
    assert!(!server.take().0.iter().any(|p| p.contains("/c/")));
    assert_eq!(snapshot(dir), pinned);
    server.serve(
        HOST,
        "/log/checkpoint",
        note(
            HOST,
            Log::Release,
            2,
            merkle(&catalog.rows),
            Some(NOW - 86_401),
        ),
    );
    assert!(run(&["update", "j@0.3.1"], 2).contains("freshness"));
    assert!(!server.take().0.iter().any(|p| p.contains("/c/")));
    assert_eq!(snapshot(dir), pinned);
    catalog.publish(NAME, "0.4.0", LIB, NOW - 7200, BTreeMap::new());
    status.push(
        serde_json::to_vec(&json!([
            "retire",
            hex(&old.0),
            null,
            "security",
            "incorrect input validation",
            "23",
            auth()
        ]))
        .unwrap(),
    );
    serve_log(&server, Log::Status, &status, false);
    assert!(run(&["update"], 2).contains("unavailable"));
    assert_eq!(snapshot(dir), pinned);
    serve_log(&server, Log::Status, &status, true);
    assert!(run(&["audit"], 2).contains("retired (security: incorrect input validation)"));
    let error = run(&["update"], 2);
    let fix_hex = hex(&fix.0);
    assert!(
        !dir.join("packages")
            .join(&fix_hex[..2])
            .join(&fix_hex[2..])
            .exists(),
        "refused young frame was installed"
    );
    assert!(error.contains("j 0.3.0 is retired (security: incorrect input validation).\nj 0.3.1 was published 2 h ago, inside the 72 h wait.\nTo take it now: hirsel update j@0.3.1"),"{error}");
    assert_eq!(snapshot(dir), pinned);
    run(&["update", "j@0.4.0"], 2);
    assert_eq!(snapshot(dir), pinned);

    // The second member binding constrains json to the exact old root.
    let edges = [(
        "json".into(),
        (
            Id {
                name: NAME.into(),
                major: 0,
            },
            Binding::Exact(old),
        ),
    )]
    .into();
    let wrap = catalog.publish("fixture.test/wrap", "1.0.0", LIB, NOW - WAIT, edges);
    let manifest = dir.join("hirsel.hcfg");
    let original = fs::read_to_string(&manifest).unwrap();
    let constrained_manifest = original.replace("dependencies: [",&format!("dependencies: [(\"wrap\", Published(package: \"fixture.test/wrap\", major: 1, root: \"{wrap}\", locator: Registry(\"https://alpha.test\"), authority: [])), "));
    fs::write(&manifest, constrained_manifest).unwrap();
    let constrained = snapshot(dir);
    let rejected = run(&["update", "j@0.3.1"], 2);
    assert!(
        rejected.contains("no eligible registry release"),
        "{rejected}"
    );
    assert_eq!(snapshot(dir), constrained);
    fs::write(&manifest, &original).unwrap();
    run(&["update", "j@0.3.1"], 0);
    assert!(
        fs::read_to_string(&manifest)
            .unwrap()
            .contains(&fix.to_string())
    );

    run(&["update"], 0);

    let osv = "OSV-HIRSEL-123";
    let mut document = json!({"affected":[{"package":{"ecosystem":"Hirsel","name":NAME},"versions":["0.3.1"]}],
        "aliases":["CVE-123"],"id":osv,"modified":"2026-11-22T00:00:00Z","severity":[{"score":"high","type":"fixture"}]});
    let mut prior = Value::Null;
    for withdrawn in [false, true] {
        if withdrawn {
            document["withdrawn"] = json!("2026-11-23T00:00:00Z");
            document["modified"] = document["withdrawn"].clone();
        }
        let evidence = serde_json::to_vec(&document).unwrap();
        let digest = hex(&Sha256::digest(&evidence));
        server.serve(HOST, &format!("/e/{digest}"), evidence);
        let entry = serde_json::to_vec(&json!([
            "advise",
            hex(&fix.0),
            prior,
            osv,
            digest,
            "24",
            auth()
        ]))
        .unwrap();
        prior = json!(hex(&merkle(std::slice::from_ref(&entry))));
        status.push(entry);
        serve_log(&server, Log::Status, &status, true);
        let audit = run(&["audit", "--json"], if withdrawn { 0 } else { 2 });
        assert_eq!(audit.contains(osv), !withdrawn, "{audit}");
        if !withdrawn {
            for field in ["CVE-123", "high", "2026-11-22"] {
                assert!(audit.contains(field), "{audit}");
            }
        }
    }

    // A valid replacement removes an export called by the consumer.
    catalog.publish(
        NAME,
        "0.3.3",
        "pub fn other() -> u64 { 42u64 }\n",
        NOW,
        BTreeMap::new(),
    );
    fs::write(
        dir.join("main.hirsel"),
        format!("{APP}pub fn uses() -> u64 {{ j.value() }}\n"),
    )
    .unwrap();
    let before = snapshot(dir);
    assert!(run(&["update", "j@0.3.3"], 2).contains("does not check"));
    assert_eq!(snapshot(dir), before);
    catalog.publish(
        NAME,
        "0.3.4",
        "pub fn value() -> u64 { 7u64 }\npub fn count(e: Env) -> u64 { env.len(e) }\n",
        NOW,
        BTreeMap::new(),
    );
    let denied = run(&["update", "j@0.3.4"], 2);
    assert!(denied.contains("authority"), "{denied}");
    assert_eq!(snapshot(dir), before);

    let pre = catalog.publish(NAME, "1.1.0-rc.1", LIB, NOW - 100, BTreeMap::new());
    let safe = catalog.publish(
        "fixture.test/tools",
        "2.0.0",
        LIB,
        NOW - WAIT,
        BTreeMap::new(),
    );
    catalog.publish(
        "fixture.test/tools",
        "2.0.1",
        "pub fn count(e: Env) -> u64 { env.len(e) }\n",
        NOW - WAIT,
        BTreeMap::new(),
    );
    let add = run(&["add", "fixture.test/tools@2", "--json"], 0);
    assert!(add.contains(&safe.to_string()), "{add}");
    let part = run(&["add", "fixture.test/json@1", "--as", "next"], 2);
    assert!(part.contains("no eligible"), "{part}");
    let exact = run(&["add", "fixture.test/json@1.1.0-rc.1", "--as", "next"], 0);
    assert!(exact.contains(&pre.to_string()), "{exact}");

    run(&["update", "next"], 0);

    // Only a transitive floor names this identity. The requested release has
    // never been a manifest row or the tested root of an edge.
    let child_name = "fixture.test/child";
    let tested = catalog.publish(child_name, "0.3.1", LIB, NOW - WAIT, BTreeMap::new());
    let selected = catalog.publish(child_name, "0.3.2", LIB, NOW - 3600, BTreeMap::new());
    catalog.publish(child_name, "0.4.0", LIB, NOW - WAIT, BTreeMap::new());
    let floors = [(
        "child".into(),
        (
            Id {
                name: child_name.into(),
                major: 0,
            },
            Binding::Floor {
                tested,
                minor: 3,
                patch: 1,
            },
        ),
    )]
    .into();
    catalog.publish("fixture.test/reader", "1.0.0", LIB, NOW - WAIT, floors);
    run(&["add", "fixture.test/reader@1.0.0"], 0);
    let retired_child = serde_json::to_vec(&json!([
        "retire",
        hex(&tested.0),
        null,
        "security",
        "replace child",
        "25",
        auth()
    ]))
    .unwrap();
    let predecessor = hex(&merkle(std::slice::from_ref(&retired_child)));
    status.push(retired_child);
    serve_log(&server, Log::Status, &status, true);
    let before = snapshot(dir);
    let refusal = run(&["update", child_name], 2);
    assert!(
        refusal.contains("To take it now: hirsel update fixture.test/child@0.3.2"),
        "{refusal}"
    );
    let young_frame = hex(&selected.0);
    assert!(
        !dir.join("packages")
            .join(&young_frame[..2])
            .join(&young_frame[2..])
            .exists()
    );
    assert_eq!(snapshot(dir), before);
    status.push(
        serde_json::to_vec(&json!([
            "reinstate",
            hex(&tested.0),
            predecessor,
            "reviewed",
            "26",
            auth()
        ]))
        .unwrap(),
    );
    serve_log(&server, Log::Status, &status, true);
    let with_floor = snapshot(dir);
    let crossing = run(&["update", "fixture.test/child@0.4.0"], 2);
    assert!(
        crossing.contains("no eligible registry release"),
        "{crossing}"
    );
    assert_eq!(snapshot(dir), with_floor);
    server.take();
    run(&["update", "fixture.test/child@0.3.2"], 0);
    let (requests, _) = server.take();
    let hints: Vec<_> = requests
        .iter()
        .filter(|path| path.contains("/versions/"))
        .collect();
    assert_eq!(hints, [&format!("{HOST}/versions/{child_name}")]);
    let text = fs::read_to_string(&manifest).unwrap();
    assert!(
        text.contains("selections: Some([") && text.contains(&selected.to_string()),
        "{text}"
    );

    config(dir, true);
    let preview = run(&["audit"], 0);
    let label = "single-operator signed-log wipeable preview (quorum 0), unwitnessed";
    assert!(preview.contains(label), "{preview}");
    let (row, intent) = crate::registry_route::laws::signed_takedown(
        HOST,
        "fixture.test",
        fix,
        prior,
        "fixture removal",
        3,
    );
    server.serve(
        HOST,
        &format!("/e/{}", hex(&Sha256::digest(&intent))),
        intent,
    );
    let head = serde_json::to_vec(&row).unwrap();
    status.push(head);
    serve_log(&server, Log::Status, &status, true);
    let audit = run(&["audit"], 2);
    assert!(
        audit.contains("taken down (legal: fixture removal)"),
        "{audit}"
    );
    let fix_hex = hex(&fix.0);
    let cached = dir.join("packages").join(&fix_hex[..2]).join(&fix_hex[2..]);
    fs::remove_file(&cached).unwrap();
    server.take();
    let refused = run(&["fetch"], 2);
    assert!(refused.contains("taken down"), "{refused}");
    assert!(!cached.exists());
    assert!(
        !server
            .take()
            .0
            .contains(&format!("{HOST}{}", frame_path(fix)))
    );
}

#[test]
fn cold_floor_proofs_reuse_the_authenticated_release_bundle() {
    let server = Server::start();
    let scratch = tempfile::tempdir().unwrap();
    let dir = scratch.path();
    config(dir, false);
    fs::write(dir.join("hirsel.hcfg"), "pkg_v1.Package(name: \"local/app\", version: \"1.0.0\", charge: ChargeV1, sources: [(\"main.hirsel\", \"main\")], dependencies: [])\n").unwrap();
    fs::write(dir.join("main.hirsel"), APP).unwrap();
    let mut catalog = Catalog {
        server: &server,
        rows: vec![],
        hints: BTreeMap::new(),
    };
    let mut bindings = BTreeMap::new();
    for i in 0..24 {
        let alias = format!("child{i}");
        let name = format!("fixture.test/{alias}");
        let root = catalog.publish(&name, "1.0.0", LIB, NOW - WAIT, BTreeMap::new());
        bindings.insert(
            alias,
            (
                Id { name, major: 1 },
                Binding::Floor {
                    tested: root,
                    minor: 0,
                    patch: 0,
                },
            ),
        );
    }
    catalog.publish(NAME, "1.0.0", LIB, NOW - WAIT, bindings);
    serve_log(&server, Log::Status, &[], true);
    invoke(&server, dir, NOW, &["add", "fixture.test/json@1.0.0"], 0);
    let (requests, connections) = server.take();
    let hints: Vec<_> = requests
        .iter()
        .enumerate()
        .filter(|(_, p)| p.contains("/versions/fixture.test/child"))
        .map(|(i, _)| i)
        .collect();
    let frames: Vec<_> = requests
        .iter()
        .enumerate()
        .filter(|(_, p)| p.contains("/c/"))
        .map(|(i, _)| i)
        .collect();
    assert!(
        hints.is_empty(),
        "proved pins performed discovery: {hints:?}"
    );
    assert_eq!(frames.len(), 25);
    assert_eq!(
        requests
            .iter()
            .filter(|p| p.contains("/log/tile/entries/"))
            .count(),
        1
    );
    assert!(
        (2..=crate::package_store::REGISTRY_WORKERS).contains(&connections),
        "{connections} connections"
    );
}

#[test]
fn decimal_hints_skip_skew_and_unrelated_advisory_lag() {
    let server = Server::start();
    let scratch = tempfile::tempdir().unwrap();
    let dir = scratch.path();
    config(dir, false);
    fs::write(dir.join("hirsel.hcfg"), "pkg_v1.Package(name: \"local/app\", version: \"1.0.0\", charge: ChargeV1, sources: [(\"main.hirsel\", \"main\")], dependencies: [])\n").unwrap();
    fs::write(dir.join("main.hirsel"), APP).unwrap();
    let mut catalog = Catalog {
        server: &server,
        rows: vec![],
        hints: BTreeMap::new(),
    };
    let nested = "fixture.test/json/other";
    let root = catalog.publish(nested, "1.0.0", LIB, NOW - WAIT, BTreeMap::new());
    catalog.publish(NAME, "1.0.0", LIB, NOW - WAIT, BTreeMap::new());
    let nested_path = format!("/versions/{nested}");
    server.serve(HOST, &nested_path, b"[]".to_vec());
    let doc = serde_json::to_vec(&json!({"id":"OSV-LAG","modified":"2026-01-01","affected":[{"package":{"name":nested,"ecosystem":"Hirsel"},"versions":["1.0.0"]}]})).unwrap();
    let digest = hex(&Sha256::digest(&doc));
    server.serve(HOST, &format!("/e/{digest}"), doc);
    let rows = [serde_json::to_vec(&json!([
        "advise",
        hex(&root.0),
        null,
        "OSV-LAG",
        digest,
        "24",
        auth()
    ]))
    .unwrap()];
    serve_log(&server, Log::Status, &rows, true);
    server.serve(
        HOST,
        &format!("/versions/{NAME}"),
        br#"[["4.0.0","8","9"],["3.0.0","1","4"],["2.0.0","0","1"],["1.0.0","1","2"]]"#.to_vec(),
    );
    invoke(&server, dir, NOW, &["add", NAME], 0);
    server.serve(HOST, &nested_path, br#"[["1.0.0","0","1"]]"#.to_vec());
    invoke(&server, dir, NOW, &["add", nested], 0);
    let audit = invoke(&server, dir, NOW, &["audit", "--frozen"], 2);
    assert!(audit.contains("OSV-LAG"), "{audit}");
}
