#!/usr/bin/env python3
"""Scripted Claude stream-json peer. Executes the supplied real MCP bridge.

For standalone host proof, put a symlink named `claude` on an isolated PATH.
Optional CLAUDE_FIXTURE_CALLS is a JSON list of {name, arguments} calls;
otherwise the peer calls threads_context. No provider or model connection.
"""
import json
import os
import subprocess
import sys
import time
import uuid

SESSION = str(uuid.uuid4())

def send(value):
    value.setdefault("session_id", SESSION)
    print(json.dumps(value), flush=True)

def ack(value):
    send(dict(value, isReplay=True))

def option(name):
    return sys.argv[sys.argv.index(name) + 1]

def initialize():
    assert "--strict-mcp-config" in sys.argv
    assert option("--setting-sources") == ""
    assert "--bare" not in sys.argv and "--resume" not in sys.argv
    assert "--continue" not in sys.argv
    assert option("--tools") == "default"
    assert "Agent" in option("--disallowedTools").split(",")
    assert "--include-partial-messages" in sys.argv
    assert os.environ["CLAUDE_CODE_DISABLE_CLAUDE_MDS"] == "1"
    assert os.environ["CLAUDE_CODE_DISABLE_AUTO_MEMORY"] == "1"
    assert not os.environ.get("CLAUDE_CODE_SIMPLE")
    cfg = json.loads(option("--mcp-config"))
    assert set(cfg["mcpServers"]) == {"hirsel"}
    server = cfg["mcpServers"]["hirsel"]
    assert server["type"] == "stdio"
    assert server["args"][0] == "thread-tool-bridge"
    bridge = subprocess.Popen([server["command"], *server["args"]],
                              stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                              text=True)
    next_id = 0
    def rpc(method, params):
        nonlocal next_id
        next_id += 1
        bridge.stdin.write(json.dumps({"jsonrpc": "2.0", "id": next_id,
                                        "method": method, "params": params}) + "\n")
        bridge.stdin.flush()
        while True:
            line = bridge.stdout.readline()
            if not line:
                raise RuntimeError("bridge closed")
            result = json.loads(line)
            if result.get("id") == next_id:
                if "error" in result:
                    raise RuntimeError("bridge rejected request")
                return result["result"]
    rpc("initialize", {"protocolVersion": "2024-11-05", "capabilities": {},
                       "clientInfo": {"name": "scripted-claude", "version": "1"}})
    bridge.stdin.write(json.dumps({"jsonrpc": "2.0", "method": "notifications/initialized"}) + "\n")
    bridge.stdin.flush()
    tools = []
    params = {}
    while True:
        page = rpc("tools/list", params)
        tools.extend(page["tools"])
        if not page.get("nextCursor"):
            break
        params = {"cursor": page["nextCursor"]}
    names = ["mcp__hirsel__" + tool["name"] for tool in tools]
    send({"type": "system", "subtype": "init", "tools": ["Read", "Bash", *names],
          "mcp_servers": [{"name": "hirsel", "status": "connected"}], "plugins": []})
    return bridge, rpc

if __name__ == "__main__":
    bridge, rpc = initialize()
    first = json.loads(sys.stdin.readline())
    ack(first)
    calls = json.loads(os.environ.get("CLAUDE_FIXTURE_CALLS", '[{"name":"threads_context","arguments":{}}]'))
    results = [rpc("tools/call", call) for call in calls]
    text = json.dumps(results)
    send({"type":"assistant", "message":{"content":[{"type":"text","text":text}]}})
    send({"type":"result", "subtype":"success", "is_error":False, "result":text})
    bridge.stdin.close()
