"""Find host Cargo and Rust without requiring a shell's rustup PATH setup.

Buck2's prepared compiler tree contains rustc but no Cargo. Generation also
needs Cargo, from the pinned rustup toolchain or the user's Cargo home.
"""
import json
import os
from pathlib import Path
import shutil
import subprocess
import tomllib


def cargo_home(root, env):
    return (root / env.get('CARGO_HOME', Path.home() / '.cargo')).resolve()


def cargo_config(directory):
    # Cargo prefers the extensionless file when both forms exist.
    return next((path for name in ('config', 'config.toml')
                 if (path := directory / name).is_file()), None)


def private_cargo_environment(home, env, source, user_config):
    for name in ('registry', 'git'):
        (home / name).symlink_to(source / name, target_is_directory=True)
    if user_config and user_config.suffix == '.toml':
        # An include keeps config-relative paths relative to the original file.
        (home / 'config.toml').write_text(f'include = [{json.dumps(str(user_config), ensure_ascii=False)}]\n')
    elif user_config:
        (home / 'config').symlink_to(user_config)
    # Cargo still takes an exclusive .package-cache lock offline. Keep its
    # lock and GC database private, and disable GC on the shared crate files.
    return env | {'CARGO_HOME': str(home), 'CARGO_NET_OFFLINE': 'true',
                  'CARGO_CACHE_AUTO_CLEAN_FREQUENCY': 'never'}


def run_offline(command, *, cwd, env, label, stdout=subprocess.PIPE):
    result = subprocess.run(command, cwd=cwd, env=env, stdout=stdout,
                            stderr=subprocess.PIPE, text=True)
    if result.returncode:
        error = result.stderr.strip()
        if 'offline' in error:
            error += '\nRun cargo fetch --locked once, then retry sync.'
        raise SystemExit(f'{label} failed\n{error}')
    return result


def environment(root, validate=False):
    env = dict(os.environ)
    home = Path.home()
    rustup_home = Path(env.get('RUSTUP_HOME', home / '.rustup')).resolve()
    directories = [root / 'tools/buck2/toolchains/rust-files/bin']
    pin = root / 'rust-toolchain.toml'
    if pin.is_file():
        channel = tomllib.loads(pin.read_text())['toolchain']['channel']
        directories += [rustup_home / 'toolchains' / channel / 'bin']
        directories += sorted((rustup_home / 'toolchains').glob(f'{channel}-*/bin'))
    directories.append(cargo_home(root, env) / 'bin')
    # Keep other host tools/hooks available, but search known Rust homes first.
    path = os.pathsep.join(map(str, directories)) + os.pathsep + env.get('PATH', os.defpath)
    cargo = shutil.which(env.get('CARGO', 'cargo'), path=path)
    rustc = shutil.which('rustc', path=path)
    if not cargo or not rustc:
        raise ValueError('no Rust toolchain found; install Cargo and rustc in CARGO_HOME or the pinned rustup toolchain')
    env.update(PATH=path, CARGO=os.path.abspath(cargo))
    if validate:
        for executable in (cargo, rustc):
            try:
                probe = subprocess.run([executable, '--version'], cwd=root, env=env,
                                       capture_output=True)
            except (OSError, subprocess.TimeoutExpired) as error:
                raise ValueError(f'Rust toolchain unavailable: {executable} --version failed') from error
            if probe.returncode:
                raise ValueError(f'Rust toolchain unavailable: {executable} --version failed')
    return env
