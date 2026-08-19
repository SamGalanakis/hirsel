// Delegation probe: ask the agent to spawn a sub-agent and relay its reply.
// Watches process-related frames and the final committed agent message.
import WebSocket from 'ws';

const ws = new WebSocket('ws://127.0.0.1:3045/ws');
ws.on('open', () => ws.send(JSON.stringify({ type: 'hello', token: 'dev-token', last_seen_msg_id: null })));
ws.on('error', (e) => { console.error('ws error', e.message); process.exit(2); });

let sent = false;
ws.on('message', (data) => {
  const f = JSON.parse(data.toString());
  if (f.type === 'hello_ok' && !sent) {
    sent = true;
    ws.send(JSON.stringify({
      type: 'send_message',
      client_id: 'probe-deleg-' + Date.now(),
      body: 'Diagnostic task: spawn one claude sub-agent whose task is exactly: reply with the single string DELEGATION-OK and nothing else. Wait for it to finish, then tell me verbatim what it replied.',
      ref: null, mentions: [], attachments: [], mode: 'send',
    }));
  } else if (String(f.type).includes('process')) {
    const p = f.process ?? f;
    console.log('PROCESS FRAME:', f.type, JSON.stringify({ kind: p.kind, state: p.state, status: p.status, outcome: p.outcome, label: p.label ?? p.model }).slice(0, 160));
  } else if (f.type === 'msg' && (f.message ?? {}).author === 'agent') {
    console.log('agent reply:', JSON.stringify(String(f.message.body).slice(0, 200)));
    if (/DELEGATION-OK/.test(f.message.body)) { console.log('DELEGATION VERIFIED'); process.exit(0); }
  }
});
setTimeout(() => { console.log('TIMEOUT'); process.exit(3); }, 300_000);
