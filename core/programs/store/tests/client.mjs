import assert from 'node:assert/strict';
import {mkdtemp, readFile, writeFile, rm} from 'node:fs/promises';
import {tmpdir} from 'node:os';
import {join} from 'node:path';
import {pathToFileURL, fileURLToPath} from 'node:url';
import {stripTypeScriptTypes} from 'node:module';
import {spawn} from 'node:child_process';

const source = new URL('../../../../sdk/client/', import.meta.url);
const bytes = (...values) => Uint8Array.of(...values);
const want = {identity: bytes(1), args_fp: 1n, result_fp: 2n};
const other = {identity: bytes(2), args_fp: 3n, result_fp: 4n};
const credential = bytes(...Array(16).fill(1));
const caller = {issuer:2,subject:bytes(...Array(16).fill(7))};
const initial = () => ({version:3,generation:1n,directory:[],nextStream:0,streams:{}});
const returned = n => ({tag:'Returned',value:bytes(n)});
const refused = tag => ({tag:'Refused',value:{tag}});
const call = (stream,p,arg,slot=0,deadline=30000) => ({tag:'Call',values:[stream,p,slot,deadline,bytes(arg)]});
const settle = async () => { for (let i=0;i<40;i++) await new Promise(resolve => setImmediate(resolve)); };

class Time {
  time=0; serial=0; timers=new Map();
  now=()=>this.time;
  random=()=>0.5;
  set=(fn,ms)=>{const id=++this.serial;this.timers.set(id,{fn,at:this.time+ms});return id;};
  clear=id=>this.timers.delete(id);
  async advance(ms) {
    const end=this.time+ms;
    for (;;) {
      const next=[...this.timers].filter(([,t])=>t.at<=end).sort((a,b)=>a[1].at-b[1].at)[0];
      if (!next) break;
      this.time=next[1].at;this.timers.delete(next[0]);next[1].fn();await settle();
    }
    this.time=end;await settle();
  }
}
class Memory {
  raw=initial();locks=new Map();waiters=new Map();beforeWrite;afterWrite;
  async read(){return structuredClone(this.raw);}
  async write(state){await this.beforeWrite?.(state);this.raw=structuredClone(state);await this.afterWrite?.(state);}
  async lock(name,wait) {
    if(this.locks.has(name)) {
      if(!wait)return null;
      await new Promise(resolve=>{const q=this.waiters.get(name)??[];q.push(resolve);this.waiters.set(name,q);});
      return this.lock(name,wait);
    }
    let end;const lost=new Promise(resolve=>{end=resolve;});let released=false;
    const lease={lost,release:async()=>{if(released)return;released=true;this.locks.delete(name);end();this.waiters.get(name)?.shift()?.();}};
    this.locks.set(name,lease);return lease;
  }
}
class Server {
  sockets=[];calls=[];sent=[];errors=[];onCall;onHello;placed=true;anonymous=1;
  constructor(codec,storage){this.codec=codec;this.storage=storage;}
  socket=()=>{
    const s={onopen:null,onmessage:null,onclose:null,onerror:null,binaryType:'',closed:false,
      send:data=>{
        const f=this.codec.decode(data);this.sent.push(f);
        if(f.tag==='Hello') {
          const identity=f.values[0].tag==='Some'?caller:{issuer:1,subject:bytes(...Array(16).fill(++this.anonymous))};
          this.emit(s,{tag:'Welcome',values:[identity,4,262144n,bytes(3),'test',1024n]});
          s.principal=identity.subject;this.onHello?.(s,f);
          if(this.placed)for(const _ of f.values[1])this.emit(s,{tag:'Placed',value:{tag:'Ready',value:0}});
        } else if(f.tag==='Call') {
          this.calls.push({frame:f,socket:s});
          const p=this.storage.raw?.streams[f.values[0]]?.pending;
          try {
            assert(p,'a send requires a durable pending request');
            assert.equal(p.p,f.values[1]);assert.deepEqual(p.args,f.values[4]);
            assert(p.attempts>0,'the send attempt must be durable');
            this.onCall?.(f,s);
          }catch(e){this.errors.push(e);}
        }
      },
      close:(code)=>{if(code!==undefined && code!==1000 && (code<3000 || code>4999))throw new Error('WebSocket.close forbids reserved codes');s.closed=true;},
    };
    this.sockets.push(s);queueMicrotask(()=>s.onopen?.());return s;
  };
  emit(socket,frame){const data=this.codec.encode(frame);queueMicrotask(()=>{if(!socket.closed)socket.onmessage?.({data:data.buffer.slice(data.byteOffset,data.byteOffset+data.byteLength)});});}
  answer(socket,stream,p,outcome,tag='Batch'){
    this.emit(socket,tag==='Batch'?{tag,values:[1n,[],[],[{stream,p,outcome}]]}:{tag,values:[1n,[{stream,p,outcome}]]});
  }
  close(socket,code){socket.closed=true;socket.onclose?.({code});}
  check(){if(this.errors.length)throw this.errors[0];}
}

