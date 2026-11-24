"""Load the pinned Wycheproof cases and the NIST P-384 SigVer records."""

from pathlib import Path
import sys

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE.parent))

from signature_vectors import fixture, read_groups, table, validity


def wycheproof(name):
    return [
        (test['tcId'], bytes.fromhex(group['publicKey']['uncompressed']),
         bytes.fromhex(test['msg']), bytes.fromhex(test['sig']), validity(test))
        for group in read_groups(HERE / name)
        for test in group['tests']
    ]


def nist():
    source = (HERE / 'SigVer-P384-SHA384.rsp').read_text()
    heading, *records = source.strip().split('\n\n')
    if heading != '[P-384,SHA-384]' or len(records) != 15:
        raise ValueError('expected 15 NIST P-384/SHA-384 records')
    for index, record in enumerate(records, 1):
        row = dict(line.split(' = ', 1) for line in record.splitlines())
        expected = {'P': True, 'F': False}[row['Result'][0]]
        key = bytes.fromhex('04' + row['Qx'].zfill(96) + row['Qy'].zfill(96))
        sig = bytes.fromhex(row['R'].zfill(96) + row['S'].zfill(96))
        yield index, key, bytes.fromhex(row['Msg']), sig, expected


def main():
    fixed = wycheproof('ecdsa_secp384r1_sha384_p1363_test.json')
    der = wycheproof('ecdsa_secp384r1_sha384_test.json')
    table('wycheproof_fixed', fixed)
    table('wycheproof_der', der)
    table('nist_sigver', list(nist()))

    _, key, msg, sig, _ = next(row for row in fixed if row[0] == 1)
    fixture('fixed_tc1', key, msg, sig)
    compressed = bytes([2 + (key[-1] & 1)]) + key[1:49]
    fixture('fixed_tc1_short_forms', compressed, key[1:], sig[:-1])


if __name__ == '__main__':
    main()
