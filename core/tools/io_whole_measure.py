#!/usr/bin/env python3
"""One LLVM sample each for growing read_string versus a fixture-known exact-size read.

Run io_measure.py first to prepare the identical 4MiB input. Logical charges/copy
counts are derived from the source GROWTH law; RSS and instructions are measured.
"""
from pathlib import Path
import argparse,json,os,subprocess
from langarena import inputs
from std_native import wait_load
root=Path(__file__).resolve().parents[2]
parser=argparse.ArgumentParser(description=__doc__)
parser.add_argument('--compiler',type=Path,required=True)
parser.add_argument('--runtime',type=Path,required=True)
parser.add_argument('--out',type=Path,required=True)
args=parser.parse_args()
base=args.out.resolve();tools={'root//:hirsel-core-driver-bin-hirsel-core':str(args.compiler.resolve()),'root//:hirsel-core-rt[staticlib]':str(args.runtime.resolve())}
manifest=base/'whole.inputs.json';manifest.write_text(json.dumps(inputs(root,'tests/io/read_whole')))
binary=base/'hirsel-llvm-whole';wait_load()
build=subprocess.run([str(Path(tools['root//:hirsel-core-driver-bin-hirsel-core'])),'build','--inputs',str(manifest),'--entry','langarena@1/tests/io/read_whole/main','--backend','llvm','--release','--runtime',str(Path(tools['root//:hirsel-core-rt[staticlib]'])),'--store',str(base/'store'),'--dir',str(base/'host'),'--grant','Stdout','--json','-o',str(binary)],capture_output=True)
(base/'whole.build.json').write_bytes(build.stdout);assert build.returncode==0,(build.stdout,build.stderr)
result={}
for mode,capacity in [('growth',8388608),('exact',4194304)]:
 stem=base/f'whole-{mode}';wait_load()
 with Path(str(stem)+'.stdout').open('wb') as out,Path(str(stem)+'.stderr').open('wb') as err:
  run=subprocess.run(['taskset','-c','16',str(root/'tools/capped'),'perf','stat','--no-big-num','-x',';','-e','instructions:u','-o',str(stem)+'.perf','--','/usr/bin/time','-f','%M','-o',str(stem)+'.rss',str(binary),mode],stdout=out,stderr=err,env=os.environ|{'HIRSEL_MEMORY_LIMIT':'2G','HIRSEL_JOBS':'1'})
 assert run.returncode==0 and Path(str(stem)+'.stdout').read_text()==f'4194304 {capacity}\n'
 instructions=int(next(line.split(';')[0] for line in Path(str(stem)+'.perf').read_text().splitlines() if ';instructions:u;' in line))
 result[mode]=dict(instructions=instructions,instructions_per_byte=instructions/4194304,peak_rss_kib=int(Path(str(stem)+'.rss').read_text()),retained=capacity)
 print(mode,result[mode],flush=True)
(base/'whole-results.json').write_text(json.dumps(result,indent=2)+'\n')
