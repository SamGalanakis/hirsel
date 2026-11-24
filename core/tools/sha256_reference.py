#!/usr/bin/env python3
"""Build the Rust SHA-256 partner and check its committed law vectors.

The partner is the workspace's RustCrypto sha2 with its default backends: SHA-NI
when the CPU has it (selected at run time by cpufeatures), its portable
code otherwise.

Run after tools/buck2/prepare.py. Output and build intermediates belong outside
this checkout, e.g. --out /tmp/sha256-reference. No Cargo feature graph changes.
"""
import argparse
import json
from pathlib import Path
import re
import subprocess

ROOT = Path(__file__).resolve().parents[2]


def closure(buck):
    """The third-party libraries the named sha2 target depends on, by target name."""
    rows = {}
    for line in buck.splitlines():
        if line.startswith('third_party_rust_library('):
            rows[re.search(r"name='([^']+)'", line[line.rindex(", name='"):])[1]] = line
    sha2 = re.search(r"^alias\(name='sha2', actual=':([^']+)'", buck, re.MULTILINE)[1]
    pending, seen = [sha2], []
    while pending:
        name = pending.pop()
        if name in seen:
            continue
        seen.append(name)
        line = rows[name]
        x86 = line[line.index("'linux-x86_64'"):line.index("'linux-arm64'")]
        pending += re.findall(r"'//third-party:([^']+)'", line[:line.index('platform=')] + x86)
    return seen


def build(out):
    targets = [f'//third-party:{name}[static]' for name in closure((ROOT / 'third-party/BUCK').read_text())]
    command = [str(ROOT / '.buck2/bin/buck2'), 'build', '--target-platforms',
               '//tools/buck2:release', *targets, '--show-json-output']
    outputs = json.loads(subprocess.check_output(command, cwd=ROOT, text=True))
    libraries = [ROOT / outputs['root' + target] for target in targets]
    rustc = ROOT / 'tools/buck2/toolchains/rust-files/bin/rustc'
    sha2, *deps = libraries
    search = [arg for library in deps for arg in ('-L', 'dependency=' + str(library.parent))]
    binary = out / 'rust-sha256'
    # Buck's static rlibs contain absolute relocations. This executable does not
    # need PIE; hash work is identical and startup is amortized by the benchmark.
    subprocess.run([str(rustc), '--edition=2024', '-Copt-level=3', '-Clink-arg=-no-pie',
                    str(ROOT / 'core/tools/sha256_oracle.rs'), '--extern',
                    'sha2=' + str(sha2), *search, '-o', str(binary)],
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
