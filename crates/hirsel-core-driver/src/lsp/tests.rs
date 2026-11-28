#[path = "dx_tests.rs"]
mod dx_tests;
#[path = "image_navigation.rs"]
mod image_navigation;

use super::documents::file_uri;
use super::stdio_fixture::{P1_APP, at, compare_cli, p1};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::fs;
use std::io::{BufRead, BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

pub(super) const APP: &str = concat!(
    "pub fn main() -> Result[(), Exit] { Ok(()) }\r\n",
    "fn poisoned(x: Missing) {}\r\n",
    "fn answer() -> u64 { let _ = \"😀\"; dep.value() }\r\n",
    "fn moved(take s: String) -> String { let t = s; s }",
);
const LIB_GOOD: &str = "pub fn value() -> u64 { 42u64 }";
pub(super) const LIB_BAD: &str = "pub fn value() -> bool { true }";
const PIN: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

#[test]
fn stdio_editor_queries() {
    let driver = driver();
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let workspace = root.join("workspace with spaces");
    let app = workspace.join("app/main.hirsel");
    let lib = workspace.join("lib/main.hirsel");
    let cache = root.join("cache");
    let archive = workspace.join("app/archive.hpkg");
    let archive_source = "/// From the verified archive.\npub fn value() -> u64 { 17u64 }\n";
    let pin = package_archive(&root, &archive, archive_source);
    let document = concat!(
        "pub fn main() -> Result[(), Exit] { Ok(()) }\r\n",
        "type Point { x: u64, y: u64 }\r\n",
        "fn shadow() -> u64 { let _ = { let x = false; x }; { let x = dep.value(); x } }\r\n",
        "fn from_archive() -> u64 { published.value() }\r\n",
        "fn unresolved() -> u64 { dep.value(true) }\r\n",
        "fn project(point: Point) -> u64 { point.x }\r\n",
        "fn refused_shadow(x: u64) -> u64 { let x = 1u64; x }\r\n",
        "fn refused() -> Missing {}",
    );
    write(
        &workspace.join("hirsel.toml"),
        "[workspace]\nmembers = [\"app\", \"lib\"]\n",
    );
    write(
        &workspace.join("app/hirsel.toml"),
        &format!(
            concat!(
                "{}\n[dependencies]\ndep = {{ package = \"fixture.test/lib\", major = 1, path = \"../lib\" }}\n",
                "published = {{ package = \"fixture.test/archive\", major = 1, root = \"{}\", archive = \"archive.hpkg\" }}\n",
            ),
            manifest("local/app", "main"),
            pin,
        ),
    );
    write(
        &workspace.join("lib/hirsel.toml"),
        &source_manifest("fixture.test/lib", "main", "lib"),
    );
    write(&app, document);
    write(&lib, LIB_GOOD);
    let package_store = cache.join("hirsel/packages");
    cli(
        &driver,
        &root,
        &workspace,
        &["lock", "--package-store", package_store.to_str().unwrap()],
        0,
    );
    let base = capture(&workspace);
    fs::create_dir_all(&cache).unwrap();
    let cache_base = capture(&cache);
    let mut client = Client::start(&driver, &root, &cache);
    client.request(
        1,
        "initialize",
        json!({
            "rootUri": file_uri(&workspace),
            "capabilities": {"textDocument": {"definition": {"linkSupport": true}}}
        }),
    );
    let initialized = client.response(1);
    assert_eq!(initialized["result"]["capabilities"]["hoverProvider"], true);
    client.open(&app, document);
    let diagnostics = client.diagnostics(&app, Some(1));
    assert!(has_rule(&diagnostics, "LANG-7"), "{diagnostics}");
    let query = |path: &Path, line: usize, character: usize| {
        json!({
            "textDocument": {"uri": file_uri(path)},
            "position": {"line": line, "character": character}
        })
    };
    let shadow = document.lines().nth(2).unwrap();
    let local = query(&app, 2, shadow.rfind('x').unwrap());
    client.request(2, "textDocument/hover", local.clone());
    let hover = client.response(2);
    assert!(
        hover["result"]["contents"]["value"]
            .as_str()
            .unwrap()
            .contains("u64"),
        "{hover}"
    );
    client.request(3, "textDocument/definition", local);
    let definition = client.response(3);
    assert_eq!(definition["result"][0]["targetUri"], file_uri(&app));
    assert_eq!(
        definition["result"][0]["targetSelectionRange"]["start"],
        json!({"line": 2, "character": shadow.rfind("x =").unwrap()}),
    );
    let library =
        format!("/// Written in the editor.\n{LIB_GOOD}\npub fn conditional(out: Stdout) {{}}");
    client.open(&lib, &library);
    assert_eq!(client.diagnostics(&lib, Some(1)), json!([]));
    let imported = query(&app, 2, shadow.find("value").unwrap());
    client.request(4, "textDocument/definition", imported.clone());
    let definition = client.response(4);
    assert_eq!(definition["result"][0]["targetUri"], file_uri(&lib));
    assert_eq!(
        definition["result"][0]["targetSelectionRange"]["start"]["line"],
        1
    );
    client.request(5, "textDocument/hover", imported);
    let hover = client.response(5);
    let value = hover["result"]["contents"]["value"].as_str().unwrap();
    assert!(value.contains("fn value() -> u64"), "{value}");
    assert!(value.contains("Written in the editor."), "{value}");
    assert!(value.contains("Package verdict · conditional"), "{value}");

    let archived_call = document.lines().nth(3).unwrap().find("value").unwrap();
    client.request(11, "textDocument/definition", query(&app, 3, archived_call));
    let definition = client.response(11);
    let target =
        super::documents::file_path(definition["result"][0]["targetUri"].as_str().unwrap())
            .unwrap();
    assert_ne!(target, lib);
    assert!(!target.starts_with(&workspace));
    assert_eq!(target.file_name().unwrap(), "main.hirsel");
    assert_eq!(fs::read_to_string(&target).unwrap(), archive_source);
    assert!(target.metadata().unwrap().permissions().readonly());
    assert_eq!(
        definition["result"][0]["targetSelectionRange"]["start"]["line"],
        1
    );

    let bad_call = document.lines().nth(4).unwrap().find("value").unwrap();
    client.request(12, "textDocument/hover", query(&app, 4, bad_call));
    let hover = client.response(12);
    let value = hover["result"]["contents"]["value"].as_str().unwrap();
    assert!(value.contains("Reached · unknown"), "{value}");
    assert!(value.contains("Package verdict · unknown"), "{value}");
    assert!(!value.contains("pure"), "{value}");

    let field = document.lines().nth(5).unwrap().rfind('x').unwrap();
    client.request(14, "textDocument/definition", query(&app, 5, field));
    let definition = client.response(14);
    assert_eq!(definition["result"][0]["targetUri"], file_uri(&app));
    assert_eq!(
        definition["result"][0]["targetSelectionRange"]["start"],
        json!({"line": 1, "character": 13}),
    );
    client.request(
        16,
        "textDocument/prepareRename",
        query(&app, 3, archived_call),
    );
    assert_eq!(client.response(16)["result"], Value::Null);
    let mut rename = query(&app, 3, archived_call);
    rename["newName"] = json!("different");
    client.request(17, "textDocument/rename", rename);
    assert_eq!(client.response(17)["error"]["code"], -32602);
    client.open(&target, archive_source);
    client.request(15, "textDocument/hover", query(&target, 1, 7));
    let archived_hover = client.response(15);
    assert!(
        archived_hover["result"]["contents"]["value"]
            .as_str()
            .unwrap()
            .contains("From the verified archive."),
        "{archived_hover}"
    );

    let params = json!({"textDocument": {"uri": file_uri(&app)}});
    client.request(6, "textDocument/documentSymbol", params.clone());
    let outline = client.response(6);
    let point = &outline["result"][1];
    assert_eq!(point["name"], "Point");
    assert_eq!(point["children"][0]["name"], "x");
    assert_eq!(point["children"][1]["name"], "y");

    let expected_file = root.join("format.hirsel");
    write(&expected_file, document);
    cli(&driver, &root, &expected_file, &["fmt"], 0);
    let canonical = fs::read_to_string(&expected_file).unwrap();
    assert_ne!(canonical, document);
    client.request(7, "textDocument/formatting", params.clone());
    let formatted = client.response(7);
    assert_eq!(formatted["result"][0]["newText"], canonical);
    assert_eq!(
        formatted["result"][0]["range"]["end"],
        json!({"line": 7, "character": document.lines().last().unwrap().len()}),
    );
    client.change(&app, 2, json!([{"text": canonical}]));
    client.request(8, "textDocument/formatting", params.clone());
    assert_eq!(client.response(8)["result"], json!([]));
    client.change(&app, 3, json!([{"text": "fn malformed( "}]));
    client.request(9, "textDocument/formatting", params.clone());
    assert_eq!(client.response(9)["result"], json!([]));
    let nested = "(".repeat(hirsel_core_syntax::PARSER_NESTING_LIMIT + 1);
    client.change(
        &app,
        4,
        json!([{"text": format!("fn refuse() {{ {nested}")}]),
    );
    client.request(13, "textDocument/formatting", params);
    assert_eq!(client.response(13)["result"], json!([]));
    client.shutdown(10);
    assert_eq!(capture(&workspace), base);
    assert_eq!(capture(&cache), cache_base);
}

#[test]
fn revision_cache_rechecks_owners_and_rebinds_sites() {
    use super::documents::{Document, Encoding};
    use super::workspace::{EditorInputs, EditorSession};

    let driver = driver();
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().canonicalize().unwrap();
    let path = root.join("main.hirsel");
    let original = concat!(
        "pub fn main() -> Result[(), Exit] { Ok(()) }\n",
        "fn seed() -> u64 { 1u64 }\n",
        "fn dependent() { let x = seed(); let _ = x; }\n",
        "fn independent() -> bool { false }\n",
    );
    write(&root.join("hirsel.toml"), &manifest("local/app", "main"));
    write(&path, original);
    cli(&driver, &root, &root, &["lock"], 0);
    let mut session = EditorSession::default();
    let mut revision = 0;
    let mut check = |text: &str| {
        revision += 1;
        let inputs = EditorInputs {
            generation: revision,
            folders: None,
            overlay: BTreeMap::from([(
                path.clone(),
                Document::open(file_uri(&path), revision as i64, text.to_owned()),
            )]),
            ..EditorInputs::default()
        };
        session.check(inputs, || false)
    };
    let open = check(original);
    assert!(
        open.diagnostics.is_empty(),
        "the pinned workspace must check"
    );
    assert!(open.stats.body_checks >= 4, "{:?}", open.stats);
    assert_eq!(open.stats.parse_hits, 0);

    let unchanged = check(original);
    assert_eq!(unchanged.stats.parse_misses, 0, "{:?}", unchanged.stats);
    assert_eq!(unchanged.stats.body_checks, 0, "{:?}", unchanged.stats);

    let body = original.replace("1u64", "2u64");
    let warm = check(&body);
    assert_eq!(warm.stats.parse_misses, 1, "{:?}", warm.stats);
    assert_eq!(warm.stats.body_checks, 1, "{:?}", warm.stats);

    let interface = body.replace("fn seed() -> u64 { 2u64 }", "fn seed() -> bool { true }");
    let changed = check(&interface);
    assert_eq!(changed.stats.body_checks, 2, "{:?}", changed.stats);
    assert!(changed.diagnostics.is_empty(), "signature edit must check");

    let moved = format!("// Inserted before the first item.\n\n{interface}");
    let moved = check(&moved);
    assert_eq!(moved.stats.body_checks, 0, "{:?}", moved.stats);
    let position = serde_json::from_value(json!({"line": 4, "character": 25})).unwrap();
    let target = moved
        .query(&path, position, true, true, Encoding::Utf16)
        .unwrap();
    assert_eq!(
        target[0]["targetSelectionRange"]["start"],
        json!({"line": 3, "character": 3})
    );
}

#[test]
fn stdio_live_diagnostics_contract() {
    let driver = driver();
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let workspace = root.join("workspace with spaces");
    let app = workspace.join("app/main.hirsel");
    let lib = workspace.join("lib/lib.hirsel");
    let lone = root.join("standalone/file.hirsel");
    let broken = root.join("broken");
    let cache = root.join("cache");

    write(
        &workspace.join("hirsel.toml"),
        "[workspace]\nmembers = [\"app\", \"lib\"]\n",
    );
    write(&app, APP);
    write(&lib, LIB_GOOD);
    write(
        &workspace.join("app/hirsel.toml"),
        &format!(
            "{}\n[dependencies]\ndep = {{ package = \"fixture.test/lib\", major = 1, path = \"../lib\" }}\n",
            manifest("local/app", "main")
        ),
    );
    write(
        &workspace.join("lib/hirsel.toml"),
        &manifest("fixture.test/lib", "lib"),
    );
    cli(&driver, &root, &workspace, &["lock"], 0);
    write(&lone, "💥");
    write(&broken.join("main.hirsel"), "💥");
    write(
        &broken.join("hirsel.toml"),
        &format!(
            "{}\n[dependencies]\nmissing = {{ package = \"fixture.test/missing\", major = 1, root = \"sha256:{PIN}\", url = \"https://example.invalid/pkg.hpkg\" }}\n",
            manifest("local/broken", "main")
        ),
    );
    write(&broken.join("hirsel.lock"), "untouched lock\n");
    write(
        &cache.join("hirsel/core-packages/keep"),
        "regular build cache\n",
    );
    let base = capture(&workspace);
    let cache_base = capture(&cache);
    let broken_base = capture(&broken);

    for encoding in ["utf-16", "utf-8"] {
        let mut client = Client::start(&driver, &root, &cache);
        let folders = [folder(&workspace), folder(&broken)];
        let general = if encoding == "utf-16" {
            json!({})
        } else {
            json!({"positionEncodings": ["utf-32", "utf-8"]})
        };
        client.request(
            1,
            "initialize",
            json!({
                "workspaceFolders": folders,
                "capabilities": {
                    "general": general,
                    "workspace": {"didChangeWatchedFiles": {"dynamicRegistration": true}}
                }
            }),
        );
        let initialized = client.response(1);
        assert_eq!(
            initialized["result"]["capabilities"]["positionEncoding"],
            encoding
        );
        client.notify("initialized", json!({}));
        let register = client.receive();
        assert_eq!(register["method"], "client/registerCapability");
        client.send(&json!({"jsonrpc": "2.0", "id": register["id"], "result": null}));
        client.open(&app, APP);
        let initial = client.diagnostics(&app, Some(1));
        assert!(has_rule(&initial, "LANG-7"), "{initial}");
        assert!(has_rule(&initial, "OWN-5"), "{initial}");
        assert!(!has_rule(&initial, "LANG-11"), "{initial}");

        client.open(&lib, LIB_GOOD);
        assert_eq!(client.diagnostics(&lib, Some(1)), json!([]));

        // Both ranges refer to the buffer that the previous change produced.
        client.change(&lib, 2, json!([
            {"range": range_text(LIB_GOOD, "u64", 0, encoding), "text": "bool"},
            {"range": range_text(&LIB_GOOD.replacen("u64", "bool", 1), "42u64", 0, encoding), "text": "true"}
        ]));
        let edited = client.diagnostics(&app, Some(1));
        assert!(has_rule(&edited, "LANG-7"), "{edited}");
        assert!(has_rule(&edited, "OWN-5"), "{edited}");
        assert!(has_rule(&edited, "LANG-11"), "{edited}");
        let moved = entry(&edited, "OWN-5");
        assert_eq!(moved["severity"], 1);
        assert!(
            moved["codeDescription"]["href"]
                .as_str()
                .unwrap()
                .contains("#own-5")
        );
        assert_eq!(moved["relatedInformation"][0]["message"], "moved here");
        assert_eq!(
            moved["relatedInformation"][0]["location"]["uri"],
            file_uri(&app)
        );
        let lines: Vec<_> = APP.split("\r\n").collect();
        let at = lines[3].rfind("s }").unwrap();
        assert_eq!(
            moved["range"],
            json!({"start": point(lines[3], at, 3, encoding), "end": point(lines[3], at + 1, 3, encoding)})
        );
        let others = entry(&edited, "LANG-11");
        assert_eq!(
            others["range"]["start"],
            point(lines[2], lines[2].find("dep.value()").unwrap(), 2, encoding)
        );

        // Exchange file events while the overlay is still open. The disk copy
        // remains good, so replacing the overlay would remove LANG-11.
        client.watch(&lib, 2);
        assert!(has_rule(&client.diagnostics(&app, Some(1)), "LANG-11"));

        // Edit and undo in the same batch. A response after the edits marks
        // the point after which no old version may be published.
        client.change(&app, 2, json!([{"text":"💥"}]));
        client.change(&app, 3, json!([{"text":APP}]));
        client.request(2, "hirsel/testBarrier", Value::Null);
        assert_eq!(client.response(2)["error"]["code"], -32601);
        client.notify(
            "textDocument/didSave",
            json!({"textDocument": {"uri": file_uri(&app)}}),
        );
        assert!(has_rule(&client.diagnostics(&app, Some(3)), "LANG-11"));

        client.close(&lib);
        assert_eq!(client.diagnostics(&lib, None), json!([]));
        let restored = client.diagnostics(&app, Some(3));
        assert!(!has_rule(&restored, "LANG-11"), "{restored}");

        client.open(&lone, "💥");
        client.notify(
            "workspace/didChangeWorkspaceFolders",
            json!({
                "event": {"added": [folder(lone.parent().unwrap())], "removed": []}
            }),
        );
        assert!(has_rule(&client.diagnostics(&lone, Some(1)), "LANG-1"));
        client.notify(
            "workspace/didChangeWorkspaceFolders",
            json!({
                "event": {"added": [], "removed": [folder(lone.parent().unwrap())]}
            }),
        );
        assert_eq!(client.diagnostics(&lone, None), json!([]));

        client.open(&broken.join("main.hirsel"), "💥");
        let configuration = client.diagnostics(&broken.join("hirsel.toml"), None);
        assert!(
            configuration[0]["message"]
                .as_str()
                .unwrap()
                .contains(if encoding == "utf-16" {
                    "is not in the package store"
                } else {
                    "is damaged in the package store"
                }),
            "{configuration}"
        );
        assert!(has_rule(
            &client.diagnostics(&broken.join("main.hirsel"), Some(1)),
            "LANG-1"
        ));

        // Compare the live semantic and causal sites to a check on an exact,
        // separate disk copy of the authored files. Check the workspace and
        // cache before the fixture itself installs the next negative input.
        assert_eq!(capture(&workspace), base);
        assert_eq!(capture(&broken), broken_base);
        assert_eq!(capture(&cache), cache_snapshot(&cache_base, encoding));
        compare_cli(&driver, &root, &workspace, &edited, encoding);
        let lock = workspace.join("hirsel.lock");
        let original = fs::read_to_string(&lock).unwrap();
        let stale = format!("{original}\n");
        write(&lock, &stale);
        client.watch(&lock, 2);
        let configuration = client.diagnostics(&lock, None);
        assert!(
            configuration[0]["message"]
                .as_str()
                .unwrap()
                .contains("hirsel.lock is stale under --frozen"),
            "{configuration}"
        );
        assert_eq!(fs::read_to_string(&lock).unwrap(), stale);
        write(&lock, &original);
        client.watch(&lock, 2);
        assert_eq!(client.diagnostics(&lock, None), json!([]));
        client.close(&app);
        assert_eq!(client.diagnostics(&app, None), json!([]));
        client.shutdown(3);

        if encoding == "utf-16" {
            let entry = cache
                .join("hirsel/packages")
                .join(&PIN[..2])
                .join(&PIN[2..]);
            write(&entry, "tampered package content");
        }
    }
}

pub(super) fn driver() -> PathBuf {
    let path = if let Some(path) = std::env::var_os("HIRSEL_DEV_TEST_DRIVER") {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .join(path)
    } else {
        std::env::current_exe()
            .unwrap()
            .parent()
            .unwrap()
            .join("../hirsel")
    };
    path.canonicalize().expect("built hirsel binary")
}

pub(super) fn manifest(name: &str, module: &str) -> String {
    source_manifest(name, module, module)
}

pub(super) fn source_manifest(name: &str, file: &str, module: &str) -> String {
    format!(
        "[package]\nname = \"{name}\"\nversion = \"1.0.0\"\ncharge = \"charge-v1\"\n[sources]\n\"{file}.hirsel\" = \"{module}\"\n"
    )
}

fn package_archive(root: &Path, archive: &Path, source: &str) -> crate::package_content::Root {
    use crate::package_content::{Library, PackageArchive};
    use crate::package_workspace::Workspace;

    let directory = root.join("archive-source");
    write(&directory.join("main.hirsel"), source);
    write(
        &directory.join("hirsel.toml"),
        &source_manifest("fixture.test/archive", "main", "archive"),
    );
    let workspace = Workspace::discover_for(&directory, false).unwrap().unwrap();
    let content = workspace.members[workspace.member.as_ref().unwrap()]
        .content
        .clone();
    let (root, encoded) = content.inventory().unwrap().encode().unwrap();
    let entries = BTreeMap::from([(
        root,
        Library {
            content,
            encoded: encoded.into(),
        },
    )]);
    fs::create_dir_all(archive.parent().unwrap()).unwrap();
    fs::write(archive, PackageArchive::encode(root, &entries).unwrap()).unwrap();
    root
}

pub(super) fn write(path: &Path, text: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
}

fn folder(path: &Path) -> Value {
    json!({"uri": file_uri(path), "name": path.file_name().unwrap().to_str().unwrap()})
}

pub(super) fn cli(
    driver: &Path,
    root: &Path,
    directory: &Path,
    flags: &[&str],
    status: i32,
) -> Vec<u8> {
    let output = Command::new(driver)
        .args(flags)
        .arg(directory)
        .current_dir(root)
        .env("XDG_CACHE_HOME", root.join("cli-cache"))
        .output()
        .unwrap();
    assert_eq!(
        output.status.code(),
        Some(status),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}

pub(super) fn capture(path: &Path) -> BTreeMap<PathBuf, Option<Vec<u8>>> {
    fn walk(root: &Path, path: &Path, out: &mut BTreeMap<PathBuf, Option<Vec<u8>>>) {
        for entry in fs::read_dir(path).unwrap() {
            let entry = entry.unwrap();
            let path = entry.path();
            let relative = path.strip_prefix(root).unwrap().to_owned();
            if entry.file_type().unwrap().is_dir() {
                out.insert(relative, None);
                walk(root, &path, out);
            } else {
                out.insert(relative, Some(fs::read(path).unwrap()));
            }
        }
    }
    let mut values = BTreeMap::new();
    walk(path, path, &mut values);
    values
}

fn cache_snapshot(
    base: &BTreeMap<PathBuf, Option<Vec<u8>>>,
    encoding: &str,
) -> BTreeMap<PathBuf, Option<Vec<u8>>> {
    let mut base = base.clone();
    if encoding == "utf-8" {
        base.insert("hirsel/packages".into(), None);
        let shard = Path::new("hirsel/packages").join(&PIN[..2]);
        base.insert(shard.clone(), None);
        base.insert(
            shard.join(&PIN[2..]),
            Some(b"tampered package content".to_vec()),
        );
    }
    base
}

fn has_rule(diagnostics: &Value, code: &str) -> bool {
    diagnostics
        .as_array()
        .unwrap()
        .iter()
        .any(|entry| entry["code"] == code)
}

pub(super) fn entry<'a>(diagnostics: &'a Value, code: &str) -> &'a Value {
    diagnostics
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["code"] == code)
        .unwrap_or_else(|| panic!("missing {code} in {diagnostics}"))
}

