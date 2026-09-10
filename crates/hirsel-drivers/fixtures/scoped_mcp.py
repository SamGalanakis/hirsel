#!/usr/bin/env python3
"""Offline MCP peer for driver tests; never an implementation of host authority.

Invoke exactly like the host: thread-tool-bridge --socket PATH --cap-file PATH.
Optional fixture configuration is read from scoped_mcp.json beside this script:
{tools: [MCP tool schemas], responses: {tool_name: JSON result}, page_size: int,
 log: absolute_path}. Capability contents are never read or logged. Each process
serves an independent connection, so preflight and provider discovery both work.
"""

import argparse
import json
from pathlib import Path
import sys


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("mode", choices=["thread-tool-bridge"])
    parser.add_argument("--socket", required=True)
    parser.add_argument("--cap-file", required=True)
    parser.parse_args()
    config_path = Path(__file__).with_name("scoped_mcp.json")
    config = json.loads(config_path.read_text()) if config_path.exists() else {}
    tools = config.get("tools", [{
        "name": "threads_context", "description": "Fixture caller context",
        "inputSchema": {"type": "object", "properties": {}, "additionalProperties": False},
    }])
    for line in sys.stdin:
        request = json.loads(line)
        method = request.get("method")
        if config.get("log"):
            with open(config["log"], "a", encoding="utf-8") as log:
                log.write(json.dumps(request) + "\n")
        if "id" not in request:
            continue
        response = {"jsonrpc": "2.0", "id": request["id"]}
        if method == "initialize":
            response["result"] = {
                "protocolVersion": request.get("params", {}).get("protocolVersion", "2024-11-05"),
                "capabilities": {"tools": {}},
                "serverInfo": {"name": "hirsel-driver-fixture", "version": "1"},
            }
        elif method == "ping":
            response["result"] = {}
        elif method == "tools/list":
            offset = int(request.get("params", {}).get("cursor", "0"))
            size = max(1, config.get("page_size", len(tools)))
            result = {"tools": tools[offset:offset + size]}
            if offset + size < len(tools):
                result["nextCursor"] = str(offset + size)
            response["result"] = result
        elif method == "tools/call":
            name = request.get("params", {}).get("name")
            if name not in [tool["name"] for tool in tools]:
                response["error"] = {"code": -32602, "message": "Unknown fixture tool"}
            else:
                result = config.get("responses", {}).get(name, {"fixture": True})
                response["result"] = {
                    "content": [{"type": "text", "text": json.dumps(result)}],
                    "isError": False,
                }
        else:
            response["error"] = {"code": -32601, "message": "Unknown MCP method"}
        print(json.dumps(response), flush=True)


if __name__ == "__main__":
    main()
