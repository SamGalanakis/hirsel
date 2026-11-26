// Run at most three controls per land. Each patch is applied only to a fresh
// temporary runtime; the production source and shared generated codec stay intact.
import assert from 'node:assert/strict';
import {pathToFileURL} from 'node:url';
import {readFile} from 'node:fs/promises';

function replace(text, from, to) {
  assert.equal(text.split(from).length,2,'mutation must name exactly one source site');
  return text.replace(from,to);
}
export const controls = {
  replay_under_new_principal: ['lost_stream_fences_generation', text => replace(text,
    "        await this.stop('Fenced', 'LocallyUnknown'); return;\n      }\n      return await fn(state);",
    "        Object.assign(this, {generation: state.generation});\n        state.directory.push(this.stream); state.streams[this.stream] = structuredClone(this.record);\n        await this.options.storage.write(state);\n      }\n      return await fn(state);")],
  shared_stream: ['stream_lease', text => replace(text,
    'lease = await options.storage.lock(streamLock(state, n), false);',
    'lease = await options.storage.lock(streamLock(state, n), false);\n        if (!lease) lease = {lost: new Promise<void>(() => {}), release: async () => {}};')],
  new_work_before_recovery: ['stream_lease', text => replace(text,
    "    if (this.phase.tag !== 'Ready' || this.record.pending) return;",
    "    if (this.phase.tag !== 'Ready') return;")],
  treat_as_superseded: ['history_unavailable_retires', text => replace(text,
    "            else await this.retire('LocallyUnknown');",
    "            else await this.completeCall({tag: 'LocallyUnknown'}, true, false);")],
  drop_pending_answer: ['history_unavailable_retires', text => replace(text,
    "            else await this.retire('LocallyUnknown');",
    "            else continue;")],
  retire_on_stale: ['history_unavailable_retires', text => replace(text,
    '      if (!pending?.inflight || pending.http || row.stream.t !== id?.t || row.stream.n !== id.n || row.p !== pending.p) continue;',
    "      if (row.outcome.tag === 'Refused' && row.outcome.value.tag === 'HistoryUnavailable' && !pending) { await this.stop('Retired'); continue; }\n      if (!pending?.inflight || pending.http || row.stream.t !== id?.t || row.stream.n !== id.n || row.p !== pending.p) continue;")],
  no_directory: ['lost_stream_fences_generation', text => {
    text=replace(text,'if (!state || !intact(state)) {\n        state = fresh', 'if (!state) {\n        state = fresh');
    return replace(text,'if (state.streams[n].retired) continue;',
      'if (!state.streams[n]) state.streams[n] = emptyStream();\n        if (state.streams[n].retired) continue;');
  }],
  advance_on_refusal: ['refused_p_reused', text => replace(text,
    'if (advance && pending.p !== maxU64) record.next = pending.p + 1n;',
    'if (pending.p !== maxU64) record.next = pending.p + 1n;')],
  send_then_commit: ['commit_before_send', text => replace(text,
    '        record.pending = pending;\n        if (mint)',
    "        record.pending = pending;\n        this.socket!.send(this.options.frame.encode({tag: 'Call', values: [record.id!, pending.p, pending.slot, 30000, pending.args]}));\n        if (mint)")],
  backoff_reset_at_welcome: ['backoff_requires_stable_session', text => replace(text,
    'const accepted = {...phase, credential:',
    'const accepted = {...phase, backoff: initialBackoff, credential:'), 'state'],
  resend_on_mismatch: ['request_mismatch_fences', text => replace(text,
    "if (refusal.tag === 'RequestMismatch') await this.fence('RequestMismatch');",
    "if (refusal.tag === 'RequestMismatch') await this.retryPending(true);")],
  advance_on_mismatch: ['request_mismatch_fences', text => replace(text,
    "if (refusal.tag === 'RequestMismatch') await this.fence('RequestMismatch');",
    "if (refusal.tag === 'RequestMismatch') await this.completeCall({tag:'RequestMismatch'}, true, false);")],
};

export async function mutations(codec,names) {
  const {laws}=await import('./client.mjs');
  assert(names.length>0 && names.length<=3,'at most three mutation controls per land');
  for(const name of names) {
    const control=controls[name];assert(control,`unknown control ${name}`);
    const target=control[2]??'client';
    const source=await readFile(new URL(`../../../../sdk/client/${target}.ts`,import.meta.url),'utf8');
    const patched=control[1](source);
    let applied=false;
    await assert.rejects(laws(codec,control[0],(module,text)=>{
      if(module!==target)return text;
      applied=true;return patched;
    }),error=>error instanceof assert.AssertionError);
    assert(applied);console.log(`${name}: CAUGHT by ${control[0]}`);
  }
  return {cases:names.length};
}

