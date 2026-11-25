#!/usr/bin/env python3
"""The Darwin import law (macOS S7): a Darwin runtime archive imports only names
on Hirsel's allowlists, tools/native-toolchain/darwin/<library>.symbols.

Each archive is linked as a program links it: the kit's ld.lld for Mach-O, the
kit's floor, -dead_strip, and the kit's one text stub per library, which holds
exactly its allowlist. The archive's `_hirsel*` definitions are the roots (the
only exports and each forced in with -u), so what the link keeps is what a
compiled program can reach. A kept reference to a name off every allowlist is an
undefined symbol and fails the link; one the program can never reach is
stripped, as it is in a shipped program.

    darwin_import_law.py KIT LLVM ARCHIVE...

KIT is the link kit tree (manifest.json, ld.lld, aarch64-apple-darwin/*.tbd).
The pinned LLVM's llvm-nm reads the roots and the linked imports.
"""

from pathlib import Path
import json
import subprocess
import sys
import tempfile

DARWIN = 'aarch64-apple-darwin'


def nm(llvm, *args):
    run = subprocess.run(
        [str(llvm / 'bin' / 'llvm-nm'), *args], capture_output=True, text=True,
        env={'LD_LIBRARY_PATH': str(llvm / 'lib'), 'PATH': '/usr/bin:/bin'})
    errors = [line for line in run.stderr.splitlines()
              if line.strip() and not line.endswith(': no symbols')]
    if run.returncode or errors:
        raise SystemExit(f'darwin import law: llvm-nm {" ".join(args)}:\n' + '\n'.join(errors))
    return run.stdout


def roots(llvm, archive):
    names = set()
    for line in nm(llvm, '--defined-only', '--extern-only', '--format=just-symbols', str(archive)).splitlines():
        if line.startswith('_hirsel'):
            names.add(line)
    if not names:
        raise SystemExit(f'darwin import law: {archive} defines no _hirsel symbol')
    return sorted(names)


def main():
    if len(sys.argv) < 4:
        raise SystemExit('usage: darwin_import_law.py KIT LLVM ARCHIVE...')
    kit = Path(sys.argv[1]).resolve()
    llvm = Path(sys.argv[2]).resolve()
    target = json.loads((kit / 'manifest.json').read_text())['targets'][DARWIN]
    floor = target['floor']
    stubs = [str(kit / DARWIN / library) for library in target['libraries']]
    failed = False
    for archive in (Path(arg).resolve() for arg in sys.argv[3:]):
        if not archive.is_file() or archive.stat().st_size == 0:
            raise SystemExit(f'darwin import law: no archive at {archive}')
        names = roots(llvm, archive)
        with tempfile.TemporaryDirectory(prefix='hirsel-darwin-imports-') as raw:
            temporary = Path(raw)
            exported = temporary / 'roots'
            exported.write_text(''.join(name + '\n' for name in names))
            image = temporary / 'image.dylib'
            link = subprocess.run(
                [str(kit / 'ld.lld'), '-flavor', 'darwin', '-arch', 'arm64',
                 '-platform_version', 'macos', floor, floor, '-dylib', '-dead_strip',
                 '-dead_strip_dylibs', '-exported_symbols_list', str(exported),
                 *[arg for name in names for arg in ('-u', name)],
                 str(archive), *stubs, '-o', str(image)],
                capture_output=True, text=True, env={'PATH': '/usr/bin:/bin'})
            if link.returncode:
                print(f'darwin import law: {archive} reaches names off the Darwin allowlists '
                      '(tools/native-toolchain/darwin/*.symbols):\n' + link.stderr, file=sys.stderr)
                failed = True
                continue
            imports = nm(llvm, '--undefined-only', '--format=just-symbols', str(image)).split()
        print(f'darwin import law: {archive} links with {len(names)} roots and '
              f'{len(imports)} imports, all allowlisted')
    return 1 if failed else 0


if __name__ == '__main__':
    sys.exit(main())