pub(super) fn point(line: &str, offset: usize, row: usize, encoding: &str) -> Value {
    let character = if encoding == "utf-8" {
        offset
    } else {
        line[..offset].encode_utf16().count()
    };
    json!({"line": row, "character": character})
}

fn range_text(line: &str, text: &str, row: usize, encoding: &str) -> Value {
    let start = line.find(text).unwrap();
    json!({"start": point(line, start, row, encoding), "end": point(line, start + text.len(), row, encoding)})
}

pub(super) struct Client {
    child: Child,
    input: BufWriter<ChildStdin>,
    output: BufReader<ChildStdout>,
}

impl Client {
    pub(super) fn start(driver: &Path, directory: &Path, cache: &Path) -> Self {
        let mut child = crate::host::offline_editor(driver)
            .arg("lsp")
            .current_dir(directory)
            .env("XDG_CACHE_HOME", cache)
            .env("TMPDIR", directory.join("workspace with spaces"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let input = BufWriter::new(child.stdin.take().unwrap());
        let output = BufReader::new(child.stdout.take().unwrap());
        Self {
            child,
            input,
            output,
        }
    }

    fn send(&mut self, message: &Value) {
        let body = serde_json::to_vec(message).unwrap();
        write!(self.input, "Content-Length: {}\r\n\r\n", body.len()).unwrap();
        self.input.write_all(&body).unwrap();
        self.input.flush().unwrap();
    }

    fn receive(&mut self) -> Value {
        let mut header = String::new();
        if self.output.read_line(&mut header).unwrap() == 0 {
            panic!("server closed stdout: {:?}", self.child.wait().unwrap());
        }
        let length = header
            .strip_prefix("Content-Length: ")
            .unwrap_or_else(|| panic!("unexpected stdout: {header}"))
            .strip_suffix("\r\n")
            .unwrap()
            .parse::<usize>()
            .unwrap();
        header.clear();
        self.output.read_line(&mut header).unwrap();
        assert_eq!(header, "\r\n");
        let mut body = vec![0; length];
        self.output.read_exact(&mut body).unwrap();
        serde_json::from_slice(&body).unwrap()
    }

    pub(super) fn request(&mut self, id: u64, method: &str, params: Value) {
        self.send(&json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}));
    }

