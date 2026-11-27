"""L31 no_secret_at_rest and L68 jwt_no_token_at_rest (RUNNER-SPEC §8, §15.10): byte searches
of a disk store after credential-bound calls.

L68: 
Each run mints a fresh P-256 key and an ES256 token with openssl, with hand-written claims and a
random marker claim. After the harness's bound calls and checkpoint, no file in the store
directory, and nothing the harness printed, holds the token's payload or signature segment, the
marker, or the key's x or y (base64url or raw).
"""
import base64
import json
import os
import subprocess
import time

from case_progress import run_case
from store_crash_tests import checked, run_crashes


def b64(data):
    return base64.urlsafe_b64encode(data).rstrip(b'=').decode('ascii')


def der_ints(der):
    """The two INTEGERs of a DER ECDSA-Sig-Value (RFC 3279), as 32-byte big-endian values."""
    assert der[0] == 0x30
    at = 3 if der[1] & 0x80 else 2
    out = []
    for _ in range(2):
        assert der[at] == 0x02
        size = der[at + 1]
        out.append(int.from_bytes(der[at + 2:at + 2 + size], 'big').to_bytes(32, 'big'))
        at += 2 + size
    return out


def mint(work):
    """A JWKS document, the token, and the byte strings L68 searches for."""
    key = work / 'l68-key.pem'
    subprocess.run(['openssl', 'genpkey', '-algorithm', 'EC', '-pkeyopt', 'ec_paramgen_curve:P-256', '-out', str(key)],
                   check=True, capture_output=True)
    spki = subprocess.run(['openssl', 'pkey', '-in', str(key), '-pubout', '-outform', 'DER'],
                          check=True, capture_output=True).stdout
    point = spki[-65:]
    assert point[0] == 0x04
    x, y = point[1:33], point[33:]
    jwks = json.dumps({'keys': [{'kty': 'EC', 'crv': 'P-256', 'kid': 'l68', 'use': 'sig', 'alg': 'ES256',
                                 'x': b64(x), 'y': b64(y)}]}, separators=(',', ':'))
    marker = os.urandom(16).hex()
    claims = {'iss': 'https://issuer.example', 'aud': 'place-a', 'sub': 'user-42',
              'exp': int(time.time()) + 600, 'l68': marker}
    header = b64(json.dumps({'alg': 'ES256', 'kid': 'l68'}, separators=(',', ':')).encode())
    payload = b64(json.dumps(claims, separators=(',', ':')).encode())
    der = subprocess.run(['openssl', 'pkeyutl', '-sign', '-inkey', str(key), '-digest', 'sha256', '-rawin'],
                         input=f'{header}.{payload}'.encode(), check=True, capture_output=True).stdout
    r, s = der_ints(der)
    signature = b64(r + s)
    key.unlink()
    needles = {'payload': payload.encode(), 'signature': signature.encode(), 'marker': marker.encode(),
               'x': b64(x).encode(), 'y': b64(y).encode(), 'x_raw': x, 'y_raw': y}
    return jwks, f'{header}.{payload}.{signature}', needles


def no_jwt_at_rest(args, work, engine, selection, row):
    name = 'no_jwt_at_rest'
    directory = work / f'{name}-{engine}-disk'
    directory.mkdir()
    binary = work / f'{name}-{engine}'
    command = [str(args.compiler.resolve()), 'build', '--inputs', str(selection), '--source-core',
               '--source-std', '--entry', 'hirsel.ai/store_crash@1/runner_crash/no_jwt_at_rest', '--backend', engine,
               '--runtime', str(args.runtime.resolve()), '--store', str(work / f'l68-objects-{engine}'),
               '--limit', str(64 * 1024 * 1024), '--dir', str(directory), '-o', str(binary),
               '--grant', 'Clock', '--grant', 'Tasks', '--grant', 'Random',
               '--grant', 'Net=Listen 127.0.0.0/8 0-65535;Connect 127.0.0.0/8 0-65535']
    if args.cc:
        command += ['--cc', str(args.cc.resolve())]
    checked(command, os.environ, engine=engine, case=f'{name}:build')
    jwks, token, needles = mint(work)
    ran = run_case(name, engine, [str(binary), jwks, token], capture_output=True)
    files = [p for p in directory.rglob('*') if p.is_file()]
    found = sorted(f'{p.relative_to(directory)}:{what}' for p in files for what, needle in needles.items()
                   if needle in p.read_bytes())
    logged = sorted(what for what, needle in needles.items() if needle in ran.stdout or needle in ran.stderr)
    passed = ran.returncode == 0 and bool(files) and not found and not logged
    row(engine=engine, law=name, passed=passed,
        result={'exit': ran.returncode, 'files': len(files), 'matches': found, 'logged': logged,
                'stderr': ran.stderr.decode('utf-8', 'replace')[-2000:]})


def no_secret_at_rest(args, work, engine, selection, row):
    name = 'no_secret_at_rest'
    directory = work / f'{name}-{engine}-disk'
    directory.mkdir()
    binary = work / f'{name}-{engine}'
    command = [str(args.compiler.resolve()), 'build', '--inputs', str(selection), '--source-core',
               '--source-std', '--entry', 'hirsel.ai/store_crash@1/runner_crash/no_secret_at_rest', '--backend', engine,
               '--runtime', str(args.runtime.resolve()), '--store', str(work / f'l31-objects-{engine}'),
               '--limit', str(64 * 1024 * 1024), '--dir', str(directory), '-o', str(binary),
               '--grant', 'Clock', '--grant', 'Tasks', '--grant', 'Random',
               '--grant', 'Net=Listen 127.0.0.0/8 0-65535;Connect 127.0.0.0/8 0-65535']
    if args.cc:
        command += ['--cc', str(args.cc.resolve())]
    checked(command, os.environ, engine=engine, case=f'{name}:build')
    nonce = os.urandom(16)
    encoded = base64.b64encode(nonce).decode('ascii')
    ran = run_case(name, engine, [str(binary), encoded], capture_output=True, text=True)
    files = [p for p in directory.rglob('*') if p.is_file()]
    found = sorted(str(p.relative_to(directory)) for p in files if nonce in p.read_bytes())
    passed = ran.returncode == 0 and bool(files) and not found
    row(engine=engine, law='no_secret_at_rest', passed=passed,
        result={'exit': ran.returncode, 'files': len(files), 'matches': found, 'stderr': ran.stderr})


def crash_part(args, root, work, engine, name, inputs_path, stop_cases, part):
    build = work / name
    build.mkdir()
    cases = {
        'deadline': ('hirsel.ai/store_crash@1/runner_crash/stop_main', (), (args.stop_case,) if args.stop_case else stop_cases),
        'activation': ('hirsel.ai/store_crash@1/runner_crash/main', ('runner-activation', 'runner-activation-genesis'),
                       ('runner-unavailable-genesis', 'runner-unavailable-install')),
        'l61-crash': ('runner@1/runner/staged_crash_main', (), ('l61-before-release', 'l61-before-flush')),
    }
    entry, scenarios, count_only = cases[part]
    return run_crashes(root, args.compiler, args.runtime, args.bridge, args.cc, inputs_path, build, engine,
                       entry=entry, adapter='runner_crash.c', scenarios=scenarios, count_only=count_only)
