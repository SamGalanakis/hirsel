"""Pack toolchain packages and build the installed host helpers."""

from datetime import date, time
import json
from pathlib import Path
import platform
import shutil
import subprocess
import sys
import tempfile
import tomllib


def toml_value(value):
    if isinstance(value, str):
        return json.dumps(value, ensure_ascii=False).replace('\x7f', r'\u007f')
    if isinstance(value, dict):
        fields = (f'{toml_value(key)} = {toml_value(item)}' for key, item in value.items())
        return '{ ' + ', '.join(fields) + ' }'
    if isinstance(value, list):
        return '[' + ', '.join(map(toml_value, value)) + ']'
    if isinstance(value, (date, time)):
        return value.isoformat()
    return str(value).lower()


def place(source, destination):
    manifest = source / 'hirsel.toml'
    document = tomllib.loads(manifest.read_text(encoding='utf-8'))
    package = document['package']
    # Installed bindings name the package by its alias and take only a major.
    dependencies = document.get('dependencies', {})
    for alias, binding in dependencies.items():
        if 'path' in binding and binding.get('package') == alias:
            dependencies[alias] = {'major': binding['major']}
    shutil.copytree(source, destination, dirs_exist_ok=True)
    # The copies keep the inputs' read-only mode under remote execution.
    installed = destination / manifest.name
    installed.unlink()
    installed.write_text(''.join(
        f'{toml_value(key)} = {toml_value(value)}\n' for key, value in document.items()
    ), encoding='utf-8')
    # The identity's major is the version's MAJOR (packages.md section 1).
    return dict(package, major=package['version'].split('.')[0])


def pack(compiler, sources, output, work, names):
    output.mkdir(parents=True, exist_ok=True)
    work.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(dir=work, prefix='prefix-') as directory:
        prefix = Path(directory)
        subprocess.run([compiler, 'std-product', '--install', prefix],
                       stdout=subprocess.DEVNULL, check=True)
        for name in names:
            package = place(sources / name, work / name)
            archive = output / f"{package['name']}-{package['major']}.hpkg"
            subprocess.run([prefix / 'hirsel', 'pack', work / name, '--package-store', work / 'packages',
                            '--output', archive], stdout=subprocess.DEVNULL, check=True)
            shutil.copyfile(archive, prefix / 'core-packages' / archive.name)


def helpers(product, root, source, work, host_image):
    cwd, fd = ('.', '/dev/fd') if platform.system() == 'Darwin' else ('/proc/self/cwd', '/proc/self/fd')
    values = dict(product['sockets'], cwd=cwd, fd=fd)
    staged_packages = {}
    directory = root / 'libexec/hirsel'
    directory.mkdir(parents=True, exist_ok=True)
    for name, helper in product['helpers'].items():
        package_name = helper.get('package', source.name)
        if package_name not in staged_packages:
            staged = work / package_name
            package = place(source.parent / package_name, staged)
            staged_packages[package_name] = staged, f"{package['name']}@{package['major']}"
        staged, identity = staged_packages[package_name]
        grants = [word for grant in helper['grants'] for word in ('--grant', grant.format_map(values))]
        dirs = [word for path in helper['dirs'] for word in ('--dir', path.format_map(values))]
        image = host_image if helper.get('embedded') else directory / name
        subprocess.run([root / 'hirsel', 'build', staged, '--entry', f"{identity}/{helper['entry']}",
                        *grants, *dirs, '--limit', '1073741824', '-o', image],
                       stdout=subprocess.DEVNULL, check=True)


if __name__ == '__main__':
    command, *args = sys.argv[1:]
    if command == 'pack':
        pack(*map(Path, args[:4]), args[4:])
    elif command == 'helpers':
        product, root, source, work, host_image = map(Path, args)
        helpers(tomllib.loads(product.read_text()), root, source, work, host_image)
    else:
        sys.exit(f'toolchain_programs.py: unknown command {command}')
