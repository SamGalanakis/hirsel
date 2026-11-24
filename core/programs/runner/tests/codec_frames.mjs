// The TypeScript partner of codec_frames.hirsel, loaded by std_native.py.
import assert from 'node:assert/strict';
import {laws as clientLaws} from './client.mjs';
const bytes = hex => Uint8Array.from(Buffer.from(hex.replaceAll(' ',''),'hex'));

export async function vectors(c,frame,entries) {
  assert.equal(frame.subprotocol,'hirsel.3');
  assert.strictEqual(frame.Frame,c.Frame);
  assert.deepEqual(frame.Raised,{Overflow:1,Bounds:2,Division:3,Unwrap:4,Fail:5,Cancelled:6,Capacity:7,OutOfTime:8});
  assert.deepEqual(frame.BadKind,{Truncated:0,Invalid:1,Utf8:2,Duplicate:3,Trailing:4,Schema:5,Value:6});
  assert.deepEqual(entries.identity,{digest:bytes('0102'),target:'t',n_place:16777216n});
  assert.equal(entries.want[0].args_fp,9007199254740993n);
  let unsubscribed = false;
  const client = entries.bind({
    call(want,args,decode) {
      if (want === entries.want[0]) {
        assert.equal(Buffer.from(args).toString('hex'),'0700000000000000');
        return Promise.resolve({tag:'Returned',value:decode(bytes('0800000000000000'))});
      }
      assert.strictEqual(want,entries.want[2]);
      assert.equal(want.result_fp,9n);
      const committed = args[0] === 1;
      const payload = committed ? '000000000900000000000000' : '0100000003000000626164';
      return Promise.resolve({tag:'Returned',value:decode(bytes(payload))});
    },
    subscribe(want,args,onChange,onEnd,decode) {
      assert.strictEqual(want,entries.want[1]);
      assert.equal(Buffer.from(args).toString('hex'),'03000000');
      if (decode) {
        const key=9007199254740993n, suffix=bytes('0020000000000001');
        assert.deepEqual(decode.key('rows',key),suffix);
        assert.equal(decode.keyPrefixBytes,4);
        const wire={slot:0,key:bytes('00000003 0020000000000001'),value:{tag:'Some',value:bytes('01000000 cd')}};
        assert.deepEqual(decode.row(wire),{slot:'rows',key,value:{tag:'Some',value:bytes('cd')}});
      }
      const subscription = {rows: () => ({tag:'Ready',value:[]}), unsubscribe() { unsubscribed = true; }};
      onChange(subscription,{replaced:true,rows:[]});
      onEnd({tag:'Refused',value:{tag:'Deadline'}});
      return subscription;
    }
  });
  assert.deepEqual(await client.call.main.toggle(7n),{tag:'Returned',value:8n});
  assert.deepEqual(await client.call.main.commit(1n),{tag:'Returned',value:{tag:'Ok',value:{n:9n}}});
  assert.deepEqual(await client.call.main.commit(0n),{tag:'Returned',value:{tag:'Error',value:{message:'bad'}}});
  let delivered = false;
  let reason;
  client.subscribe.main.rows(3, (sub,change) => {
    assert.deepEqual(sub.rows(0),{tag:'Ready',value:[]});
    assert.deepEqual(change,{replaced:true,rows:[]});
    delivered = true;
  }, end => { reason = end; }).unsubscribe();
  client.subscribeDecoded.main.rows(3, () => {}, () => {}).unsubscribe();
  assert.equal(delivered,true); assert.equal(unsubscribed,true);
  assert.deepEqual(reason,{tag:'Refused',value:{tag:'Deadline'}});
  let cases = 0;
  function vector(value,hex) {
    cases++;
    assert.equal(Buffer.from(c.Frame.encode(value)).toString('hex'),hex.replaceAll(' ',''));
    assert.deepEqual(c.Frame.decode(bytes(hex)),value);
    assert.throws(() => c.Frame.decode(bytes(hex+'00')),c.Malformed);
    for (let n=0;n<bytes(hex).length;n++) assert.throws(() => c.Frame.decode(bytes(hex).slice(0,n)),c.Malformed);
  }
  vector({tag:'Hello',values:[{tag:'None'},[]]},'00000000 00000000 00000000');
  vector({tag:'Hello',values:[{tag:'Some',value:bytes('aa')},[{identity:bytes('bb'),args_fp:1n,result_fp:2n}]]},'00000000 01000000 01000000 aa 01000000 01000000 bb 0100000000000000 0200000000000000');
  vector({tag:'Call',values:[7,9007199254740993n,2,3,bytes('ff')]},'01000000 07000000 0100000000002000 02000000 03000000 01000000 ff');
  vector({tag:'Subscribe',values:[1,2,bytes(''),{tag:'None'}]},'02000000 01000000 02000000 00000000 00000000');
  vector({tag:'Subscribe',values:[1,2,bytes(''),{tag:'Some',value:bytes('aa')}]},'02000000 01000000 02000000 00000000 01000000 01000000 aa');
  vector({tag:'Unsubscribe',value:7},'03000000 07000000');
  vector({tag:'Welcome',values:[{issuer:2,subject:bytes('aa')},2,3n,bytes(''),'t',4n]},'04000000 02 01000000 aa 02000000 0300000000000000 00000000 01000000 74 0400000000000000');
  vector({tag:'Placed',value:{tag:'Ready',value:3}},'05000000 00000000 03000000');
  vector({tag:'Placed',value:{tag:'Resolve',values:[3,bytes('07')]}},'05000000 01000000 03000000 01000000 07');
  vector({tag:'Placed',value:{tag:'Gone'}},'05000000 02000000');
  vector({tag:'Batch',values:[2n,[],[],[]]},'06000000 0200000000000000 00000000 00000000 00000000');
  vector({tag:'Batch',values:[2n,[7],[{sub:7,value:bytes(''),rows:[]}],[]]},
    '06000000 0200000000000000 01000000 07000000 01000000 07000000 00000000 00000000 00000000');
  vector({tag:'Batch',values:[2n,[],[{sub:3,value:bytes(''),rows:[{slot:2,key:bytes('aa'),value:{tag:'None'}}]}],[{stream:4,p:5n,outcome:{tag:'Returned',value:bytes('')}}]]},
    '06000000 0200000000000000 00000000 01000000 03000000 00000000 01000000 0200 01000000 aa 00000000 01000000 04000000 0500000000000000 00000000 00000000');
  vector({tag:'Batch',values:[2n,[7],[{sub:7,value:bytes('cc'),rows:[{slot:3,key:bytes('aa'),value:{tag:'Some',value:bytes('bb')}}]}],[]]},
    '06000000 0200000000000000 01000000 07000000 01000000 07000000 01000000 cc 01000000 0300 01000000 aa 01000000 01000000 bb 00000000');
  vector({tag:'Reset',values:[2n,[]]},'08000000 0200000000000000 00000000');
  vector({tag:'Reset',values:[2n,[{stream:3,p:4n,outcome:{tag:'Refused',value:{tag:'HistoryUnavailable'}}}]]},'08000000 0200000000000000 01000000 03000000 0400000000000000 02000000 07000000');
  for (const [outcome,hex] of [
    [{tag:'Returned',value:bytes('aa')},'00000000 01000000 aa'],
    [{tag:'Failed',value:{tag:'Raised',value:1}},'01000000 00000000 01000000'],
    [{tag:'Failed',value:{tag:'Raised',value:4294967295}},'01000000 00000000 ffffffff'],
    [{tag:'Failed',value:{tag:'OutOfBytes',value:{requested:2n,available:3n}}},'01000000 01000000 0200000000000000 0300000000000000'],
    [{tag:'Failed',value:{tag:'OutOfTime',value:5n}},'01000000 02000000 0500000000000000'],
    [{tag:'Failed',value:{tag:'Gone'}},'01000000 03000000'],
    [{tag:'Failed',value:{tag:'Conflict',value:'ix'}},'01000000 04000000 02000000 6978'],
    [{tag:'Refused',value:{tag:'NoEntry'}},'02000000 00000000'],
    [{tag:'Refused',value:{tag:'Malformed',value:{kind:1,at:2n}}},'02000000 01000000 01000000 0200000000000000'],
    [{tag:'Refused',value:{tag:'Overloaded'}},'02000000 02000000'],
    [{tag:'Refused',value:{tag:'Deadline'}},'02000000 03000000'],
    [{tag:'Refused',value:{tag:'Budget',value:{requested:2n,available:3n}}},'02000000 04000000 0200000000000000 0300000000000000'],
    [{tag:'Refused',value:{tag:'NotAllowed'}},'02000000 05000000'],
    [{tag:'Refused',value:{tag:'RequestMismatch'}},'02000000 06000000'],
    [{tag:'Refused',value:{tag:'HistoryUnavailable'}},'02000000 07000000'],
    [{tag:'Abandoned'},'03000000'],[{tag:'InDoubt'},'04000000']
  ]) vector({tag:'Ended',values:[3,outcome]},'07000000 03000000 '+hex);
  assert.throws(() => c.Frame.decode(bytes('09000000')),c.Malformed);
  assert.throws(() => c.Frame.decode(bytes('0500000003000000')),c.Malformed);
  assert.throws(() => c.Frame.decode(bytes('0000000002000000')),c.Malformed);
  assert.throws(() => c.Frame.decode(bytes('070000000000000005000000')),c.Malformed);
  assert.throws(() => c.Frame.decode(bytes('07000000000000000200000008000000')),c.Malformed);
  assert.throws(() => c.Frame.encode({tag:'Ended',values:[3,{tag:'Superseded'}]}),TypeError);
  cases += (await clientLaws(c.Frame)).cases;
  return {cases};
}

