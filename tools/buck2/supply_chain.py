#!/usr/bin/env python3
"""Check the locked Rust workspace with a pinned cargo-deny."""

import hashlib
import json
from pathlib import Path
import shutil
import subprocess
import tarfile
import tempfile
import urllib.request

import bootstrap_store
import rust_environment

ROOT = Path(__file__).resolve().parents[2]


def digest(path):
    with path.open('rb') as source:
        return hashlib.file_digest(source, 'sha256').hexdigest()


def install(root, pin):
    def fetch(tree):
        tree.mkdir()
        archive = tree.parent / 'cargo-deny.tar.gz'
        with urllib.request.urlopen(pin['url'], timeout=60) as source, archive.open('wb') as output:
            shutil.copyfileobj(source, output)
        if digest(archive) != pin['archive_sha256']:
            raise SystemExit('cargo-deny release archive checksum mismatch')
        with tarfile.open(archive, 'r:gz') as bundle:
            member = bundle.getmember(pin['member'])
            if not member.isfile():
                raise SystemExit('cargo-deny release executable is not a regular file')
            with bundle.extractfile(member) as source, (tree / 'cargo-deny').open('wb') as output:
                shutil.copyfileobj(source, output)
        (tree / 'cargo-deny').chmod(0o755)

    state = root / '.buck2'
    state.mkdir(exist_ok=True)
    executable = state / f"cargo-deny-{pin['executable_sha256']}"
    if executable.is_file() and digest(executable) == pin['executable_sha256']:
        return executable
    with tempfile.TemporaryDirectory(prefix='cargo-deny-', dir=state) as scratch:
        tree = Path(scratch) / 'tool'
        if not bootstrap_store.materialize(root, 'cargo-deny', pin, fetch, tree):
            fetch(tree)
        binary = tree / 'cargo-deny'
        if digest(binary) != pin['executable_sha256']:
            raise SystemExit('cargo-deny executable checksum mismatch')
        binary.replace(executable)
    return executable


def check(root, pin, checks, env, *, offline=False):
    command = [str(install(root, pin)), '--locked', '--config', str(root / 'deny.toml')]
    if offline:
        command.append('--offline')
    return subprocess.run([*command, 'check', '--show-stats', *checks], cwd=root, env=env).returncode


if __name__ == '__main__':
    # Advisory DB downloads and Cargo's cache are private to the scheduled run.
    with tempfile.TemporaryDirectory(prefix='hirsel-advisories-') as cargo_home:
        env = rust_environment.environment(ROOT) | {'CARGO_HOME': cargo_home}
        pin = json.loads(Path(__file__).with_name('pins.json').read_text())['cargo-deny']
        raise SystemExit(check(ROOT, pin, ('advisories',), env))
