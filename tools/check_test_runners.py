#!/usr/bin/env python3
"""Audit test files against generated Buck targets and the other repo runners.

Run tools/buck2/sync.py --if-stale before invoking this directly. The land
formatter already synchronizes Buck before calling check().
"""
import ast
from collections import defaultdict
from dataclasses import dataclass
from pathlib import Path
import re
import subprocess
import sys
import tomllib


ROOT = Path(__file__).resolve().parents[1]
IGNORED = {'--ignored', '--include-ignored'}
RUST_LITERAL = re.compile(
    r'//[^\n]*|/\*.*?\*/|(?P<raw>b?r(?P<hashes>\#*)".*?"(?P=hashes))'
    r'|b?"(?:\\.|[^"\\])*"|\'(?:\\.|[^\'\\])\'', re.S)
RUST_MODULE = re.compile(r'\bmod\s+(\w+)\s*([{;])')
RUST_ATTR = re.compile(r'#\[\s*(?:test\s*\]|cfg_attr\(\s*[^,]+,\s*ignore\b|ignore\b)')
RUST_IGNORE = re.compile(r'#\[\s*(?:ignore\b|cfg_attr\(\s*[^,]+,\s*ignore\b)')
RUST_INCLUDE = re.compile(r'\binclude!\(\s*"([^"\n]+\.rs)"\s*\)')
RUST_PATH = re.compile(r'#\[\s*path\s*=\s*"([^"\n]+)"\s*\]\s*(?:pub(?:\([^)]*\))?\s+)?$')
PYTHON_TEST = re.compile(r'\bdef\s+test_\w+\s*\(')
PYTHON_COMMAND = re.compile(r'(?:^[ \t]*(?:- )?|[;&|]\s*)(?:run:\s+|exec\s+)?python(?:3(?:\.\d+)?)?\s+([\w./${}"\'-]+\.py)', re.M)


def workspace_files(root):
    paths = subprocess.check_output(
        ['git', 'ls-files', '-z', '--cached', '--others', '--exclude-standard',
         '--', '*.py', '*.rs', '*.hirsel', '*hirsel.toml', '*test.toml', '*Cargo.toml', '*BUCK'], cwd=root)
    return {root / name for name in map(Path, paths.decode().split('\0')[:-1])
            if (root / name).is_file()}


def buck_rules(path):
    for node in ast.parse(path.read_text(), filename=str(path)).body:
        if isinstance(node, ast.Expr) and isinstance(node.value, ast.Call) and isinstance(node.value.func, ast.Name):
            yield node.value.func.id, {item.arg: item.value for item in node.value.keywords}


def literal(attrs, name, default=None):
    return ast.literal_eval(attrs[name]) if name in attrs else default


def blank_rust(text):
    return RUST_LITERAL.sub(lambda match: re.sub(r'[^\n]', ' ', match[0]), text)


def closing(text, start, delimiters='{}'):
    depth = 1
    for at in range(start, len(text)):
        depth += (text[at] == delimiters[0]) - (text[at] == delimiters[1])
        if depth == 0:
            return at
    raise ValueError('unclosed Rust delimiter')


@dataclass(frozen=True)
class RustModule:
    path: Path
    name: tuple
    directory: Path


