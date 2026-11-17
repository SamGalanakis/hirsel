#!/usr/bin/env python3
"""Build the portable Rust SHA-256 partner and check its committed law vectors.

Run after tools/buck2/prepare.py. Output and build intermediates belong outside
this checkout, e.g. --out /tmp/sha256-reference. No Cargo feature graph changes.
"""
import argparse
import json
from pathlib import Path
import re
import subprocess

ROOT = Path(__file__).resolve().parents[2]
DEPS = ('block-buffer', 'cfg-if', 'crypto-common', 'digest', 'generic-array', 'typenum')


def build(out):
    names = re.findall(r"third_party_rust_library\(name='([^']+)'", (ROOT / 'third-party/BUCK').read_text())
    targets = []
    for dependency in DEPS:
        matches = [n for n in names if n.startswith(dependency + '-')]
        if len(matches) != 1:
            raise ValueError(f'expected one {dependency} target, found {matches}')
        targets.append(f'//third-party:{matches[0]}[static]')
    command = [str(ROOT / '.buck2/bin/buck2'), 'build', '--target-platforms',
               '//tools/buck2:release', *targets, '//third-party:sha2[sources]', '--show-json-output']
    outputs = json.loads(subprocess.check_output(command, cwd=ROOT, text=True))
    libraries = [ROOT / outputs['root' + target] for target in targets]
    sources = ROOT / outputs['root//third-party:sha2[sources]']
    crate = sources / 'sha2-0.10.9.crate/src/lib.rs'
    rustc = ROOT / 'tools/buck2/toolchains/rust-files/bin/rustc'
    search = [arg for library in libraries for arg in ('-L', 'dependency=' + str(library.parent))]
    externs = []
    for name in ('cfg_if', 'digest'):
        library, = [p for p in libraries if p.name.startswith('lib' + name + '-')]
        externs += ['--extern', name + '=' + str(library)]
    subprocess.run([str(rustc), '--edition=2018', '--crate-name=sha2', '--crate-type=rlib',
                    '-Copt-level=3', '-Ccodegen-units=16', '--cfg', 'feature="std"',
                    '--cfg', 'feature="force-soft"', str(crate), *externs, *search,
                    '-o', str(out / 'libsha2.rlib')], cwd=ROOT, check=True)
    binary = out / 'rust-sha256'
    # Buck's static rlibs contain absolute relocations. This executable does not
    # need PIE; hash work is identical and startup is amortized by the benchmark.
    subprocess.run([str(rustc), '--edition=2024', '-Copt-level=3', '-Clink-arg=-no-pie',
                    str(ROOT / 'core/tools/sha256_oracle.rs'), '--extern',
                    'sha2=' + str(out / 'libsha2.rlib'), *search, '-o', str(binary)],
                   cwd=ROOT, check=True)
    return binary


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--out', type=Path, required=True)
    args = parser.parse_args()
    out = args.out.resolve()
    if out == ROOT or ROOT in out.parents:
        parser.error('--out must be outside the checkout')
    out.mkdir(parents=True, exist_ok=True)
    binary = build(out)
    vectors = subprocess.check_output([str(binary), 'vectors'], text=True).splitlines()
    if len(vectors) != 201 or any(not re.fullmatch('[0-9a-f]{64}', h) for h in vectors):
        raise ValueError('oracle did not emit 201 SHA-256 digests')
    expected = [int(h[i:i + 8], 16) for h in vectors for i in range(0, 64, 8)]
    source = (ROOT / 'core/std/tests/sha256.hirsel').read_text()
    table = re.search(r'const HASHES: Inline\[u32, 1608\]\s*=\s*\[(.*?)\]', source, re.S)
    actual = [int(h, 16) for h in re.findall(r'0x([0-9a-f]+)u32', table[1])]
    if actual != expected:
        raise ValueError('committed SHA-256 vectors differ from RustCrypto sha2')
    print(f'201 Rust SHA-2 vectors and 20,301 streaming splits agree; partner {binary}')


if __name__ == '__main__':
    main()
