"""Read selected byte-oriented SHA CAVP records and emit a Hirsel table."""
import re
from pathlib import Path


def generate(directory, algorithm, short, long):
    rows = []
    for part, requested in (('Short', short), ('Long', long)):
        source = directory / f'{algorithm.upper()}{part}Msg.rsp'
        text = source.read_text(encoding='ascii')
        found = set()
        for size, message, digest in re.findall(r'Len = (\d+)\nMsg = ([0-9a-f]+)\nMD = ([0-9a-f]+)', text):
            bits = int(size)
            if bits not in requested:
                continue
            raw = bytes.fromhex(message) if bits else b''
            if len(raw) * 8 != bits or bits in found:
                raise ValueError(f'wrong length or repeated record: {source.name} Len={bits}')
            rows.append((raw, digest))
            found.add(bits)
        if found != set(requested):
            raise ValueError(f'missing vectors from {source.name}: {set(requested) - found}')
    print(f'fn {algorithm}_cavp_data() -> Inline[(Bytes, String), {len(rows)}] {{')
    print('    [')
    for raw, digest in rows:
        literal = ''.join(f'\\x{byte:02x}' for byte in raw)
        print(f'        (result.unwrap(b"{literal}"), "{digest}"),')
    print('    ]')
    print('}')