def rust_inventory(entry, read):
    """Walk external and inline modules and literal include! in libtest's namespace."""
    visited = set()
    modules = defaultdict(set)
    ignored = set()

    def visit(module):
        if module in visited:
            return
        visited.add(module)
        text, syntax = read(module.path)
        scan(module, text, syntax, 0, len(text))

    def scan(module, text, syntax, start, end):
        modules[module.path].add(module.name)
        cursor = start
        while cursor < end:
            item = RUST_MODULE.search(syntax, cursor, end)
            brace = syntax.find('{', cursor, end)
            if brace >= 0 and (item is None or brace < item.start()):
                cursor = closing(syntax, brace + 1) + 1
                continue
            if item is None:
                break
            name, body = item.groups()
            child = module.name + (name,)
            if body == '{':
                stop = closing(syntax, item.end())
                scan(RustModule(module.path, child, module.directory / name), text, syntax, item.end(), stop)
                cursor = stop + 1
            else:
                if path := RUST_PATH.search(text, start, item.start()):
                    candidate = (module.directory / path[1]).resolve()
                    if not candidate.is_file():
                        candidate = (module.path.parent / path[1]).resolve()
                    candidates = [candidate]
                else:
                    candidates = [module.directory / f'{name}.rs', module.directory / name / 'mod.rs']
                for path in candidates:
                    if path.is_file():
                        directory = path.parent if path.name == 'mod.rs' else path.with_suffix('')
                        visit(RustModule(path, child, directory))
                        break
                cursor = item.end()
            items(module, text, syntax, start, item.start())
            start = cursor
        items(module, text, syntax, start, end)

    def items(module, text, syntax, start, end):
        for match in RUST_INCLUDE.finditer(text, start, end):
            if syntax[match.start():].startswith('include!'):
                path = (module.path.parent / match[1]).resolve()
                if path.is_file():
                    visit(RustModule(path, module.name, module.directory))
        for name, body in ignored_declarations(syntax[start:end]):
            ignored.add((module.path, '::'.join((*module.name, name)), start + body))

    visit(RustModule(entry, (), entry.parent))
    return modules, ignored


def ignored_declarations(syntax):
    """Resolve the repo's fn-name macro templates to their invocation names."""
    for attribute in RUST_IGNORE.finditer(syntax):
        function = re.search(r'\bfn\s+(\$?\w+)[^{]*\{', syntax[attribute.end():])
        if not function:
            raise ValueError('Rust ignore attribute has no function')
        name = function[1]
        body = attribute.end() + function.end()
        if not name.startswith('$'):
            yield name, body
            continue
        definition = list(re.finditer(r'macro_rules!\s+(\w+)\s*\{\s*\((.*?)\)\s*=>',
                                      syntax[:attribute.start()], re.S))[-1]
        macro, matcher = definition.groups()
        index = re.findall(r'\$(\w+):\w+', matcher).index(name[1:])
        for invocation in re.finditer(r'\b' + re.escape(macro) + r'!\s*([(\[{])', syntax):
            opening = invocation[1]
            stop = closing(syntax, invocation.end(), opening + {'(': ')', '[': ']', '{': '}'}[opening])
            arguments = syntax[invocation.end():stop]
            groups = re.findall(r'\(([^()]*)\)', arguments) if matcher.lstrip().startswith('$((') else [arguments]
            for group in groups:
                fields = [field.strip() for field in group.split(',')]
                if len(fields) > index and re.fullmatch(r'\w+', fields[index]):
                    yield fields[index], body


def selected(args, name):
    if not IGNORED.intersection(args):
        return False
    filters = []
    skips = []
    exact = '--exact' in args
    args = iter(args)
    for arg in args:
        if arg == '--skip':
            skips.append(next(args))
        elif arg.startswith('--skip='):
            skips.append(arg.split('=', 1)[1])
        elif not arg.startswith('-'):
            filters.append(arg)
    matches = (lambda term: term == name) if exact else (lambda term: term in name)
    return (not filters or any(map(matches, filters))) and not any(map(matches, skips))


def ignored_buck_runners(root, rules):
    runners = defaultdict(list)
    defs = root / 'tools/buck2/defs.bzl'
    if not defs.is_file():
        return runners
    macros = {node.name: node for node in ast.parse(defs.read_text()).body if isinstance(node, ast.FunctionDef)}
    for rule, attrs in rules:
        if rule == 'hirsel_core_driver_native_test' and passes_ignored(macros[rule]):
            args = [literal(attrs, 'case_filter', 'native::tests::'), '--ignored']
            if literal(attrs, 'delegated_cgroup', False):
                args.append('--exact')
            runners['hirsel-core-driver-unittest'].append(args)
        if rule == 'hirsel_core_llvm_native_test':
            runners[literal(attrs, 'binary', 'hirsel-llvm-unittest')].append(literal(attrs, 'selection') or [])
        if rule == 'hirsel_driver_archive_test':
            runners['hirsel-core-driver-unittest'].append(literal(attrs, 'selection'))
    return runners


