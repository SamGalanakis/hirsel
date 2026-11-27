use super::documents::file_uri;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::fs;
use std::io::{BufRead, BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

const APP: &str = concat!(
    "pub fn main() -> Result[(), Exit] { Ok(()) }\r\n",
    "fn poisoned(x: Missing) {}\r\n",
    "fn answer() -> u64 { let _ = \"😀\"; dep.value() }\r\n",
    "fn moved(take s: String) -> String { let t = s; s }",
);
const LIB_GOOD: &str = "pub fn value() -> u64 { 42u64 }";
const LIB_BAD: &str = "pub fn value() -> bool { true }";
const PIN: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

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
        client.request(3, "shutdown", Value::Null);
        assert_eq!(client.response(3)["result"], Value::Null);
        client.notify("exit", Value::Null);
        client.finish();

        if encoding == "utf-16" {
            let entry = cache
                .join("hirsel/packages")
                .join(&PIN[..2])
                .join(&PIN[2..]);
            write(&entry, "tampered package content");
        }
    }
}

fn driver() -> PathBuf {
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

fn manifest(name: &str, module: &str) -> String {
    format!(
        "[package]\nname = \"{name}\"\nversion = \"1.0.0\"\ncharge = \"charge-v1\"\n[sources]\n\"{module}.hirsel\" = \"{module}\"\n"
    )
}

fn write(path: &Path, text: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
}

fn folder(path: &Path) -> Value {
    json!({"uri": file_uri(path), "name": path.file_name().unwrap().to_str().unwrap()})
}

fn cli(driver: &Path, root: &Path, directory: &Path, flags: &[&str], status: i32) -> Vec<u8> {
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

fn capture(path: &Path) -> BTreeMap<PathBuf, Option<Vec<u8>>> {
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

fn compare_cli(driver: &Path, root: &Path, workspace: &Path, lsp: &Value, encoding: &str) {
    let comparison = root.join("cli-workspace");
    for (relative, bytes) in capture(workspace) {
        if let Some(bytes) = bytes {
            let path = comparison.join(relative);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, bytes).unwrap();
        }
    }
    write(&comparison.join("lib/lib.hirsel"), LIB_BAD);
    let scratch = root.join("cli-store");
    let result: Value = serde_json::from_slice(&cli(
        driver,
        root,
        &comparison.join("app"),
        &[
            "check",
            "--frozen",
            "--json",
            "--store",
            scratch.to_str().unwrap(),
        ],
        1,
    ))
    .unwrap();
    let cli = result["diagnostics"].as_array().unwrap();
    let source: Vec<_> = cli
        .iter()
        .filter(|d| d["primary"]["module"] == "main.hirsel")
        .collect();
    assert_eq!(
        lsp.as_array().unwrap().len(),
        source.len(),
        "{lsp}\n{result}"
    );
    for diagnostic in source {
        let live = entry(lsp, diagnostic["rule"].as_str().unwrap());
        assert_eq!(
            live["range"],
            byte_range(APP, &diagnostic["primary"]["span"], encoding)
        );
        let roles: Vec<Value> = diagnostic["causal"]
            .as_array()
            .unwrap()
            .iter()
            .map(|cause| {
                assert_eq!(cause["site"]["module"], "main.hirsel");
                json!({
                    "location": {
                        "uri": file_uri(&workspace.join("app/main.hirsel")),
                        "range": byte_range(APP, &cause["site"]["span"], encoding)
                    },
                    "message": cause["role"].as_str().unwrap().replace('_', " ")
                })
            })
            .collect();
        assert_eq!(
            live.get("relatedInformation").cloned().unwrap_or(json!([])),
            json!(roles)
        );
    }
}

fn has_rule(diagnostics: &Value, code: &str) -> bool {
    diagnostics
        .as_array()
        .unwrap()
        .iter()
        .any(|entry| entry["code"] == code)
}

fn entry<'a>(diagnostics: &'a Value, code: &str) -> &'a Value {
    diagnostics
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["code"] == code)
        .unwrap_or_else(|| panic!("missing {code} in {diagnostics}"))
}

fn byte_range(source: &str, span: &Value, encoding: &str) -> Value {
    let position = |offset: &Value| {
        let offset = offset.as_u64().unwrap() as usize;
        let prefix = &source[..offset];
        point(
            prefix.rsplit('\n').next().unwrap(),
            prefix.rsplit('\n').next().unwrap().len(),
            prefix.bytes().filter(|&byte| byte == b'\n').count(),
            encoding,
        )
    };
    json!({"start": position(&span["start"]), "end": position(&span["end"])})
}

fn point(line: &str, offset: usize, row: usize, encoding: &str) -> Value {
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

struct Client {
    child: Child,
    input: BufWriter<ChildStdin>,
    output: BufReader<ChildStdout>,
}

impl Client {
    fn start(driver: &Path, directory: &Path, cache: &Path) -> Self {
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

    fn request(&mut self, id: u64, method: &str, params: Value) {
        self.send(&json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}));
    }

    fn response(&mut self, id: u64) -> Value {
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

    fn notify(&mut self, method: &str, params: Value) {
        self.send(&json!({"jsonrpc":"2.0", "method":method, "params":params}));
    }

    fn open(&mut self, path: &Path, text: &str) {
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

    fn diagnostics(&mut self, path: &Path, version: Option<i64>) -> Value {
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

    fn finish(mut self) {
        self.input.flush().unwrap();
        drop(self.input);
        assert!(self.output.fill_buf().unwrap().is_empty());
        assert!(self.child.wait().unwrap().success());
    }
}
