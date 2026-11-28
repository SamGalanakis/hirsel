"""Check the packed client and the standalone generated SDK in isolated projects."""
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile


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
        subprocess.run([*npm, 'test'], cwd=publisher, env=npm_env, check=True)
        packed = subprocess.check_output([*npm, 'pack', '--ignore-scripts', '--json', '--pack-destination', work],
                                         cwd=publisher, env=npm_env, text=True)
        package = work / json.loads(packed)[0]['filename']
        subprocess.run([*install, package, *tool_tarballs], cwd=consumer, env=npm_env, check=True)

        installed = consumer / 'node_modules/@hirsel/client'
        exports = json.loads((installed / 'package.json').read_text())['exports']
        # TypeScript can fall back to a declaration beside the JavaScript when
        # the explicit `types` target is missing. Check that advertised path too.
        for subpath in ('.', './memory', './node'):
            declaration = exports[subpath]['types']
            if not declaration.startswith('./') or not (installed / declaration).is_file():
                raise ValueError(f'@hirsel/client export {subpath!r} has a missing types target: {declaration}')

        # A source package under this library root has the same launch metadata
        # access as the installed runner when the CLI asks the Place to describe itself.
        libraries = work / 'lib'
        programs = libraries / 'programs'
        app = programs / 'client'
        app.mkdir(parents=True)
        for name in ('core', 'std'):
            (libraries / name).symlink_to(root / 'core' / name, target_is_directory=True)
        for name in ('store', 'runner'):
            source = root / 'core/programs' / name
            for directory, subdirs, files in os.walk(source):
                subdirs[:] = [d for d in subdirs if not d.startswith('.')]
                for filename in files:
                    if filename.endswith(('.hirsel', '.hcfg')):
                        dest = programs / name / Path(directory).relative_to(source) / filename
                        dest.parent.mkdir(parents=True, exist_ok=True)
                        shutil.copyfile(Path(directory) / filename, dest)
        (programs / 'hirsel.hcfg').write_text('pkg_v1.Workspace(members: ["store", "runner", "client"])\n')
        shutil.copyfile(fixture, app / 'main.hirsel')
        shutil.copyfile(fixture.with_suffix('.hcfg'), app / 'hirsel.hcfg')
        executable = work / 'fixture'
        subprocess.run([compiler, 'build', app, '--libraries', libraries, '--source-std', '--runtime', runtime,
                        '--entry', 'local/sdk_test@1/packed_consumer/main', '--grant', 'Stdout',
                        '--grant', 'Clock', '--grant', 'Random', '--grant', 'Tasks',
                        '--store', work / 'store', '-o', executable], cwd=work, check=True)
        subprocess.run([compiler, 'sdk', 'ts', executable, '--output', consumer / 'generated'],
                       cwd=work, check=True)
        (consumer / 'payload.bin').write_bytes(subprocess.check_output([executable], cwd=work))
        shutil.copyfile(fixture.with_suffix('.ts'), consumer / 'consumer.ts')
        example = 'subscription-cache.ts'
        shutil.copyfile(fixture.with_name(example), consumer / example)

        isolated_node = [node, '--permission', f'--allow-fs-read={consumer}']
        subprocess.run([*isolated_node, consumer / 'node_modules/typescript/bin/tsc', '--noEmit',
                        '--strict', '--module', 'NodeNext', '--target', 'ES2022',
                        '--allowImportingTsExtensions', 'consumer.ts'],
                       cwd=consumer, env=env, check=True)
        subprocess.run([*isolated_node, f'--allow-fs-write={consumer}', 'consumer.ts'],
                       cwd=consumer, env=env, check=True)

        standalone = work / 'standalone'
        standalone.mkdir()
        shutil.copyfile(consumer / 'package.json', standalone / 'package.json')
        shutil.copyfile(consumer / example, standalone / example)
        subprocess.run([compiler, 'sdk', 'ts', executable, '--output', standalone / 'generated'],
                       cwd=work, check=True)
        def contents(directory):
            return {p.name: p.read_bytes() for p in directory.iterdir()}
        if contents(consumer / 'generated') != contents(standalone / 'generated'):
            raise ValueError('generated SDK differs between consumers')
        subprocess.run([*install, *tool_tarballs], cwd=standalone, env=npm_env, check=True)
        standalone_source = (consumer / 'consumer.ts').read_text()
        standalone_source = standalone_source.replace("'@hirsel/client'", "'./generated/client.js'")
        standalone_source = standalone_source.replace("'@hirsel/client/memory'", "'./generated/memory.js'")
        standalone_source = standalone_source.replace("'@hirsel/client/node'", "'./generated/node.js'")
        (standalone / 'consumer.ts').write_text(standalone_source)
        compiled = standalone / 'compiled'
        standalone_node = [node, '--permission', f'--allow-fs-read={standalone}',
                           f'--allow-fs-write={standalone}']
        subprocess.run([*standalone_node, standalone / 'node_modules/typescript/bin/tsc',
                        '--strict', '--module', 'NodeNext', '--target', 'ES2022',
                        '--rewriteRelativeImportExtensions', '--outDir', compiled,
                        'consumer.ts', 'generated/place.ts'],
                       cwd=standalone, env=env, check=True)
        for module in (standalone / 'generated').glob('*.js'):
            shutil.copyfile(module, compiled / 'generated' / module.name)
        shutil.copyfile(fixture.with_name('open-place.js'), compiled / 'open-place.js')
        subprocess.run([*standalone_node, 'open-place.js'], cwd=compiled, env=env, check=True)
        shutil.copyfile(consumer / 'payload.bin', compiled / 'payload.bin')
        subprocess.run([*standalone_node, 'consumer.js'],
                       cwd=compiled, env=env, check=True)
