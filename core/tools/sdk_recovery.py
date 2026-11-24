#!/usr/bin/env python3
"""Build the disk-backed SDK recovery Place and run the client laws over loopback."""

import argparse
import json
import os
from pathlib import Path
import subprocess
import tempfile

from std_native import ROOT, source_manifest


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--compiler', type=Path, required=True)
    parser.add_argument('--runtime', type=Path, default=os.environ.get('HIRSEL_CORE_RT_ARCHIVE'))
    parser.add_argument('--node', type=Path, required=True)
    args = parser.parse_args()
    compiler = args.compiler.resolve()
    tests = ROOT / 'core/programs/runner/tests'
    with tempfile.TemporaryDirectory(prefix='hirsel-sdk-recovery-') as tmp:
        work = Path(tmp)
        manifest = work / 'inputs.json'
        manifest.write_text(json.dumps(source_manifest(ROOT, 'runner', ['runner'], tests / 'client_recovery_place.hirsel')))
        executable = work / 'place'
        command = [compiler, 'build', '--source-core', '--source-std', '--inputs', manifest,
                   '--entry', 'runner@1/runner/recovery_main', '--runtime', args.runtime.resolve(),
                   '--store', work / 'compiler', '--limit', '268435456', '--dir', work, '-o', executable]
        for capability in ('Clock', 'Random', 'Tasks', 'Stdin', 'Stdout',
                           'Net=Listen 127.0.0.0/8 0-65535'):
            command.extend(['--grant', capability])
        subprocess.run(command, cwd=work, check=True)
        sdk = work / 'sdk'
        subprocess.run([compiler, 'sdk', 'ts', executable, '--n-place', '1048576', '--output', sdk],
                       cwd=work, check=True)
        partner = tests / 'client.mjs'
        program = f'import {{recoveryLaws}} from {json.dumps(partner.as_uri())}; await recoveryLaws(...process.argv.slice(1));'
        subprocess.run([args.node.resolve(), '--input-type=module', '--eval', program, compiler, executable, sdk],
                       cwd=work, check=True)


if __name__ == '__main__':
    main()