def passes_ignored(node):
    return any(isinstance(arg, ast.Constant) and isinstance(arg.value, str) and arg.value in IGNORED
               for call in ast.walk(node)
               if isinstance(call, ast.Call) and isinstance(call.func, ast.Name) and call.func.id == '_rust_test'
               for keyword in call.keywords if keyword.arg == 'args'
               for arg in ast.walk(keyword.value))


def python_runners(root, rules):
    runners = {root / literal(attrs, 'script') for rule, attrs in rules if rule == 'hirsel_script_test'}
    scripts = [*(root / 'tools').glob('nightly-*.sh'), *(root / '.github/workflows').glob('*.y*ml')]
    for script in scripts:
        for match in PYTHON_COMMAND.finditer(script.read_text()):
            command = match[1].strip('"\'')
            for prefix in ('$tools/', '${tools}/', '$checkout/', '${checkout}/'):
                if command.startswith(prefix):
                    command = ('tools/' if 'tools' in prefix else '') + command[len(prefix):]
                    break
            runners.add((root / command).resolve())
    return runners


def python_tests(path):
    text = path.read_text(errors='replace')
    if not PYTHON_TEST.search(text):
        return False
    tree = ast.parse(text, filename=str(path))
    for node in ast.walk(tree):
        if isinstance(node, ast.ClassDef) and any(test_function(item) for item in node.body):
            return True
    for node in tree.body:
        if not test_function(node):
            continue
        if path.name.startswith('test_') or path.parent.name == 'tests':
            return True
        # Production functions such as event_logs.test_history return a value.
        if not (node.args.args or node.args.posonlyargs or node.args.kwonlyargs) and not any(
                isinstance(item, ast.Return) and item.value is not None for item in ast.walk(node)):
            return True
    return False


def test_function(node):
    return isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef)) and node.name.startswith('test_')


def hirsel_problems(root, files):
    registered = set()
    staged = {}
    for manifest in files:
        if manifest.name == 'hirsel.toml':
            registered.update(hirsel_test_sources(manifest))
        elif manifest.name == 'test.toml' and manifest.parent.name == 'vectors':
            # The codec scripts stage these fragments into the package manifest.
            script = manifest.parent.parent / 'test-vectors.sh'
            staged.update((path, script) for path in hirsel_test_sources(manifest))
    for path in files:
        if path.suffix != '.hirsel' or path in registered or path.is_relative_to(root / 'core/front/cases'):
            continue
        source = path.read_text(errors='replace')
        if not re.search(r'^[ \t]*@test\b', source, re.M):
            continue
        marker = re.search(r'^//\s*test-runner:\s*(\S+)', source, re.M)
        runner = (root / marker[1]).resolve() if marker else staged.get(path)
        if runner and runner.is_relative_to(root) and runner.is_file():
            names = re.findall(r'^[ \t]*@test\s+fn\s+(\w+)', source, re.M)
            command = '\n'.join(line for line in runner.read_text().splitlines() if not line.lstrip().startswith('#'))
            if names and re.search(r'\btest\s+', command) and all(f'/{name}' in command for name in names):
                continue
        yield f'{path.relative_to(root)} (@test outside [tests.sources])'


def hirsel_test_sources(manifest):
    tests = tomllib.loads(manifest.read_text()).get('tests', {})
    return {(manifest.parent / name).resolve() for name in tests.get('sources', {})}


