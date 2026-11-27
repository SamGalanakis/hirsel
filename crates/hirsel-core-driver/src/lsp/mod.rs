mod diagnostics;
mod documents;
mod protocol;
mod workspace;

#[cfg(test)]
mod tests;

use self::documents::{Change, Document, Encoding, file_path};
use self::workspace::{EditorInputs, EditorSession, EditorSnapshot};
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
            if !state.obsolete(generation)
                && let Err(error) = state.publish(snapshot)
            {
                eprintln!("hirsel lsp: {error}");
                state.stop();
                return;
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
                self.phase = Phase::Running;
                protocol::result(
                    id,
                    json!({
                        "capabilities": {
                            "positionEncoding": self.encoding.name(),
                            "textDocumentSync": {"openClose": true, "change": 2, "save": true},
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
            // No request method in this slice leaves work outstanding. Push
            // diagnostics are notifications and coalesce by input generation.
            "$/cancelRequest" => {}
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
            if let Some(value) = diagnostic.lsp(&snapshot.origins, self.encoding, |path| {
                diagnostics::uri(path, &inputs.overlay)
            }) {
                by_path.entry(diagnostic.path()).or_default().push(value);
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
