// Run at most three controls per land. Each patch is applied only to a fresh
// temporary runtime; the production source and shared generated codec stay intact.
import assert from 'node:assert/strict';
import {laws} from './client.mjs';
import {pathToFileURL} from 'node:url';
import {readFile} from 'node:fs/promises';

function replace(text, from, to) {
  assert.equal(text.split(from).length,2,'mutation must name exactly one source site');
  return text.replace(from,to);
}
export const controls = {
  replay_under_new_principal: ['lost_stream_fences_generation', text => replace(text,
    "if (state.generation !== this.generation || !stream || !sameCaller(stream.caller, this.record.caller)) {\n        await this.stop('Fenced', 'LocallyUnknown'); return;\n      }",
    "if (state.generation !== this.generation || !stream || !sameCaller(stream.caller, this.record.caller)) {\n        Object.assign(this, {generation: state.generation}); \n        const record = structuredClone(this.record); if (record.pending) record.pending.generation = state.generation;\n        state.streams[this.stream] = record; this.record = record; await this.options.storage.write(state);\n      }")],
  shared_stream: ['stream_lease', text => replace(text,
    'lease = await options.storage.lock(`stream-${state.generation}-${n}`, false);',
    'lease = await options.storage.lock(`stream-${state.generation}-${n}`, false);\n        if (!lease) lease = {lost: new Promise<void>(() => {}), release: async () => {}};')],
  new_work_before_recovery: ['stream_lease', text => replace(text,
    "      if (this.record.pending) throw new ClientError('RecoveringOrBusy');\n", '')],
  treat_as_superseded: ['history_unavailable_retires', text => replace(text,
    "if (outcome.tag === 'Refused' && outcome.value.tag === 'HistoryUnavailable') { await this.retire('LocallyUnknown'); continue; }",
    "if (outcome.tag === 'Refused' && outcome.value.tag === 'HistoryUnavailable') { await this.installation(async state => { const record = state.streams[this.stream]; record.next = pending.p + 1n; delete record.pending; await this.options.storage.write(state); this.record = structuredClone(record); this.finish({tag: 'LocallyUnknown'}); }); continue; }")],
  drop_pending_answer: ['history_unavailable_retires', text => replace(text,
    "if (outcome.tag === 'Refused' && outcome.value.tag === 'HistoryUnavailable') { await this.retire('LocallyUnknown'); continue; }",
    "if (outcome.tag === 'Refused' && outcome.value.tag === 'HistoryUnavailable') { continue; }")],
  retire_on_stale: ['history_unavailable_retires', text => replace(text,
    'if (!pending || row.stream !== this.stream || row.p !== pending.p) continue;',
    "if (row.stream === this.stream && row.outcome.tag === 'Refused' && row.outcome.value.tag === 'HistoryUnavailable' && (!pending || row.p !== pending.p)) { await this.retire('LocallyUnknown'); continue; }\n      if (!pending || row.stream !== this.stream || row.p !== pending.p) continue;")],
  no_directory: ['lost_stream_fences_generation', text => {
    text=replace(text,'if (!state || !intact(state)) {\n        const generation', 'if (!state) {\n        const generation');
    return replace(text,'if (state.streams[n].retired) continue;',
      'if (!state.streams[n]) state.streams[n] = {next: 0n, retired: false};\n        if (state.streams[n].retired) continue;');
  }],
  advance_on_refusal: ['refused_p_reused', text => replace(text,
    'record.terminal = {p: pending.p, outcome: structuredClone(outcome)};\n          delete record.pending;\n          await this.options.storage.write(state);',
    'record.terminal = {p: pending.p, outcome: structuredClone(outcome)};\n          record.next = pending.p + 1n;\n          delete record.pending;\n          await this.options.storage.write(state);')],
  send_then_commit: ['commit_before_send', text => replace(text,
    'record.pending = pending;\n        await this.options.storage.write(state);',
    "record.pending = pending;\n        this.socket!.send(this.options.frame.encode({tag: 'Call', values: [this.stream, pending.p, pending.slot, 30000, pending.args]}));\n        await this.options.storage.write(state);")],
  backoff_reset_at_welcome: ['backoff_requires_stable_session', text => replace(text,
    'this.welcomed = true; this.welcomedAt = this.clock.now();',
    'this.welcomed = true; this.welcomedAt = this.clock.now(); this.backoff = 100;')],
  resend_on_mismatch: ['request_mismatch_fences', text => replace(text,
    "if (outcome.tag === 'Refused' && outcome.value.tag === 'RequestMismatch') { await this.fence('RequestMismatch'); continue; }",
    "if (outcome.tag === 'Refused' && outcome.value.tag === 'RequestMismatch') { this.sent = false; this.timer = this.clock.set(() => { this.timer = undefined; void this.enqueue(async () => this.sendPending()); }, 100); continue; }")],
  advance_on_mismatch: ['request_mismatch_fences', text => replace(text,
    "if (outcome.tag === 'Refused' && outcome.value.tag === 'RequestMismatch') { await this.fence('RequestMismatch'); continue; }",
    "if (outcome.tag === 'Refused' && outcome.value.tag === 'RequestMismatch') { await this.installation(async state => { const record = state.streams[this.stream]; record.next = pending.p + 1n; delete record.pending; await this.options.storage.write(state); this.record = structuredClone(record); this.finish({tag: 'RequestMismatch'}); }); continue; }")],
};

export async function mutations(codec,names) {
  assert(names.length>0 && names.length<=3,'at most three mutation controls per land');
  for(const name of names) {
    const control=controls[name];assert(control,`unknown control ${name}`);
    const source=await readFile(new URL('../../../../sdk/client/client.ts',import.meta.url),'utf8');
    const patched=control[1](source);
    let applied=false;
    await assert.rejects(laws(codec,control[0],(module,text)=>{
      if(module!=='client')return text;
      applied=true;return patched;
    }),error=>error instanceof assert.AssertionError);
    assert(applied);console.log(`${name}: CAUGHT by ${control[0]}`);
  }
  return {cases:names.length};
}
if(process.argv[1] && import.meta.url===pathToFileURL(process.argv[1]).href) {
  const {Frame}=await import(pathToFileURL(process.argv[2]).href);
  console.log(await mutations(Frame,process.argv.slice(3)));
}