def standalone_rust_roots(root, files):
    """The nested benchmark, corpus and grammar workspaces have their own Cargo runners."""
    for manifest in sorted(files):
        directory = manifest.parent
        if manifest.name != 'Cargo.toml' or not any(directory.is_relative_to(root / name)
                                                  for name in ('bench/wire', 'core/twins', 'editors/tree-sitter-hirsel')):
            continue
        config = tomllib.loads(manifest.read_text())
        for kind, default in [('lib', 'src/lib.rs'), ('bin', 'src/main.rs')]:
            targets = config.get(kind, [{}])
            for target in [targets] if isinstance(targets, dict) else targets:
                if target.get('test', True) and (path := directory / target.get('path', default)).is_file():
                    yield path
        yield from directory.glob('tests/*.rs')


def rust_problems(root, files, rules):
    cache = {}

    def read(path):
        if path not in cache:
            text = path.read_text(errors='replace')
            cache[path] = text, blank_rust(text)
        return cache[path]

    runners = ignored_buck_runners(root, rules)
    reached = set()
    ignored = set()
    covered = set()
    entries = [(literal(attrs, 'name'), root / literal(attrs, 'crate_root'))
               for rule, attrs in rules if rule == 'hirsel_test']
    entries.extend((None, path) for path in standalone_rust_roots(root, files))
    fixtures = {label.removeprefix('//:').removesuffix('-harness'): root / literal(attrs, 'script')
                for rule, attrs in rules if rule == 'hirsel_script_test'
                for label in (literal(attrs, 'files_env', {}) or {}).values()
                if label.endswith('-harness')}
    for harness, entry in entries:
        if not entry.is_file():
            continue
        paths, cases = rust_inventory(entry, read)
        reached.update(paths)
        child_runners = {p: read(p)[0] for p in paths if any(flag in read(p)[0] for flag in IGNORED)}
        fixture = fixtures.get(harness)
        for path, name, body in cases:
            case = path, name
            ignored.add(case)
            if fixture and fixture.is_file() and any(flag in fixture.read_text() for flag in IGNORED):
                covered.add(case)
                continue
            if any(selected(args, name) for args in runners[harness]):
                covered.add(case)
                continue
            quote = '"' + name + '"'
            _, syntax = read(path)
            end = closing(syntax, body)
            if any(spawns(source[:body] + source[end:] if candidate == path else source, quote)
                   for candidate, source in child_runners.items()):
                covered.add(case)
    for path, name in sorted(ignored - covered):
        yield f'{path.relative_to(root)}::{name} (ignored, no runner passes --ignored)'
    for path in sorted(files - reached):
        if path.suffix == '.rs' and RUST_ATTR.search(read(path)[1]):
            yield f'{path.relative_to(root)} (#[test] file no harness compiles)'


def spawns(source, quoted_name):
    literals = {match[0] for match in RUST_LITERAL.finditer(source) if match[0].startswith('"')}
    return (quoted_name in literals and any(f'"{flag}"' in literals for flag in IGNORED)
            and re.search(r'\b(?:Command::new|test_process::command)\s*\(', blank_rust(source)))


def problems(root):
    files = workspace_files(root)
    if not (root / 'BUCK').is_file():
        return ['BUCK is missing. Run python3 tools/buck2/sync.py --if-stale.']
    rules = list(buck_rules(root / 'BUCK'))
    for path in sorted(files):
        if path.name == 'BUCK' and path != root / 'BUCK':
            rules.extend(buck_rules(path))
    runners = python_runners(root, rules)
    findings = [f'{path.relative_to(root)} (Python tests without a runner)'
                for path in sorted(files - runners) if path.suffix == '.py' and python_tests(path)]
    findings.extend(rust_problems(root, files, rules))
    findings.extend(hirsel_problems(root, files))
    return sorted(findings)


def check(root):
    missing = problems(root)
    for problem in missing:
        print(f'runner-completeness: {problem}')
    return not missing


if __name__ == '__main__':
    sys.exit(0 if check(ROOT) else 1)
