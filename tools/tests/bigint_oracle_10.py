#!/usr/bin/env python3
"""Independent bigint_mul reference; run with --derive to print its provenance.

Python integers derive the budget oracle without running Hirsel.
No Karatsuba, machine-word multiplication, or Hirsel runtime code is ported.
"""
import hashlib
import json
from pathlib import Path
import sys

sys.dont_write_bytecode = True
ROOT = Path(__file__).resolve().parents[2]
MASK = (1 << 64) - 1
SOURCE_SHA256 = 'c11bee845c4f95af0537d9994ecc6b16f34f1565897803d6e0ac08e9bedfb567'
REFERENCE_COMMIT = 'ebe2feefc400f813aefdc8a036e66d2619da6649'
REFERENCE_SHA256 = {
    'src/math/big/natmul.go': 'f1767186e544a77fea89923a0051db1f3a36658d34f12ef4b35b78bb42234d40',
    'src/math/big/int_test.go': '9c49e5f12402151e7cc0776ec296ca89012df603bae922571296432c71cc6a08',
}


def operand(n, salt):
    value, state = 0, salt
    for _ in range(n):
        state = (state ^ (state << 13)) & MASK
        state = (state ^ (state >> 7)) & MASK
        state = (state ^ (state << 17)) & MASK
        value = (value << 64) + (state | (1 << 63))
    return value


def checksum(n, rounds, extra=0, multiply=lambda a, b: a * b):
    a, b = operand(n, 123456789), operand(n + extra, 987654321)
    total = 0
    for _ in range(rounds):
        product = multiply(a, b)
        total = (total + ((product & MASK) ^ product.bit_length())) & MASK
        a += 1
    return total


def derivation():
    a, b = operand(1000, 123456789), operand(1000, 987654321)
    p = a * b
    return dict(source_sha256=SOURCE_SHA256, reference_commit=REFERENCE_COMMIT,
                reference_sha256=REFERENCE_SHA256, arguments=[1000, 1, 0],
                operand_bits=[a.bit_length(), b.bit_length()],
                operand_sha256=[hashlib.sha256(v.to_bytes(8000, 'big')).hexdigest() for v in (a, b)],
                product_bits=p.bit_length(), product_low64=p & MASK,
                checksum=checksum(1000, 1), stdout=f'{checksum(1000, 1)}\n')


if __name__ == '__main__':
    if sys.argv[1:] == ['--derive']:
        print(json.dumps(derivation(), indent=2))
