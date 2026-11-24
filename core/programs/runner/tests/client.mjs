import assert from 'node:assert/strict';
import {getEventListeners} from 'node:events';
import {mkdtemp, readFile, writeFile, rm} from 'node:fs/promises';
import {tmpdir} from 'node:os';
import {join} from 'node:path';
import {pathToFileURL, fileURLToPath} from 'node:url';
import {stripTypeScriptTypes} from 'node:module';
import {spawn} from 'node:child_process';
import {optimisticLaws} from './client_mutations.mjs';

const source = new URL('../../../../sdk/client/', import.meta.url);
const bytes = (...values) => Uint8Array.of(...values);
const want = {identity: bytes(1), args_fp: 1n, result_fp: 2n};
const other = {identity: bytes(2), args_fp: 3n, result_fp: 4n};
const credential = bytes(...Array(16).fill(1));
const caller = {issuer:2,subject:bytes(...Array(16).fill(7))};
const initial = () => ({version:4,installation:2n,generation:1n,directory:[],streams:{}});
const birth = 14000000n, retention = 5184000000n;
const streamKey = id => typeof id === 'object'
  ? id.t.toString(16).padStart(16,'0') + id.n.toString(16).padStart(16,'0') : String(id);
const savedStream = (state, position=0) => state.streams[state.directory[position]];
const returned = n => ({tag:'Returned',value:bytes(n)});
const refused = tag => ({tag:'Refused',value:{tag}});
const call = (stream,p,arg,slot=0,deadline=30000) => ({tag:'Call',values:[stream,p,slot,deadline,bytes(arg)]});
const equalBytes = (a, b) => a.length === b.length && a.every((x, i) => x === b[i]);
const settle = async () => { for (let i=0;i<40;i++) await new Promise(resolve => setImmediate(resolve)); };
const row = (slot, key, value) => ({slot,key:bytes(key),value:value === undefined
  ? {tag:'None'} : {tag:'Some',value:bytes(value)}});
const group = (sub, rows, value = bytes()) => ({sub,value,rows});
const batch = (version, complete, groups, outcomes = []) => ({tag:'Batch',values:[version,complete,groups,outcomes]});

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
          if(this.welcome===false)return;
          const identity=f.values[0].tag==='Some'?caller:{issuer:1,subject:bytes(...Array(16).fill(++this.anonymous))};
          this.emit(s,{tag:'Welcome',values:[identity,4,262144n,bytes(3),'test',1024n,birth,retention]});
          s.principal=identity.subject;this.onHello?.(s,f);
          if(this.placed)for(const _ of f.values[1])this.emit(s,{tag:'Placed',value:{tag:'Ready',value:0}});
        } else if(f.tag==='Call') {
          this.calls.push({frame:f,socket:s});
          const p=this.storage.raw?.streams[streamKey(f.values[0])]?.pending;
          try {
            assert(p,'a send requires a durable pending request');
            assert.equal(p.p,f.values[1]);assert.deepEqual(p.args,f.values[4]);
            assert(p.inflight,'the send attempt must be durable');
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
    if(stream===0)stream=this.calls.findLast(row=>row.socket===socket)?.frame.values[0];
    this.emit(socket,tag==='Batch'?{tag,values:[1n,[],[],[{stream,p,outcome}]]}:{tag,values:[1n,[{stream,p,outcome}]]});
  }
  close(socket,code,reason){socket.closed=true;socket.onclose?.({code,reason});}
  check(){if(this.errors.length)throw this.errors[0];}
}

// The fixture implements STORE P1-P3 directly: one retained digest and reply
// per principal/stream, consecutive positions, and no effects on AH1/AH2.
class Ledger {
  streams=new Map();effects=0;byValue=new Map();
  attempt(principal,frame) {
    const [stream,p,,,arg]=frame.values,k=Buffer.from(principal).toString('hex')+':'+streamKey(stream);
    const old=this.streams.get(k);
    if(old && p<old.p)return refused('HistoryUnavailable');
    if(old && p===old.p)return Buffer.from(arg).equals(Buffer.from(old.arg))?old.outcome:refused('RequestMismatch');
    assert.equal(p,old?old.p+1n:0n);
    this.effects++;
    this.byValue.set(arg[0],(this.byValue.get(arg[0])??0)+1);
    const outcome=returned(arg[0]);this.streams.set(k,{p,arg:arg.slice(),outcome});return outcome;
  }
}

export async function loadRuntime(mutant) {
  const dir=await mkdtemp(join(tmpdir(),'hirsel-client-'));
  try {
    await writeFile(join(dir,'package.json'),'{"type":"module"}');
    for(const name of ['client','image','persistence','browser','node']) {
      let text=await readFile(new URL(name+'.ts',source),'utf8');
      if(mutant)text=mutant(name,text);
      await writeFile(join(dir,name+'.js'),stripTypeScriptTypes(text,{mode:'transform'}));
    }
    const client=await import(pathToFileURL(join(dir,'client.js')).href);
    const node=await import(pathToFileURL(join(dir,'node.js')).href);
    const persistence=await import(pathToFileURL(join(dir,'persistence.js')).href);
    const browser=await import(pathToFileURL(join(dir,'browser.js')).href);
    return {...client,...node,...persistence,...browser,dir,dispose:()=>rm(dir,{recursive:true,force:true})};
  }catch(error){
    await rm(dir,{recursive:true,force:true});
    throw error;
  }
}

