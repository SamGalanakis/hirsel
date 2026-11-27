pub(crate) mod cache;
mod code_actions;
mod completion;
mod diagnostics;
mod documents;
mod formatting;
mod hover;
mod inlay_hints;
mod navigation;
mod protocol;
mod references;
mod rename;
mod snapshot;
mod symbols;
mod workspace;

#[cfg(test)]
mod tests;

use self::documents::{Change, Document, Encoding, Position, file_path};
use self::snapshot::EditorSnapshot;
use self::workspace::{EditorInputs, EditorSession};
use hirsel_core_syntax::Span;
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Condvar, Mutex};

pub(crate) fn run() -> io::Result<i32> {
    let server = Server::default();
    std::thread::scope(|scope| {
        scope.spawn(|| server.compile());
        let mut input = io::stdin().lock();
        let result = (|| {
            while let Some(message) = protocol::read(&mut input)? {
                let mut state = server.state.lock().unwrap();
                let message = match message {
                    Ok(message) => message,
                    Err(_) => {
                        state.send(&protocol::error(&Value::Null, -32700, "Parse error"))?;
                        continue;
                    }
                };
                if let Some(code) = state.handle(&message)? {
                    return Ok(code);
                }
                server.ready.notify_one();
            }
            Ok(0)
        })();
        server.state.lock().unwrap().stop();
        server.ready.notify_one();
        result
    })
}

#[derive(Default)]
struct Server {
    state: Mutex<State>,
    ready: Condvar,
}

impl Server {
    fn compile(&self) {
        let mut session = EditorSession::default();
        let trace = std::env::var_os("HIRSEL_LSP_STATS").is_some();
        loop {
            let inputs = {
                let mut state = self
                    .ready
                    .wait_while(self.state.lock().unwrap(), |state| {
                        !state.pending && state.phase != Phase::Shutdown
                    })
                    .unwrap();
                if state.phase == Phase::Shutdown {
                    return;
                }
                state.pending = false;
                state.inputs.clone()
            };
            let generation = inputs.generation;
            let snapshot =
                session.check(inputs, || self.state.lock().unwrap().obsolete(generation));
            let mut state = self.state.lock().unwrap();
            if !state.obsolete(generation) {
                if trace {
                    let stats = snapshot.stats;
                    eprintln!(
                        "hirsel lsp: generation={generation} parse_hits={} parse_misses={} body_hits={} body_checks={} cache_bytes={}",
                        stats.parse_hits,
                        stats.parse_misses,
                        stats.body_hits,
                        stats.body_checks,
                        stats.retained_bytes,
                    );
                }
                if let Err(error) = state.publish(snapshot) {
                    eprintln!("hirsel lsp: {error}");
                    state.stop();
                    return;
                }
            }
        }
    }
}

#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum Phase {
    #[default]
    BeforeInitialize,
    Running,
    Shutdown,
}

impl Default for EditorInputs {
    fn default() -> Self {
        Self {
            generation: 0,
            folders: None,
            overlay: BTreeMap::new(),
        }
    }
}

#[derive(Default)]
struct State {
    phase: Phase,
    encoding: Encoding,
    inputs: EditorInputs,
    pending: bool,
    register_watchers: bool,
    published: BTreeMap<PathBuf, String>,
    snapshot: Option<EditorSnapshot>,
    queries: Vec<Query>,
    definition_links: bool,
}

struct Query {
    id: Value,
    generation: u64,
    path: PathBuf,
    kind: QueryKind,
}

#[derive(Clone, Copy, Deserialize)]
struct QueryRange {
    start: Position,
    end: Position,
}

enum QueryKind {
    Hover(Position),
    Definition(Position),
    Symbols,
    Format,
    Completion(Position),
    References(Position, bool),
    PrepareRename(Position),
    Rename(Position, String),
    CodeAction(QueryRange, Value),
    InlayHint(QueryRange),
}

#[derive(Deserialize)]
struct DocumentParams<T> {
    #[serde(rename = "textDocument")]
    document: T,
}

