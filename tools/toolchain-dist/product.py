#!/usr/bin/env python3
"""Render release workflow and standalone installer inputs from product.toml."""

import argparse
import json
from pathlib import Path
import platform
import shlex
import tomllib

PRODUCT = Path(__file__).with_suffix('.toml')
APPLE_TARGETS = PRODUCT.parents[1] / 'native-toolchain/apple-targets.json'
ASSET = 'hirsel-{tag}-{target}.tar.gz'


def load():
    return tomllib.loads(PRODUCT.read_text())


def llvm_engine(host):
    library = Path(host['llvm_library'])
    return Path(host.get('llvm_engine_dir', library.parent)) / library.name


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
    hosts = load()['hosts']
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


if __name__ == '__main__':
    main()
