#!/usr/bin/env python3
"""TLS-9's server fixtures, made by openssl: a test CA, P-256 leaves for
alpha.test and beta.test, Ed25519, RSA-2048, P-384 and RSA-1024 alpha.test
leaves, every key as PKCS#8 DER, each leaf's DER and its SHA-256 digest, and
padding certificates that make [alpha, pad] a Certificate message of exactly
65,535 bytes (4 + sum of DER length + 5, RFC 8446 section 4.4.2) and
[alpha, pad-over] one byte more.

Run from this directory: python3 -I generate.py
(python3 -I generate.py pads remakes only the padding certificates.)
"""
import hashlib
import subprocess
import tempfile
from pathlib import Path

HERE = Path(__file__).resolve().parent
DAYS = '36500'
KEYS = {
    'ca': ['-algorithm', 'EC', '-pkeyopt', 'ec_paramgen_curve:P-256'],
    'alpha': ['-algorithm', 'EC', '-pkeyopt', 'ec_paramgen_curve:P-256'],
    'beta': ['-algorithm', 'EC', '-pkeyopt', 'ec_paramgen_curve:P-256'],
    'alpha-ed25519': ['-algorithm', 'ED25519'],
    'alpha-rsa2048': ['-algorithm', 'RSA', '-pkeyopt', 'rsa_keygen_bits:2048'],
    'alpha-p384': ['-algorithm', 'EC', '-pkeyopt', 'ec_paramgen_curve:P-384'],
    'alpha-rsa1024': ['-algorithm', 'RSA', '-pkeyopt', 'rsa_keygen_bits:1024'],
    'pad': ['-algorithm', 'EC', '-pkeyopt', 'ec_paramgen_curve:P-256'],
}
HOSTS = {'beta': 'beta.test'}
CHAIN_MESSAGE = 65_535


def openssl(*args, data=None):
    return subprocess.run(['openssl', *args], input=data, check=True, capture_output=True).stdout


def der(pem: Path) -> bytes:
    return openssl('x509', '-in', str(pem), '-outform', 'der')


def main():
    with tempfile.TemporaryDirectory() as scratch:
        tmp = Path(scratch)
        for name, options in KEYS.items():
            pem = tmp / f'{name}.key'
            openssl('genpkey', *options, '-out', str(pem))
            if name in ('ca', 'pad'):
                continue
            (HERE / f'{name}.p8').write_bytes(
                openssl('pkcs8', '-topk8', '-nocrypt', '-in', str(pem), '-outform', 'der'))
        openssl('req', '-x509', '-new', '-key', str(tmp / 'ca.key'), '-days', DAYS,
                '-subj', '/CN=Hirsel TLS-9 test CA', '-addext', 'basicConstraints=critical,CA:TRUE',
                '-addext', 'keyUsage=critical,keyCertSign', '-out', str(HERE / 'ca.pem'))
        for name in KEYS:
            if name in ('ca', 'pad'):
                continue
            host = HOSTS.get(name, 'alpha.test')
            csr = tmp / f'{name}.csr'
            openssl('req', '-new', '-key', str(tmp / f'{name}.key'), '-subj', f'/CN={host}',
                    '-out', str(csr))
            ext = tmp / f'{name}.ext'
            ext.write_text(f'subjectAltName=DNS:{host}\nbasicConstraints=critical,CA:FALSE\n')
            pem = tmp / f'{name}.pem'
            openssl('x509', '-req', '-in', str(csr), '-CA', str(HERE / 'ca.pem'), '-CAkey',
                    str(tmp / 'ca.key'), '-set_serial', str(len(name) * 7919), '-days', DAYS,
                    '-extfile', str(ext), '-out', str(pem))
            leaf = der(pem)
            (HERE / f'{name}.der').write_bytes(leaf)
            (HERE / f'{name}.sha256').write_text(hashlib.sha256(leaf).hexdigest() + '\n')
        alpha = (HERE / 'alpha.der').read_bytes()
        pads(tmp, alpha)


def pads(tmp: Path, alpha: bytes):
    """The padding certificates: self-signed, an opaque extension sized so
    [alpha, pad] fills the Certificate message exactly and [alpha, pad-over]
    is one byte over."""
    key = tmp / 'pad.key'
    if not key.exists():
        openssl('genpkey', *KEYS['pad'], '-out', str(key))
    for name, message in (('pad', CHAIN_MESSAGE), ('pad-over', CHAIN_MESSAGE + 1)):
        want = message - 4 - (len(alpha) + 5) - 5
        filler = want - 600
        for _ in range(16):
            pem = tmp / f'{name}.pem'
            openssl('req', '-x509', '-new', '-key', str(key), '-days', DAYS,
                    '-subj', '/CN=padding', '-addext',
                    f'1.3.6.1.4.1.55555.1=DER:04{"82" + format(filler, "04x")}{"00" * filler}',
                    '-out', str(pem))
            got = len(der(pem))
            if got == want:
                break
            filler += want - got
        else:
            raise SystemExit('padding did not converge')
        (HERE / f'{name}.der').write_bytes(der(pem))
        assert 4 + sum(len(c) + 5 for c in (alpha, der(pem))) == message

if __name__ == '__main__':
    import sys
    if sys.argv[1:] == ['pads']:
        with tempfile.TemporaryDirectory() as scratch:
            pads(Path(scratch), (HERE / 'alpha.der').read_bytes())
    else:
        main()
