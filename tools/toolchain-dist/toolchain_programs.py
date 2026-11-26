"""Pack toolchain packages and build the installed host helpers."""

from pathlib import Path
import platform
import re
import shutil
import subprocess
import sys
import tomllib


def place(source, destination):
    manifest = source / 'hirsel.toml'
    text = manifest.read_text()
    package = tomllib.loads(text)['package']
    shutil.copytree(source, destination, dirs_exist_ok=True)
    # The copies keep the inputs' read-only mode under remote execution.
    # Rebind workspace paths to the packages supplied by the toolchain.
    installed = destination / manifest.name
    installed.unlink()
    installed.write_text(re.sub(
        r'^([a-z_]+) = \{ major = ([0-9]+), path = "[^"]*" \}$',
        r'\1 = { major = \2 }', text, flags=re.M))
    return package


def pack(compiler, sources, output, work, names):
    output.mkdir(parents=True, exist_ok=True)
    work.mkdir(parents=True, exist_ok=True)
    for name in names:
        package = place(sources / name, work / name)
        subprocess.run([compiler, 'pack', work / name, '--package-store', work / 'packages',
                        '--output', output / f"{package['name']}-{package['major']}.hpkg"],
                       stdout=subprocess.DEVNULL, check=True)


def helpers(product, root, source, work):
    cwd, fd = ('.', '/dev/fd') if platform.system() == 'Darwin' else ('/proc/self/cwd', '/proc/self/fd')
    values = dict(product['sockets'], cwd=cwd, fd=fd)
    staged = work / source.name
    package = place(source, staged)
    identity = f"{package['name']}@{package['major']}"
    directory = root / 'libexec/hirsel'
    directory.mkdir(parents=True, exist_ok=True)
    for name, helper in product['helpers'].items():
        grants = [word for grant in helper['grants'] for word in ('--grant', grant.format_map(values))]
        dirs = [word for path in helper['dirs'] for word in ('--dir', path.format_map(values))]
        subprocess.run([root / 'hirsel', 'build', staged, '--entry', f"{identity}/{helper['entry']}",
                        *grants, *dirs, '--limit', '1073741824', '-o', directory / name],
                       stdout=subprocess.DEVNULL, check=True)


if __name__ == '__main__':
    command, *args = sys.argv[1:]
    if command == 'pack':
        pack(*map(Path, args[:4]), args[4:])
    elif command == 'helpers':
        product, root, source, work = map(Path, args)
        helpers(tomllib.loads(product.read_text()), root, source, work)
    else:
        sys.exit(f'toolchain_programs.py: unknown command {command}')
