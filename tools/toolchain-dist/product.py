#!/usr/bin/env python3
"""Render release workflow and standalone installer inputs from product.toml."""

import argparse
import json
from pathlib import Path
import platform
import shlex
import shutil
import subprocess
import tomllib

PRODUCT = Path(__file__).with_suffix('.toml')
APPLE_TARGETS = PRODUCT.parents[1] / 'native-toolchain/apple-targets.json'
ASSET = 'hirsel-{tag}-{target}.tar.gz'


def load():
    return tomllib.loads(PRODUCT.read_text())


def llvm_engine(host):
    library = Path(host['llvm_library'])
    return Path(host.get('llvm_engine_dir', library.parent)) / library.name


def runtime_slots(product, target):
    slots = product['runtime_slots']
    return {slot: slots[slot] for slot in product['runtime_targets'][target].get('slots', slots)
            if 'buck_suffix' in slots[slot]}


def runtime_flavour(config):
    return config['observation'], 'record' in config['features'], 'standalone' not in config['features']


def bundled_runtimes(prefix, target, slots):
    by_flavour = {runtime_flavour(config): slot for slot, config in slots.items()}
    registration = json.loads((prefix / 'hirsel-core-toolchain.json').read_text())
    archives = {}
    for row in registration['runtimes']:
        if row['target'] != target:
            continue
        flavour = row['observation'], row['record'], row['embedded']
        slot = by_flavour.get(flavour)
        if slot is None or slot in archives:
            raise ValueError(f'{prefix}: unexpected or duplicate {target} runtime registration {flavour}')
        path = prefix / row['path']
        if not path.is_file():
            raise ValueError(f'{target} {slot}: runtime archive missing at {path}')
        archives[slot] = path
    if missing := slots.keys() - archives.keys():
        raise ValueError(f'{prefix}: missing {target} runtime slots: {", ".join(sorted(missing))}')
    return archives


def prebuilt_runtimes(directory, slots):
    archives = {slot: directory / f'{slot}.a' for slot in slots}
    for slot, path in archives.items():
        if not path.is_file():
            raise ValueError(f'{slot}: prebuilt runtime archive missing at {path}')
    return archives


def host_target(hosts):
    name = f'{platform.system()}-{platform.machine()}'
    for target, host in hosts.items():
        if host['platform'] == name:
            return target
    raise SystemExit(f'no published Hirsel host for {name}')


def installer(hosts):
    template = PRODUCT.with_name('install.sh.in').read_text()
    macos = json.loads(APPLE_TARGETS.read_text())['targets']['macos']
    cases = []
    for target, host in hosts.items():
        cases.append(f"    {shlex.quote(host['platform'])})\n"
                     f"        target={shlex.quote(target)}\n"
                     f"        llvm_library={shlex.quote(host['llvm_library'])}\n"
                     '        ;;')
    return (template.replace('@HOSTS@', '\n'.join(cases))
            .replace('@MACOS_FLOOR@', shlex.quote(macos['floor']))
            .replace('@ARCHIVE@', ASSET.format(tag='v$version', target='$target')))


def main():
    product = load()
    hosts = product['hosts']
    parser = argparse.ArgumentParser(description=__doc__)
    subcommands = parser.add_subparsers(dest='command', required=True)
    subcommands.add_parser('matrix')
    subcommands.add_parser('installer')
    subcommands.add_parser('target')
    subcommands.add_parser('assets').add_argument('tag')
    asset = subcommands.add_parser('asset')
    asset.add_argument('tag')
    asset.add_argument('target', nargs='?', choices=hosts)
    for name in ('llvm-path', 'llvm-engine'):
        subcommands.add_parser(name).add_argument('target', nargs='?', choices=hosts)
    for name in ('export-runtimes', 'compare-runtimes'):
        runtimes = subcommands.add_parser(name)
        runtimes.add_argument('target', choices=product['runtime_targets'])
        runtimes.add_argument('prefix', type=Path, help='unpacked toolchain bundle')
        runtimes.add_argument('archives', type=Path, help='directory of prebuilt runtime archives')
    args = parser.parse_args()

    match args.command:
        case 'matrix':
            print(json.dumps({'include': [dict(target=target, runner=host['runner'])
                                          for target, host in hosts.items()]}))
        case 'installer':
            print(installer(hosts), end='')
        case 'target':
            print(host_target(hosts))
        case 'assets':
            for target in hosts:
                print(ASSET.format(tag=args.tag, target=target))
        case 'asset':
            print(ASSET.format(tag=args.tag, target=args.target or host_target(hosts)))
        case 'llvm-path':
            print(hosts[args.target or host_target(hosts)]['llvm_library'])
        case 'llvm-engine':
            print(llvm_engine(hosts[args.target or host_target(hosts)]))
        case 'export-runtimes' | 'compare-runtimes':
            slots = runtime_slots(product, args.target)
            try:
                installed = bundled_runtimes(args.prefix.resolve(), args.target, slots)
                directory = args.archives.resolve()
                if args.command == 'export-runtimes':
                    directory.mkdir(parents=True)
                    for slot, source in installed.items():
                        shutil.copyfile(source, directory / f'{slot}.a')
                else:
                    prebuilt = prebuilt_runtimes(directory, slots)
                    for slot in slots:
                        result = subprocess.run(['cmp', installed[slot], prebuilt[slot]])
                        if result.returncode:
                            raise ValueError(f'{args.target} {slot}: runtime archive bytes differ')
                    print(f'{args.target}: all {len(slots)} runtime archives match byte for byte')
            except (OSError, ValueError) as error:
                raise SystemExit(str(error)) from error


if __name__ == '__main__':
    main()
