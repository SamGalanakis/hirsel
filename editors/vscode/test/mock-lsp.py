#!/usr/bin/env python3
"""Stdio peer for the editor smoke tests. Run through a symlink named hirsel."""

import json
import os
from pathlib import Path
import sys
from urllib.parse import unquote, urlparse

POINT = {"line": 0, "character": 0}
FIRST_WORD = {"start": POINT, "end": {"line": 0, "character": 2}}
CAPABILITIES = {
    "textDocumentSync": {"openClose": True, "change": 1},
    "hoverProvider": True,
    "definitionProvider": True,
    "documentFormattingProvider": True,
}


def receive():
    headers = {}
    while line := sys.stdin.buffer.readline():
        if not line.strip():
            return json.loads(sys.stdin.buffer.read(int(headers["content-length"])))
        key, value = line.decode("ascii").split(":", 1)
        headers[key.lower()] = value.strip()
    return None


def send(**message):
    body = json.dumps({"jsonrpc": "2.0", **message}).encode()
    sys.stdout.buffer.write(f"Content-Length: {len(body)}\r\n\r\n".encode() + body)
    sys.stdout.buffer.flush()


def record(**entry):
    with open(os.environ["HIRSEL_MOCK_LOG"], "a", encoding="utf-8") as log:
        log.write(json.dumps(entry) + "\n")


def serve():
    argv = [Path(sys.argv[0]).name, *sys.argv[1:]]
    assert argv == ["hirsel", "lsp"], argv
    record(argv=argv)

    documents = {}
    while (message := receive()) is not None:
        method = message.get("method")
        params = message.get("params") or {}
        document = params.get("textDocument", {})
        uri = document.get("uri")

        if method:
            record(method=method, uri=uri, root=params.get("rootUri"), changes=params.get("changes"))
        match method:
            case "initialize":
                result = {"capabilities": CAPABILITIES}
            case "textDocument/didOpen":
                assert document["languageId"] == "hirsel"
                documents[uri] = document["text"]
            case "textDocument/didChange":
                [change] = params["contentChanges"]
                assert "range" not in change
                documents[uri] = change["text"]
                send(method="textDocument/publishDiagnostics", params={
                    "uri": uri,
                    "version": document["version"],
                    "diagnostics": [{
                        "range": FIRST_WORD, "severity": 2,
                        "code": "MOCK-1", "message": "Scripted diagnostic after edit",
                    }],
                })
            case "textDocument/didClose":
                documents.pop(uri, None)
            case "textDocument/hover":
                assert uri in documents
                result = {"contents": {"kind": "markdown", "value": "`u8`"}}
            case "textDocument/definition":
                assert uri in documents
                target = Path(unquote(urlparse(uri).path)).with_name("target.hirsel")
                result = {"uri": target.as_uri(), "range": FIRST_WORD}
            case "textDocument/formatting":
                result = []
                if documents[uri].startswith("fn  "):
                    result.append({"range": {
                        "start": {"line": 0, "character": 3},
                        "end": {"line": 0, "character": 4},
                    }, "newText": ""})
            case "shutdown":
                result = None
            case "exit":
                return
            case _:
                if "id" in message:
                    send(id=message["id"], error={"code": -32601, "message": str(method)})
                continue
        if "id" in message:
            send(id=message["id"], result=result)


if __name__ == "__main__":
    if sys.argv[1:] == ["--version"]:
        print("hirsel 0.0.1-test")
    else:
        serve()
