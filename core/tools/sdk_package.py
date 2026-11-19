"""Build the client tarball and consume it in an isolated TypeScript project."""
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile

from std_native import source_manifest


SELECTOR = 'sdk.client:packed_consumer'


def check(root, compiler, runtime, node_dist, tool_tarballs):
    sdk = root / 'sdk/client'
    fixture = sdk / 'tests/packed_consumer.hirsel'
    node = node_dist / 'bin/node'
    env = os.environ.copy()
    env.pop('NODE_PATH', None)
    env.pop('NODE_OPTIONS', None)
    env['PATH'] = str(node.parent) + os.pathsep + env.get('PATH', os.defpath)

    with tempfile.TemporaryDirectory(prefix='hirsel-sdk-package-') as directory:
        work = Path(directory)
        npm_home = work / 'npm-home'
        npm_home.mkdir()
        npm_env = dict(env, HOME=str(npm_home))
        npm = [node, node_dist / 'lib/node_modules/npm/bin/npm-cli.js', '--offline',
               '--no-update-notifier', '--cache', work / 'npm-cache']
        publisher = work / 'publisher'
        consumer = work / 'consumer'
        shutil.copytree(sdk, publisher, ignore=shutil.ignore_patterns('dist', 'node_modules'))
        consumer.mkdir()
        (consumer / 'package.json').write_text('{"private":true,"type":"module"}\n')

        # The Buck toolchain supplies pre-fetched archives. Both npm installs
        # use only local tarballs, including the compiler and ambient types.
        install = [*npm, 'install', '--ignore-scripts', '--no-audit', '--no-fund', '--no-package-lock', '--no-save']
        subprocess.run([*install, *tool_tarballs], cwd=publisher, env=npm_env, check=True)
        subprocess.run([*npm, 'run', 'build'], cwd=publisher, env=npm_env, check=True)
        packed = subprocess.check_output([*npm, 'pack', '--ignore-scripts', '--json', '--pack-destination', work],
                                         cwd=publisher, env=npm_env, text=True)
        package = work / json.loads(packed)[0]['filename']
        subprocess.run([*install, package, *tool_tarballs], cwd=consumer, env=npm_env, check=True)

        installed = consumer / 'node_modules/@hirsel/client'
        exports = json.loads((installed / 'package.json').read_text())['exports']
        # TypeScript can fall back to a declaration beside the JavaScript when
        # the explicit `types` target is missing. Check that advertised path too.
        for subpath in ('.', './node'):
            declaration = exports[subpath]['types']
            if not declaration.startswith('./') or not (installed / declaration).is_file():
                raise ValueError(f'@hirsel/client export {subpath!r} has a missing types target: {declaration}')

        inputs = work / 'inputs.json'
        inputs.write_text(json.dumps(source_manifest(root, 'store', ['packed_consumer'], fixture)))
        executable = work / 'fixture'
        subprocess.run([compiler, 'build', '--source-core', '--inputs', inputs, '--runtime', runtime,
                        '--entry', 'store@1/packed_consumer/main', '--grant', 'Stdout',
                        '--store', work / 'store', '-o', executable], cwd=work, check=True)
        subprocess.run([compiler, 'sdk', 'ts', executable, '--out', consumer / 'generated',
                        '--runtime', runtime], cwd=work, check=True)
        (consumer / 'payload.bin').write_bytes(subprocess.check_output([executable], cwd=work))
        shutil.copyfile(fixture.with_suffix('.ts'), consumer / 'consumer.ts')

        isolated_node = [node, '--permission', f'--allow-fs-read={consumer}']
        subprocess.run([*isolated_node, consumer / 'node_modules/typescript/bin/tsc', '--noEmit',
                        '--strict', '--module', 'NodeNext', '--target', 'ES2022', 'consumer.ts'],
                       cwd=consumer, env=env, check=True)
        # NodeStorage spawns flock(1) to hold a lease on its private state directory.
        subprocess.run([*isolated_node, f'--allow-fs-write={consumer}', '--allow-child-process', 'consumer.ts'],
                       cwd=consumer, env=env, check=True)