    pub(super) fn response(&mut self, id: u64) -> Value {
        loop {
            let message = self.receive();
            if message["id"] == id {
                return message;
            }
            assert_eq!(
                message["method"], "textDocument/publishDiagnostics",
                "{message}"
            );
        }
    }

    pub(super) fn notify(&mut self, method: &str, params: Value) {
        self.send(&json!({"jsonrpc":"2.0", "method":method, "params":params}));
    }

    pub(super) fn open(&mut self, path: &Path, text: &str) {
        self.notify(
            "textDocument/didOpen",
            json!({"textDocument": {
                "uri": file_uri(path), "languageId": "hirsel", "version": 1, "text": text
            }}),
        );
    }

    fn change(&mut self, path: &Path, version: i64, changes: Value) {
        self.notify(
            "textDocument/didChange",
            json!({
                "textDocument": {"uri": file_uri(path), "version": version},
                "contentChanges": changes
            }),
        );
    }

    fn close(&mut self, path: &Path) {
        self.notify(
            "textDocument/didClose",
            json!({"textDocument": {"uri": file_uri(path)}}),
        );
    }

    fn watch(&mut self, path: &Path, kind: u32) {
        self.notify(
            "workspace/didChangeWatchedFiles",
            json!({"changes": [
                {"uri": file_uri(path), "type": kind}
            ]}),
        );
    }

