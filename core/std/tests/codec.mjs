// TypeScript half of codec.hirsel's laws, invoked by std_native.py.
import assert from 'node:assert/strict';

export function vectors(c) {
  let cases = 0;
  const hex = bytes => Buffer.from(bytes).toString('hex');
  const from = value => Uint8Array.from(Buffer.from(value.replaceAll(' ',''),'hex'));
  function vector(codec,value,bytes) {
    cases++;
    assert.equal(hex(codec.encode(value)),bytes.replaceAll(' ',''));
    assert.deepEqual(codec.decode(from(bytes)),value);
    assert.throws(() => codec.decode(from(bytes + '00')),c.Malformed);
    for (let n=0;n<from(bytes).length;n++) assert.throws(() => codec.decode(from(bytes).slice(0,n)),c.Malformed);
  }
  vector(c.values.u64,0n,'0000000000000000');
  vector(c.values.u64,18446744073709551615n,'ffffffffffffffff');
  vector(c.values.u64,9007199254740993n,'0100000000002000');
  vector(c.values.i64,-9223372036854775808n,'0000000000000080');
  assert.throws(() => c.values.u64.encode(9007199254740993),TypeError);
  for (const [name,value,bytes] of [['unit',undefined,''],['bool',true,'01'],['char','é','e9000000'],['u8',255,'ff'],['u16',4660,'3412'],['u32',7,'07000000'],['i8',-1,'ff'],['i16',-2,'feff'],['i32',-3,'fdffffff'],['f32',1,'0000803f'],['f64',-0.5,'000000000000e0bf'],['string','hé','0300000068c3a9'],['string','\ufeff','03000000efbbbf']]) vector(c.values[name],value,bytes);
  vector(c.values.bytes,from('00ff'),'0200000000ff');
  for (const [op,value,bytes] of [[6,0n,'0000000000000000'],[5,7,'07000000']]) {
    vector(c.makeCodec([14,[op]]),[],'00000000');
    vector(c.makeCodec([14,[op]]),[value],'01000000'+bytes);
  }
  const option = c.makeCodec([16,[16,[5]]]);
  vector(option,{tag:'None'},'00000000');
  vector(option,{tag:'Some',value:{tag:'None'}},'0100000000000000');
  vector(option,{tag:'Some',value:{tag:'Some',value:7}},'010000000100000007000000');
  vector(c.makeCodec([16,[0]]),{tag:'Some',value:undefined},'01000000');
  vector(c.makeCodec([17,[6],[13]]),{tag:'Ok',value:3n},'000000000300000000000000');
  vector(c.makeCodec([17,[6],[13]]),{tag:'Error',value:'e'},'010000000100000065');
  const neverResult = c.makeCodec([17,[3],[20]]);
  vector(neverResult,{tag:'Ok',value:7},'0000000007');
  assert.throws(() => neverResult.decode(from('01000000')),c.Malformed);
  assert.throws(() => neverResult.encode({tag:'Error',value:undefined}),TypeError);
  vector(c.makeCodec([15,[13],[3]]),new Map([['a',7],['b',8]]),'02000000010000006107010000006208');
  vector(c.makeCodec([18,[[6],[13]]]),[5n,'a'],'05000000000000000100000061');
  const defs = [{kind:0,body:[{name:'x',type:[5]},{name:'gone',type:[6],removed:true},{name:'y',type:[13]}]},{kind:1,body:[{name:'A',fields:[]},{name:'B',fields:[[6]]},{name:'C',fields:[[3],[13]]},{name:'Gone',fields:[],removed:true}]},{kind:2,body:[14,[6]]}];
  vector(c.makeCodec([19,0],defs),{x:7,y:'a'},'070000000100000061');
  vector(c.makeCodec([19,1],defs),{tag:'A'},'00000000');
  vector(c.makeCodec([19,1],defs),{tag:'B',value:5n},'010000000500000000000000');
  vector(c.makeCodec([19,1],defs),{tag:'C',values:[7,'a']},'02000000070100000061');
  vector(c.makeCodec([19,2],defs),[3n],'010000000300000000000000');
  const keyDefs = [{kind:0,body:[{name:'id',type:[5]}]}];
  const keyed = c.makeCodec([15,[19,0],[3]],keyDefs);
  vector(keyed,new Map([[{id:1},7],[{id:2},8]]),'0200000001000000070200000008');
  assert.throws(() => keyed.decode(from('0200000001000000070100000008')),c.Malformed);
  assert.throws(() => keyed.encode(new Map([[{id:1},7],[{id:1},8]])),TypeError);
  assert.throws(() => keyed.encode(new Map([[{id:1,extra:2},7],[{id:1},8]])),TypeError);
  assert.throws(() => c.makeCodec([15,[16,[0]],[0]]).encode(new Map([[{tag:'Some'},undefined],[{tag:'Some',value:undefined},undefined]])),TypeError);
  assert.throws(() => c.makeCodec([15,[16,[3]],[0]]).decode(from('020000000000000000000000')),c.Malformed);
  assert.throws(() => c.makeCodec([19,1],defs).decode(from('03000000')),c.Malformed);
  for (const bytes of ['01000000ff','02000000c080','03000000eda080']) assert.throws(() => c.values.string.decode(from(bytes)),c.Malformed);
  assert.throws(() => c.values.string.encode('\ud800'),TypeError);
  for (const [name,bytes] of [['bool','02'],['char','00d80000'],['char','00001100'],['f32','0100c07f'],['f64','010000000000f87f']]) assert.throws(() => c.values[name].decode(from(bytes)),c.Malformed);
  assert.equal(hex(c.values.f32.encode(NaN)),'0000c07f');
  assert.equal(hex(c.values.f64.encode(NaN)),'000000000000f87f');
  assert.throws(() => c.makeCodec([16,[3]]).decode(from('02000000')),c.Malformed);
  assert.throws(() => c.makeCodec([15,[14,[3]],[0]]).decode(from('0200000001000000070100000007')),c.Malformed);
  for (const [name,value,bytes] of [['u8',255,'ff'],['u16',4660,'1234'],['u32',7,'00000007'],['u64',9007199254740993n,'0020000000000001'],['i8',-1,'7f'],['i16',-2,'7ffe'],['i32',-3,'7ffffffd'],['i64',-9223372036854775808n,'0000000000000000'],['bool',true,'01'],['string','a\0b','6100ff620000'],['bytes',from('00ff'),'00ffff0000']]) vector(c.keys[name],value,bytes);
  vector(c.makeCodec([18,[[9],[13]]],[],true),[-1,'a'],'7fffffff610000');
  for (const bytes of ['00','0001']) assert.throws(() => c.keys.bytes.decode(from(bytes)),c.Malformed);
  return {cases};
}

export function differential(c) {
  const row = c.makeCodec([19,0],[{kind:0,body:[{name:'n',type:[6]},{name:'label',type:[13]},{name:'nested',type:[16,[16,[5]]]}]}]);
  const chunks = []; let state = 1066;
  for (let i=0;i<10000;i++) {
    state = (Math.imul(state,1664525) + 1013904223) >>> 0;
    const value = {n:(BigInt(state)<<32n) | BigInt(i), label:i%2 === 0 ? 'hé' : 'x', nested:i%3 === 0 ? {tag:'None'} : {tag:'Some',value:i%3 === 1 ? {tag:'None'} : {tag:'Some',value:state}}};
    const bytes = row.encode(value); assert.deepEqual(row.decode(bytes),value); chunks.push(bytes);
  }
  return {cases:10000,hex:Buffer.concat(chunks).toString('hex')};
}
