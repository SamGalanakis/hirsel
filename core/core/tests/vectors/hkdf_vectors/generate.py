"""Pack Wycheproof HKDF-SHA256 inputs and independent HMAC oracles."""
import hmac
import json
from pathlib import Path
import struct

# C2SP/Wycheproof commit 65ca494727a52944c204441c860b039e6786b99b.
corpus = json.loads(Path(__file__).with_name('hkdf_sha256_test.json').read_text())
data = bytearray()
for group in corpus['testGroups']:
    for test in group['tests']:
        ikm, salt, info = (bytes.fromhex(test[name]) for name in ('ikm', 'salt', 'info'))
        tag = (
            hmac.digest(bytes.fromhex(test['okm']), b'x', 'sha256')
            if test['result'] == 'valid'
            else b''
        )
        data.extend(struct.pack('<6H', test['tcId'], test['size'], len(ikm),
                                len(salt), len(info), len(tag)))
        data.extend(ikm + salt + info + tag)
literal = ''.join(f'\\x{byte:02x}' for byte in data)
print(f'fn wycheproof_data() -> Bytes {{ b"{literal}" }}')
