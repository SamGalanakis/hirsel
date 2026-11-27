#!/usr/bin/env python3
"""Print the public Store contract without implementation bodies or locations."""

import argparse
from pathlib import Path
import re
import tomllib


DECLARATION = re.compile(
    r'^(?P<public>pub[ \t]+)?(?P<qualifiers>(?:(?:opaque|resource|scoped)[ \t]+)*)'
    r'(?P<kind>type|fn|const)[ \t]+(?P<name>[A-Za-z_]\w*)',
    re.MULTILINE,
)
TOKEN = re.compile(
    r'//[^\n]*|\b(?:br|r)(?P<hashes>\#*)"[^\n]*?"(?P=hashes)'
    r'|(?:b)?"(?:\\.|[^"\\])*"|\'(?:\\.|[^\'\\])*\'|\w+|[^\s]',
)
PAIRS = {'(': ')', '[': ']', '{': '}'}


def declarations(root, include_tests=False):
    package = root / 'core/programs/store'
    manifest = tomllib.loads((package / 'hirsel.toml').read_text())
    sources = dict(manifest['sources'])
    if include_tests:
        sources.update(manifest['tests']['sources'])
    for filename, module in sources.items():
        text = (package / filename).read_text()
        items = list(DECLARATION.finditer(text))
        for index, item in enumerate(items):
            end = items[index + 1].start() if index + 1 < len(items) else len(text)
            yield filename, module, item, text[item.start():end]


def contract(item, source):
    kind = item['kind']
    opaque = 'opaque' in item['qualifiers'].split()
    tokens = [match[0] for match in TOKEN.finditer(source) if not match[0].startswith('//')]
    output = []
    nesting = []
    hidden = False
    for token in tokens:
        if not nesting:
            if token == '@':
                break
            if token == '=' and (kind == 'const' or opaque):
                break
            if token == '{' and kind == 'fn':
                break
            if token == 'drop' and kind == 'type':
                hidden = False
            if token == '{' and opaque:
                if 'drop' in output:
                    break
                hidden = True
        if token in PAIRS:
            nesting.append(PAIRS[token])
        if not hidden:
            output.append(token)
        if nesting and token == nesting[-1]:
            nesting.pop()
    return ' '.join(output)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('root', type=Path, nargs='?', default=Path(__file__).resolve().parents[1])
    args = parser.parse_args()
    rows = []
    for _, module, item, source in declarations(args.root):
        if module in ('store', 'status') and item['public']:
            rows.append((item['name'], module, contract(item, source)))
    for _, module, signature in sorted(rows):
        print(f'{module}\t{signature}')


if __name__ == '__main__':
    main()
