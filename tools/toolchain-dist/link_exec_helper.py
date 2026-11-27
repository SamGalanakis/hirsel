"""Link the Exec helper's Rust main against the bundle's pinned desktop kit."""

import json
from pathlib import Path
import subprocess
import sys


def main(kit, target, archive, output):
    row = json.loads((kit / 'manifest.json').read_text())['targets'][target]
    directory = kit / target
    libraries = [directory / name for name in row['libraries']]
    if target.endswith('-apple-darwin'):
        floor = row['floor']
        args = ['-flavor', 'darwin', '-arch', 'arm64',
                '-platform_version', row['platform_version_name'], floor, floor,
                '-adhoc_codesign', '-dead_strip', '-dead_strip_dylibs', '-S', archive, *libraries]
    else:
        args = ['-pie', '--eh-frame-hdr', '--hash-style=gnu', '-z', 'noexecstack',
                '--gc-sections', '--strip-debug', '-dynamic-linker', row['interpreter'],
                *row['options'], *(directory / name for name in row['start']), archive,
                directory / row['unwind_script'], '--as-needed', *libraries,
                directory / row['nonshared'], *(directory / name for name in row['end'])]
    subprocess.run([kit / 'ld.lld', *args, '-o', output], check=True)


if __name__ == '__main__':
    main(Path(sys.argv[1]), sys.argv[2], Path(sys.argv[3]), Path(sys.argv[4]))
