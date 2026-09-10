#!/usr/bin/env python3
"""Hermetic supplied host executable; never contacts a provider or real host."""
import json
import pathlib
import sys

assert sys.argv[1] == 'thread-tool-bridge'
assert sys.argv[2] == '--socket' and sys.argv[4] == '--cap-file'
capability = pathlib.Path(sys.argv[5])
assert capability.read_text() == 'fixture-capability'
directory = capability.parent
names = ['threads_context', 'threads_delegate', 'threads_report']
for line in sys.stdin:
    request = json.loads(line)
    if 'id' not in request:
        continue
    method = request['method']
    if method == 'initialize':
        result = {'protocolVersion': '2024-11-05', 'capabilities': {'tools': {}}, 'serverInfo': {'name': 'scoped-fixture', 'version': '1'}}
    elif method == 'tools/list':
        result = {'tools': [{'name': name, 'inputSchema': {'type': 'object'}} for name in names]}
    elif method == 'tools/call':
        name = request['params']['name']
        assert name in names
        arguments = request['params']['arguments']
        if name in ['threads_delegate', 'threads_report']:
            assert arguments['artifact_ids'] == []
        if name == 'threads_delegate':
            assert arguments['title'] == 'grandchild' and arguments['brief'] == 'focused work'
        if name == 'threads_report':
            assert arguments['summary'] == 'grandchild accepted'
        with (directory / 'tool-calls').open('a') as log:
            log.write(json.dumps(request['params']) + '\n')
        value = {'thread_id': 17} if name == 'threads_context' else {'thread_id': 18, 'turn_id': 23}
        result = {'content': [{'type': 'text', 'text': json.dumps(value)}], 'isError': False}
    else:
        raise RuntimeError('unexpected MCP method')
    print(json.dumps({'jsonrpc': '2.0', 'id': request['id'], 'result': result}), flush=True)