#[derive(Deserialize)]
struct OpenDocument {
    uri: String,
    version: i64,
    text: String,
}

#[derive(Deserialize)]
struct ChangeParams {
    #[serde(rename = "textDocument")]
    document: VersionedDocument,
    #[serde(rename = "contentChanges")]
    changes: Vec<Change>,
}

#[derive(Deserialize)]
struct VersionedDocument {
    uri: String,
    version: i64,
}

#[derive(Deserialize)]
struct DocumentUri {
    uri: String,
}

#[derive(Deserialize)]
struct FolderEvent {
    added: Vec<DocumentUri>,
    removed: Vec<DocumentUri>,
}

#[derive(Deserialize)]
struct FileEvent {
    uri: String,
    #[serde(rename = "type")]
    kind: u32,
}

impl State {
    fn send(&self, value: &Value) -> io::Result<()> {
        protocol::write(&mut io::stdout().lock(), value)
    }

    fn handle(&mut self, message: &Value) -> io::Result<Option<i32>> {
        let id = message.get("id");
        if message["jsonrpc"] != "2.0"
            || id.is_some_and(|id| !(id.is_string() || id.is_i64() || id.is_u64() || id.is_null()))
        {
            self.send(&protocol::error(&Value::Null, -32600, "Invalid request"))?;
            return Ok(None);
        }
        let Some(method) = message["method"].as_str() else {
            if id.is_none() || (message.get("result").is_none() && message.get("error").is_none()) {
                self.send(&protocol::error(
                    id.unwrap_or(&Value::Null),
                    -32600,
                    "Invalid request",
                ))?;
            }
            return Ok(None);
        };
        if let Some(id) = id {
            if self.phase == Phase::Running {
                if let Some(query) = self.editor_request(method, id, &message["params"]) {
                    return query.map(|_| None);
                }
                if method == "shutdown" {
                    for query in std::mem::take(&mut self.queries) {
                        self.send(&protocol::error(&query.id, -32800, "Request cancelled"))?;
                    }
                }
            }
            let answer = self.request(method, id, &message["params"]);
            self.send(&answer)?;
        } else if method == "exit" {
            let status = i32::from(self.phase != Phase::Shutdown);
            self.stop();
            return Ok(Some(status));
        } else if self.phase == Phase::Running {
            self.notification(method, &message["params"])?;
        }
        Ok(None)
    }

    fn request(&mut self, method: &str, id: &Value, params: &Value) -> Value {
        match (self.phase, method) {
            (Phase::BeforeInitialize, "initialize") if params.is_object() => {
                self.encoding = Encoding::negotiate(params);
                self.inputs.folders = params["workspaceFolders"]
                    .as_array()
                    .map(|folders| {
                        folders
                            .iter()
                            .filter_map(|folder| file_path(folder["uri"].as_str()?))
                            .collect::<BTreeSet<_>>()
                    })
                    .filter(|folders| !folders.is_empty())
                    .or_else(|| {
                        params["rootUri"]
                            .as_str()
                            .and_then(file_path)
                            .map(|path| BTreeSet::from([path]))
                    })
                    .or_else(|| {
                        params["rootPath"]
                            .as_str()
                            .map(PathBuf::from)
                            .filter(|path| path.is_absolute())
                            .map(|path| BTreeSet::from([path]))
                    });
                self.register_watchers = params
                    .pointer("/capabilities/workspace/didChangeWatchedFiles/dynamicRegistration")
                    .and_then(Value::as_bool)
                    .unwrap_or_default();
                self.definition_links = params
                    .pointer("/capabilities/textDocument/definition/linkSupport")
                    .and_then(Value::as_bool)
                    .unwrap_or_default();
                self.phase = Phase::Running;
                protocol::result(
                    id,
                    json!({
                        "capabilities": {
                            "positionEncoding": self.encoding.name(),
                            "textDocumentSync": {"openClose": true, "change": 2, "save": true},
                            "hoverProvider": true,
                            "definitionProvider": true,
                            "documentSymbolProvider": true,
                            "documentFormattingProvider": true,
                            "completionProvider": {"triggerCharacters": ["."]},
                            "referencesProvider": true,
                            "renameProvider": {"prepareProvider": true},
                            "codeActionProvider": {"codeActionKinds": ["quickfix"]},
                            "inlayHintProvider": {"resolveProvider": true},
                            "workspace": {
                                "workspaceFolders": {"supported": true, "changeNotifications": true}
                            }
                        },
                        "serverInfo": {"name": "Hirsel", "version": crate::installation::version()}
                    }),
                )
            }
            (Phase::BeforeInitialize, "initialize") => {
                protocol::error(id, -32602, "Invalid params")
            }
            (Phase::BeforeInitialize, _) => protocol::error(id, -32002, "Server not initialized"),
            (Phase::Running, "inlayHint/resolve") => {
                match params.pointer("/data/generation").and_then(Value::as_u64) {
                    Some(generation) if generation == self.inputs.generation => {
                        protocol::result(id, params.clone())
                    }
                    Some(_) => protocol::error(id, -32801, "Content modified"),
                    None => protocol::error(id, -32602, "Invalid inlay hint"),
                }
            }
            (Phase::Running, "shutdown") => {
                self.stop();
                protocol::result(id, Value::Null)
            }
            (Phase::Running, "initialize") | (Phase::Shutdown, _) => {
                protocol::error(id, -32600, "Invalid request")
            }
            (Phase::Running, _) => protocol::error(id, -32601, "Method not found"),
        }
    }

