"""Render the pinned CAVP byte vectors and Python hashlib digests as Hirsel."""
from pathlib import Path
import re

HERE = Path(__file__).parent


def literal(raw):
    return 'b"' + ''.join(f'\\x{byte:02x}' for byte in raw) + '"'


def emit_cavp(algorithm, part):
    path = HERE / f'SHA{algorithm}{part}Msg.rsp'
    rows = re.findall(
        r'Len = (\d+)\nMsg = ([0-9a-f]+)\nMD = ([0-9a-f]+)',
        path.read_text(encoding='ascii'),
    )
    expected_count = 129 if part == 'Short' else 128
    if len(rows) != expected_count:
        raise ValueError(f'{path}: expected {expected_count} records, got {len(rows)}')
    name = f'cavp_{algorithm}_{part.lower()}_data'
    print(f'fn {name}() -> Inline[(Bytes, Bytes), {len(rows)}] {{')
    print('    [')
    for length, message, digest in rows:
        length = int(length)
        raw = bytes.fromhex(message)[:length // 8]
        md = bytes.fromhex(digest)
        if len(raw) * 8 != length or len(md) != algorithm // 8:
            raise ValueError(f'{path}: malformed Len={length} record')
        print(f'        ({literal(raw)}, {literal(md)}),')
    print('    ]')
    print('}')


for algorithm in (384, 512):
    for part in ('Short', 'Long'):
        emit_cavp(algorithm, part)

rows = [tuple(bytes.fromhex(word) for word in line.split())
        for line in (HERE / 'hashlib.txt').read_text(encoding='ascii').splitlines()]
if len(rows) != 301 or any(tuple(map(len, row)) != (64, 48) for row in rows):
    raise ValueError('hashlib.txt: expected SHA-512 and SHA-384 for each length 0 to 300')
print(f'fn hashlib_data() -> Bytes {{ {literal(b"".join(a + b for a, b in rows))} }}')
