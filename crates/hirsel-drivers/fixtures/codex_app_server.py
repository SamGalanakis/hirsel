
import json, os, subprocess, sys, time
mode, directory = sys.argv[1:3]
with open(directory + '/pid', 'w') as f:
    f.write(str(os.getpid()))

def send(value):
    print(json.dumps(value), flush=True)

def reply(request, result):
    send({'id': request['id'], 'result': result})

def reject(request):
    send({'id': request['id'], 'error': {'code': -32000, 'message': 'fixture rejected ' + request['method']}})

def event(method, thread='root', **params):
    if method.startswith('item/'):
        params.setdefault('turnId', 'turn-a' if thread == 'root' else 'child-turn')
        if params.get('item', {}).get('type') == 'agentMessage':
            params['item'].setdefault('phase', 'final_answer')
    send({'method': method, 'params': {'threadId': thread, **params}})

def complete(thread='root', turn='turn-a'):
    event('turn/completed', thread, turn={'id': turn, 'status': 'completed'})

initialized = False
helper = None
bridge = None
helper_id = 0

def mcp(method, params=None):
    global helper_id
    helper_id += 1
    helper.stdin.write(json.dumps({'jsonrpc': '2.0', 'id': helper_id, 'method': method, 'params': params or {}}) + '\n')
    helper.stdin.flush()
    response = json.loads(helper.stdout.readline())
    assert response['id'] == helper_id
    if 'error' in response:
        raise RuntimeError('scoped helper rejected fixture request')
    return response['result']

def scoped_config():
    flags = {'features': {'apps': False, 'plugins': False, 'multi_agent': False, 'multi_agent_v2': False, 'memories': False}, 'agents': {'enabled': False}, 'memories': {'use_memories': False, 'generate_memories': False}}
    flags['mcp_servers'] = {'owner': {'command': 'NEVER_EXECUTE', 'env': {'SECRET': 'SECRET_CANARY'}}}
    if mode == 'unsafe-config': flags['features']['apps'] = True
    return {'config': flags, 'layers': [{'config': {'mcp_servers': {'project.owner': {'command': 'NEVER_EXECUTE'}}}, 'disabledReason': 'project not trusted'}]}