// The fixture implements STORE P1-P3 directly: one retained digest and reply
// per principal/stream, consecutive positions, and no effects on AH1/AH2.
class Ledger {
  streams=new Map();effects=0;
  attempt(principal,frame) {
    const [stream,p,,,arg]=frame.values,k=Buffer.from(principal).toString('hex')+':'+stream;
    const old=this.streams.get(k);
    if(old && p<old.p)return refused('HistoryUnavailable');
    if(old && p===old.p)return Buffer.from(arg).equals(Buffer.from(old.arg))?old.outcome:refused('RequestMismatch');
    assert.equal(p,old?old.p+1n:0n);
    this.effects++;
    const outcome=returned(arg[0]);this.streams.set(k,{p,arg:arg.slice(),outcome});return outcome;
  }
}

export async function loadRuntime(mutant) {
  const dir=await mkdtemp(join(tmpdir(),'hirsel-client-'));
  await writeFile(join(dir,'package.json'),'{"type":"module"}');
  for(const name of ['client','persistence','browser','node']) {
    let text=await readFile(new URL(name+'.ts',source),'utf8');
    if(mutant)text=mutant(name,text);
    await writeFile(join(dir,name+'.js'),stripTypeScriptTypes(text,{mode:'transform'}));
  }
  const client=await import(pathToFileURL(join(dir,'client.js')).href);
  const node=await import(pathToFileURL(join(dir,'node.js')).href);
  const persistence=await import(pathToFileURL(join(dir,'persistence.js')).href);
  const browser=await import(pathToFileURL(join(dir,'browser.js')).href);
  return {...client,...node,...persistence,...browser,dir,dispose:()=>rm(dir,{recursive:true,force:true})};
}

