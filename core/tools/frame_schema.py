#!/usr/bin/env python3
"""Regenerate the Runner frame schema used by the SDK and terminal client."""
import argparse
import json
import os
from pathlib import Path
import subprocess
import tempfile

from store_tests import ROOT, inputs


PROBE = """pub fn main(out: Stdout) -> Result[(), Exit] waits {
    let frame: std.codec.Codec[runner.Frame] = std.codec.of()
    let schema = result.unwrap(std.codec.schema(frame))
    match stdout.write_bytes(out, schema) {
        Ok(()) => Ok(()),
        Error(_) => Error(Exit(status: 1u8)),
    }
}
"""


def generate(root, compiler, runtime):
    with tempfile.TemporaryDirectory(prefix='hirsel-frame-schema-') as directory:
        work = Path(directory)
        source = work / 'probe.hirsel'
        source.write_text(PROBE)
        manifest = inputs(root, package='runner')
        manifest['packages'].append({
            'package': {'name': 'frame_schema', 'version': '1'},
            'dependencies': [{'name': name, 'version': '1'} for name in ('core', 'std', 'runner')],
            'sources': [{'file': str(source), 'modules': ['probe'], 'signatures': False}],
        })
        path = work / 'inputs.json'
        path.write_text(json.dumps(manifest))
        executable = work / 'probe'
        command = [compiler, 'build', '--source-core', '--source-std', '--inputs', path,
                   '--entry', 'frame_schema@1/probe/main', '--grant', 'Stdout',
                   '--store', work / 'store', '-o', executable]
        if runtime:
            command.extend(['--runtime', runtime])
        subprocess.run(command, cwd=work, check=True, stdout=subprocess.DEVNULL)
        return subprocess.check_output([executable], cwd=work)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--compiler', type=Path, required=True)
    parser.add_argument('--runtime', type=Path, default=os.environ.get('HIRSEL_CORE_RT_ARCHIVE'))
    parser.add_argument('--check', action='store_true')
    args = parser.parse_args()
    path = ROOT / 'core/programs/runner/frame-schema.bin'
    current = generate(ROOT, args.compiler.resolve(), args.runtime.resolve() if args.runtime else None)
    if args.check:
        if not path.is_file() or path.read_bytes() != current:
            raise SystemExit('frame-schema.bin is stale. Regenerate it with core/tools/frame_schema.py.')
    else:
        path.write_bytes(current)
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
