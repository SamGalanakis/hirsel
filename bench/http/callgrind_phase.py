"""Exclusive phase estimates from complete, startup-subtracted Callgrind profiles."""

import collections
import json
import subprocess
from pathlib import Path

import attribution

def rows(path):
    bucket = collections.Counter()
    summary = 0
    with path.open() as profile:
        function = filename = actual = binary = ''
        called = False
        for line in profile:
            if line.startswith('ob='):
                binary = line.strip()[3:]
            elif line.startswith('fl='):
                filename = actual = line.strip()[3:]
            elif line.startswith('fn='):
                function = line.strip()[3:].split("'")[0]
                actual = filename
            elif line.startswith(('fi=', 'fe=')):
                actual = line.strip()[3:]
            elif line.startswith('summary:'):
                summary = int(line.split()[1])
            elif line.startswith('calls='):
                called = True
            elif line.startswith('0x'):
                if not called:
                    addr, lineno, count = line.split()
                    bucket[binary, filename, actual, function, int(addr, 16), int(lineno)] += int(count)
                called = False
    return bucket, summary

def estimate(directory, binaries, http_source, http1_source):
    source = http_source.read_text().splitlines()
    stdlib = http1_source.read_text().splitlines()
    results = {}
    for name in binaries:
        low, startup = rows(directory / f"{name}.1024.callgrind")
        high, loaded = rows(directory / f"{name}.8192.callgrind")
        high.subtract(low)
        weight = collections.Counter()
        hirsel = name == 'hirsel'
        executable = binaries[name].resolve()
        locations = {}
        if hirsel:
            # Callgrind records ELF virtual addresses for position-independent executables.
            pcs = sorted({key[-2] for key in high if Path(key[0]) == executable})
            output = subprocess.run(['llvm-symbolizer',f'--obj={executable}','--inlines','--output-style=JSON'],
                                    input=''.join(f'{pc:#x}\n' for pc in pcs), text=True, capture_output=True, check=True)
            locations = {pc: json.loads(line)['Symbol'] for pc,line in zip(pcs,output.stdout.splitlines(),strict=True)}
        for (binary, _, actual, symbol, pc, lineno), count in high.items():
            if not count:
                continue
            phase = None
            if hirsel and Path(binary) == executable:
                for frame in locations.get(pc, []):
                    file = Path(frame['StartFileName']).name
                    start = frame['StartLine']
                    line = frame['Line']
                    code = source if file == 'http.hirsel' else stdlib if file == 'http1.hirsel' else []
                    function = ''
                    if code and 1 <= start <= len(code):
                        match = attribution.FUNCTION.search(code[start-1])
                        function = match[1] if match else ''
                    text = code[line-1] if 1 <= line <= len(code) else ''
                    phase = attribution.hirsel_phase(file,function,text)
                    if phase:
                        break
            if not hirsel and Path(actual).name=='main.rs' and name=='tokio' and 21 <= lineno <= 31:
                phase = 'Parse' if lineno < 26 else 'Handler' if lineno < 29 else 'Response write' if lineno == 29 else 'Copies'
            symbol = attribution.CRATE_HASH.sub('',symbol)
            phase = phase or attribution.rust_phase(symbol)
            if not phase:
                if any(s in actual for s in ('/tokio-', '/mio-')):
                    phase = 'Scheduler and reactor'
                elif 'httparse' in actual or any(s in actual for s in ('/http-1.5.0/src/uri/', '/hyper-1.12.0/src/proto/h1/role.rs')):
                    phase = 'Parse'
                elif '/hyper-1.12.0/src/proto/h1/' in actual:
                    phase = 'Response write'
                elif 'serde' in actual or '/alloc/' in actual or 'halloc' in actual:
                    phase = 'Accounting and charges'
            phase = phase or 'Unattributed'
            weight[phase] += count
        total = sum(weight.values())
        if total <= 0 or abs(total / (loaded - startup) - 1) > 0.005:
            raise RuntimeError(f"incomplete self-cost attribution for {name}")
        shares = {p: weight[p] / total for p in attribution.PHASES}
        results[name] = ((loaded - startup) / (8192 - 1024), shares)
    return results