    pub(super) fn diagnostics(&mut self, path: &Path, version: Option<i64>) -> Value {
        let uri = file_uri(path);
        loop {
            let value = self.receive();
            assert_eq!(
                value["method"], "textDocument/publishDiagnostics",
                "{value}"
            );
            let params = &value["params"];
            if params["uri"] == uri {
                if version.is_some() {
                    assert_eq!(
                        params["version"],
                        json!(version),
                        "obsolete or unversioned {value}"
                    );
                }
                if params["version"] == json!(version) {
                    return params["diagnostics"].clone();
                }
            }
        }
    }

    pub(super) fn shutdown(mut self, id: u64) {
        self.request(id, "shutdown", Value::Null);
        assert_eq!(self.response(id)["result"], Value::Null);
        self.notify("exit", Value::Null);
        self.finish();
    }

    fn finish(mut self) {
        self.input.flush().unwrap();
        drop(self.input);
        assert!(self.output.fill_buf().unwrap().is_empty());
        assert!(self.child.wait().unwrap().success());
    }
}

#[test]
fn stdio_completion_filters_scope_and_receiver() {
    p1(
        P1_APP,
        "/// The result.\npub fn value() -> u64 { 1u64 }\nfn secret() -> u64 { 2u64 }",
        |client, app, _, source| {
            client.request(2, "textDocument/completion", at(app, source, 2, "value"));
            let reply = client.response(2);
            let items = reply["result"]
                .as_array()
                .unwrap_or_else(|| panic!("{reply}"));
            assert_eq!(items.len(), 1);
            assert_eq!(items[0]["label"], "value");
            assert_eq!(
                items[0]["textEdit"]["range"],
                range_text(source.lines().nth(2).unwrap(), "value", 2, "utf-16")
            );
            client.request(4, "textDocument/completion", at(app, source, 4, "n }"));
            let reply = client.response(4);
            let local = reply["result"]
                .as_array()
                .unwrap()
                .iter()
                .find(|item| item["label"] == "n")
                .unwrap_or_else(|| panic!("{reply}"));
            assert_eq!(local["detail"], "u64");

            assert!(items[0]["detail"].as_str().unwrap().contains("fn value()"));
            assert!(
                items[0]["documentation"]
                    .to_string()
                    .contains("The result.")
            );
            let incomplete = source.replace("point.x", "point.");
            client.change(app, 2, json!([{"text": incomplete}]));
            let mut query = at(app, &incomplete, 6, ". }");
            query["position"]["character"] =
                json!(query["position"]["character"].as_u64().unwrap() + 1);
            client.request(3, "textDocument/completion", query);
            let reply = client.response(3);
            let labels: Vec<_> = reply["result"]
                .as_array()
                .unwrap_or_else(|| panic!("{reply}"))
                .iter()
                .map(|item| item["label"].as_str().unwrap())
                .collect();
            assert_eq!(labels, ["x", "y"]);
        },
    );
}

