"""Shared request-cost-design-948 manifests and native build command (§1).

The suite edit inserts `let _ = 0u8` in dispatch on an unedited store.
This module is used by both the design experiment and the front land gate.
"""
import copy
import json
from pathlib import Path
import sys


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


def command(compiler, runtime, out, manifest, entry, store, tag):
    host = out / 'host'
    host.mkdir(exist_ok=True)
    return [str(compiler), 'build', '--inputs', str(manifest), '--entry', entry,
            '--store', str(store), '--json', '--backend', 'cranelift',
            '--runtime', str(runtime), '--dir', str(host), '--grant', 'Stdout',
            '--grant', 'Tasks', '-o', str(out / (tag + '.bin')),
            '--metrics', str(out / (tag + '.metrics.json'))]
