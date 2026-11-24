#!/usr/bin/env python3
"""LAYOUT family build cells: check/lower, fresh, record reuse and one body edit.

No runtime ratio is inferred from these compilation totals. Rust libraries are
prebuilt as MEASURE.md requires; all first-party source lines are counted.
The JSON fresh-build row also counts its executable lifecycle for the runtime budget.
"""
import argparse
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys

ROOT = Path(__file__).resolve().parents[3]
sys.path.insert(0, str(ROOT / 'core/tools'))
import measure
import measurement_artifacts
from run import manifest
from service import execute, sample


def runtime_budget(group, backend, side, observed):
    # The matched Rust JSON family costs 378.7 M instructions with checked
    # arithmetic, x86-64, O3 and the committed documents.json. Allow Rust +2%,
    # rounded up to 0.1 M, independently of Hirsel's current count (json-regress-1080).
    if (group, backend, side) != ('json', 'llvm', 'hirsel'):
        return {}
    rust = 378_700_000
    maximum = (rust * 102 // 100 + 99_999) // 100_000 * 100_000
    instructions = observed.get('instructions')
    return dict(rust_instructions=rust, instruction_budget=maximum,
                budget_passed=observed.get('status') == 'ok'
                and type(instructions) is int and 0 < instructions <= maximum)


def inputs(args, module, source):
    request = manifest(module)
    request['packages'][-1]['sources'][0]['file'] = str(source)
    file = source.with_suffix('.inputs.json')
    file.write_text(json.dumps(request))
    return file, request


def source_sets(args, group):
    folder = Path(__file__).resolve().parent
    source = (folder / (group + '.hirsel')).read_text()
    if group == 'graph':
        adapter = '\npub fn build_family(out:Stdout)->Result[(),Exit] waits {let n=result.unwrap(lifetime());show(out,Ok(n+0))}\n'
        expected = b'45533440\n'
    elif group == 'documents':
        adapter = '''
pub fn build_family(out:Stdout)->Result[(),Exit] waits {
    let mut n:u64=0
    for _ in 0u64 .. 64 {n+=result.unwrap(ordinary(8192));n+=result.unwrap(compact_sized(8192));n+=result.unwrap(compact_streaming(8192))}
    show(out,n+0)
}
'''
        expected = b'6447952320\n'
    else:
        # Calls the two existing capabilities/entry bodies in order. Two reads,
        # separate parse/print lifetimes and two outputs are preserved on Rust.
        adapter = '\npub fn build_family(out:Stdout,d:Dir)->Result[(),Exit] waits {json_parse_main(out,d)?;json_print_main(out,d)?;show(out,0+0)}\n'
        expected = b'262144\n241502240\n0\n'
    own = args.out / (group + '.hirsel')
    own.write_text(source + adapter)
    request, data = inputs(args, group, own)
    if group == 'graph':
        candidates = {'matched': 'graph_matched.rs', 'idiomatic': 'graph_idiomatic.rs'}
    elif group == 'documents':
        candidates = {'matched': 'rust.rs', 'idiomatic': 'rust.rs'}
    else:
        candidates = {'matched': 'json_matched.rs', 'idiomatic': 'json_serde.rs'}
    rust = {}
    for side, name in candidates.items():
        text = (folder / name).read_text().split('fn main()', 1)[0]
        if group == 'graph':
            text = text.replace('include!("graph_inputs.rs");', 'include!(' + json.dumps(str(folder / 'graph_inputs.rs')) + ');')
            if side == 'matched':
                body = 'exclusive(EXCLUSIVE_NODES,EXCLUSIVE_ROUNDS).unwrap()+churn(CHURN_NODES,CHURN_ROUNDS).unwrap()+snapshots_rc(SNAPSHOT_NODES,SNAPSHOT_VERSIONS).unwrap()+snapshots_arc(SNAPSHOT_NODES,SNAPSHOT_VERSIONS).unwrap()'
            else:
                body = 'exclusive(EXCLUSIVE_NODES,EXCLUSIVE_ROUNDS)+churn(CHURN_NODES,CHURN_ROUNDS)+snapshots_rc(SNAPSHOT_NODES,SNAPSHOT_VERSIONS)+snapshots_arc(SNAPSHOT_NODES,SNAPSHOT_VERSIONS)'
            text += '\nfn main(){let n=' + body + ';println!("{}",n+0);}\n'
        elif group == 'documents':
            text = text.split('#[derive(Clone)]', 1)[0]
            term = 'ordinary(8192)+packed(8192,false)+packed(8192,true)' if side == 'matched' else 'ordinary(8192)+ordinary(8192)+ordinary(8192)'
            text += '\nfn main(){let mut n=0u64;for _ in 0..64{n+=' + term + ';}println!("{}",n+0);}\n'
        elif side == 'matched':
            text += '''
fn main(){
 let path=std::env::args().nth(1).unwrap();
 {let input=std::fs::read_to_string(&path).unwrap();let n:u64=(0..32).map(|_|read32(&parse(&input),1) as u64).sum();println!("{n}");}
 {let input=std::fs::read_to_string(&path).unwrap();let v=parse(&input);let n:u64=(0..32).map(|_|{let mut out=Vec::new();print(&v,0,&mut out);out.into_iter().map(u64::from).sum::<u64>()}).sum();println!("{n}");}
 println!("{}",0+0);
}
'''
        else:
            text += '''
fn main(){
 let path=std::env::args().nth(1).unwrap();
 {let input=std::fs::read_to_string(&path).unwrap();let n:u64=(0..32).map(|_|serde_json::from_str::<serde_json::Value>(&input).unwrap().as_array().unwrap().len() as u64).sum();println!("{n}");}
 {let input=std::fs::read_to_string(&path).unwrap();let v=serde_json::from_str::<serde_json::Value>(&input).unwrap();let n:u64=(0..32).map(|_|serde_json::to_string(&v).unwrap().bytes().map(u64::from).sum::<u64>()).sum();println!("{n}");}
 println!("{}",0+0);
}
'''
        file = args.out / (group + '-' + side + '.rs')
        file.write_text(text)
        rust[side] = file, text
    return own, source + adapter, request, data, rust, expected


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--artifacts', type=Path, required=True)
    parser.add_argument('--out', type=Path, required=True)
    parser.add_argument('--group', choices=['graph', 'documents', 'json'], required=True)
    parser.add_argument('--backend', choices=['llvm', 'cranelift'], required=True)
    parser.add_argument('--release-twins', type=Path, required=True)
    parser.add_argument('--dev-twins', type=Path, required=True)
    parser.add_argument('--release-serde-core', type=Path)
    parser.add_argument('--stage', action='append', choices=['check', 'fresh', 'reuse', 'edit'])
    parser.add_argument('--side', action='append', choices=['hirsel', 'matched', 'idiomatic'])
    parser.add_argument('--reuse-hirsel-check', type=Path)
    args = parser.parse_args()
    os.environ['HIRSEL_MEMORY_LIMIT'] = '2G'
    args.out = args.out.resolve()
    args.out.mkdir(parents=True, exist_ok=False)
    artifacts = json.loads(args.artifacts.read_text())
    identity = measurement_artifacts.identity(Path(artifacts['compiler']), Path(artifacts['runtime']), artifacts['compiler_build_profile'])
    own, original, request, data, twins, expected = source_sets(args, args.group)
    sources = [Path(x['file']) for p in data['packages'] for x in p['sources']]
    app = [Path(x['file']) for x in data['packages'][-1]['sources']]
    counts = dict(request_code_lines=sum(measure.source_lines(p) for p in sources), own_code_lines=sum(measure.source_lines(p) for p in app))
    rows = []
    def save(row):
        rows.append(row)
        (args.out / 'results.json').write_text(json.dumps(rows, indent=2, default=str) + '\n')

    primed = False
    for stage in ['check', 'fresh', 'reuse', 'edit']:
        if args.stage and stage not in args.stage:
            continue
        if stage == 'edit':
            own.write_text(original.replace('+0', '+1'))
            for file, text in twins.values():
                file.write_text(text.replace('+0', '+1'))
        for side in args.side or ['hirsel', 'matched', 'idiomatic']:
            if side == 'hirsel' and stage == 'check' and args.reuse_hirsel_check:
                row = next(x for x in json.loads(args.reuse_hirsel_check.read_text()) if x['side'] == 'hirsel' and x['stage'] == 'check')
                row = dict(row, backend=args.backend, reused_check_from=str(args.reuse_hirsel_check))
                save(row)
                continue
            row = dict(group=args.group, backend=args.backend, stage=stage, side=side, **identity, **counts)
            if side == 'hirsel' and stage in ('reuse', 'edit') and not primed:
                row.update(status='blocked', cause='no successful fresh build in this store')
                save(row)
                print(json.dumps(row, default=str), flush=True)
                continue
            binary = args.out / f'{stage}-{side}-program'
            directory = args.out / f'{stage}-{side}'
            if side == 'hirsel':
                command = [artifacts['compiler'], 'check' if stage == 'check' else 'build', '--inputs', request,
                           '--entry', f'layout@1/{args.group}/build_family', '--source-core', '--store', args.out / 'store' / ('check' if stage == 'check' else 'build'),
                           '--json', '--metrics', args.out / (stage + '.metrics.json')]
                if stage != 'check':
                    command += ['--backend', args.backend, '--runtime', artifacts['runtime'], '--grant', 'Stdout', '-o', binary]
                    if args.backend == 'llvm':
                        command += ['--release']
                    if args.group == 'json':
                        command += ['--dir', Path(__file__).with_name('inputs').resolve()]
            else:
                file, _ = twins[side]
                flags = ['--edition=2024', '--target=x86_64-unknown-linux-gnu', '-C', 'target-cpu=x86-64',
                         '-C', 'opt-level=' + ('3' if args.backend == 'llvm' else '0'), '-C', 'overflow-checks=yes',
                         '-C', 'debug-assertions=' + ('no' if args.backend == 'llvm' else 'yes'),
                         '-C', 'codegen-units=1', '-C', 'lto=no', '-C', 'debuginfo=0']
                command = [ROOT / 'tools/buck2/toolchains/rust-files/bin/rustc', *flags, file, '-o', binary]
                if stage == 'check':
                    command += ['--emit=metadata']
                if args.group == 'json' and side == 'idiomatic':
                    deps = (args.release_twins if args.backend == 'llvm' else args.dev_twins).resolve()
                    command += ['--extern', f'serde_json={deps / "libserde_json.rlib"}', '-L', f'dependency={deps}']
                    if args.backend == 'llvm' and args.release_serde_core:
                        command += ['-L', f'dependency={args.release_serde_core.resolve().parent}']
            if side != 'hirsel':
                row['own_code_lines'] = measure.source_lines(twins[side][0])
                if args.group == 'graph':
                    row['own_code_lines'] += measure.source_lines(Path(__file__).with_name('graph_inputs.rs'))
            row.update(sample(command, directory))
            if side == 'hirsel':
                try:
                    product = json.loads((directory / 'stdout').read_text())
                    if not product.get('accepted') or stage != 'check' and not product.get('native'):
                        row['status'] = 'refused-or-incomplete'
                        row['cause'] = product.get('stopped') or product.get('diagnostics') or product.get('incomplete')
                except ValueError:
                    row['status'] = 'invalid-product'
                metrics = args.out / (stage + '.metrics.json')
                if metrics.exists():
                    report = json.loads(metrics.read_text())
                    row['check_lower_instructions'] = next((x['instructions'] for x in report.get('stages', []) if x['name'] == 'check/lower'), None)
            if stage != 'check' and row['status'] == 'ok':
                command = [binary]
                if side != 'hirsel' and args.group == 'json':
                    command += [Path(__file__).with_name('inputs').resolve() / 'documents.json']
                want = expected if stage != 'edit' else (b'262144\n241502240\n1\n' if args.group == 'json' else f'{int(expected)+1}\n'.encode())
                if (stage, args.group, args.backend, side) == ('fresh', 'json', 'llvm', 'hirsel'):
                    runtime_directory = args.out / f'{stage}-{side}-run'
                    runtime = sample(command, runtime_directory)
                    output = (runtime_directory / 'stdout').read_bytes()
                    if output != want:
                        runtime.update(status='wrong-output', output=output.decode(errors='replace'))
                    row['runtime'] = runtime
                    row.update(runtime_budget(args.group, args.backend, side, runtime))
                    if runtime['status'] != 'ok':
                        row['status'] = runtime['status']
                else:
                    observed = execute(command)
                    if observed.returncode or observed.stdout != want or observed.stderr:
                        row.update(status='wrong-output', output=observed.stdout.decode(errors='replace'), error=observed.stderr.decode(errors='replace'))
            if side == 'hirsel' and stage == 'fresh':
                primed = row['status'] == 'ok'
            save(row)
            print(json.dumps(row, default=str), flush=True)
            if stage != 'check' and binary.exists():
                binary.unlink()
    store = args.out / 'store'
    if store.is_dir():
        shutil.rmtree(store)
    return int(any(r['status'] != 'ok' or r.get('budget_passed') is False for r in rows))


if __name__ == '__main__':
    raise SystemExit(main())