export function differential(c) {
  let state = 1066; const chunks = [];
  for (let i=0;i<10000;i++) {
    state = (Math.imul(state,1664525)+1013904223)>>>0;
    const p = (BigInt(state)<<32n)|BigInt(i), n = BigInt(state);
    let value;
    switch (i%9) {
      case 0: value={tag:'Hello',values:[i%2 === 0 ? {tag:'None'} : {tag:'Some',value:bytes('0102')},[{identity:bytes('03'),args_fp:n,result_fp:p}]]}; break;
      case 1: value={tag:'Call',values:[i,p,state%4,123,Uint8Array.of(state&255)]}; break;
      case 2: value={tag:'Subscribe',values:[i,2,bytes('07'),i%2 === 0 ? {tag:'None'} : {tag:'Some',value:bytes('08')}]}; break;
      case 3: value={tag:'Unsubscribe',value:i}; break;
      case 4: value={tag:'Welcome',values:[{issuer:2,subject:bytes('0102')},4,n,bytes('03'),'t',16777216n]}; break;
      case 5: value={tag:'Placed',value:state%3 === 0 ? {tag:'Ready',value:i} : state%3 === 1 ? {tag:'Resolve',values:[i,bytes('09')]} : {tag:'Gone'}}; break;
      case 6: value={tag:'Batch',values:[n,state%2 === 0 ? [i] : [],[{sub:i,value:bytes(''),rows:[{slot:0,key:bytes('01'),value:{tag:'Some',value:bytes('03')}}]}],[{stream:i,p,outcome:{tag:'Returned',value:bytes('04')}}]]}; break;
      case 7: value={tag:'Ended',values:[i,i%2 === 0 ? {tag:'Failed',value:{tag:'Raised',value:state}} : {tag:'Refused',value:{tag:'Deadline'}}]}; break;
      default: value={tag:'Reset',values:[n,[{stream:i,p,outcome:[{tag:'Abandoned'},{tag:'InDoubt'},{tag:'Refused',value:{tag:'RequestMismatch'}},{tag:'Refused',value:{tag:'HistoryUnavailable'}}][state%4]}]]};
    }
    const encoded = c.Frame.encode(value); assert.deepEqual(c.Frame.decode(encoded),value); chunks.push(encoded);
  }
  return {cases:10000,hex:Buffer.concat(chunks).toString('hex')};
}
