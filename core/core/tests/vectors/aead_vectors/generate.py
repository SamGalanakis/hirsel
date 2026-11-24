"""Pack the TLS-5 AEAD vectors for the shared core law runner."""
import json
from pathlib import Path
import re
import struct

HERE = Path(__file__).parent
CHACHA, AES128, AES256 = range(3)
SEAL, OPEN, BOTH, TAMPERED, INVALID = range(5)


def cavp():
    # NIST gcmtestvectors.zip, all IVlen=96 and Taglen=128 records.
    # The excerpts retain the original group headers, field values and Count.
    for size, alg in [(128, AES128), (256, AES256)]:
        for name, mode in [('gcmEncryptExtIV', SEAL), ('gcmDecrypt', OPEN)]:
            source = HERE / f'{name}{size}.rsp'
            rows = re.split(r'^Count = \d+\n', source.read_text(), flags=re.M)[1:]
            if len(rows) != 375:
                raise ValueError(f'{source.name}: expected 375 records, got {len(rows)}')
            for row in rows:
                fields = dict(re.findall(r'^(Key|IV|AAD|PT|CT|Tag) = ?([\da-f]*)$', row, re.M))
                outcome = TAMPERED if 'FAIL' in row else mode
                yield alg, outcome, [fields['Key'], fields['IV'], fields['AAD'],
                                     fields.get('PT', ''), fields['CT'] + fields['Tag']]


def wycheproof():
    # C2SP/Wycheproof commit 65ca494727a52944c204441c860b039e6786b99b.
    for name in ['aes_gcm', 'chacha20_poly1305']:
        corpus = json.loads((HERE / f'{name}_test.json').read_text())
        for group in corpus['testGroups']:
            key_size = group['keySize']
            expected_algs = (
                [CHACHA] if name != 'aes_gcm' else
                {128: [AES128], 192: [AES128, AES256], 256: [AES256]}[key_size]
            )
            for test in group['tests']:
                if group['ivSize'] != 96 or key_size == 192:
                    mode = INVALID
                else:
                    mode = {'valid': BOTH, 'invalid': TAMPERED}[test['result']]
                fields = [test[field] for field in ['key', 'iv', 'aad', 'msg']]
                fields.append(test['ct'] + test['tag'])
                for alg in expected_algs:
                    yield alg, mode, fields


def emit(name, vectors):
    data = bytearray()
    for alg, mode, fields in vectors:
        fields = [bytes.fromhex(value) for value in fields]
        data.extend(struct.pack('<BB5H', alg, mode, *map(len, fields)))
        for field in fields:
            data.extend(field)
    literal = ''.join(f'\\x{byte:02x}' for byte in data)
    print(f'fn {name}() -> Bytes {{ b"{literal}" }}')


emit('cavp_data', cavp())
emit('wycheproof_data', wycheproof())
