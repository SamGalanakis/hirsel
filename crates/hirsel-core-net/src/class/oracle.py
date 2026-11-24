#!/usr/bin/env python3
"""Write `cases.rs`, the `public_class_table` law's cases (NET-18), from two oracles.

    python3.14 crates/hirsel-core-net/src/class/oracle.py [IPV4_CSV IPV6_CSV]

The expected class of each address comes from NET-18's table as transcribed below,
read with the `ipaddress` module, not from `class.rs`. The script then checks that
table against the IANA special-purpose registries (fetched, or the two CSVs given)
and records CPython 3.14.0's `is_global` beside each case, refusing any disagreement
the spec does not name.
"""
import csv
import io
import ipaddress
import sys
import urllib.request
from pathlib import Path

assert sys.version_info[:3] == (3, 14, 0), 'the spec names CPython 3.14.0'

N = ipaddress.ip_network
A = ipaddress.ip_address

# NET-18's table, rows in its order. Embedding rows give where the IPv4 sits.
V4 = [
    ('0.0.0.0/32', 'Unspecified'), ('0.0.0.0/8', 'ThisNetwork'),
    ('10.0.0.0/8', 'Private'), ('172.16.0.0/12', 'Private'), ('192.168.0.0/16', 'Private'),
    ('100.64.0.0/10', 'SharedCgnat'), ('127.0.0.0/8', 'Loopback'),
    ('169.254.0.0/16', 'LinkLocal'), ('192.0.0.0/24', 'Protocol'),
    ('192.0.2.0/24', 'Documentation'), ('198.51.100.0/24', 'Documentation'),
    ('203.0.113.0/24', 'Documentation'), ('192.88.99.0/24', 'Relay6to4'),
    ('198.18.0.0/15', 'Benchmarking'), ('224.0.0.0/4', 'Multicast'),
    ('255.255.255.255/32', 'Broadcast'), ('240.0.0.0/4', 'Reserved'),
]
EMBED = [('::ffff:0:0/96', 'low'), ('64:ff9b::/96', 'low'), ('2002::/16', 'sixtofour'),
         ('2001::/32', 'teredo')]
V6 = [
    ('::/128', 'Unspecified'), ('::1/128', 'Loopback'), ('64:ff9b:1::/48', 'Nat64Local'),
    ('2001::/23', 'Protocol'), ('2001:db8::/32', 'Documentation'),
    ('3fff::/20', 'Documentation'), ('fc00::/7', 'UniqueLocal'), ('fe80::/10', 'LinkLocal'),
    ('ff00::/8', 'Multicast'),
]
NOT_GLOBAL = ['100::/64', '5f00::/16', 'fec0::/10']  # named examples of NotGlobal6


def v4_class(ip):
    return next((c for net, c in V4 if ip in N(net)), 'Public')


def spec_class(ip):
    if ip.version == 4:
        return v4_class(ip)
    n = int(ip)
    for net, form in EMBED:
        if ip in N(net):
            if form == 'low':
                return v4_class(A(n & 0xffffffff))
            if form == 'sixtofour':
                return v4_class(A((n >> 80) & 0xffffffff))
            server = v4_class(A((n >> 64) & 0xffffffff))
            return server if server != 'Public' else v4_class(A(~n & 0xffffffff))
    for net, c in V6:
        if ip in N(net):
            return c
    return 'Public' if ip in N('2000::/3') else 'NotGlobal6'


def addresses():
    rows = [net for net, _ in V4] + [net for net, _ in EMBED] + [net for net, _ in V6]
    rows += NOT_GLOBAL + ['2000::/3']
    out = []
    for net in rows:
        net = N(net)
        out += [net[0], net[-1]]
        for edge in (int(net[0]) - 1, int(net[-1]) + 1):
            if 0 <= edge < 2 ** net.max_prefixlen:
                outside = (ipaddress.IPv6Address if net.version == 6 else ipaddress.IPv4Address)(edge)
                if spec_class(outside) == 'Public':
                    out.append(outside)
    # NET-18's named embedded cases.
    out += [A(a) for a in [
        '64:ff9b::a00:1', '64:ff9b::a9fe:a9fe', '64:ff9b::808:808', '2002:7f00:1::1',
        '2002:808:808::1', '2001:0:808:808::80ff:fffe', '2001:0:808:808::f7f7:f7f7',
        '2001:0:a00:1::f7f7:f7f7', '::7f00:1', '169.254.169.254', '192.0.0.9',
        '192.0.0.10', '2001:1::1', '2001:1::2', '2001:3::1', '2001:4:112::1',
        '2001:20::1', '2001:30::1', 'fec0::1', '5f00::1', '8.8.8.8', '2606:4700::1111',
    ]]
    seen, unique = set(), []
    for a in out:
        if a not in seen:
            seen.add(a)
            unique.append(a)
    return unique


# The disagreements NET-18 names: CPython's is_global against this table.
NAMED_GLOBAL_DENIED = ['224.0.0.0/4', '192.88.99.0/24', '192.0.0.9/32', '192.0.0.10/32',
                       'ff00::/8', '2001:1::1/128', '2001:1::2/128', '2001:3::/32',
                       '2001:4:112::/48', '2001:20::/28', '2001:30::/28', '64:ff9b::/96']
NAMED_PUBLIC_NOT_GLOBAL = ['2002::/16', '2001::/32']


def named(a, nets):
    return any(a in N(n) for n in nets)


def registry(text):
    for row in csv.DictReader(io.StringIO(text)):
        reach = row['Globally Reachable'].split(' ')[0]
        for block in row['Address Block'].split(','):
            yield N(block.split()[0]), reach


def main():
    if len(sys.argv) == 3:
        texts = [Path(p).read_text() for p in sys.argv[1:]]
    else:
        base = 'https://www.iana.org/assignments/iana-ipv{0}-special-registry/iana-ipv{0}-special-registry-1.csv'
        texts = [urllib.request.urlopen(base.format(v)).read().decode() for v in (4, 6)]
    # IANA: a block that is not globally reachable must be denied, at both ends,
    # except the forms judged by the IPv4 inside.
    for text in texts:
        for net, reach in registry(text):
            if reach == 'False' and not named(net[0], ['::ffff:0:0/96', '2001::/32']):
                for a in (net[0], net[-1]):
                    assert spec_class(a) != 'Public', (net, a)
    lines = []
    for a in addresses():
        expected = spec_class(a)
        is_global = a.is_global
        if (expected == 'Public') != is_global:
            if is_global:
                # Also every IPv6 address outside 2000::/3 that CPython does not list.
                outside = a.version == 6 and a not in N('2000::/3')
                assert outside or named(a, NAMED_GLOBAL_DENIED), (a, expected)
            else:
                assert named(a, NAMED_PUBLIC_NOT_GLOBAL), (a, expected)
        lines.append(f'    ("{a}", {expected}, {str(is_global).lower()}),\n')
    out = Path(__file__).with_name('cases.rs')
    out.write_text(
        '// @generated by oracle.py (CPython 3.14.0): address, NET-18 class, CPython is_global.\n'
        'pub(super) const CASES: &[(&str, AddressClass, bool)] = &[\n' + ''.join(lines) + '];\n')
    print(f'{len(lines)} cases written to {out}')


main()