export async function laws(codec, only, mutant, entries, http) {
  const runtime=await loadRuntime(mutant), tests={};
  const clients=[];
  async function setup(storage=new Memory(),options={}) {
    const time=new Time(),server=new Server(codec,storage),events=[],diagnostics=[];
    const opts={url:'ws://scripted',storage,frame:codec,wants:[want],socket:server.socket,clock:time,
      credential:async()=>credential,onRecovered:o=>events.push(o),onDiagnostic:d=>diagnostics.push(d),...options};
    options.prepare?.(server);
    const client=await runtime.Client.open(opts);clients.push(client);await settle();
    return {client,server,storage,time,events,diagnostics,opts};
  }
  async function ready(h){await h.client.whenReady();h.server.check();}
  tests.fresh_installations_share_caller=async()=>{
    const store=new Ledger();
    const prepare=server=>{server.onCall=(f,s)=>server.answer(s,f.values[0],f.values[1],store.attempt(s.principal,f));};
    const first=await setup(new Memory(),{prepare});await ready(first);
    const second=await setup(new Memory(),{prepare});await ready(second);
    assert.deepEqual(await first.client.call(want,bytes(21),x=>x),returned(21));
    assert.deepEqual(await second.client.call(want,bytes(22),x=>x),returned(22));
    assert.notDeepEqual(first.server.calls[0].frame.values[0],second.server.calls[0].frame.values[0]);
    assert.equal(store.effects,2);
    assert.deepEqual(store.byValue,new Map([[21,1],[22,1]]));
    first.server.check();second.server.check();
  };
  tests.cleared_storage_restarts_under_same_caller=async()=>{
    const store=new Ledger(),storage=new Memory();
    const prepare=server=>{server.onCall=(f,s)=>server.answer(s,f.values[0],f.values[1],store.attempt(s.principal,f));};
    const h=await setup(storage,{prepare});await ready(h);
    assert.deepEqual(await h.client.call(want,bytes(21),x=>x),returned(21));
    const old=storage.raw.installation;
    await h.client.close();storage.raw=null;
    const reopened=await setup(storage,{prepare});await ready(reopened);
    assert.deepEqual(await reopened.client.call(want,bytes(22),x=>x),returned(22));
    assert.notEqual(storage.raw.installation,old);
    assert.notDeepEqual(h.server.calls[0].frame.values[0],reopened.server.calls[0].frame.values[0]);
    assert.deepEqual(store.byValue,new Map([[21,1],[22,1]]));
  };
  tests.concurrent_same_caller_bridge=async()=>{
    const store=new Ledger();
    let finish;
    const h=await setup(new Memory(),{prepare:server=>{server.onCall=(f,s)=>{
      const returned=store.attempt(s.principal,f);
      finish=()=>server.answer(s,f.values[0],f.values[1],returned);
    };}});await ready(h);
    const browser=h.client.call(want,bytes(21),x=>x);
    await settle();
    const bridgeId={t:birth,n:crypto.getRandomValues(new BigUint64Array(1))[0]};
    assert.notDeepEqual(h.server.calls[0].frame.values[0],bridgeId);
    assert.deepEqual(store.attempt(caller.subject,call(bridgeId,0n,22)),returned(22));
    finish();
    assert.deepEqual(await browser,returned(21));
    assert.deepEqual(store.byValue,new Map([[21,1],[22,1]]));
  };
  tests.unmarked_503_after_commit=async()=>{
    const posts=[],effects=new Map();
    const http={url:'https://scripted.invalid',fetch:async(_url,request)=>{
      assert.equal(request.method,'POST');
      const key=request.headers.get('Idempotency-Key');
      posts.push(key);
      if(posts.length===1){
        effects.set('X',1);
        return new Response('{}',{status:503});
      }
      if(posts.length===2)return new Response('{}',{status:410,headers:{'Hirsel-Refused':`history_unavailable;key=${key}`}});
      effects.set('X',(effects.get('X')??0)+1);
      return new Response('{}',{headers:{'Idempotent-Outcome':'committed'}});
    }};
    const h=await setup(new Memory(),{http});await ready(h);
    const result=h.client.callHttp(want,{path:'/.hirsel/op/put',body:'"X"'});
    await settle();
    assert.equal(posts.length,1);
    await h.time.advance(1000);
    assert.deepEqual(await result,{tag:'LocallyUnknown'});
    assert.deepEqual(posts,[posts[0],posts[0]]);
    assert.equal(effects.get('X'),1);
  };
  tests.marked_410_after_invisible_delivery_through_reopen=async()=>{
    const posts=[],effects=new Map(),storage=new Memory();
    const http={url:'https://scripted.invalid',fetch:async(_url,request)=>{
      const key=request.headers.get('Idempotency-Key');
      assert.match(key,/^"[a-f0-9]{32}\.0"$/);
      posts.push(key);
      if(posts.length===1){
        effects.set('X',1);
        throw new TypeError('connection lost after the effect committed');
      }
      if(posts.length===2)return new Response(null,{status:410,headers:{'Hirsel-Refused':`history_unavailable;key=${key}`}});
      assert.equal(request.body,'"Y"');
      effects.set('Y',1);
      return new Response('{}',{headers:{'Idempotent-Outcome':'committed'}});
    }};
    const h=await setup(storage,{http});await ready(h);
    const x=h.client.callHttp(want,{path:'/.hirsel/op/put',body:'"X"'});
    await settle();
    assert.equal(posts.length,1);
    await h.client.close();
    assert.deepEqual(await x,{tag:'LocallyUnknown'});
    const replay=await setup(storage,{http});await settle();
    assert.equal(replay.client.state,'Retired');
    assert.deepEqual(replay.events,[{tag:'LocallyUnknown'}]);
    assert.deepEqual(posts,[posts[0],posts[0]]);
    const fresh=await setup(storage,{http});await ready(fresh);
    assert.equal((await fresh.client.callHttp(want,{path:'/.hirsel/op/put',body:'"Y"'})).tag,'Returned');
    assert.notEqual(posts[2],posts[0]);
    assert.deepEqual(effects,new Map([['X',1],['Y',1]]));
  };
  tests.first_marked_410_can_hide_an_upstream_retry=async()=>{
    let posts=0,effects=0;
    const http={url:'https://scripted.invalid',fetch:async(_url,request)=>{
      const key=request.headers.get('Idempotency-Key');
      posts++;
      // A proxy delivered the original request and lost the reply. The
      // response visible to the SDK belongs to the proxy's repeat delivery.
      effects++;
      return new Response(null,{status:410,headers:{'Hirsel-Refused':`history_unavailable;key=${key}`}});
    }};
    const h=await setup(new Memory(),{http});await ready(h);
    assert.deepEqual(await h.client.callHttp(want,{path:'/.hirsel/op/put',body:'"X"'}),{tag:'LocallyUnknown'});
    await h.time.advance(1000);
    assert.equal(posts,1);assert.equal(effects,1);
  };
  tests.older_remint_refused_once=async()=>{
    const store=new Ledger();
    const h=await setup(new Memory(),{prepare:server=>{server.onCall=(f,s)=>{
      const [id,p,,,arg]=f.values;
      // The quota-one victim is ahead of both server samples. This first
      // workload owns its mint, so the explicit refusal permits one remint.
      const outcome=arg[0]===21?store.attempt(s.principal,f):refused('HistoryUnavailable');
      server.answer(s,id,p,outcome);
    };}});await ready(h);
    assert.deepEqual(await h.client.call(want,bytes(21),x=>x),returned(21));
    const x=h.client.call(want,bytes(22),x=>x);
    await settle();await h.time.advance(1000);
    assert.deepEqual(await x,refused('HistoryUnavailable'));
    const [first,aged,replacement]=h.server.calls.map(row=>row.frame.values);
    assert.deepEqual(aged[0],first[0]);assert.equal(aged[1],1n);
    assert.notDeepEqual(replacement[0],aged[0]);assert.equal(replacement[1],0n);
    assert.equal(replacement[0].t,birth);
    assert.equal(h.server.calls.length,3);assert.equal(h.client.state,'Retired');
    assert.deepEqual(store.byValue,new Map([[21,1]]));
  };
  async function subscribed(decode) {
    const h=await setup();await ready(h);
    const changes=[];
    const sub=h.client.subscribe(want,bytes(1),(_,change)=>changes.push(change),undefined,decode);
    await settle();
    const emit=async(version,complete,rows,value)=>{
      h.server.emit(h.server.sockets[0],batch(BigInt(version),complete ? [0] : [],[group(0,rows,value)]));
      await settle();
    };
    return {sub,changes,emit};
  }
  tests.lost_stream_fences_generation=async()=>{
    const h=await setup();await ready(h);
    const answer=h.client.call(want,bytes(7),x=>x);await settle();
    assert.deepEqual(h.server.calls[0].frame,call(savedStream(h.storage.raw).id,0n,7));
    delete h.storage.raw.streams[h.storage.raw.directory[0]];
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
    assert.equal((await a).tag,'Refused');assert.equal(savedStream(h.storage.raw).next,0n);
    a=h.client.call(want,bytes(8),x=>x);await settle();
    assert.deepEqual(h.server.calls[1].frame,call(savedStream(h.storage.raw).id,0n,8));
    h.server.answer(h.server.sockets[0],0,0n,returned(8));await settle();await a;
    assert.equal(savedStream(h.storage.raw).next,1n);
    a=h.client.call(want,bytes(9),x=>x);await settle();assert.deepEqual(h.server.calls[2].frame,call(savedStream(h.storage.raw).id,1n,9));
    h.server.answer(h.server.sockets[0],0,1n,returned(9));await settle();await a;h.server.check();
  };
  tests.commit_before_send=async()=>{
    for(const cut of ['commit','send','outcome']) {
      const store=new Ledger();
      const h=await setup(undefined,{prepare:server=>{server.onCall=(f,s)=>{
        const outcome=store.attempt(s.principal,f);
        if(cut==='outcome')server.answer(s,0,0n,outcome);
      };}});await ready(h);
      if(cut==='commit')h.storage.afterWrite=state=>{if(savedStream(state).pending?.inflight===false)throw new Error('crash after durable commit');};
      if(cut==='outcome')h.storage.beforeWrite=state=>{if(savedStream(state).terminal)throw new Error('crash before terminal commit');};
      const answer=h.client.call(want,bytes(7),x=>x);void answer.catch(()=>{});await settle();
      assert.equal(savedStream(h.storage.raw).pending.p,0n);
      assert.deepEqual(savedStream(h.storage.raw).pending.args,bytes(7));
      if(cut==='commit')assert.equal(h.server.calls.length,0);else assert.equal(h.server.calls.length,1);
      h.storage.afterWrite=undefined;h.storage.beforeWrite=undefined;await h.client.close();await settle();
      const recovered=await setup(h.storage,{wants:[other,want],prepare:server=>{server.onCall=(f,s)=>server.answer(s,0,0n,store.attempt(s.principal,f));}});
      await ready(recovered);
      assert.deepEqual(recovered.server.calls[0].frame,call(savedStream(h.storage.raw).id,0n,7,1));
      assert.equal(store.effects,1);assert.equal(savedStream(h.storage.raw).next,1n);
      assert.deepEqual(savedStream(h.storage.raw).terminal,{p:0n,outcome:returned(7)});
      assert.deepEqual(recovered.events,[returned(7)]);h.server.check();recovered.server.check();
      await recovered.client.close();
    }
  };
  tests.stream_lease=async()=>{
    const h=await setup();await ready(h);
    const b=await setup(h.storage);await ready(b);assert.notEqual(h.storage.raw.directory[0],h.storage.raw.directory[1]);
    const a=h.client.call(want,bytes(7),x=>x);await settle();
    await h.client.close();await a;await settle();
    const c=await setup(h.storage);await ready(c);assert.equal(h.storage.raw.directory.length,2);
    const next=c.client.call(want,bytes(8),x=>x);await settle();
    assert.deepEqual(savedStream(h.storage.raw).pending.args,bytes(7));
    assert.deepEqual(c.server.calls.map(x=>x.frame),[call(savedStream(h.storage.raw).id,0n,7)]);
    c.server.answer(c.server.sockets[0],0,0n,returned(7));await settle();
    assert.deepEqual(c.server.calls[1].frame,call(savedStream(h.storage.raw).id,1n,8));
    c.server.answer(c.server.sockets[0],0,1n,returned(8));await settle();await next;c.server.check();
  };
  tests.calls_wait_in_order=async()=>{
    const h=await setup();await ready(h);
    const first=h.client.call(want,bytes(7),x=>x);
    const args=bytes(8), second=h.client.call(want,args,x=>x);
    args[0]=0;
    const third=h.client.call(want,bytes(9),x=>x);
    const closed=assert.rejects(third,{code:'Closed'});
    await settle();
    assert.deepEqual(h.server.calls.map(x=>x.frame),[call(savedStream(h.storage.raw).id,0n,7)]);
    assert.deepEqual(savedStream(h.storage.raw).pending.args,bytes(7));
    h.server.answer(h.server.sockets[0],0,0n,returned(7));await settle();
    assert.deepEqual(await first,returned(7));
    assert.deepEqual(h.server.calls.map(x=>x.frame),[call(savedStream(h.storage.raw).id,0n,7),call(savedStream(h.storage.raw).id,1n,8)]);
    await h.client.close();
    assert.equal((await second).tag,'LocallyUnknown');
    await closed;
    assert.deepEqual(savedStream(h.storage.raw).pending.args,bytes(8));
    assert.equal(h.server.calls.length,2);h.server.check();
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
    assert.deepEqual(h.server.calls.at(-1).frame,call(savedStream(h.storage.raw).id,0n,7));
    h.server.answer(h.server.sockets.at(-1),0,0n,returned(7));await settle();await a;
    h.server.check();
    const gone=await setup();await ready(gone);
    gone.server.placed=false;gone.server.close(gone.server.sockets[0],1001);await settle();await gone.time.advance(50);
    gone.server.emit(gone.server.sockets.at(-1),{tag:'Placed',value:{tag:'Gone'}});await settle();await gone.time.advance(120000);
    assert.equal(gone.client.state,'Incompatible');assert.equal(gone.server.sockets.length,2);
  };
  // Close 1008 before Welcome is the place's binding refusing the Hello: its own terminal
  // state, not Incompatible, naming the missing credential when none was sent.
  tests.unbound_before_welcome=async()=>{
    const refuse=server=>{server.onHello=s=>server.close(s,1008);};
    for(const given of [undefined,async()=>credential]) {
      const h=await setup(new Memory(),{credential:given,prepare:refuse});await settle();await h.time.advance(120000);
      assert.equal(h.client.state,'Unbound');assert.equal(h.server.sockets.length,1);
      assert.equal(h.diagnostics.some(d=>d.includes('pass `credential`')),given===undefined);
      h.server.onHello=undefined;h.client.reconnect();await ready(h);
      assert.equal(h.client.state,'Ready');h.server.check();
    }
  };
  if (http) {
    for (const name of ['http.js', 'codec.js', 'entries.js']) {
      await writeFile(join(runtime.dir, name), await readFile(new URL(name, http)));
    }
    const {bindHttp} = await import(pathToFileURL(join(runtime.dir, 'http.js')).href);
    const encoder = new TextEncoder();
    const batchEvent = (version, complete, rows = []) => 'event: batch\ndata: ' + JSON.stringify({
      version, complete: complete ? [0] : [], groups: [{sub: 0, ...(complete && {value:{}}), rows}], outcomes: [],
    }) + '\n\n';
    const content = {headers: {'Content-Type': 'text/event-stream'}};
    const expire = 'event: expired\ndata: {"reason":"auth_expired"}\n\n';
    const flow = () => {
      const requests = [];
      const fetch = (url, options) => new Promise((resolve, reject) => {
        assert.equal(url.pathname, '/events');
        assert.equal(options.headers.get('Accept'), 'text/event-stream');
        assert.equal(options.headers.has('Last-Event-ID'), false);
        const abort = () => reject(options.signal.reason);
        options.signal.addEventListener('abort', abort, {once:true});
        requests.push({auth:options.headers.get('Authorization'), signal:options.signal,
          loss:() => reject(new TypeError('connection lost')),
          reject:(status,body='') => resolve(new Response(body,{status})),
          open:() => {
            let stream;
            resolve(new Response(new ReadableStream({start(controller){stream=controller;}}),content));
            return {send:text=>stream.enqueue(encoder.encode(text)), end:()=>stream.close()};
          },
        });
      });
      return {requests, connect:(h, options) => bindHttp(h.client, {url:'https://scripted', fetch, clock:h.time}).events(undefined, options)};
    };
    tests.http_subscribe_replaces_and_reconnects=async()=>{
      const c=source();
      const h=await setup(new Memory(),{credential:c.fn});await c.give(0,credential);await ready(h);
      const f=flow(), changes=[];
      const sub=f.connect(h,{onChange:(handle,change)=>{assert.equal(handle,sub);changes.push(change);}});
      await settle();
      const first=f.requests[0].open();
      first.send('\ufeff: comment\r\nretry: 40\r'); first.send('\n\r\n');
      first.send(batchEvent('9007199254740993',true,[{slot:'unit',table:'units',key:1,present:true,value:null}]));
      first.send(batchEvent('9007199254740992',false).slice(0,-1));
      await settle();
      assert.deepEqual(sub.get('unit',1),{tag:'Ready',value:{slot:'unit',key:1,value:{tag:'Some',value:undefined}}});
      first.end();await settle();
      assert.equal(f.requests.length,2); assert.equal(c.asks.length,1);
      assert.equal(f.requests[1].auth,f.requests[0].auth); assert.equal(sub.value().tag,'Pending');
      const second=f.requests[1].open();second.send(batchEvent('1',true));
      await settle(); assert.deepEqual(sub.rows('unit'),{tag:'Ready',value:[]});
      second.send('retry: 40\n\nevent: reset\ndata: {"version":"2","outcomes":[]}\n\n');
      await settle();assert.equal(sub.current(),false);assert.equal(f.requests.length,2);
      await h.time.advance(19);assert.equal(f.requests.length,2);
      await h.time.advance(1);assert.equal(f.requests.length,3);
      const third=f.requests[2].open();
      third.send(batchEvent('0',true));
      third.send('event: ended\ndata: {"sub":0,"outcome":{"Refused":"Deadline"}}\n\n');
      assert.deepEqual(await sub.done,{tag:'Ended',reason:{tag:'Refused',value:{tag:'Deadline'}}});
      assert.equal(sub.current(),false);assert.equal(c.asks.length,1);
      assert.equal(changes.filter(change=>change.invalidated).length,2);
    };
    tests.http_subscribe_refreshes_once=async()=>{
      for (const refusal of ['event','401']) {
        const c=source(), fresh=bytes(...Array(16).fill(3));
        const h=await setup(new Memory(),{credential:c.fn});await c.give(0,credential);await ready(h);
        const f=flow(), sub=f.connect(h);
        await settle();
        if(refusal==='event') {
          const stream=f.requests[0].open();stream.send(batchEvent('1',true));await settle();
          assert.equal(sub.current(),true);stream.send(expire);
        } else {
          f.requests[0].open().end();
          await settle();assert.equal(f.requests.length,2);assert.equal(c.asks.length,1);
          assert.equal(f.requests[1].auth,f.requests[0].auth);f.requests[1].reject(401);
        }
        await settle();
        assert.equal(h.client.state,'CredentialNeeded'); assert.equal(sub.current(),false); assert.equal(c.asks.length,2);
        await c.give(1,fresh);await settle();
        assert.notEqual(f.requests.at(-1).auth,f.requests[0].auth);
        f.requests.at(-1).reject(401);
        await assert.rejects(sub.done,{code:'Unbound'});assert.equal(c.asks.length,2);
      }
    };
    tests.http_subscribe_shares_refresh_and_aborts=async()=>{
      const c=source();
      const h=await setup(new Memory(),{credential:c.fn});await c.give(0,credential);await ready(h);
      const f=flow(), abort=new AbortController();
      const sub=f.connect(h,{signal:abort.signal});
      await settle();h.server.close(h.server.sockets[0],4401);
      await settle();assert.equal(c.asks.length,2);
      assert.equal(f.requests[0].signal.aborted,true);
      abort.abort();assert.deepEqual(await sub.done,{tag:'Aborted'});
      const second=f.connect(h);await settle();assert.equal(f.requests.length,1);
      const closed=h.client.close();await c.give(1,bytes(...Array(16).fill(2)));await closed;
      await assert.rejects(second.done,{code:'Closed'});assert.equal(f.requests.length,1);
    };
    tests.http_subscribe_distinguishes_refusal_from_loss=async()=>{
      const h=await setup();await ready(h);
      const f=flow(), sub=f.connect(h);
      await settle();f.requests[0].loss();await settle();
      assert.equal(f.requests.length,1);
      await h.time.advance(499);assert.equal(f.requests.length,1);
      await h.time.advance(1);assert.equal(f.requests.length,2);
      f.requests[1].reject(503,'too large');
      assert.deepEqual(await sub.done,{tag:'Refused',status:503,body:'too large'});
      await h.time.advance(10000);assert.equal(f.requests.length,2);
    };
  }
  // A credential source answers in the order its promises are settled by the law.
  const source=()=>{
    const asks=[];const fn=()=>new Promise((resolve,reject)=>asks.push({resolve,reject}));
    return {fn,asks,give:async(i,b)=>{asks[i].resolve(b);await settle();},fail:async i=>{asks[i].reject(new Error('down'));await settle();}};
  };
  const hello=f=>f.tag==='Hello';
  const helloCredential=(h,i)=>h.server.sent.filter(hello)[i]?.values[0];
  // 4401 ends a Welcomed session: images clear at once, the expired bytes are never sent
  // again, one refresh reconnects, and the pending call replays only after the same Caller's Welcome.
  tests.auth_expired_refreshes_once_and_recovers=async()=>{
    const c=source(),fresh=bytes(...Array(16).fill(2));
    const h=await setup(new Memory(),{credential:c.fn});await c.give(0,credential);await ready(h);
    const changes=[];h.client.subscribe(want,bytes(1),(_,change)=>changes.push(change));await settle();
    h.server.emit(h.server.sockets[0],batch(1n,[0],[group(0,[row(0,1,1)])]));await settle();
    const answer=h.client.call(want,bytes(7),x=>x);await settle();
    assert.deepEqual(h.server.calls[0].frame,call(savedStream(h.storage.raw).id,0n,7));
    const seen=changes.length;
    h.server.close(h.server.sockets[0],4401);
    assert(changes.length>seen,'the public image clears when the expiry is observed');
    await settle();assert.equal(h.client.state,'CredentialNeeded');
    await h.time.advance(120000);
    assert.equal(h.server.sockets.length,1,'no Hello may carry the expired credential');assert.equal(c.asks.length,2);
    await c.give(1,fresh);await settle();
    assert.deepEqual(helloCredential(h,1),{tag:'Some',value:fresh});
    assert.deepEqual(h.server.calls[1].frame,call(savedStream(h.storage.raw).id,0n,7));
    h.server.answer(h.server.sockets[1],savedStream(h.storage.raw).id,0n,returned(7));await settle();
    assert.deepEqual(await answer,returned(7));assert.equal(h.client.state,'Ready');h.server.check();
  };
  // An offline client whose ticket lapsed sees the expiry as 1008 before Welcome and recovers the same way.
  tests.offline_expiry_seen_as_1008_recovers=async()=>{
    const c=source(),fresh=bytes(...Array(16).fill(2));
    const h=await setup(new Memory(),{credential:c.fn});await c.give(0,credential);await ready(h);
    const answer=h.client.call(want,bytes(7),x=>x);await settle();
    h.server.close(h.server.sockets[0],1001);await settle();
    h.server.onHello=(s,f)=>{if(equalBytes(f.values[0].value,credential))h.server.close(s,1008);};
    await h.time.advance(50);
    assert.equal(h.client.state,'CredentialNeeded');assert.equal(h.server.calls.length,1);
    await c.give(1,fresh);await settle();
    assert.deepEqual(h.server.calls[1].frame,call(savedStream(h.storage.raw).id,0n,7));
    h.server.answer(h.server.sockets.at(-1),savedStream(h.storage.raw).id,0n,returned(7));await settle();
    assert.deepEqual(await answer,returned(7));h.server.check();
  };
  // One refresh per recovery attempt: an error, the refused bytes again, or a second refusal is Unbound.
  tests.one_refresh_then_unbound=async()=>{
    for(const ending of ['error','same','refused']) {
      const c=source();
      const h=await setup(new Memory(),{credential:c.fn});await c.give(0,credential);await ready(h);
      h.server.onHello=s=>h.server.close(s,1008);
      h.server.close(h.server.sockets[0],4401);await settle();
      if(ending==='error')await c.fail(1);
      else await c.give(1,ending==='same'?credential:bytes(...Array(16).fill(3)));
      await h.time.advance(120000);
      assert.equal(h.client.state,'Unbound',ending);assert.equal(c.asks.length,2,ending);
      assert.equal(h.server.sockets.length,ending==='refused'?2:1,ending);
    }
  };
  // WS and HTTP refusals share one refresh in flight; a refresh that settles after close is discarded.
  tests.refresh_shared_and_discarded_after_close=async()=>{
    const c=source();let replies=[];
    const fetch=()=>new Promise(resolve=>replies.push(resolve));
    const h=await setup(new Memory(),{credential:c.fn,http:{url:'http://scripted/',fetch}});await c.give(0,credential);await ready(h);
    const answer=h.client.callHttp(want,{path:'/.hirsel/op/save'});await settle();
    assert.equal(replies.length,1);
    h.server.close(h.server.sockets[0],4401);await settle();
    replies[0]({status:401,text:async()=>'',headers:new Headers()});await settle();
    assert.equal(c.asks.length,2,'one refresh serves both refusals');
    const closed=h.client.close();await c.give(1,bytes(...Array(16).fill(2)));await closed;await h.time.advance(120000);
    assert.equal(h.client.state,'Closed');assert.equal(h.server.sockets.length,1);
    assert.deepEqual(await answer,{tag:'LocallyUnknown'});
  };
  // The scripted Welcome serves n_place 1024; bindings generated for another value name both.
  tests.n_place_mismatch_names_both=async()=>{
    const h=await setup(new Memory(),{identity:{digest:bytes(3),target:'test',n_place:2048n}});await ready(h);
    const named=h.diagnostics.filter(d=>d.startsWith('launch identity mismatch'));
    assert.equal(named.length,1);assert(named[0].includes('2048')&&named[0].includes('1024'),named[0]);
  };
  tests.history_unavailable_retires=async()=>{
    const h=await setup();await ready(h);
    let a=h.client.call(want,bytes(7),x=>x);await settle();h.server.answer(h.server.sockets[0],0,0n,returned(7));await settle();await a;
    h.server.answer(h.server.sockets[0],0,0n,refused('HistoryUnavailable'));await settle();
    assert.equal(h.client.state,'Ready');assert.equal(savedStream(h.storage.raw).retired,false);
    a=h.client.call(want,bytes(8),x=>x);await settle();
    h.server.close(h.server.sockets[0],1001);await settle();await h.time.advance(50);
    h.server.answer(h.server.sockets[1],0,1n,refused('HistoryUnavailable'),'Reset');await settle();
    assert.equal(h.client.state,'Retired');assert(savedStream(h.storage.raw).retired);assert.deepEqual(await a,{tag:'LocallyUnknown'});
    await assert.rejects(h.client.call(want,bytes(9),x=>x),e=>e.code==='Retired');
    const b=await setup(h.storage);await ready(b);assert.notEqual(h.storage.raw.directory[0],h.storage.raw.directory[1]);assert.equal(b.server.calls.length,0);
    assert.equal(h.server.calls.length,3);h.server.check();
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
    savedStream(storage.raw).next=0n;
    const b=await setup(storage,{prepare:configure});await ready(b);assert.equal(storage.raw.directory.length,1);
    const rejected=b.client.call(want,bytes(8),x=>x);await settle();
    await b.time.advance(100);assert.equal(b.server.calls.length,1,'mismatched bytes must never be resent');
    assert.equal(b.client.state,'Fenced');
    assert.deepEqual(await rejected,{tag:'RequestMismatch'});assert.equal(store.effects,1);assert.notEqual(storage.raw.generation,h.client.generation);
    await b.time.advance(120000);assert.equal(b.server.calls.length,1);
    assert.deepEqual(storage.raw.directory,[]);
    const c=await setup(storage);await ready(c);assert.equal(c.server.calls.length,0);b.server.check();h.server.check();
  };
  tests.anonymous_reconnect_does_not_replay=async()=>{
    const h=await setup(undefined,{credential:undefined});await ready(h);
    const original=savedStream(h.storage.raw).caller;
    const pending=h.client.call(want,bytes(7),x=>x);await settle();
    h.server.close(h.server.sockets[0],1001);await settle();await h.time.advance(50);
    assert.deepEqual(await pending,{tag:'LocallyUnknown'});
    assert.notDeepEqual(savedStream(h.storage.raw,1).caller,original);
    assert.equal(h.server.calls.length,1);
    const fresh=h.client.call(want,bytes(8),x=>x);await settle();
    assert.deepEqual(h.server.calls[1].frame,call(savedStream(h.storage.raw,1).id,0n,8));
    h.server.answer(h.server.sockets[1],0,0n,returned(8));await settle();
    assert.deepEqual(await fresh,returned(8));h.server.check();
  };
  tests.retry_and_reconnect_order=async()=>{
    const h=await setup();await ready(h);const caches=[];
    h.client.subscribe(want,bytes(1),sub=>caches.push(sub.rows(0).value));await settle();
    const a=h.client.call(want,bytes(7),x=>x);await settle();
    h.server.close(h.server.sockets[0],1001);await settle();
    h.server.placed=false;await h.time.advance(50);
    assert.equal(h.server.calls.length,1,'must wait for every Placed');
    h.server.emit(h.server.sockets[1],{tag:'Placed',value:{tag:'Ready',value:0}});await settle();
    assert.deepEqual(h.server.calls[1].frame,call(savedStream(h.storage.raw).id,0n,7));
    assert.deepEqual(h.server.sent.slice(-2),[call(savedStream(h.storage.raw).id,0n,7),{tag:'Subscribe',values:[1,0,bytes(1),{tag:'None'}]}]);
    h.server.answer(h.server.sockets[1],0,0n,refused('Deadline'));await settle();await h.time.advance(100);
    assert.deepEqual(h.server.calls[2].frame,call(savedStream(h.storage.raw).id,0n,7));
    h.server.answer(h.server.sockets[1],0,0n,returned(7));await settle();await a;h.server.check();
  };
  tests.uncertain_refusal_retires=async()=>{
    const h=await setup();await ready(h);const a=h.client.call(want,bytes(7),x=>x);await settle();
    h.server.close(h.server.sockets[0],1001);await settle();await h.time.advance(50);
    h.server.answer(h.server.sockets[1],0,0n,refused('NoEntry'));await settle();
    assert.deepEqual(await a,{tag:'LocallyUnknown'});assert.equal(h.client.state,'Retired');
    assert.equal(savedStream(h.storage.raw).next,0n);assert.deepEqual(savedStream(h.storage.raw).pending.args,bytes(7));
  };
  tests.batch_complete=async()=>{
    const h=await setup();await ready(h);const caches=[];
    h.client.subscribe(want,bytes(1),(sub,change)=>{if(!change.invalidated)caches.push(sub.rows(0).value);});await settle();
    const a=h.client.call(want,bytes(7),x=>x);await settle();
    h.server.emit(h.server.sockets[0],{tag:'Batch',values:[1n,[0],[{sub:0,value:bytes(),rows:[{slot:0,key:bytes(1),value:{tag:'Some',value:bytes(2)}}]}],[{stream:savedStream(h.storage.raw).id,p:0n,outcome:returned(7)}]]});await settle();await a;
    assert.equal(caches[0][0].value.value[0],2);
    h.server.emit(h.server.sockets[0],{tag:'Reset',values:[2n,[]]});await settle();assert.equal(caches.length,1);
    h.server.emit(h.server.sockets[0],{tag:'Batch',values:[2n,[],[{sub:0,value:bytes(),rows:[{slot:0,key:bytes(1),value:{tag:'None'}}]}],[]]});await settle();assert.equal(caches.length,1);
    h.server.emit(h.server.sockets[0],{tag:'Batch',values:[2n,[1],[{sub:1,value:bytes(),rows:[]}],[]]});await settle();assert.deepEqual(caches,[caches[0],[]]);
    const before=caches.length;
    h.server.emit(h.server.sockets[0],{tag:'Reset',values:[3n,[]]});await settle();
    h.server.emit(h.server.sockets[0],{tag:'Batch',values:[3n,[],[{sub:2,value:bytes(),rows:[{slot:0,key:bytes(1),value:{tag:'Some',value:bytes(3)}}]}],[]]});await settle();
    assert.equal(h.client.state,'Incompatible');assert.equal(caches.length,before);
  };
  tests.grouped_accessors=async()=>{
    const h=await setup(); await ready(h);
    let unitDecodes=0; const calls=[];
    const access={
      value(data){ if(data.length===0)unitDecodes++; return data[0]; },
      slot(i){ return ['left','right'][i]; },
      row(r){ return {...r,slot:this.slot(r.slot)}; },
      key(_slot,data){ return data; },
    };
    const first=h.client.subscribe(want,bytes(1),(sub,change)=>{
      if(!change.invalidated)calls.push([sub.value(),second.value()]);
    },undefined,access);
    const second=h.client.subscribe(want,bytes(2),()=>{},undefined,access);
    const pending={tag:'Pending'};
    assert.deepEqual(first.value(),pending);
    assert.deepEqual(second.rows('left'),pending);
    await settle();
    const socket=h.server.sockets[0];
    const sample={slot:0,key:bytes(8),value:{tag:'Some',value:bytes(12)}};
    const batch=(version,complete,groups)=>h.server.emit(socket,{tag:'Batch',values:[version,complete,groups,[]]});
    batch(1n,[0,1],[{sub:0,value:bytes(),rows:[sample]},{sub:1,value:bytes(9),rows:[]}]);
    await settle();
    assert.deepEqual(calls,[[{tag:'Ready',value:undefined},{tag:'Ready',value:9}]]);
    assert.equal(unitDecodes,1);
    assert.deepEqual(first.rows('left'),{tag:'Ready',value:[{...sample,slot:'left'}]});
    assert.deepEqual(second.rows('right'),{tag:'Ready',value:[]});
    batch(2n,[],[
      {sub:0,value:bytes(),rows:[{...sample,value:{tag:'None'}},{...sample,slot:1}]},
      {sub:1,value:bytes(11),rows:[]},
    ]);
    await settle();
    assert.equal(unitDecodes,1);
    assert.deepEqual(calls.at(-1),[{tag:'Ready',value:undefined},{tag:'Ready',value:11}]);
    assert.deepEqual(first.rows('left'),{tag:'Ready',value:[]});
    assert.deepEqual(first.rows('right'),{tag:'Ready',value:[{...sample,slot:'right'}]});

    h.server.emit(socket,{tag:'Reset',values:[3n,[]]}); await settle();
    assert.deepEqual(first.rows('right'),pending);
    assert.deepEqual(second.value(),pending);
    batch(3n,[],[{sub:0,value:bytes(42),rows:[]}]); await settle();
    assert.equal(calls.length,2,'the retired id must be ignored');
    batch(4n,[2],[{sub:2,value:bytes(4),rows:[]}]); await settle();
    assert.deepEqual(first.value(),{tag:'Ready',value:4});
    h.server.close(socket,1001);
    assert.deepEqual(first.value(),pending);
    first.unsubscribe();
    assert.deepEqual(first.value(),{tag:'Ended'});
    assert.deepEqual(first.rows('left'),{tag:'Ended'});
  };
  tests.L_D1a_identical_complete=async()=>{
    const {sub,changes,emit}=await subscribed();
    const sample=row(0,8,12);
    await emit(1,true,[sample],bytes(9));
    assert.deepEqual(changes,[{replaced:true,value:{new:bytes(9)},rows:[{slot:0,key:'08',new:sample}]}]);
    const original=sub.rows(0).value;
    await emit(2,true,[sample],bytes(9));
    await emit(3,false,[sample,row(0,10)],bytes(9));
    assert.equal(changes.length,1,'identical images and no-op edits are silent');
    assert.strictEqual(sub.rows(0).value,original);
  };
  tests.L_D1b_memo_per_slot=async()=>{
    const {sub,emit}=await subscribed();
    await emit(1,true,[row(0,1,2),row(1,1,3)]);
    const left=sub.rows(0).value,right=sub.rows(1).value,index=sub.map(1).value;
    await emit(2,false,[row(0,1,4)]);
    assert.notStrictEqual(sub.rows(0).value,left);
    assert.strictEqual(sub.rows(1).value,right);
    assert.strictEqual(sub.map(1).value,index);
    assert.strictEqual(sub.get(1,bytes(1)).value,right[0]);
    assert.strictEqual(index.get('01'),right[0]);
  };
  tests.L_D1c_frozen_rows=async()=>{
    const typed={
      value: data=>({total:data[0]}),
      slot: ()=>'data',
      row: r=>({field:r.key[0],array:[r.value.value[0]],tuple:[{value:7},false]}),
      key: (_slot,k)=>bytes(k),
    };
    const {sub,changes,emit}=await subscribed(typed);
    await emit(1,true,[row(0,8,12)],bytes(9));
    const list=sub.rows('data').value;
    const [sample]=list;
    assert.throws(()=>{sample.field=3;},TypeError);
    assert.throws(()=>{sample.array[0]=3;},TypeError);
    assert.throws(()=>{sample.tuple[1]=true;},TypeError);
    assert.throws(()=>{sample.tuple[0].value=3;},TypeError);
    assert.throws(()=>{list.push(sample);},TypeError);
    assert.throws(()=>{sub.value().value.total=3;},TypeError);
    assert.deepEqual(sub.get('data',8).value,{field:8,array:[12],tuple:[{value:7},false]});
    assert.deepEqual(sub.value().value,{total:9});
    assert.strictEqual(changes[0].rows[0].new,sample);
  };
  tests.L_D1d_replay_batch_changes=async()=>{
    const {sub,changes,emit}=await subscribed();
    const expected=[new Map(),new Map()];
    let seed=2198;
    const pick=n=>{
      seed=(Math.imul(seed,1664525)+1013904223)>>>0;
      return (seed>>>16)%n;
    };
    await emit(1,true,[]);
    for(let v=2;v<130;v++){
      const complete=pick(5)===0;
      const previous=[sub.map(0).value,sub.map(1).value];
      const replay=previous.map(map=>new Map(map));
      const rows=[];
      for(let slot=0;slot<2;slot++){
        if(complete)expected[slot].clear();
        for(let k=0;k<6;k++){
          if(pick(3)!==0)continue;
          const val=pick(4) || undefined;
          if(complete && val===undefined)continue;
          const edit=row(slot,k,val),hex='0'+k;
          rows.push(edit);
          if(val===undefined)expected[slot].delete(hex);else expected[slot].set(hex,edit);
        }
      }
      const before=changes.length;
      await emit(v,complete,rows);
      assert(changes.length<=before+1);
      for(const change of changes.slice(before)){
        assert.equal(change.replaced,complete);
        const seen=new Set();
        for(const edit of change.rows){
          const map=replay[edit.slot],identity=edit.slot+':'+edit.key;
          assert(!seen.has(identity));
          seen.add(identity);
          assert.strictEqual(edit.old,map.get(edit.key));
          if('new' in edit)map.set(edit.key,edit.new);else map.delete(edit.key);
        }
      }
      for(let slot=0;slot<2;slot++){
        assert.deepEqual(replay[slot],sub.map(slot).value);
        assert.deepEqual(sub.map(slot).value,expected[slot]);
      }
    }
  };
  tests.L_D1e_invalidate_before_replacement=async()=>{
    const h=await setup();await ready(h);
    const pending={tag:'Pending'}, invalidation={invalidated:true,replaced:false,rows:[]};
    const writeViews=[], events=[];
    const read=sub=>[sub.value(),sub.rows(0),sub.map(0),sub.get(0,bytes(1))];
    const view=()=>[read(left),read(right)];
    const record=(sub,change)=>events.push({sub,change,view:view()});
    const left=h.client.subscribe(want,bytes(1),record);
    const right=h.client.subscribe(want,bytes(2),record);
    const bothPending=[Array(4).fill(pending),Array(4).fill(pending)];
    await settle();
    const first=h.server.sockets[0];
    const emit=async(socket,frame)=>{h.server.emit(socket,frame);await settle();};
    const secret=row(0,1,41), fresh=row(0,2,17);
    await emit(first,batch(1n,[0,1],[group(0,[secret],bytes(90)),group(1,[secret],bytes(91))]));
    const expectCleared=(offset,handles=[left,right])=>{
      assert.deepEqual(events.slice(offset),handles.map(sub=>({sub,change:invalidation,view:bothPending})));
    };
    await emit(first,{tag:'Reset',values:[2n,[]]});
    expectCleared(2);
    await emit(first,batch(2n,[0],[group(0,[secret],bytes(90))]));
    expectCleared(2);
    await emit(first,batch(3n,[2],[group(2,[fresh],bytes(17))]));
    assert.deepEqual(events[4].change,{replaced:true,value:{new:bytes(17)},rows:[{slot:0,key:'02',new:fresh}]});
    assert.deepEqual(events[4].view[1],bothPending[1]);

    // A staged group waits for a durable outcome while the old socket closes.
    const result=h.client.call(want,bytes(5),v=>v);
    await settle();
    let release;
    h.storage.beforeWrite=state=>{
      if(savedStream(state).terminal?.p===0n)return new Promise(resolve=>{release=resolve;});
    };
    await emit(first,batch(4n,[3],[group(3,[secret],bytes(91))],[{stream:savedStream(h.storage.raw).id,p:0n,outcome:returned(5)}]));
    try {
      assert.equal(typeof release,'function');
      const late=first.onmessage;
      h.server.close(first,1001);
      expectCleared(5,[left]);
      late({data:codec.encode(batch(5n,[2],[group(2,[secret],bytes(90))]))});
    } finally {
      release?.();
    }
    assert.deepEqual(await result,returned(5));
    await settle();await h.time.advance(50);
    expectCleared(5,[left]);

    const second=h.server.sockets[1];
    await emit(second,batch(1n,[4,5],[group(4,[]),group(5,[fresh],bytes(17))]));
    assert.deepEqual(events[6].change,{replaced:true,value:{new:bytes()},rows:[]});
    assert.deepEqual(events[7].change,{replaced:true,value:{new:bytes(17)},rows:[{slot:0,key:'02',new:fresh}]});
    h.opts.onState=state=>{if(state==='StorageUnavailable')writeViews.push(view());};
    h.opts.onDiagnostic=()=>writeViews.push(view());
    h.storage.beforeWrite=()=>{throw new Error('storage unavailable');};
    await assert.rejects(h.client.call(want,bytes(5),v=>v),/storage unavailable/);
    await settle();
    expectCleared(8);
    assert.deepEqual(writeViews,[bothPending,bothPending]);
    h.storage.beforeWrite=undefined;
  };
  tests.protocol_reload_or_reply_error=async()=>{
    const retired=await setup(undefined,{prepare:server=>{server.welcome=false;}});
    const ready=retired.client.whenReady();
    void ready.catch(()=>{});
    retired.server.close(retired.server.sockets[0],1009,'hirsel.4');
    await assert.rejects(ready,{code:'Reload'});
    await retired.time.advance(60000);
    assert.equal(retired.server.sockets.length,1);
    assert.match(retired.diagnostics.at(-1),/Reload required/);
    const current=await setup(); await current.client.whenReady();
    const pending=current.client.call(want,bytes(3),data=>data);
    void pending.catch(()=>{}); await settle();
    current.server.close(current.server.sockets[0],1009,'hirsel.4');
    await assert.rejects(pending,{code:'ReplyTooLarge'});
    assert.equal(savedStream(current.storage.raw).pending.p,0n);
    assert.match(current.diagnostics.at(-1),/Stored reply/);
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
    assert.deepEqual(subscription.value(),{tag:'Ended'});
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
  tests.browser_wakes_only_in_backoff=async()=>{
    const browser=new EventTarget(),page=new EventTarget();
    const originalWindow=Object.getOwnPropertyDescriptor(globalThis,'window');
    const originalDocument=Object.getOwnPropertyDescriptor(globalThis,'document');
    const online=()=>browser.dispatchEvent(new Event('online'));
    const visibility=state=>{page.visibilityState=state;page.dispatchEvent(new Event('visibilitychange'));};
    Object.defineProperty(globalThis,'window',{configurable:true,value:browser});
    Object.defineProperty(globalThis,'document',{configurable:true,value:page});
    try {
      const h=await setup(undefined,{prepare:server=>{server.welcome=false;}});
      let ready=false;
      const pending=h.client.whenReady();
      void pending.then(()=>{ready=true;},()=>{});
      h.server.close(h.server.sockets[0],1001);await settle();
      assert.equal(h.client.state,'Backoff');
      const scheduled=[...h.time.timers];
      visibility('hidden');await settle();
      assert.equal(h.client.state,'Backoff');assert.equal(h.server.sockets.length,1);
      assert.deepEqual([...h.time.timers],scheduled);

      h.server.welcome=true;
      online();online();await settle();
      assert.equal(h.server.sockets.length,2);assert.equal(h.client.state,'Ready');assert.equal(ready,true);
      await pending;
      online();visibility('hidden');await h.time.advance(50);
      assert.equal(h.client.state,'Ready');assert.equal(h.server.sockets.length,2);
      assert.equal(h.server.sockets[1].closed,false);

      h.server.close(h.server.sockets[1],1001);await settle();
      visibility('visible');await settle();
      assert.equal(h.server.sockets.length,3);assert.equal(h.client.state,'Ready');

      h.server.close(h.server.sockets[2],1001);await settle();
      const elapsed=h.time.advance(200);
      online();await elapsed;
      assert.equal(h.server.sockets.length,4);

      h.server.close(h.server.sockets[3],1001);await settle();
      assert.equal(getEventListeners(browser,'online').length,1);
      assert.equal(getEventListeners(page,'visibilitychange').length,1);
      await h.client.close();
      assert.equal(getEventListeners(browser,'online').length,0);
      assert.equal(getEventListeners(page,'visibilitychange').length,0);
      online();visibility('visible');await h.time.advance(1000);
      assert.equal(h.client.state,'Closed');assert.equal(h.server.sockets.length,4);
    }finally{
      if(originalWindow)Object.defineProperty(globalThis,'window',originalWindow);else delete globalThis.window;
      if(originalDocument)Object.defineProperty(globalThis,'document',originalDocument);else delete globalThis.document;
    }
  };
  tests.failed_advances_and_request_is_immutable=async()=>{
    const h=await setup();await ready(h);
    const arg=bytes(7),w=structuredClone(want);
    const a=h.client.call(w,arg,x=>x);arg[0]=9;w.identity[0]=9;await settle();
    assert.deepEqual(h.server.calls[0].frame,call(savedStream(h.storage.raw).id,0n,7));
    assert.deepEqual(savedStream(h.storage.raw).pending.want,want);
    h.server.answer(h.server.sockets[0],0,0n,{tag:'Failed',value:{tag:'Raised',value:1}});await settle();
    assert.deepEqual(await a,{tag:'Failed',value:{tag:'Raised',value:1}});assert.equal(savedStream(h.storage.raw).next,1n);
  };
  tests.retryable_outcomes_keep_identity=async()=>{
    const h=await setup();await ready(h);const a=h.client.call(want,bytes(7),x=>x);await settle();
    for(const outcome of [{tag:'Abandoned'},refused('Overloaded'),{tag:'Refused',value:{tag:'Budget',value:{requested:10n,available:0n}}}]) {
      const n=h.server.calls.length;
      h.server.answer(h.server.sockets[0],0,0n,outcome);await settle();
      assert.equal(savedStream(h.storage.raw).next,0n);
      await h.time.advance(1000);
      assert.equal(h.server.calls.length,n+1);assert.deepEqual(h.server.calls.at(-1).frame,call(savedStream(h.storage.raw).id,0n,7));
      assert(!savedStream(h.storage.raw).pending.untouched);
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
  Object.assign(tests,optimisticLaws({setup,ready,want,bytes,row,group,batch,settle,returned,entries,savedStream}));
  let cases=0;
  try {
    for(const [name,test] of Object.entries(tests)) {
      if(only && !(Array.isArray(only)?only.includes(name):name===only))continue;
      await test();cases++;console.error(`client ${name}: PASS`);
    }
    if(!cases)throw new Error('zero client laws selected');
    return {cases};
  }finally{
    try{for(const client of clients)await client.close();}
    finally{await runtime.dispose();}
  }
}