pending = []
for line in sys.stdin:
    request = json.loads(line)
    with open(directory + '/requests', 'a') as f:
        f.write(json.dumps(request) + '\n')
    if 'method' not in request:
        assert request['error']['code'] == -32601
        assert request['id'] == 6
        reply(pending.pop(), {'turnId': 'turn-a'})
        continue
    method = request['method']
    if method == 'initialize':
        if mode == 'malformed':
            print('{broken-json', flush=True)
            time.sleep(60)
        if mode == 'hang-startup':
            time.sleep(60)
        if mode == 'reject-initialize':
            reject(request)
            continue
        if mode == 'stderr':
            os.write(2, b'x' * 262144)
        reply(request, {'userAgent': 'hermetic-codex'})
    elif method == 'initialized':
        initialized = True
    elif method == 'config/read':
        assert request['params']['includeLayers'] is True
        assert request['params']['cwd'] == directory
        config_response = scoped_config()
        if mode == 'missing-layers': del config_response['layers']
        if mode == 'unsafe-config-warning':
            send({'method': 'configWarning', 'params': {'summary': 'SECRET_CANARY'}})
            config_response['config']['agents']['enabled'] = True
        reply(request, config_response)
    elif method == 'thread/start':
        assert initialized, 'thread/start before initialized'
        if mode == 'reject-thread':
            reject(request)
            continue
        config = request['params']['config']
        assert config['mcp_servers']['owner']['enabled'] is False
        assert config['mcp_servers']['project.owner']['enabled'] is False
        assert config['agents.enabled'] is False
        for name, spec in config['mcp_servers'].items():
            if spec.get('enabled'):
                bridge = name
                helper = subprocess.Popen([spec['command'], *spec['args']], stdin=subprocess.PIPE, stdout=subprocess.PIPE, text=True)
                with open(directory + '/helper-pid', 'w') as f: f.write(str(helper.pid))
                mcp('initialize', {'protocolVersion': '2024-11-05', 'capabilities': {}, 'clientInfo': {'name': 'codex-fixture', 'version': '1'}})
                helper.stdin.write(json.dumps({'jsonrpc': '2.0', 'method': 'notifications/initialized'}) + '\n')
                helper.stdin.flush()
        assert helper is not None
        send({'method': 'thread/started', 'params': {'thread': {'id': 'child'}}})
        reply(request, {'thread': {'id': 'root'}})
    elif method == 'mcpServerStatus/list':
        assert request['params']['threadId'] == 'root'
        catalog = mcp('tools/list')['tools']
        if mode == 'wrong-tools': catalog.append({'name': 'global_owner', 'inputSchema': {'type': 'object'}})
        row = {'name': bridge, 'runtimeStatus': 'connected', 'tools': {t['name']: t for t in catalog}, 'resources': [], 'resourceTemplates': [], 'authStatus': 'unsupported'}
        foreign = {'name': 'owner', 'runtimeStatus': 'connected' if mode == 'unsafe-status' else 'disabled', 'tools': {}, 'resources': [], 'resourceTemplates': [], 'authStatus': 'unsupported'}
        if mode == 'null-status': foreign['runtimeStatus'] = None
        if mode == 'paged-status':
            if request['params'].get('cursor') is None:
                reply(request, {'data': [foreign], 'nextCursor': 'page-2'})
            else:
                assert request['params']['cursor'] == 'page-2'
                reply(request, {'data': [row], 'nextCursor': None})
        elif mode == 'bad-cursor':
            reply(request, {'data': [], 'nextCursor': 'repeat'})
        elif mode == 'missing-bridge':
            reply(request, {'data': [foreign], 'nextCursor': None})
        else:
            reply(request, {'data': [foreign, row], 'nextCursor': None})
    elif method == 'turn/start':
        assert request['params']['threadId'] == 'root'
        if mode == 'reject-turn':
            reject(request)
            continue
        reply(request, {'turn': {'id': 'turn-a'}})
        event('turn/started', turn={'id': 'turn-a'})
        if mode in ['commentary-eof', 'commentary-failed', 'unknown-eof', 'unknown-failed', 'commentary-done']:
            phase = None if mode.startswith('unknown') else 'commentary'
            event('item/completed', item={'type': 'agentMessage', 'phase': phase, 'text': 'intermediate planning'})
            if mode.endswith('eof'): sys.exit(0)
            event('turn/completed', turn={'id': 'turn-a', 'status': 'completed' if mode.endswith('done') else 'failed'})
            continue
        if mode == 'malformed-completion':
            event('turn/completed', turn={'status': 'completed'})
            continue
        if mode == 'child':
            event('turn/started', 'child', turn={'id': 'child-turn'})
            event('item/completed', 'child', item={'type': 'agentMessage', 'text': 'CHILD RESULT'})
            complete('child', 'child-turn')
            complete('root', 'stale-root-turn')
            send({'method': 'turn/completed', 'params': {'turn': {'id': 'turn-a', 'status': 'completed'}}})
            event('item/completed', item={'type': 'agentMessage', 'text': 'root barrier'})
        if mode == 'closed-stdin-final':
            os.close(0)
            send({'id': 'closing-native-request', 'method': 'item/tool/requestUserInput', 'params': {'threadId': 'root'}})
            time.sleep(0.05)
            event('item/completed', item={'type': 'agentMessage', 'text': 'last root result'})
            complete()
            sys.exit(0)
        if mode == 'reply-backpressure':
            descendant = subprocess.Popen([sys.executable, '-c', 'import time;time.sleep(60)'])
            with open(directory + '/descendant', 'w') as f:
                f.write(str(descendant.pid))
            while not os.path.exists(directory + '/send-native-request'):
                time.sleep(0.001)
            send({'id': 'native-request', 'method': 'item/tool/requestUserInput', 'params': {'threadId': 'root'}})
            while not os.path.exists(directory + '/exit-now'):
                time.sleep(0.001)
            sys.exit(0)
        if mode in ['exit-zero', 'inherited-pipes']:
            if mode == 'inherited-pipes':
                descendant = subprocess.Popen([sys.executable, '-c', 'import time;time.sleep(60)'])
                with open(directory + '/descendant', 'w') as f:
                    f.write(str(descendant.pid))
            sys.exit(0)
        if mode == 'scoped-recursive':
            context = mcp('tools/call', {'name': 'threads_context', 'arguments': {}})
            child = mcp('tools/call', {'name': 'threads_delegate', 'arguments': {'title': 'grandchild', 'brief': 'focused work', 'artifact_ids': []}})
            mcp('tools/call', {'name': 'threads_report', 'arguments': {'summary': 'grandchild accepted', 'artifact_ids': []}})
            event('item/completed', item={'type': 'agentMessage', 'text': json.dumps({'context': context, 'child': child})})
            complete()
            sys.exit(0)
        if mode in ['missing-status', 'invalid-status', 'empty-done', 'long-output', 'failed-final']:
            if mode in ['long-output', 'failed-final']:
                event('item/completed', item={'type': 'agentMessage', 'text': 'z' * 30000})
            turn = {'id': 'turn-a'}
            if mode != 'missing-status': turn['status'] = 'invalid' if mode == 'invalid-status' else ('failed' if mode == 'failed-final' else 'completed')
            event('turn/completed', turn=turn)
            sys.exit(0)
        if mode in ['done', 'paged-status']:
            event('item/completed', item={'type': 'agentMessage', 'text': 'root result'})
            complete()
            sys.exit(0)
    elif method == 'turn/steer':
        assert request['params']['threadId'] == 'root'
        assert request['params']['expectedTurnId'] == 'turn-a'
        if mode == 'hang-control':
            continue
        if mode == 'close-control':
            sys.exit(0)
        if mode == 'control-errors':
            reject(request)
        elif mode == 'out-of-order':
            pending.append(request)
        elif mode == 'server-request':
            pending.append(request)
            send({'id': request['id'], 'method': 'item/tool/requestUserInput', 'params': {'threadId': 'root'}})
        elif mode == 'wrong-steer-turn':
            reply(request, {'turnId': 'unexpected-turn'})
        else:
            reply(request, {'turnId': 'turn-a'})
    elif method == 'turn/interrupt':
        assert request['params']['threadId'] == 'root'
        assert request['params']['turnId'] == 'turn-a'
        if mode == 'control-errors':
            reject(request)
        elif mode == 'out-of-order':
            pending.append(request)
        elif mode == 'interrupt-ack-only':
            reply(request, {})
        else:
            reply(request, {})
            event('item/completed', item={'type': 'agentMessage', 'text': 'ROOT RESULT'})
            complete()
    if len(pending) == 2:
        for waiting in reversed(pending):
            if waiting['method'] == 'turn/steer':
                reject(waiting)
            else:
                reply(waiting, {})
        complete()
        pending.clear()
