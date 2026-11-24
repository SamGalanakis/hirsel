"""Load the pinned Wycheproof cases and RFC 7515 Appendix A.2's JWS."""

import base64
from pathlib import Path
import sys

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE.parent))

from signature_vectors import fixture, read_groups, table, validity


def cases(name, *, sha256_only=False):
    for group in read_groups(HERE / name):
        if sha256_only and group['sha'] != 'SHA-256':
            continue
        key = group['privateKey' if sha256_only else 'publicKey']
        n = bytes.fromhex(key['modulus'])
        e = bytes.fromhex(key['publicExponent'])
        for test in group['tests']:
            yield test, n, e, bytes.fromhex(test['msg']), bytes.fromhex(test['sig'])


def bounded_key_oracle(test):
    if test['result'] != 'acceptable' or 'SmallModulus' not in test['flags']:
        raise ValueError(f"tcId {test['tcId']}: expected a small-modulus fixture")
    return False


def pkcs1_oracle(test):
    if test['tcId'] == 8 and test['result'] == 'acceptable' and test['flags'] == ['MissingNull']:
        # RFC 8017 §9.2 specifies the SHA-256 DigestInfo bytes, including NULL.
        return False
    return validity(test)


def b64(value):
    return base64.urlsafe_b64decode(value + '=' * (-len(value) % 4))


def main():
    pkcs1 = list(cases('rsa_signature_2048_sha256_test.json'))
    pss = list(cases('rsa_pss_2048_sha256_mgf1_32_test.json'))
    empty_salt = cases('rsa_pss_2048_sha256_mgf1_0_test.json')
    small = cases('rsa_pkcs1_1024_sig_gen_test.json', sha256_only=True)
    table('wycheproof_pkcs1', [
        (test['tcId'], *parts, pkcs1_oracle(test)) for test, *parts in pkcs1
    ])
    table('wycheproof_pss', [
        (test['tcId'], *parts, validity(test)) for test, *parts in pss
    ])
    table('wycheproof_empty_salt', [
        (test['tcId'], *parts, False) for test, *parts in empty_salt if validity(test)
    ])
    table('wycheproof_small_modulus', [
        (test['tcId'], *parts, bounded_key_oracle(test)) for test, *parts in small
    ])

    n, e, msg, sig = next(parts for test, *parts in pkcs1 if test['tcId'] == 1)
    fixture('pkcs1_tc1', n, e, msg, sig)
    fixture('pkcs1_tc1_padded', b'\x00' + n, b'\x00' + e)
    fixture('pss_tc1', *next(parts for test, *parts in pss if test['tcId'] == 1))

    _, encoded_n, encoded_e, compact = (HERE / 'rfc7515-a2.txt').read_text().splitlines()
    signing_input, signature = compact.rsplit('.', 1)
    msg = signing_input.encode('ascii')
    edited = msg[:-1] + bytes([msg[-1] ^ 1])
    fixture('rfc7515_a2', b64(encoded_n), b64(encoded_e), msg, b64(signature), edited)


if __name__ == '__main__':
    main()
