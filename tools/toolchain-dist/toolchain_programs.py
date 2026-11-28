"""Pack toolchain packages and build the installed host helpers."""

from pathlib import Path
import platform
import shutil
import subprocess
import sys
import tempfile

import hcfg


def place(compiler, source, destination, libraries=None):
    package = hcfg.read(source / 'hirsel.hcfg', 'pkg_v1.Package', compiler)
    shutil.copytree(source, destination, dirs_exist_ok=True)
    manifest = destination / 'hirsel.hcfg'
    stage = [compiler, '--stage-toolchain-package', destination]
    if libraries is not None:
        stage.append(libraries)
    edited = subprocess.check_output(stage)
    # A Buck action's staged source files can have read-only permissions.
    with tempfile.NamedTemporaryFile(dir=destination, delete=False) as output:
        output.write(edited)
    Path(output.name).replace(manifest)
    return package['name'], package['version'].split('.')[0]


def helper_args(grants, values):
    args = []
    network = []
    for grant in grants:
        kind, fields = grant['case'], grant['fields']
        if kind == 'Net':
            rule = fields[0]
            case, body = rule['case'], rule['fields']
            if case.startswith('Unix'):
                network.append(f"{case} {body['label']} {body['path'].format_map(values)}")
            elif case in ('Resolve', 'Tls'):
                network.append(f'{case} {body[0]}')
            else:
                public = ' public' if case == 'Connect' and hcfg.option(body['public_only']) else ''
                network.append(f"{case} {body['cidr']} {body['first_port']}-{body['last_port']}{public}")
        elif kind == 'Env':
            args.extend(['--grant', 'Env=' + ','.join(fields[0])])
        elif kind == 'Secret':
            binding = fields['source']
            source = {'Environment': 'env', 'File': 'file'}[binding['case']]
            args.extend(['--grant', f"Secret:{fields['parameter']}={source}:{binding['fields'][0]} {fields['policy']}"])
        elif kind == 'Dir':
            args.extend(['--dir', fields[0].format_map(values)])
        elif not fields:
            args.extend(['--grant', kind])
        else:
            raise ValueError(f'unsupported installed helper grant {kind}')
    if network:
        args.extend(['--grant', 'Net=' + ';'.join(network)])
    return args


def pack(compiler, sources, output, work, names):
    output.mkdir(parents=True, exist_ok=True)
    work.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(dir=work, prefix='prefix-') as directory:
        prefix = Path(directory)
        subprocess.run([compiler, 'std-product', '--install', prefix],
                       stdout=subprocess.DEVNULL, check=True)
        libraries = prefix / 'libraries'
        (libraries / 'programs').mkdir(parents=True)
        for library in ('core', 'std'):
            (libraries / library).symlink_to((sources.parent / library).resolve(), target_is_directory=True)
        for name in names:
            staged = work / name
            (libraries / 'programs' / name).symlink_to(staged.resolve(), target_is_directory=True)
            bound, major = place(prefix / 'hirsel', sources / name, staged, libraries)
            archive = output / f"{bound}-{major}.hpkg"
            subprocess.run([prefix / 'hirsel', 'pack', staged, '--libraries', libraries, '--package-store', work / 'packages',
                            '--output', archive], stdout=subprocess.DEVNULL, check=True)
            shutil.copyfile(archive, prefix / 'core-packages' / archive.name)


def helpers(product, root, source, work, host_image):
    cwd, fd = ('.', '/dev/fd') if platform.system() == 'Darwin' else ('/proc/self/cwd', '/proc/self/fd')
    product = hcfg.product(product, root / 'hirsel')
    values = dict(product['sockets'], cwd=cwd, fd=fd)
    staged_packages = {}
    directory = root / 'libexec/hirsel'
    directory.mkdir(parents=True, exist_ok=True)
    for name, helper in product['helpers'].items():
        package_name = helper.get('package', source.name)
        if package_name not in staged_packages:
            staged = work / package_name
            bound, major = place(root / 'hirsel', source.parent / package_name, staged)
            staged_packages[package_name] = staged, f'{bound}@{major}'
        staged, identity = staged_packages[package_name]
        grants = helper_args(helper['grants'], values)
        image = host_image if helper.get('embedded') else directory / name
        subprocess.run([root / 'hirsel', 'build', staged, '--entry', f"{identity}/{helper['entry']}",
                        *grants, '--limit', '1073741824', '-o', image],
                       stdout=subprocess.DEVNULL, check=True)


if __name__ == '__main__':
    command, *args = sys.argv[1:]
    if command == 'pack':
        pack(*map(Path, args[:4]), args[4:])
    elif command == 'helpers':
        product, root, source, work, host_image = map(Path, args)
        helpers(product, root, source, work, host_image)
    else:
        sys.exit(f'toolchain_programs.py: unknown command {command}')
