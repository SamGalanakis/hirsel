"""Shared request-cost-design-948 manifests and native build command (§1).

The suite edit inserts `let _ = 0u8` in dispatch on an unedited store; the
signature edit makes `helper.named` public.
This module is used by both the design experiment and the front cost check.
"""
import copy
import json
from pathlib import Path
import sys

from measure import parse_json, read_json


def manifests(root, out):
    sys.path.insert(0, str(root / 'core/tools'))
    sys.path.insert(0, str(root / 'core/langarena'))
    from langarena import inputs
    import run as larun
    out.mkdir(parents=True, exist_ok=True)
    spec = inputs(root, 'suite')
    for source in spec['packages'][1]['sources']:
        source['signatures'] = True
    text = larun.suite_source()
    suite = out / 'suite.hirsel'
    suite.write_text(text)
    app = spec['packages'][-1]
    app['sources'] = [s for s in app['sources'] if len(s['modules']) == 1]
    app['sources'].append({'file': str(suite), 'modules': ['suite'], 'signatures': False})
    (out / 'suite-inputs.json').write_text(json.dumps(spec))
    edited = out / 'suite-edited.hirsel'
    marker = text.index('{', text.index('pub fn dispatch'))
    edited.write_text(text[:marker + 1] + '\nlet _ = 0u8\n' + text[marker + 1:])
    changed = copy.deepcopy(spec)
    next(s for s in changed['packages'][-1]['sources'] if s['modules'] == ['suite'])['file'] = str(edited)
    (out / 'suite-edited-inputs.json').write_text(json.dumps(changed))
    # The signature edit: one item's header changes (helper.named becomes
    # public) and nothing else, so every body key is derived again (B2's shape).
    helper = next(s for s in spec['packages'][-1]['sources'] if s['modules'] == ['helper'])
    header = Path(helper['file']).read_text()
    assert header.count('\nfn named[') == 1
    signature = out / 'helper-signature.hirsel'
    signature.write_text(header.replace('\nfn named[', '\npub fn named['))
    published = copy.deepcopy(spec)
    next(s for s in published['packages'][-1]['sources'] if s['modules'] == ['helper'])['file'] = str(signature)
    (out / 'suite-signature-inputs.json').write_text(json.dumps(published))


def command(compiler, runtime, out, manifest, entry, store, tag, cc, profile_front=False):
    grants = ['--grant', 'Stdout']
    if entry == 'langarena@1/suite/main':
        host = out / 'host'
        host.mkdir(exist_ok=True)
        grants.extend(['--dir', str(host), '--grant', 'Tasks'])
    return [str(compiler), 'build', *(['--profile-front'] if profile_front else []), '--inputs', str(manifest), '--entry', entry,
            '--store', str(store), '--json', '--backend', 'cranelift',
            '--runtime', str(runtime), '--cc', str(cc), *grants, '-o', str(out / (tag + '.bin')),
            '--metrics', str(out / (tag + '.metrics.json'))]


def toolchain(root, buck_inputs=None):
    """Select current Buck outputs, or explicit inputs supplied by a Buck test.

    Standalone callers always ask Buck, regardless of inherited LLVM variables.
    The link editor is the one clang actually selects, checked against the lock.
    """
    import hashlib
    import os
    import subprocess
    import time
    if buck_inputs is None:
        while os.getloadavg()[0] > 32:
            time.sleep(10)
        command = [
            str(root / '.buck2/bin/buck2'), 'build', '--show-json-output',
            '//tools/buck2/native-toolchain:llvm', '//tools/buck2/native-toolchain:kit']
        outputs = parse_json(subprocess.check_output(command, cwd=root), f'stdout from {command!r}')
        llvm = (root / outputs['root//tools/buck2/native-toolchain:llvm']).resolve()
        kit = (root / outputs['root//tools/buck2/native-toolchain:kit']).resolve()
    else:
        # These paths are target outputs injected by hirsel_script_test, not a
        # guessed configuration directory. No nested Buck invocation in a test.
        llvm, kit = (Path(buck_inputs[k]).resolve() for k in ('llvm', 'kit'))
    env = os.environ | {'LD_LIBRARY_PATH': str(llvm / 'lib')}
    clang = llvm / 'bin/clang'
    selected = subprocess.check_output([str(clang), '-print-prog-name=ld.lld'], env=env, text=True).strip()
    lld = Path(selected)
    if not lld.is_absolute():
        raise ValueError('pinned clang did not select an absolute ld.lld: ' + selected)
    digest = hashlib.sha256(lld.read_bytes()).hexdigest()
    pin = next(line.split()[1] for line in (root / 'tools/native-toolchain/llvm-23.lock').read_text().splitlines()
               if line.startswith('static_lld '))
    if digest != pin:
        raise ValueError(f'selected ld.lld {lld} has sha256 {digest}; lock pins {pin}')
    return dict(llvm=str(llvm), kit=str(kit), clang=str(clang), linker=str(lld.resolve()), linker_sha256=digest)


def stamp_metrics(path, tools):
    row = read_json(path)
    row['linker_sha256'] = tools['linker_sha256']
    row['linker'] = tools['linker']
    path.write_text(json.dumps(row, indent=2) + '\n')
    return row