#[test]
fn stdio_references_follow_bindings_across_files() {
    p1(P1_APP, LIB_GOOD, |client, app, lib, source| {
        for (id, path, text, row, name, lines) in [
            (2, app, source, 4, "n }", vec![4, 4]),
            (3, lib, LIB_GOOD, 0, "value", vec![2, 3, 5, 0]),
            (4, app, source, 2, "dep.", vec![2, 5]),
        ] {
            let mut query = at(path, text, row, name);
            query["context"] = json!({"includeDeclaration": true});
            client.request(id, "textDocument/references", query);
            let reply = client.response(id);
            let refs = reply["result"]
                .as_array()
                .unwrap_or_else(|| panic!("{reply}"));
            assert_eq!(
                refs.iter()
                    .map(|hit| hit["range"]["start"]["line"].as_u64().unwrap())
                    .collect::<Vec<_>>(),
                lines
            );
            if id == 2 {
                assert!(refs[0]["range"]["start"]["character"].as_u64().unwrap() > 50);
            }
        }
        client.close(app);
        assert_eq!(client.diagnostics(app, None), json!([]));
        let mut query = at(lib, LIB_GOOD, 0, "value");
        query["context"] = json!({"includeDeclaration": false});
        client.request(5, "textDocument/references", query);
        let reply = client.response(5);
        let hits = reply["result"]
            .as_array()
            .unwrap_or_else(|| panic!("{reply}"));
        assert_eq!(hits.len(), 3);
        assert!(hits.iter().all(|hit| hit["uri"] == file_uri(app)));
    });
}