export async function laws(codec, only, mutant) {
  const runtime=await loadRuntime(mutant), tests={};
  const clients=[];
  async function setup(storage=new Memory(),options={}) {
    const time=new Time(),server=new Server(codec,storage),events=[],diagnostics=[];
    const opts={url:'ws://scripted',storage,frame:codec,wants:[want],socket:server.socket,clock:time,
      credential,onRecovered:o=>events.push(o),onDiagnostic:d=>diagnostics.push(d),...options};
    options.prepare?.(server);
    const client=await runtime.Client.open(opts);clients.push(client);await settle();
    return {client,server,storage,time,events,diagnostics,opts};
  }
  async function ready(h){await h.client.whenReady();h.server.check();}
  tests.lost_stream_fences_generation=async()=>{
    const h=await setup();await ready(h);
    const answer=h.client.call(want,bytes(7),x=>x);await settle();
    assert.deepEqual(h.server.calls[0].frame,call(0,0n,7));
    delete h.storage.raw.streams[0];
    const fresh=await setup(h.storage);await ready(fresh);
    assert.notEqual(fresh.client.generation,h.client.generation);
    assert.equal(fresh.server.calls.length,0,'lost bytes must not replay under the new principal');
    h.server.close(h.server.sockets[0],1001);await settle();await h.time.advance(50);
    assert.equal(h.client.state,'Fenced');assert.equal((await answer).tag,'LocallyUnknown');
    assert.equal(h.server.calls.length,1);h.server.check();
  };
  tests.refused_p_reused=async()=>{
    const h=await setup();await ready(h);
    let a=h.client.call(want,bytes(7),x=>x);await settle();
    h.server.answer(h.server.sockets[0],0,0n,{tag:'Refused',value:{tag:'Malformed',value:{kind:1,at:0n}}});await settle();
    assert.equal((await a).tag,'Refused');assert.equal(h.storage.raw.streams[0].next,0n);
    a=h.client.call(want,bytes(8),x=>x);await settle();
    assert.deepEqual(h.server.calls[1].frame,call(0,0n,8));
    h.server.answer(h.server.sockets[0],0,0n,returned(8));await settle();await a;
    assert.equal(h.storage.raw.streams[0].next,1n);
    a=h.client.call(want,bytes(9),x=>x);await settle();assert.deepEqual(h.server.calls[2].frame,call(0,1n,9));
    h.server.answer(h.server.sockets[0],0,1n,returned(9));await settle();await a;h.server.check();
  };
  tests.commit_before_send=async()=>{
    for(const cut of ['commit','send','outcome']) {
      const store=new Ledger();
      const h=await setup(undefined,{prepare:server=>{server.onCall=(f,s)=>{
        const outcome=store.attempt(s.principal,f);
        if(cut==='outcome')server.answer(s,0,0n,outcome);
      };}});await ready(h);
      if(cut==='commit')h.storage.afterWrite=state=>{if(state.streams[0].pending?.attempts===0)throw new Error('crash after durable commit');};
      if(cut==='outcome')h.storage.beforeWrite=state=>{if(state.streams[0].terminal)throw new Error('crash before terminal commit');};
      const answer=h.client.call(want,bytes(7),x=>x);void answer.catch(()=>{});await settle();
      assert.equal(h.storage.raw.streams[0].pending.p,0n);
      assert.deepEqual(h.storage.raw.streams[0].pending.args,bytes(7));
      if(cut==='commit')assert.equal(h.server.calls.length,0);else assert.equal(h.server.calls.length,1);
      h.storage.afterWrite=undefined;h.storage.beforeWrite=undefined;await h.client.close();await settle();
      const recovered=await setup(h.storage,{wants:[other,want],prepare:server=>{server.onCall=(f,s)=>server.answer(s,0,0n,store.attempt(s.principal,f));}});
      await ready(recovered);
      assert.deepEqual(recovered.server.calls[0].frame,call(0,0n,7,1));
      assert.equal(store.effects,1);assert.equal(h.storage.raw.streams[0].next,1n);
      assert.deepEqual(h.storage.raw.streams[0].terminal,{p:0n,outcome:returned(7)});
      assert.deepEqual(recovered.events,[returned(7)]);h.server.check();recovered.server.check();
      await recovered.client.close();
    }
  };
  tests.stream_lease=async()=>{
    const h=await setup();await ready(h);
    const b=await setup(h.storage);await ready(b);assert.equal(b.client.stream,1);
    const a=h.client.call(want,bytes(7),x=>x);await settle();
    await h.client.close();await a;await settle();
    const c=await setup(h.storage);await ready(c);assert.equal(c.client.stream,0);
    let failure;
    void c.client.call(want,bytes(8),x=>x).then(()=>{},error=>{failure=error;});await settle();
    assert.equal(failure?.code,'RecoveringOrBusy');
    assert.deepEqual(h.storage.raw.streams[0].pending.args,bytes(7));
    assert.deepEqual(c.server.calls.map(x=>x.frame),[call(0,0n,7)]);
    c.server.answer(c.server.sockets[0],0,0n,returned(7));await settle();
    const next=c.client.call(want,bytes(8),x=>x);await settle();assert.deepEqual(c.server.calls[1].frame,call(0,1n,8));
    c.server.answer(c.server.sockets[0],0,1n,returned(8));await settle();await next;c.server.check();
  };
  tests.incompatible_is_terminal=async()=>{
    const h=await setup();await ready(h);
    h.server.close(h.server.sockets[0],1008);await settle();await h.time.advance(120000);
    assert.equal(h.client.state,'Incompatible');assert.equal(h.server.sockets.length,1);
    h.client.reconnect();await settle();assert.equal(h.server.sockets.length,2);
    for(const code of [1007,1009]) {
      h.server.close(h.server.sockets.at(-1),code);await settle();await h.time.advance(120000);
      assert.equal(h.client.state,'Incompatible');h.client.reconnect();await settle();
    }
    const a=h.client.call(want,bytes(7),x=>x);await settle();
    h.server.close(h.server.sockets.at(-1),1008);await settle();h.client.reconnect();await settle();
    assert.deepEqual(h.server.calls.at(-1).frame,call(0,0n,7));
    h.server.answer(h.server.sockets.at(-1),0,0n,returned(7));await settle();await a;
    h.server.check();
    const gone=await setup();await ready(gone);
    gone.server.placed=false;gone.server.close(gone.server.sockets[0],1001);await settle();await gone.time.advance(50);
    gone.server.emit(gone.server.sockets.at(-1),{tag:'Placed',value:{tag:'Gone'}});await settle();await gone.time.advance(120000);
    assert.equal(gone.client.state,'Incompatible');assert.equal(gone.server.sockets.length,2);
  };
  tests.history_unavailable_retires=async()=>{
    const h=await setup();await ready(h);
    let a=h.client.call(want,bytes(7),x=>x);await settle();h.server.answer(h.server.sockets[0],0,0n,returned(7));await settle();await a;
    h.server.answer(h.server.sockets[0],0,0n,refused('HistoryUnavailable'));await settle();
    assert.equal(h.client.state,'Ready');assert.equal(h.storage.raw.streams[0].retired,false);
    a=h.client.call(want,bytes(8),x=>x);await settle();
    h.server.answer(h.server.sockets[0],0,1n,refused('HistoryUnavailable'),'Reset');await settle();
    assert.equal(h.client.state,'Retired');assert(h.storage.raw.streams[0].retired);assert.deepEqual(await a,{tag:'LocallyUnknown'});
    await assert.rejects(h.client.call(want,bytes(9),x=>x),e=>e.code==='Retired');
    const b=await setup(h.storage);await ready(b);assert.equal(b.client.stream,1);assert.equal(b.server.calls.length,0);
    assert.equal(h.server.calls.length,2);h.server.check();
  };
  tests.request_mismatch_fences=async()=>{
    const store=new Ledger(),storage=new Memory();
    const configure=server=>{server.onCall=(f,s)=>server.answer(s,0,0n,store.attempt(s.principal,f));};
    const h=await setup(storage,{prepare:configure});await ready(h);
    const a=h.client.call(want,bytes(7),x=>x);await settle();await a;
    // Deliberately violate lease exclusivity and restore a stale local stream
    // record to exercise AH1 against the first writer's real committed digest.
    const locked=storage.lock.bind(storage);
    storage.lock=async(name,wait)=>name.startsWith('stream-')?{lost:new Promise(()=>{}),release:async()=>{}}:locked(name,wait);
    storage.raw.streams[0]={caller,next:0n,retired:false};
    const b=await setup(storage,{prepare:configure});await ready(b);assert.equal(b.client.stream,0);
    const rejected=b.client.call(want,bytes(8),x=>x);await settle();
    await b.time.advance(100);assert.equal(b.server.calls.length,1,'mismatched bytes must never be resent');
    assert.equal(b.client.state,'Fenced');
    assert.deepEqual(await rejected,{tag:'RequestMismatch'});assert.equal(store.effects,1);assert.equal(storage.raw.generation,2n);
    await b.time.advance(120000);assert.equal(b.server.calls.length,1);
    assert.deepEqual(storage.raw.directory,[]);
    const c=await setup(storage);await ready(c);assert.equal(c.server.calls.length,0);b.server.check();h.server.check();
  };
  tests.anonymous_reconnect_does_not_replay=async()=>{
    const h=await setup(undefined,{credential:undefined});await ready(h);
    const original=h.storage.raw.streams[0].caller;
    const pending=h.client.call(want,bytes(7),x=>x);await settle();
    h.server.close(h.server.sockets[0],1001);await settle();await h.time.advance(50);
    assert.deepEqual(await pending,{tag:'LocallyUnknown'});
    assert.notDeepEqual(h.storage.raw.streams[0].caller,original);
    assert.equal(h.server.calls.length,1);
    const fresh=h.client.call(want,bytes(8),x=>x);await settle();
    assert.deepEqual(h.server.calls[1].frame,call(0,0n,8));
    h.server.answer(h.server.sockets[1],0,0n,returned(8));await settle();
    assert.deepEqual(await fresh,returned(8));h.server.check();
  };
  tests.retry_and_reconnect_order=async()=>{
    const h=await setup();await ready(h);const caches=[];
    h.client.subscribe(want,bytes(1),rows=>caches.push(rows));await settle();
    const a=h.client.call(want,bytes(7),x=>x);await settle();
    h.server.close(h.server.sockets[0],1001);await settle();
    h.server.placed=false;await h.time.advance(50);
    assert.equal(h.server.calls.length,1,'must wait for every Placed');
    h.server.emit(h.server.sockets[1],{tag:'Placed',value:{tag:'Ready',value:0}});await settle();
    assert.deepEqual(h.server.calls[1].frame,call(0,0n,7));
    assert.deepEqual(h.server.sent.slice(-2),[call(0,0n,7),{tag:'Subscribe',values:[1,0,bytes(1),{tag:'None'}]}]);
    h.server.answer(h.server.sockets[1],0,0n,refused('Deadline'));await settle();await h.time.advance(100);
    assert.deepEqual(h.server.calls[2].frame,call(0,0n,7));
    h.server.answer(h.server.sockets[1],0,0n,returned(7));await settle();await a;h.server.check();
  };
  tests.uncertain_refusal_retires=async()=>{
    const h=await setup();await ready(h);const a=h.client.call(want,bytes(7),x=>x);await settle();
    h.server.close(h.server.sockets[0],1001);await settle();await h.time.advance(50);
    h.server.answer(h.server.sockets[1],0,0n,refused('NoEntry'));await settle();
    assert.deepEqual(await a,{tag:'LocallyUnknown'});assert.equal(h.client.state,'Retired');
    assert.equal(h.storage.raw.streams[0].next,0n);assert.deepEqual(h.storage.raw.streams[0].pending.args,bytes(7));
  };
  tests.batch_complete_and_predictions=async()=>{
    const h=await setup();await ready(h);const caches=[],predictions=[];
    h.opts.onPrediction=p=>predictions.push(new Map(p));
    h.client.subscribe(want,bytes(1),rows=>caches.push(rows));await settle();
    const a=h.client.call(want,bytes(7),x=>x,'overlay');await settle();assert.equal(h.client.predictions.size,1);
    h.server.emit(h.server.sockets[0],{tag:'Batch',values:[1n,[0],[{sub:0,key:bytes(1),value:{tag:'Some',value:bytes(2)}}],[{stream:0,p:0n,outcome:returned(7)}]]});await settle();await a;
    assert.equal(h.client.predictions.size,0);assert.equal(caches[0][0].value.value[0],2);
    h.server.emit(h.server.sockets[0],{tag:'Reset',values:[2n,[]]});await settle();assert.equal(caches.length,1);
    h.server.emit(h.server.sockets[0],{tag:'Batch',values:[2n,[],[{sub:0,key:bytes(1),value:{tag:'None'}}],[]]});await settle();assert.equal(caches.length,1);
    h.server.emit(h.server.sockets[0],{tag:'Batch',values:[2n,[1],[],[]]});await settle();assert.deepEqual(caches,[caches[0],[]]);
    const before=caches.length;
    h.server.emit(h.server.sockets[0],{tag:'Reset',values:[3n,[]]});await settle();
    h.server.emit(h.server.sockets[0],{tag:'Batch',values:[3n,[],[{sub:2,key:bytes(1),value:{tag:'Some',value:bytes(3)}}],[]]});await settle();
    assert.equal(h.client.state,'Incompatible');assert.equal(caches.length,before);
  };
  tests.subscription_end_after_reset=async()=>{
    const h=await setup();await ready(h);
    const reasons=[];
    const subscription=h.client.subscribe(want,bytes(1),()=>assert.fail('unexpected row delivery'),reason=>reasons.push(reason));
    await settle();
    const socket=h.server.sockets[0];
    h.server.emit(socket,{tag:'Reset',values:[2n,[]]});await settle();
    assert.deepEqual(reasons,[],'Reset only resubscribes');
    assert.deepEqual(h.server.sent.at(-1),{tag:'Subscribe',values:[1,0,bytes(1),{tag:'None'}]});
    const reason={tag:'Refused',value:{tag:'Budget',value:{requested:9007199254740993n,available:4n}}};
    h.server.emit(socket,{tag:'Ended',values:[1,reason]});await settle();
    assert.deepEqual(reasons,[reason],'Ended delivers the decoded refusal');
    h.server.emit(socket,{tag:'Ended',values:[1,reason]});
    h.server.emit(socket,{tag:'Reset',values:[3n,[]]});await settle();
    subscription.unsubscribe();await settle();
    assert.deepEqual(reasons,[reason]);
    assert.deepEqual(h.server.sent.filter(f=>f.tag==='Subscribe').map(f=>f.values[0]),[0,1]);
    assert.equal(h.server.sent.some(f=>f.tag==='Unsubscribe'),false);
  };
  tests.storage_unavailable_no_sends=async()=>{
    const h=await setup();await ready(h);
    h.storage.beforeWrite=()=>{throw new Error('disk unavailable');};
    await assert.rejects(h.client.call(want,bytes(7),x=>x),/disk unavailable/);await settle();
    assert.equal(h.client.state,'StorageUnavailable');assert.equal(h.server.calls.length,0);
    h.storage.beforeWrite=undefined;
  };
  tests.backoff_requires_stable_session=async()=>{
    const h=await setup();await ready(h);
    h.server.close(h.server.sockets[0],1001);await settle();await h.time.advance(50);
    h.server.close(h.server.sockets[1],1013);await settle();await h.time.advance(50);
    assert.equal(h.server.sockets.length,2);await h.time.advance(50);assert.equal(h.server.sockets.length,3);
    await h.time.advance(30000);h.server.close(h.server.sockets[2],1001);await settle();await h.time.advance(50);
    assert.equal(h.server.sockets.length,4);
  };
  tests.failed_advances_and_request_is_immutable=async()=>{
    const h=await setup();await ready(h);
    const arg=bytes(7),w=structuredClone(want);
    const a=h.client.call(w,arg,x=>x);arg[0]=9;w.identity[0]=9;await settle();
    assert.deepEqual(h.server.calls[0].frame,call(0,0n,7));
    assert.deepEqual(h.storage.raw.streams[0].pending.want,want);
    h.server.answer(h.server.sockets[0],0,0n,{tag:'Failed',value:{tag:'Raised',value:1}});await settle();
    assert.deepEqual(await a,{tag:'Failed',value:{tag:'Raised',value:1}});assert.equal(h.storage.raw.streams[0].next,1n);
  };
  tests.retryable_outcomes_keep_identity=async()=>{
    const h=await setup();await ready(h);const a=h.client.call(want,bytes(7),x=>x,'overlay');await settle();
    for(const outcome of [{tag:'Abandoned'},refused('Overloaded'),{tag:'Refused',value:{tag:'Budget',value:{requested:10n,available:0n}}}]) {
      const n=h.server.calls.length;
      h.server.answer(h.server.sockets[0],0,0n,outcome);await settle();
      assert.equal(h.storage.raw.streams[0].next,0n);assert.equal(h.client.predictions.size,0);
      await h.time.advance(1000);
      assert.equal(h.server.calls.length,n+1);assert.deepEqual(h.server.calls.at(-1).frame,call(0,0n,7));
      assert(h.storage.raw.streams[0].pending.possiblyAdmitted);
    }
    h.server.answer(h.server.sockets[0],0,0n,returned(7));await settle();await a;h.server.check();
  };
  tests.browser_lock_lifetime_and_strict_commit=async()=>{
    const originalNavigator=Object.getOwnPropertyDescriptor(globalThis,'navigator');
    const originalIndexedDB=Object.getOwnPropertyDescriptor(globalThis,'indexedDB');
    const memory=new Memory(),names=[],transactions=[];let encoded;
    const db={createObjectStore(){},close(){},transaction(name,mode,options){
      assert.equal(name,'state');
      const tx={oncomplete:null,onabort:null,error:null,value:undefined,
        objectStore:()=>({
          get:()=>{const request={result:encoded};queueMicrotask(()=>tx.oncomplete?.());return request;},
          put:value=>{tx.value=value;},
        }),
        commit:()=>{encoded=tx.value;tx.oncomplete?.();},
      };
      if(mode==='readwrite'){assert.deepEqual(options,{durability:'strict'});transactions.push(tx);}
      return tx;
    }};
    Object.defineProperty(globalThis,'navigator',{configurable:true,value:{locks:{request:async(name,options,fn)=>{
      names.push(name);assert.equal(options.mode,'exclusive');
      const lease=await memory.lock(name,!options.ifAvailable);
      try{return await fn(lease?{}:null);}finally{await lease?.release();}
    }}}});
    Object.defineProperty(globalThis,'indexedDB',{configurable:true,value:{open:()=>{
      const request={result:db};queueMicrotask(()=>{request.onupgradeneeded?.();request.onsuccess?.();});return request;
    }}});
    try {
      const storage=new runtime.BrowserStorage();
      const a=await storage.lock('stream-1-0',false);assert(a);assert.equal(await storage.lock('stream-1-0',false),null);
      await a.release();const b=await storage.lock('stream-1-0',false);assert(b);await b.release();
      const install=await storage.lock('install',true);await install.release();
      assert.deepEqual(names,['hirsel-stream-1-0','hirsel-stream-1-0','hirsel-stream-1-0','hirsel-install']);
      let committed=false;const write=storage.write(initial()).then(()=>{committed=true;});await settle();
      assert.equal(committed,false);transactions[0].commit();await write;
      assert.deepEqual(await storage.read(),initial());
      const failed=storage.write(initial());await settle();transactions[1].onabort();await assert.rejects(failed,/aborted/);
      storage.close();
    }finally{
      if(originalNavigator)Object.defineProperty(globalThis,'navigator',originalNavigator);else delete globalThis.navigator;
      if(originalIndexedDB)Object.defineProperty(globalThis,'indexedDB',originalIndexedDB);else delete globalThis.indexedDB;
    }
  };
  tests.node_durable_replacement_and_flock=async()=>{
    const dir=await mkdtemp(join(tmpdir(),'hirsel-node-state-'));
    try {
      const storage=new runtime.NodeStorage(dir),state=initial();
      const a=await storage.lock('stream-1-0',false);assert(a);
      assert.equal(await storage.lock('stream-1-0',false),null);
      await storage.write(state);assert.deepEqual(await storage.read(),state);
      await a.release();const b=await storage.lock('stream-1-0',false);assert(b);await b.release();
      await writeFile(join(dir,'state.json'),'broken');await assert.rejects(storage.read(),runtime.CorruptState);
      // A different process dies without releasing its lease. EOF releases the
      // flock owner; a subsequent blocking acquisition must make progress.
      const child=spawn(process.execPath,['--input-type=module','-e',
        `import {NodeStorage} from ${JSON.stringify(pathToFileURL(join(runtime.dir,'node.js')).href)};await new NodeStorage(${JSON.stringify(dir)}).lock('stream-1-0',true);console.log('held');process.exit(0);`],{stdio:['ignore','pipe','inherit']});
      await new Promise((resolve,reject)=>{child.once('error',reject);child.once('close',code=>code===0?resolve():reject(new Error('lease process failed')));});
      const c=await storage.lock('stream-1-0',true);assert(c);await c.release();
    }finally{await rm(dir,{recursive:true,force:true});}
  };
  let cases=0;
  try {
    for(const [name,test] of Object.entries(tests)) {
      if(only && !(Array.isArray(only)?only.includes(name):name===only))continue;
      await test();cases++;console.error(`client ${name}: PASS`);
    }
    if(!cases)throw new Error('zero client laws selected');
    return {cases};
  }finally{
    for(const client of clients)await client.close();
    await runtime.dispose();
  }
}
