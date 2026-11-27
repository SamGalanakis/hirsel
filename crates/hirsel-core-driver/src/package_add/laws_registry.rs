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
        self.hints
            .entry(name.into())
            .or_default()
            .push(json!([version.to_string(), index, size]));
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
        dir.join("config/hirsel/config.toml"),
        route("*", HOST, preview, 3),
    )
    .unwrap();
}

fn snapshot(dir: &Path) -> [Option<Vec<u8>>; 2] {
    ["hirsel.toml", "hirsel.lock"].map(|name| fs::read(dir.join(name)).ok())
}

#[test]
fn g6_cli_named_add_update_audit() {
    let server = Server::start();
    let scratch = tempfile::tempdir().unwrap();
    let dir = scratch.path();
    config(dir, false);
    fs::write(dir.join("hirsel.toml"),"[package]\nname = \"local/app\"\nversion = \"1.0.0\"\ncharge = \"charge-v1\"\n[sources]\n\"main.hirsel\" = \"main\"\n").unwrap();
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
    let manifest = dir.join("hirsel.toml");
    let original = fs::read_to_string(&manifest).unwrap();
    fs::write(&manifest,format!("{original}\nwrap = {{ package = \"fixture.test/wrap\", major = 1, root = \"{wrap}\", registry = \"https://alpha.test\" }}\n")).unwrap();
    let constrained = snapshot(dir);
    run(&["update", "j@0.3.1"], 2);
    assert_eq!(snapshot(dir), constrained);
    fs::write(&manifest, &original).unwrap();
    run(&["update", "j@0.3.1"], 0);
    assert!(
        fs::read_to_string(&manifest)
            .unwrap()
            .contains(&fix.to_string())
    );

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

    config(dir, true);
    let preview = run(&["audit"], 0);
    assert!(preview.contains("preview (quorum 0)"), "{preview}");
}