#[test]
fn stdio_rename_rejects_capture_and_versions_workspace_edits() {
    p1(P1_APP, LIB_GOOD, |client, app, lib, source| {
        let mut query = at(lib, LIB_GOOD, 0, "value");
        client.request(2, "textDocument/prepareRename", query.clone());
        assert_eq!(client.response(2)["result"]["placeholder"], "value");
        query["newName"] = json!("quantity");
        client.request(3, "textDocument/rename", query);
        let reply = client.response(3);
        let changes = reply["result"]["documentChanges"]
            .as_array()
            .unwrap_or_else(|| panic!("{reply}"));
        assert_eq!(changes.len(), 2);
        assert_eq!(
            changes[0]["textDocument"],
            json!({"uri": file_uri(app), "version": 1})
        );
        assert_eq!(changes[0]["edits"].as_array().unwrap().len(), 3);
        assert_eq!(
            changes[1]["textDocument"],
            json!({"uri": file_uri(lib), "version": 1})
        );
        for (id, name) in [(4, "answer"), (5, "let"), (6, "dep")] {
            let mut query = at(app, source, 5, "item }");
            query["newName"] = json!(name);
            client.request(id, "textDocument/rename", query);
            assert_eq!(client.response(id)["error"]["code"], -32602);
        }
    });
}

