from pathlib import Path
import os,sys,subprocess,json,fcntl,time,argparse
p=argparse.ArgumentParser(description="Measure reserved List/Bytes/String append; reject LLVM carrier-escape regressions")
p.add_argument('--builder',type=Path,required=True)
p.add_argument('--runtime',type=Path,required=True)
p.add_argument('--out',type=Path,required=True)
p.add_argument('--prepare',action='store_true')
p.add_argument('--check',action='store_true',help='Check the measured LLVM instructions/push against the regression envelope')
a=p.parse_args()
root=Path(__file__).resolve().parents[2];out=a.out.resolve();out.mkdir(parents=True,exist_ok=True);work=out;side='candidate';prepare=a.prepare;builder=a.builder.resolve();rt=a.runtime.resolve();cc=root/'tools/buck2/toolchains/llvm-files/bin/hirsel-clang'
sys.path.insert(0,str(root/'core/tools'));import m1
spec=m1.library_inputs(root,{'name':'core','version':'1'});spec['packages'].append(dict(package={'name':'hot','version':'1'},dependencies=[{'name':'core','version':'1'},{'name':'std','version':'1'}],sources=[dict(file=str(root/'crates/hirsel-core-rt/testdata/append.hirsel'),modules=['loops'],signatures=False)]));ip=work/'inputs.json';ip.write_text(json.dumps(spec|{'charge_policy':'charge-v1'}));env=os.environ|dict(HIRSEL_MEMORY_LIMIT='2G',HIRSEL_JOBS='1',LD_LIBRARY_PATH=str(root/'tools/buck2/toolchains/llvm-files/lib'),XDG_CACHE_HOME=str(work/'cache'),XDG_DATA_HOME=str(work/'data'))
def run(cmd,log):
 while os.getloadavg()[0]>32:time.sleep(15)
 with log.open('w') as f:r=subprocess.run([str(x) for x in cmd],env=env,stdout=f,stderr=subprocess.STDOUT)
 assert r.returncode==0,(log,log.read_text()[-4000:])
caller=(root/'crates/hirsel-core-rt/testdata/append.c').read_text()
rows=[]
for engine in ['llvm','cranelift']:
 d=work/engine;d.mkdir(exist_ok=True);cmd=[root/'tools/capped',builder,ip,d,rt,root/'crates/hirsel-core-rt/include',cc,'list_push,byte_push,char_push']
 if engine=='cranelift':cmd.append(engine)
 if not (d/'symbols.json').exists():run(cmd,d/'build.log')
 names=json.loads((d/'symbols.json').read_text())
 for op,want in [('list_push',523776),('byte_push',1536),('char_push',122880)]:
  entry=names[op];symbols=set()
  for obj in entry['objects']:symbols.update(l.split()[-1] for l in subprocess.check_output(['nm','-u',obj],text=True).splitlines() if 'hirsel_core_site_' in l)
  src=d/(op+'.c');src.write_text(caller.replace('BODY',entry['symbol']).replace('EXPECTED',str(want))+'\n'+''.join('const uint64_t '+v+'=0;\n' for v in symbols));binary=d/op
  if not binary.exists():run([root/'tools/capped',cc,'-O3','-no-pie','-Wl,--wrap=hirsel_core_grow','-I',root/'crates/hirsel-core-rt/include',src,*entry['objects'],rt,'-lgcc_s','-lpthread','-ldl','-lm','-lrt','-lutil','-o',binary],d/(op+'-link.log'))
  if prepare:continue
  while os.getloadavg()[0]>32:time.sleep(15)
  with open('/tmp/hirsel-langarena-measure.lock','a') as la,open('/tmp/core-llvm-905-measure.lock','a') as co:
   fcntl.flock(co,fcntl.LOCK_EX);fcntl.flock(la,fcntl.LOCK_EX);r=subprocess.run([root/'tools/capped','taskset','-c','16',binary],env=env,capture_output=True,text=True)
  assert r.returncode==0,(op,engine,r.stderr);row=dict(side=side,engine=engine,operation=op,**json.loads(r.stdout));rows.append(row);print(row,flush=True);(out/'rows.json').write_text(json.dumps(rows,indent=2))

if a.check and not prepare:
 # Broad ceilings separate the observed carrier escape (16.43/17.12/31.21)
 # from promoted locals (14.43/15.38/16.21), with room for call/setup noise.
 ceilings={'list_push':15.5,'byte_push':16.2,'char_push':25.0}
 failures=[r for r in rows if r['engine']=='llvm' and r['instructions_per_push']>ceilings[r['operation']]]
 if failures: raise SystemExit('reserved append regression: '+json.dumps(failures))