export function optimisticLaws({setup,ready,want,bytes,row,group,batch,settle,returned,entries,savedStream}) {
  async function board(second=false) {
    const h=await setup(); await ready(h);
    const subscribe=()=>{
      const changes=[];
      const sub=h.client.subscribe(want,bytes(1),(handle,change)=>{
        changes.push({change, image:handle.map(0), value:handle.value()});
      });
      return {sub,changes};
    };
    const left=subscribe(), right=second ? subscribe() : undefined;
    await settle();
    const emit=async(version,complete,groups,outcomes=[])=>{
      h.server.emit(h.server.sockets.at(-1),batch(BigInt(version),complete,groups,outcomes));
      await settle();
    };
    const done=(p,outcome=returned(8))=>({stream:savedStream(h.storage.raw).id,p:BigInt(p),outcome});
    await emit(1,second ? [0,1] : [0],
      [group(0,[row(0,1,10)],bytes(4)),...(second ? [group(1,[row(0,1,20)])] : [])]);
    const call=optimistic=>h.client.call(want,bytes(8),x=>x,{optimistic});
    return {...h,...left,right,emit,done,call};
  }
  const at=(sub,k=1)=>sub.get(0,bytes(k)).value?.value.value[0];
  // Holds the next storage.write until released; a failing law still releases it
  // so closing the client does not wait behind the held write.
  async function holdingWrite(storage,law) {
    let release=()=>{};
    storage.beforeWrite=()=>{storage.beforeWrite=undefined;return new Promise(r=>{release=r;});};
    try { await law(()=>release()); } finally { release(); }
  }

  return {
    async 'L-D3a_queue_order'() {
      const {sub,call,server,emit,done}=await board();
      const first=call(draft=>draft.put(sub,0,bytes(1),row(0,1,11)));
      const second=call(draft=>{
        const previous=draft.get(sub,0,bytes(1));
        if (previous) draft.put(sub,0,bytes(2),row(0,2,previous.value.value[0]+1));
      });
      const third=call(draft=>draft.put(sub,0,bytes(3),row(0,3,13)));
      await settle();
      assert.equal(server.calls.length,1);
      assert.deepEqual([at(sub),at(sub,2),at(sub,3)],[11,12,13]);
      await emit(2,[],[group(0,[row(0,1,11)])],[done(0)]);
      await emit(3,[],[group(0,[row(0,2,12)])],[done(1)]);
      await emit(4,[],[group(0,[row(0,3,13)])],[done(2)]);
      assert.deepEqual((await Promise.all([first,second,third])).map(x=>x.tag),Array(3).fill('Returned'));
    },
    async 'L-D3b_atomic_confirmation'() {
      const {sub,changes,call,emit,done}=await board();
      const result=call(draft=>{
        draft.put(sub,0,bytes(1),row(0,1,11));
        draft.put(sub,0,bytes(2),row(0,2,30));
      });
      await settle();
      assert.deepEqual([at(sub),at(sub,2)],[11,30]);
      changes.length=0;
      await emit(2,[],[group(0,[row(0,1,11),row(0,2,31)])],[done(0)]);
      assert.equal((await result).tag,'Returned');
      assert.deepEqual([at(sub),at(sub,2)],[11,31]);
      assert.equal(changes.length,1);
      assert.deepEqual(changes[0].change.rows,[{slot:0,key:'02',old:row(0,2,30),new:row(0,2,31)}]);
      assert.deepEqual(changes[0].image.value,sub.map(0).value);

      const equal=call(draft=>draft.put(sub,0,bytes(1),row(0,1,12)));
      await settle();
      const visible=sub.rows(0).value;
      changes.length=0;
      await emit(3,[],[group(0,[row(0,1,12)])],[done(1)]);
      assert.equal((await equal).tag,'Returned');
      assert.equal(changes.length,0);
      assert.strictEqual(sub.rows(0).value,visible);

      const h=await setup(undefined,{wants:entries.want,kinds:entries.kind});
      await ready(h);
      const api=entries.bind(h.client), decodedChanges=[];
      const decoded=api.subscribeDecoded.main.rows(3,(_,change)=>decodedChanges.push(change));
      await settle();
      const send=async frame=>{h.server.emit(h.server.sockets[0],frame);await settle();};
      await send(batch(1n,[0],[group(0,[])]));
      assert.deepEqual(decoded.rows('rows'),{tag:'Ready',value:[]});
      decodedChanges.length=0;
      const inserted={slot:'rows',key:7n,value:{tag:'Some',value:bytes(9)}};
      const confirmation=api.call.main.toggle(8n,{optimistic(draft){
        draft.put(decoded,'rows',7n,inserted);
      }});
      await settle();
      const index=decoded.map('rows').value;
      assert.deepEqual([...index],[['0000000000000007',inserted]]);
      assert.deepEqual(decodedChanges[0].rows,[{slot:'rows',key:'0000000000000007',new:inserted}]);
      const wire={slot:0,key:bytes(0,0,2,31,0,0,0,0,0,0,0,7),value:{tag:'Some',value:bytes(1,0,0,0,9)}};
      const outcome={tag:'Returned',value:bytes(8,0,0,0,0,0,0,0)};
      await send(batch(2n,[],[group(0,[wire])],[{stream:savedStream(h.storage.raw).id,p:0n,outcome}]));
      assert.deepEqual(await confirmation,{tag:'Returned',value:8n});
      assert.equal(decodedChanges.length,1,'confirming an empty-slot insert must emit no second change');
      assert.strictEqual(decoded.map('rows').value,index);
      assert.deepEqual(decoded.get('rows',7n),{tag:'Ready',value:inserted});
    },
    async 'L-D3c_rebase'() {
      const {sub,changes,call,emit,done}=await board();
      const result=call(draft=>{
        const previous=draft.get(sub,0,bytes(1));
        if (previous) draft.put(sub,0,bytes(1),row(0,1,previous.value.value[0]+1));
      });
      await settle();
      assert.equal(at(sub),11);
      changes.length=0;
      await emit(2,[],[group(0,[row(0,1,40)])]);
      assert.equal(at(sub),41);
      assert.equal(changes.length,1);
      assert.deepEqual(changes[0].change.rows,[{slot:0,key:'01',old:row(0,1,11),new:row(0,1,41)}]);
      await emit(3,[],[group(0,[row(0,1,41)])],[done(0)]);
      assert.equal((await result).tag,'Returned');
      assert.equal(changes.length,1);
    },
    async 'L-D3d_failed_and_throwing_overlays'() {
      const {sub,changes,diagnostics,call,emit,done}=await board();
      const result=call(draft=>{
        draft.delete(sub,0,bytes(1));
        assert.equal(draft.get(sub,0,bytes(1)),undefined);
        draft.value(sub,bytes(9));
      });
      await settle();
      assert.equal(at(sub),undefined);
      assert.deepEqual(sub.value().value,bytes(9));
      const throwing=call(draft=>{
        draft.put(sub,0,bytes(1),row(0,1,99));
        throw new Error('optimistic recipe rejected');
      });
      await settle();
      assert.equal(at(sub),undefined);
      assert.equal(diagnostics.filter(d=>d.includes('optimistic recipe rejected')).length,1);
      changes.length=0;
      const failed={tag:'Failed',value:{tag:'Raised',value:1}};
      await emit(2,[],[],[done(0,failed)]);
      assert.deepEqual(await result,failed);
      assert.equal(changes.length,1);
      assert.deepEqual(changes[0].image.value.get('01'),row(0,1,10));
      assert.deepEqual(changes[0].value.value,bytes(4));
      await emit(3,[],[],[done(1)]);
      assert.equal((await throwing).tag,'Returned');
      assert.equal(changes.length,1);
    },
    async 'L-D3f_overlay_on_calling_tick'() {
      // An earlier call holds the queue on its durable write; the next call's
      // overlay is still visible and notified before the caller's tick ends.
      const {sub,changes,call,server,storage,emit,done}=await board();
      await holdingWrite(storage,async release=>{
        const first=call();
        await settle();
        assert.equal(server.calls.length,0,'the first call must be waiting on storage.write');
        changes.length=0;
        const second=call(draft=>draft.put(sub,0,bytes(1),row(0,1,12)));
        for (const pending of [first,second]) void pending.catch(()=>{});
        assert.equal(at(sub),12);
        assert.deepEqual(changes.map(({change})=>change.rows),[[{slot:0,key:'01',old:row(0,1,10),new:row(0,1,12)}]]);
        release();await settle();
        assert.equal(server.calls.length,1);
        await emit(2,[],[],[done(0)]);
        await emit(3,[],[group(0,[row(0,1,12)])],[done(1)]);
        assert.deepEqual((await Promise.all([first,second])).map(x=>x.tag),['Returned','Returned']);
        assert.equal(at(sub),12);
      });
    },
    async 'L-D3h_overlay_during_outcome_commit'() {
      // A Batch is persisting the earlier call's outcome when the next call starts:
      // its overlay and change still arrive on the calling tick, and the batch then
      // confirms the first overlay with a single change.
      const {sub,changes,call,server,storage,emit,done}=await board();
      const first=call(draft=>draft.put(sub,0,bytes(2),row(0,2,20)));
      await settle();
      assert.equal(server.calls.length,1);
      await holdingWrite(storage,async release=>{
        void emit(2,[],[group(0,[row(0,2,20)])],[done(0)]);
        await settle();
        changes.length=0;
        const second=call(draft=>draft.put(sub,0,bytes(1),row(0,1,12)));
        for (const pending of [first,second]) void pending.catch(()=>{});
        assert.equal(at(sub),12);
        assert.deepEqual(changes.map(({change})=>change.rows),[[{slot:0,key:'01',old:row(0,1,10),new:row(0,1,12)}]]);
        release();await settle();
        assert.equal((await first).tag,'Returned');
        assert.equal(changes.length,1,'confirming the first overlay must be silent');
        assert.deepEqual([at(sub),at(sub,2)],[12,20]);
        await emit(3,[],[group(0,[row(0,1,12)])],[done(1)]);
        assert.equal((await second).tag,'Returned');
      });
    },
    async 'L-D3g_refused_send_drops_early_overlay'() {
      // The earlier call's send fails, so the client backs off before the next
      // call reaches the queue: that call is refused and its overlay must not
      // return on the resubscribed image.
      const {sub,call,server,storage,time,emit}=await board();
      await holdingWrite(storage,async release=>{
        const first=call();
        void first.catch(()=>{});
        await settle();
        server.sockets.at(-1).send=()=>{throw new Error('socket refused the Call');};
        const second=call(draft=>draft.put(sub,0,bytes(1),row(0,1,12)));
        const refused=assert.rejects(second,error=>error.code==='Backoff');
        assert.equal(at(sub),12);
        release();await settle();
        await refused;
        await time.advance(50);
        await emit(1,[1],[group(1,[row(0,1,10)],bytes(4))]);
        assert.equal(sub.current(),true);
        assert.equal(at(sub),10);
      });
    },
    async 'L-D3e_early_done_after_loss'() {
      const {sub,right,changes,call,server,time,emit,done}=await board(true);
      const result=call(draft=>{
        draft.put(sub,0,bytes(1),row(0,1,11));
        if (draft.get(right.sub,0,bytes(1))) draft.put(right.sub,0,bytes(1),row(0,1,21));
      });
      await settle();
      assert.deepEqual([at(sub),at(right.sub)],[11,21]);
      changes.length=0;
      server.close(server.sockets[0],1001);
      assert.deepEqual(sub.rows(0),{tag:'Pending'});
      assert.equal(sub.current(),false);
      assert.deepEqual(changes.map(({change})=>change),[{invalidated:true,replaced:false,rows:[]}]);
      await settle(); await time.advance(50);
      assert.equal(server.calls.length,2);
      await emit(1,[],[],[done(0)]);
      assert.equal((await result).tag,'Returned');
      assert.deepEqual(sub.get(0,bytes(1)),{tag:'Pending'});
      assert.equal(changes.length,1);
      await emit(2,[2],[group(2,[row(0,1,12)],bytes(4))]);
      assert.equal(sub.current(),true);
      assert.equal(right.sub.current(),false);
      assert.equal(at(sub),11,'the recovered overlay must survive until both subscriptions are current');
      assert.deepEqual(changes[1].image.value.get('01'),row(0,1,11));
      await emit(3,[3],[group(3,[row(0,1,21)])]);
      assert.deepEqual([at(sub),at(right.sub)],[12,21]);
      assert.deepEqual(changes.slice(1).map(({image})=>image.value.get('01').value.value[0]),[11,12]);
    },
  };
}

if(process.argv[1] && import.meta.url===pathToFileURL(process.argv[1]).href) {
  const {Frame}=await import(pathToFileURL(process.argv[2]).href);
  console.log(await mutations(Frame,process.argv.slice(3)));
}