#[test]
fn stdio_code_actions_use_compiler_applicability_and_versions() {
    let app = format!(
        "{P1_APP}fn literal() -> u64 {{ let x = 1; x }}\nfn missing(b: bool) -> u64 {{ match b {{ true => 1u64 }} }}\n"
    );
    let lib = format!("{LIB_GOOD}\nfn edit(mut xs: List[u64]) {{ list.clear(xs) }}\n");
    p1(&app, &lib, |client, app, library, _| {
        client.change(library, 7, json!([{"text": lib}]));
        let mut old = Value::Null;
        let params = |path: &Path, row, diagnostics: Value| {
            json!({
                "textDocument": {"uri": file_uri(path)}, "context": {"diagnostics": diagnostics},
                "range": {"start": {"line": row, "character": 0}, "end": {"line": row + 1, "character": 0}}
            })
        };
        for (id, path, row, version, applicability, preferred) in [
            (2, library, 1, 7, "safe", true),
            (3, app, 9, 1, "maybe", false),
            (4, app, 10, 1, "placeholder", false),
        ] {
            client.request(id, "textDocument/codeAction", params(path, row, json!([])));
            let reply = client.response(id);
            let action = &reply["result"][0];
            assert_eq!(action["data"]["applicability"], applicability, "{reply}");
            assert_eq!(action["isPreferred"], preferred);
            assert_eq!(
                action["edit"]["documentChanges"][0]["textDocument"],
                json!({"uri": file_uri(path), "version": version})
            );
            if id == 2 {
                old = action["diagnostics"].clone();
            }
        }
        client.change(library, 8, json!([{"text": format!("{lib}\n")}]));
        client.request(5, "textDocument/codeAction", params(library, 1, old));
        assert_eq!(client.response(5)["result"], json!([]));
    });
}

#[test]
fn stdio_inlay_hints_are_range_filtered_and_generation_guarded() {
    p1(P1_APP, LIB_GOOD, |client, app, _, source| {
        let range = |row| json!({"start": {"line": row, "character": 0}, "end": {"line": row + 1, "character": 0}});
        client.request(
            2,
            "textDocument/inlayHint",
            json!({"textDocument": {"uri": file_uri(app)}, "range": range(8)}),
        );
        let reply = client.response(2);
        let hints = reply["result"]
            .as_array()
            .unwrap_or_else(|| panic!("{reply}"));
        assert_eq!(
            hints
                .iter()
                .map(|hint| hint["label"].as_str().unwrap())
                .collect::<Vec<_>>(),
            [": u64", "amount:"]
        );
        client.change(app, 2, json!([{"text": format!("{source}\n")}]));
        client.request(3, "inlayHint/resolve", hints[0].clone());
        assert_eq!(client.response(3)["error"]["code"], -32801);
    });
}