    fn notification(&mut self, method: &str, params: &Value) -> io::Result<()> {
        match method {
            "initialized" => {
                if std::mem::take(&mut self.register_watchers) {
                    self.send(&json!({
                        "jsonrpc": "2.0",
                        "id": "hirsel/watch-files",
                        "method": "client/registerCapability",
                        "params": {"registrations": [{
                            "id": "hirsel/watch-files",
                            "method": "workspace/didChangeWatchedFiles",
                            "registerOptions": {"watchers": [
                                {"globPattern": "**/*.hirsel"},
                                {"globPattern": "**/hirsel.toml"},
                                {"globPattern": "**/hirsel.lock"}
                            ]}
                        }]}
                    }))?;
                }
            }
            "textDocument/didOpen" => {
                if let Ok(DocumentParams { document }) =
                    serde_json::from_value::<DocumentParams<OpenDocument>>(params.clone())
                    && let Some(path) = file_path(&document.uri)
                    && let std::collections::btree_map::Entry::Vacant(entry) =
                        self.inputs.overlay.entry(path)
                {
                    entry.insert(Document::open(
                        document.uri,
                        document.version,
                        document.text,
                    ));
                    self.invalidate();
                }
            }
            "textDocument/didChange" => {
                if let Ok(ChangeParams { document, changes }) =
                    serde_json::from_value(params.clone())
                    && let Some(open) =
                        file_path(&document.uri).and_then(|path| self.inputs.overlay.get_mut(&path))
                    && open
                        .change(document.version, changes, self.encoding)
                        .is_some()
                {
                    self.invalidate();
                }
            }
            "textDocument/didSave" => self.invalidate(),
            "textDocument/didClose" => {
                if let Ok(DocumentParams { document }) =
                    serde_json::from_value::<DocumentParams<DocumentUri>>(params.clone())
                    && let Some(path) = file_path(&document.uri)
                    && let Some(open) = self.inputs.overlay.remove(&path)
                {
                    self.clear(&path, &open.uri)?;
                    self.invalidate();
                }
            }
            "workspace/didChangeWorkspaceFolders" => {
                if let Ok(FolderEvent { added, removed }) =
                    serde_json::from_value(params["event"].clone())
                {
                    let folders = self.inputs.folders.get_or_insert_with(BTreeSet::new);
                    for folder in removed
                        .into_iter()
                        .filter_map(|folder| file_path(&folder.uri))
                    {
                        folders.remove(&folder);
                    }
                    folders.extend(
                        added
                            .into_iter()
                            .filter_map(|folder| file_path(&folder.uri)),
                    );
                    let removed = self
                        .published
                        .iter()
                        .filter(|(path, _)| !self.inputs.owns(path))
                        .map(|(path, uri)| (path.clone(), uri.clone()))
                        .collect::<Vec<_>>();
                    for (path, uri) in removed {
                        self.clear(&path, &uri)?;
                    }
                    self.invalidate();
                }
            }
            "workspace/didChangeWatchedFiles" => {
                if let Ok(changes) =
                    serde_json::from_value::<Vec<FileEvent>>(params["changes"].clone())
                {
                    for change in changes {
                        if change.kind == 3
                            && let Some(path) = file_path(&change.uri)
                            && !self.inputs.overlay.contains_key(&path)
                            && let Some(uri) = self.published.get(&path).cloned()
                        {
                            self.clear(&path, &uri)?;
                        }
                    }
                    self.invalidate();
                }
            }
            "$/cancelRequest" => {
                if let Some(index) = self
                    .queries
                    .iter()
                    .position(|query| query.id == params["id"])
                {
                    let query = self.queries.remove(index);
                    self.send(&protocol::error(&query.id, -32800, "Request cancelled"))?;
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn invalidate(&mut self) {
        self.inputs.generation += 1;
        self.pending = true;
    }

    fn obsolete(&self, generation: u64) -> bool {
        self.phase == Phase::Shutdown || self.inputs.generation != generation
    }

    fn stop(&mut self) {
        self.phase = Phase::Shutdown;
        self.pending = false;
    }

    fn editor_request(
        &mut self,
        method: &str,
        id: &Value,
        params: &Value,
    ) -> Option<io::Result<()>> {
        let position = || serde_json::from_value(params["position"].clone()).ok();
        let range = || serde_json::from_value(params["range"].clone()).ok();
        let kind = match method {
            "textDocument/hover" => position().map(QueryKind::Hover),
            "textDocument/definition" => position().map(QueryKind::Definition),
            "textDocument/documentSymbol" => Some(QueryKind::Symbols),
            "textDocument/formatting" => Some(QueryKind::Format),
            "textDocument/completion" => position().map(QueryKind::Completion),
            "textDocument/references" => position()
                .zip(params["context"]["includeDeclaration"].as_bool())
                .map(|(position, include)| QueryKind::References(position, include)),
            "textDocument/prepareRename" => position().map(QueryKind::PrepareRename),
            "textDocument/rename" => position()
                .zip(params["newName"].as_str())
                .map(|(position, name)| QueryKind::Rename(position, name.to_owned())),
            "textDocument/codeAction" => range()
                .filter(|_| params["context"].is_object())
                .map(|range| QueryKind::CodeAction(range, params["context"].clone())),
            "textDocument/inlayHint" => range().map(QueryKind::InlayHint),
            _ => return None,
        };
        let path = params
            .pointer("/textDocument/uri")
            .and_then(Value::as_str)
            .and_then(file_path);
        let (Some(kind), Some(path)) = (kind, path) else {
            return Some(self.send(&protocol::error(id, -32602, "Invalid document parameters")));
        };
        let query = Query {
            id: id.clone(),
            generation: self.inputs.generation,
            path,
            kind,
        };
        if (self.inputs.overlay.contains_key(&query.path)
            || self
                .snapshot
                .as_ref()
                .is_some_and(|snapshot| snapshot.origins.contains_key(&query.path)))
            && self
                .snapshot
                .as_ref()
                .is_none_or(|snapshot| snapshot.inputs.generation != query.generation)
        {
            self.queries.push(query);
            Some(Ok(()))
        } else {
            Some(self.send(&self.answer(query)))
        }
    }

    fn answer(&self, query: Query) -> Value {
        let Query {
            id,
            generation,
            path,
            kind,
        } = query;
        if generation != self.inputs.generation {
            return protocol::error(&id, -32801, "Content modified");
        }
        let snapshot = self
            .snapshot
            .as_ref()
            .filter(|snapshot| snapshot.inputs.generation == generation);
        let span = |range: QueryRange| {
            let snapshot = snapshot?;
            let start = snapshot.offset(&path, range.start, self.encoding)?;
            let end = snapshot.offset(&path, range.end, self.encoding)?;
            (start <= end).then_some(Span { start, end })
        };
        let answer = match kind {
            QueryKind::Completion(position) => snapshot
                .and_then(|snapshot| snapshot.complete(&path, position, self.encoding))
                .unwrap_or_else(|| json!([])),
            QueryKind::References(position, include) => snapshot
                .and_then(|snapshot| snapshot.references(&path, position, include, self.encoding))
                .unwrap_or_else(|| json!([])),
            QueryKind::PrepareRename(position) => snapshot
                .and_then(|snapshot| snapshot.prepare_rename(&path, position, self.encoding))
                .unwrap_or_default(),
            QueryKind::Rename(position, new_name) => {
                let edit = snapshot
                    .ok_or("No checked source for rename")
                    .and_then(|snapshot| {
                        snapshot.rename(&path, position, &new_name, self.encoding)
                    });
                match edit {
                    Ok(edit) => edit,
                    Err(message) => return protocol::error(&id, -32602, message),
                }
            }
            QueryKind::CodeAction(range, context) => snapshot
                .zip(span(range))
                .map(|(snapshot, span)| {
                    json!(snapshot.code_actions(&path, span, &context, self.encoding))
                })
                .unwrap_or_else(|| json!([])),
            QueryKind::InlayHint(range) => snapshot
                .zip(span(range))
                .and_then(|(snapshot, span)| snapshot.inlay_hints(&path, span, self.encoding))
                .unwrap_or_else(|| json!([])),
            QueryKind::Hover(position) | QueryKind::Definition(position) => snapshot
                .and_then(|snapshot| {
                    snapshot.query(
                        &path,
                        position,
                        matches!(kind, QueryKind::Definition(_)),
                        self.definition_links,
                        self.encoding,
                    )
                })
                .unwrap_or_default(),
            QueryKind::Symbols | QueryKind::Format => {
                let text = self
                    .inputs
                    .overlay
                    .get(&path)
                    .filter(|_| self.inputs.owns(&path))
                    .map(|open| &open.text)
                    .or_else(|| snapshot?.origins.get(&path));
                let mode = snapshot
                    .and_then(|snapshot| snapshot.modes.get(&path))
                    .copied()
                    .unwrap_or(hirsel_core_syntax::ParseMode::Source);
                match (text, kind) {
                    (Some(text), QueryKind::Symbols) => symbols::outline(text, mode, self.encoding),
                    (Some(text), QueryKind::Format)
                        if snapshot.is_none_or(|snapshot| !snapshot.immutable.contains(&path)) =>
                    {
                        formatting::edits(text, mode, self.encoding)
                    }
                    _ => json!([]),
                }
            }
        };
        protocol::result(&id, answer)
    }

    fn clear(&mut self, path: &Path, uri: &str) -> io::Result<()> {
        self.published.remove(path);
        self.send(&publication(uri, None, Vec::new()))
    }

    fn publish(&mut self, snapshot: EditorSnapshot) -> io::Result<()> {
        let inputs = &snapshot.inputs;
        let mut by_path: BTreeMap<&Path, Vec<Value>> = inputs
            .overlay
            .keys()
            .filter(|path| inputs.owns(path))
            .map(|path| (path.as_path(), Vec::new()))
            .collect();
        for diagnostic in &snapshot.diagnostics {
            let path = diagnostic.path();
            if path.extension().is_some_and(|ext| ext == "hirsel")
                && !inputs.overlay.contains_key(path)
            {
                continue;
            }
            if let Some(mut value) = diagnostic.lsp(&snapshot.origins, self.encoding, |path| {
                diagnostics::uri(path, &inputs.overlay)
            }) {
                if snapshot.fixes.contains_key(path) {
                    value["data"] = json!({"generation": inputs.generation});
                }
                by_path.entry(path).or_default().push(value);
            }
        }
        for (path, uri) in &self.published {
            if !by_path.contains_key(path.as_path()) {
                self.send(&publication(uri, None, Vec::new()))?;
            }
        }
        let mut published = BTreeMap::new();
        for (path, diagnostics) in by_path {
            let uri = diagnostics::uri(path, &inputs.overlay);
            let version = inputs.overlay.get(path).map(|open| open.version);
            self.send(&publication(&uri, version, diagnostics))?;
            published.insert(path.to_owned(), uri);
        }
        self.published = published;
        self.snapshot = Some(snapshot);
        for query in std::mem::take(&mut self.queries) {
            self.send(&self.answer(query))?;
        }
        Ok(())
    }
}

fn publication(uri: &str, version: Option<i64>, diagnostics: Vec<Value>) -> Value {
    let mut params = json!({"uri": uri, "diagnostics": diagnostics});
    if let Some(version) = version {
        params["version"] = json!(version);
    }
    protocol::notify("textDocument/publishDiagnostics", params)
}

#[cfg(test)]
mod stdio_fixture {
    use super::documents::file_uri;
    use super::tests::{
        APP, Client, LIB_BAD, capture, cli, driver, entry, manifest, point, source_manifest, write,
    };
    use serde_json::{Value, json};
    use std::fs;
    use std::path::Path;

    pub(super) const P1_APP: &str = concat!(
        "pub fn main() -> Result[(), Exit] { Ok(()) }\n",
        "type Point { x: u64, y: u64 }\n",
        "fn use_dep() -> u64 { dep.value() }\n",
        "fn other() -> u64 { second.value() }\n",
        "fn siblings() -> u64 { let _ = { let n = false; n }; { let n = 1u64; n } }\n",
        "fn capture() -> u64 { let answer = 3u64; let item = answer; let _ = dep.value(); item }\n",
        "fn draw(point: Point) -> u64 { point.x }\n",
        "fn width(amount: u64) -> u64 { amount }\n",
        "fn hints() -> u64 { let size = width(3u64); size }\n",
    );

    pub(super) fn p1(app: &str, lib: &str, check: impl FnOnce(&mut Client, &Path, &Path, &str)) {
        let driver = driver();
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let workspace = root.join("workspace with spaces");
        let app_path = workspace.join("app/main.hirsel");
        let lib_path = workspace.join("lib/main.hirsel");
        write(
            &workspace.join("hirsel.toml"),
            "[workspace]\nmembers = [\"app\", \"lib\"]\n",
        );
        let dependency = "{ package = \"fixture.test/lib\", major = 1, path = \"../lib\" }";
        write(
            &workspace.join("app/hirsel.toml"),
            &format!(
                "{}\n[dependencies]\ndep = {dependency}\nsecond = {dependency}\n",
                manifest("local/app", "main"),
            ),
        );
        write(
            &workspace.join("lib/hirsel.toml"),
            &source_manifest("fixture.test/lib", "main", "lib"),
        );
        write(&app_path, app);
        write(&lib_path, lib);
        cli(&driver, &root, &workspace, &["lock"], 0);
        let cache = root.join("cache");
        fs::create_dir(&cache).unwrap();
        let before = capture(&workspace);
        let mut client = Client::start(&driver, &root, &cache);
        client.request(1, "initialize", json!({"rootUri": file_uri(&workspace)}));
        assert!(client.response(1)["result"].is_object());
        client.open(&app_path, app);
        client.open(&lib_path, lib);
        client.diagnostics(&lib_path, Some(1));
        check(&mut client, &app_path, &lib_path, app);
        client.shutdown(99);
        assert_eq!(capture(&workspace), before);
        assert!(capture(&cache).is_empty());
    }

    pub(super) fn at(path: &Path, source: &str, row: usize, word: &str) -> Value {
        let line = source.lines().nth(row).unwrap();
        json!({"textDocument": {"uri": file_uri(path)}, "position": point(line, line.rfind(word).unwrap(), row, "utf-16")})
    }

    pub(super) fn compare_cli(
        driver: &Path,
        root: &Path,
        workspace: &Path,
        lsp: &Value,
        encoding: &str,
    ) {
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
}
