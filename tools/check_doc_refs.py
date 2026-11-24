#!/usr/bin/env python3
"""Check repository paths and Hirsel CLI names in the user-facing docs.

An adjacent <!-- doc-ref: historical --> preserves a path quoted from an old
revision. Use <!-- doc-ref: planned --> for planned CLI and
<!-- doc-ref: external --> for options outside the driver CLI. A marker applies
only to the preceding code span.
"""

from pathlib import Path
import re
import sys

ROOT = Path(__file__).resolve().parents[1]
DOCS = (
    'README.md',
    'docs/building.md',
    'docs/PRIMER.md',
    'core/programs/store/STORE-SPEC.md',
    'core/programs/runner/RUNNER-SPEC.md',
    'core/programs/harness/README.md',
)
DRIVER = 'crates/hirsel-core-driver/src/cli.rs'
EXEMPT = r'<!-- doc-ref: (historical|planned|external) -->'
CODE_SPAN = re.compile(r'(?<!`)(`+)(?!`)(.+?)(?<!`)\1(?!`)(?:[ \t]*' + EXEMPT + r')?')
FENCE = re.compile(r'^\s*(`{3,}|~{3,})([^`~]*)$')
REPO_PATH = re.compile(r'(?:core|crates|docs|tools)/[^\s:#]+(?::\d+(?:-\d+)?)?(?:#[^\s]+)?$')
FLAG = re.compile(r'(?<![\w/-])--[a-zA-Z][\w-]*')
COMMAND = re.compile(r'^hirsel\s+([a-zA-Z][\w-]*)(?=\s|$)(?:\s+([a-zA-Z][\w-]*)(?=\s|$))?')
SHELL_SEPARATOR = re.compile(r'\s+(?:&&|\|\||[;|])\s+|\s+#.*$')


def cli_names(root):
    driver = root / DRIVER
    source = driver.read_text(encoding='utf-8')
    catalogue = source.split('\ncommands! {', 1)[1].split('\n}', 1)[0]
    commands = {}
    for name, usage in re.findall(r'^\s*"([\w -]+)", "((?:[^"\\]|\\.)*)"', catalogue, re.M):
        parent, _, child = name.partition(' ')
        children = commands.setdefault(parent, set())
        if child:
            children.add(child)
        elif match := re.match(r'[\w-]+ ([a-z][\w-]*)\b', usage):
            children.add(match[1])

    flags = set()
    package_options = driver.with_name('cli_package_options.rs').read_text(encoding='utf-8')
    for module in (source, package_options):
        options = module.split('static OPTIONS:', 1)[1].split('\n];', 1)[0]
        for names in re.findall(r'option!\(\s*\w+,\s*\[([^\]]+)\]', options):
            flags.update(FLAG.findall(names))
    # Top-level switches such as --version are handled directly by parse().
    parser = driver.with_name('cli_parse.rs').read_text(encoding='utf-8')
    flags.update(re.findall(r'\bif name == "(--[\w-]+)"', parser))
    if not commands or not flags:
        raise ValueError('empty command or option catalogue')
    return commands, flags


def references(document):
    fence = None
    for line_no, line in enumerate(document.splitlines(), 1):
        marker = FENCE.fullmatch(line)
        info = marker[2].strip() if marker else ''
        if fence:
            delimiter, shell = fence
            if marker and not info and marker[1].startswith(delimiter):
                fence = None
            elif shell:
                for command in SHELL_SEPARATOR.split(line):
                    command = command.strip().removeprefix('$ ').lstrip()
                    if command.startswith('hirsel '):
                        yield line_no, command
        elif marker:
            fence = marker[1], info in ('', 'sh', 'bash', 'shell', 'console', 'text')
        else:
            for span in CODE_SPAN.finditer(line):
                if not span[3]:
                    yield line_no, span[2].strip()


def check(root):
    commands, flags = cli_names(root)
    docs = sorted([*(root / name for name in DOCS), *(root / 'docs/spec').glob('*.md')])
    for doc in docs:
        for line, ref in references(doc.read_text(encoding='utf-8')):
            location = f'{doc.relative_to(root)}:{line}'
            if REPO_PATH.fullmatch(ref):
                path = re.split(r'[:#]', ref, maxsplit=1)[0]
                if not (root / path).exists():
                    yield f'{location}: missing repo path {path}'
                continue

            command = COMMAND.match(ref)
            if command:
                name, child = command.groups()
                if name not in commands:
                    yield f'{location}: unknown Hirsel command {name}'
                elif child and commands[name] and child not in commands[name]:
                    yield f'{location}: unknown Hirsel command {name} {child}'
                elif name == 'help' and child and child not in commands:
                    yield f'{location}: unknown Hirsel command {child}'
            if command or ref.startswith(('--', 'hirsel --')) or ref.split(' ', 1)[0] in commands:
                # A bare -- forwards the rest of the example to the program.
                arguments = re.split(r'(?<![\w/-])--(?=\s|$)|\s+#', ref, maxsplit=1)[0]
                for flag in FLAG.findall(arguments):
                    if flag not in flags:
                        yield f'{location}: unknown Hirsel flag {flag}'


def main():
    try:
        errors = list(check(ROOT))
    except (IndexError, OSError, ValueError) as error:
        print(f'{DRIVER}: could not check documentation references ({error})', file=sys.stderr)
        return 1
    if errors:
        print('\n'.join(errors), file=sys.stderr)
    return bool(errors)


if __name__ == '__main__':
    sys.exit(main())
