"""Fixtures for the native runner host laws."""
import ctypes
import ctypes.util
import hashlib
import json
import os
import shutil
import struct

from case_progress import run_case
from store_tests import inputs
from store_crash_tests import checked


def cutover_survives_host_loss(root, work, engine, compiler, state, launch_store, register, deploy,
                               running_host, outcome, ready, welcome_n_place):
    with running_host():
        _, registered = outcome(register, 'T1:register')
        old = ready()
        _, deployed = outcome(deploy, 'T1:publish-shared-release')
        new = ready()
        assert registered['assembly'] != deployed['assembly']
        assert deployed['activation'] == 2, deployed

    executable = launch_store / 'place'
    images = state / 'objects' / 'images'
    shutil.copy2(images / old['digest'], executable)
    hosted = 1048576
    bare = 524288

    def blob(raw):
        return struct.pack('<I', len(raw)) + raw

    def native(command, case):
        result = run_case(case, engine, command, cwd=launch_store, capture_output=True)
        assert result.returncode == 0, result.stderr.decode()
        return result.stdout

    def identity(exe, n_place):
        # Describe is two length-prefixed diagnostics, a u64 allowance, then
        # the revision and export list, both already in value-codec form.
        raw = native([str(exe), f'--hirsel-n-place={n_place}', '--hirsel-describe'], 'T1:describe')
        offset = 0
        for _ in range(2):
            size, = struct.unpack_from('<I', raw, offset)
            offset += 4 + size
        allowance, revision_bytes = struct.unpack_from('<QI', raw, offset)
        assert allowance == n_place and revision_bytes == 32
        return hashlib.sha256(b'\x12hirsel.identity.v1' + raw[offset + 8:offset + 44]
                              + raw[offset:offset + 8] + raw[offset + 44:]).digest()

    hosted_id = identity(executable, hosted)
    bare_id = identity(executable, bare)
    assert hosted_id != bare_id and hosted_id == identity(images / new['digest'], hosted)

    # Activations 3 through 9 carry OLD's binding. Bare installs at 10 and 11
    # leave no prepared witness for the failed deploy in the fixture catalog.
    allowance = f'--hirsel-n-place={hosted}'
    for activation in range(3, 10):
        native(['./place', allowance, '--hirsel-install'], f'T1:install-{activation}')
    for activation in (10, 11):
        native(['./place', '--hirsel-install'], f'T1:bare-{activation}')

    platform_inputs = work / 'recovery-catalog.json'
    platform_inputs.write_text(json.dumps(inputs(root, tests=True, package='platform')))
    catalog = work / 'recovery-catalog'
    command = [compiler, 'build', '--inputs', str(platform_inputs), '--source-core',
               '--source-std', '--backend', engine, '--store', str(work / 'publish-cache'),
               '--entry', 'platform@1/platform/hand_install_catalog', '--limit', str(268435456),
               '--dir', str(state / 'catalog'), '-o', str(catalog)]
    for capability in ('Clock', 'Random', 'Tasks', 'Stdin'):
        command += ['--grant', capability]
    checked(command, engine=engine, case='T1:catalog-build')
    fixture = b''.join(map(blob, [bytes.fromhex(registered['assembly']),
                                 bytes.fromhex(deployed['assembly']), hosted_id, bare_id]))
    for mode in ('seed', 'inspect'):
        result = run_case(f'T1:catalog-{mode}', engine, [str(catalog), mode], input=fixture,
                          capture_output=True)
        assert result.returncode == 0, result.stderr.decode()
        if mode == 'seed':
            native(['./place', allowance, '--hirsel-install'], 'T1:hand-install-12')
            with running_host():
                adopted = ready()
                assert adopted['digest'] == old['digest'], adopted
                assert adopted['n_place'] == hosted and welcome_n_place() == hosted


def unpinned(tape, copy):
    # The tape's header block with a fresh tape id, which no catalog has pinned.
    recorded = tape.read_bytes()
    length = int.from_bytes(recorded[12:16], 'little')
    head, payload = recorded[:16], bytearray(recorded[16:16 + length])
    tape_id = os.urandom(16)
    payload[3:19] = tape_id
    xxh3 = ctypes.CDLL(ctypes.util.find_library('xxhash')).XXH3_64bits
    xxh3.argtypes, xxh3.restype = [ctypes.c_void_p, ctypes.c_size_t], ctypes.c_uint64
    checked_bytes = tape_id + head + bytes(payload)
    check = xxh3(checked_bytes, len(checked_bytes))
    copy.write_bytes(head + bytes(payload) + check.to_bytes(8, 'little'))
    return copy
