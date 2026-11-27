"""Discover source-level contracts and their entry fixtures."""

import dataclasses
import json
from pathlib import Path
import re


@dataclasses.dataclass(frozen=True)
class Contract:
    file: Path
    line: int
    call: str
    expected: str
    waits: bool
    stdout: bytes = b""
    stderr: bytes = b""
    first_write: int | None = None
    test_of: str | None = None
    host_op: int = 0
    package: str = "std"
    evaluator_only: str | None = None
    # `// engine-fact: <fact>` before the law: an engine states <fact> to its code
    # generator on the strength of this law, so the native suites run it on each engine.
    engine_fact: str | None = None
    # `// schedule: seeds N [max_events M]` before the law: the native run repeats the
    # law under N seeded schedules (core/tools/std_schedule.c).
    schedule: tuple[int, int | None] | None = None
    root_limit: int = 64 << 20
    evaluator_instructions: int = 1_000_000_000
    # File-level `// test-env: NAME=VALUE`: each native law of the file runs once
    # more with NAME=VALUE set, e.g. to select a portable runtime path.
    run_env: tuple[tuple[str, str], ...] = ()
    # Exact child environment, also the grant names for laws that take Env.
    env_fixture: tuple[tuple[str, bytes | None], ...] = ()
    # A per-law reason why the evaluator cannot run this native contract.
    native_only: str | None = None
    argv: tuple[str, ...] | None = None

    @property
    def name(self):
        prefix = self.package + '.' if self.package != 'std' else ''
        name, _, inputs = self.call.partition('(')
        # Table rows can pass a literal selector after their entry parameters.
        selector = re.search(r', (\d+)\)$', inputs) if self.argv is not None else None
        suffix = ':' + selector[1] if selector else ''
        return f'{prefix}{self.file.stem}:{name}{suffix}'


def masked_source(source):
    return re.sub(r'//[^\n]*|/\*.*?\*/|"(?:\\.|[^"\\])*"|\'(?:\\.|[^\'\\])*\'',
                    lambda m: ' ' * len(m[0]), source, flags=re.S)


def declared_names(source):
    # Inspect only named module-level declarations, not callable parameter types.
    masked = masked_source(source)
    depth = 0
    names = []
    tokens = list(re.finditer(r'[A-Za-z_]\w*|[{}]', masked))
    for i, token in enumerate(tokens):
        text = token[0]
        if text == '{':
            depth += 1
        elif text == '}':
            depth -= 1
        elif depth == 0 and text in ('fn', 'type', 'const') and i + 1 < len(tokens) and re.match(r'\s+[A-Za-z_]', masked[token.end():]):
            names.append((text, tokens[i + 1][0]))
    return names


def test_module(root, file, source):
    first = source.splitlines()[0] if source.splitlines() else ''
    if not first.startswith('// test-of:'):
        return None
    match = re.fullmatch(r'// test-of: (core|std)\.([a-z][a-z0-9_]*)', first)
    if not match:
        raise ValueError(f'{file}: invalid first-line test-of marker')
    package, module = match.groups()
    if file.parent != root / 'core' / package / 'tests':
        raise ValueError(f'{file}: test-of must name its own library package')
    library = root / 'core' / package / (module + '.hirsel')
    if not library.is_file():
        raise ValueError(f'{file}: test-of module does not exist')
    if not re.search(r'^// test-reason: \S.+', source, re.M):
        raise ValueError(f'{file}: marked test needs // test-reason: why public API cannot express it')
    names = declared_names(source)
    if any(kind != 'fn' for kind, _ in names) or re.search(r'^\s*pub\b', source, re.M):
        raise ValueError(f'{file}: marked test may add only private test/helper functions')
    masked = masked_source(source)
    functions = list(re.finditer(r'\bfn\s+[A-Za-z_]\w*', masked))
    if any('{' not in masked[f.end():functions[i+1].start() if i+1 < len(functions) else len(masked)]
           for i, f in enumerate(functions)):
        raise ValueError(f'{file}: marked helper functions must have bodies')
    original = {name for _, name in declared_names(library.read_text())}
    additions = [name for _, name in names]
    if len(additions) != len(set(additions)) or original.intersection(additions):
        raise ValueError(f'{file}: test-of declaration name collision')
    if 'native_contract' in original or 'native_contract' in additions:
        raise ValueError(f'{file}: reserved adapter name collision')
    return module


def matches_filter(root, case, selector):
    return (case.name.startswith(selector)
            or case.package == 'std' and ('std.' + case.name).startswith(selector)
            or str(case.file.relative_to(root)).startswith(selector))


def env_fixture(file, source):
    # // test-env-fixture: {"MISSING": null, "TEXT": "value", "RAW": [255]}
    fixtures = re.findall(r'^// test-env-fixture: (.+)$', source, re.M)
    if not fixtures:
        return ()
    if len(fixtures) != 1:
        raise ValueError(f'{file}: expected one Env fixture')
    entries = json.loads(fixtures[0])
    if not isinstance(entries, dict) or not entries:
        raise ValueError(f'{file}: Env fixture must name at least one key')
    result = []
    for name, value in entries.items():
        if not re.fullmatch(r'[A-Z0-9_]+', name):
            raise ValueError(f'{file}: invalid Env fixture key {name!r}')
        if isinstance(value, str):
            value = value.encode()
        elif isinstance(value, list):
            value = bytes(value)
        elif value is not None:
            raise ValueError(f'{file}: invalid Env fixture value for {name}')
        if value is not None and b'\0' in value:
            raise ValueError(f'{file}: NUL in Env fixture value for {name}')
        result.append((name, value))
    return tuple(result)


def discover(root, filters):
    contracts = []
    files = [p for package in ['core', 'std'] for p in (root / 'core' / package / 'tests').glob('*.hirsel')]
    files += list((root / 'core/programs/runner/tests').glob('*.hirsel'))
    for file in sorted(files):
        source = file.read_text()
        test_of = test_module(root, file, source)
        backend = re.findall(r'^// test-backend: (.+)$', source, re.M)
        if backend and backend != ['evaluator']:
            raise ValueError(f'{file}: only // test-backend: evaluator is supported')
        reason = re.search(r'^// test-reason: (\S.+)$', source, re.M)
        if backend and not reason:
            raise ValueError(f'{file}: evaluator-only law needs // test-reason:')
        evaluator_only = reason[1] if backend else None
        run_env = tuple(tuple(pair) for pair in re.findall(r'^// test-env: ([A-Z0-9_]+)=(\S+)$', source, re.M))
        fixture = env_fixture(file, source)
        stdout = stderr = b""
        first_write, host_op = None, 0
        engine_fact = None
        schedule = None
        root_limit = 64 << 20
        evaluator_instructions = 1_000_000_000
        native_only = None
        argv = None
        for line, text in enumerate(source.splitlines(), 1):
            if text.startswith("// test-argv: "):
                argv = json.loads(text.removeprefix("// test-argv: "))
                if not isinstance(argv, list) or any(not isinstance(a, str) or '\0' in a for a in argv):
                    raise ValueError(f'{file}:{line}: argv must be a list of strings without NUL')
            if text.startswith("// test-limit: "):
                root_limit = int(text.removeprefix("// test-limit: "))
                if not 0 < root_limit < 1 << 64:
                    raise ValueError(f'{file}:{line}: test limit is outside u64')
            if text.startswith("// test-instructions: "):
                evaluator_instructions = int(text.removeprefix("// test-instructions: "))
                if not 0 < evaluator_instructions < 1 << 64:
                    raise ValueError(f'{file}:{line}: evaluator instruction limit is outside u64')
            if text.startswith("// test-native-only:"):
                native_only = text.removeprefix("// test-native-only:").strip()
                if not native_only or evaluator_only:
                    raise ValueError(f'{file}:{line}: native-only law needs a reason and cannot be evaluator-only')
            if text.startswith("// schedule: "):
                words = text.removeprefix("// schedule: ").split()
                if len(words) not in (2, 4) or words[0] != 'seeds' or words[2:3] not in ([], ['max_events']):
                    raise ValueError(f'{file}:{line}: unreadable schedule line')
                schedule = (int(words[1]), int(words[3]) if len(words) == 4 else None)
            if text.startswith("// engine-fact: "):
                engine_fact = text.removeprefix("// engine-fact: ").strip()
            if text.startswith("// expect: stdout "):
                stdout = json.loads(text.removeprefix("// expect: stdout ")).encode()
            if text.startswith("// expect: stderr "):
                stderr = json.loads(text.removeprefix("// expect: stderr ")).encode()
            if text.startswith("// host: first_write "):
                words = text.removeprefix("// host: first_write ").split()
                first_write = int(words[0])
                host_op = 2 if words[1:] == ["file"] else 0
            if text == "// host: remove_tree_prefix a":
                first_write, host_op = 0, 9
            if text == "// host: replace_faults":
                first_write, host_op = 0, 119
            if text == "// host: glob_list_paths":
                first_write, host_op = 0, 44
            match = re.fullmatch(r'\s*// expect: (reference )?run (.*?) = (.*?)\s*', text)
            if not match:
                if text.strip().startswith(('// expect: run ', '// expect: reference run ')):
                    raise ValueError(f'{file}:{line}: unreadable run oracle')
                continue
            reference, call, expected = match.groups()
            reference_reason = evaluator_only or ('Reference count under MEM-30' if reference else None)
            takes_args = re.search(r'\(args(?:,|\))', call) is not None
            if argv is not None and not takes_args:
                raise ValueError(f'{file}:{line}: test-argv requires an Args entry')
            if ('(env)' in call or '(args, env' in call) and not fixture:
                raise ValueError(f'{file}:{line}: Env law needs a fixture')
            if expected.startswith('fault ') and not reference_reason:
                raise ValueError(f'{file}:{line}: fault oracle requires a reference-only run')
            if expected.startswith('fault '):
                if not isinstance(json.loads(expected.removeprefix('fault ')), str):
                    raise ValueError(f'{file}:{line}: fault oracle must be an exact JSON string')
            if reference_reason and (native_only or not call.endswith('()') or first_write is not None or schedule):
                raise ValueError(f'{file}:{line}: evaluator-only laws take no host arguments')
            name = call.split('(')[0]
            header = re.search(r'\bfn\s+' + re.escape(name) + r'\s*\([^)]*\)([^\{]*)\{', source)
            if header is None:
                raise ValueError(f'{file}:{line}: no function for {call}')
            package = 'runner' if file.parent == root / 'core/programs/runner/tests' else file.relative_to(root).parts[1]
            case = Contract(file, line, call, expected, bool(re.search(r'\bwaits\b', header[1])), stdout, stderr, first_write, test_of, host_op, package, reference_reason, engine_fact, schedule, root_limit, evaluator_instructions, run_env, fixture,
                            native_only=native_only, argv=tuple(argv or ()) if takes_args else None)
            argv = None
            schedule = None
            root_limit = 64 << 20
            evaluator_instructions = 1_000_000_000
            native_only = None
            stdout = stderr = b""
            first_write = None
            host_op = 0
            engine_fact = None
            if not filters or any(matches_filter(root, case, selector) for selector in filters):
                contracts.append(case)
    if not contracts:
        raise ValueError(f'no expect: run tests match {filters!r}')
    return contracts


def host_parameter(case):
    if case.argv is not None:
        return 'native_args:Args' + (',native_env:Env' if '(args, env' in case.call else ''), ''
    for pattern, parameters in (
        ('(https)', 'native_net:Net,native_clock:Clock,token:Secret,native_out:Stdout,native_err:Stderr'),
        ('(network)', 'native_net:Net,native_clock:Clock,native_tasks:Tasks'),
        ('(unix, dir, tasks)', 'native_net:Net,native_dir:Dir,native_tasks:Tasks'),
        ('(exec, tasks)', 'native_tasks:Tasks,native_exec:Exec'),
        ('(granted, tasks)', 'native_tasks:Tasks,native_granted:Secret'),
        ('(dir, tasks)', 'native_dir:Dir,native_tasks:Tasks'),
        ('(clock, tasks)', 'native_clock:Clock,native_tasks:Tasks'),
    ):
        if pattern in case.call:
            return parameters, ''
    for argument, kind in [('dir', 'Dir'), ('out', 'Stdout'), ('err', 'Stderr'), ('clock', 'Clock'), ('tasks', 'Tasks'), ('env', 'Env')]:
        if '(' + argument + ')' in case.call:
            name = 'native_' + argument
            return name + ':' + kind, name
    return '', ''


# The bodies whose arms decide on the bucket index of a Map. A map places each
# key by `hash`, `place` or `strong`, which the runtime keys per process from
# getrandom (`map_hash_key`), so which groups a walk visits, whether a group
# has a match or an empty lane, and whether a 7-bit tag match is the key are
# not functions of the program. Their entries are still compared, except
# more_groups', which a walk calls only past a group with no empty lane. The
# Crowded path (probe_staged and put_absent arms, commit_crowded, promote)
# depends on the seed only when eight groups are full, and stays compared.
SEEDED_ARMS = ('core@1/map/probe@', 'core@1/map/vacancy@', 'core@1/map/reprobe_crowded@', 'core@1/map/more_groups@',
               'core@1/map/more_groups#e')


def reached_sites(profile_path):
    """Each site key of a written coverage profile, with whether a run reached
    it, less the arms whose reach depends on the map hash seed."""
    profile = json.loads(profile_path.read_text())
    return {row['site']: bool(row['passed_runs'] or row['failed_runs']) for row in profile['sites']
            if not row['site'].startswith(SEEDED_ARMS)}


METER_REPEATS = 3


def check_metering(run, *, first=None, reference=None):
    samples = [run(None) if first is None else first]
    samples.extend(run(None) for _ in range(METER_REPEATS - 1))
    floor = samples[0]['step_floor'] if reference is None else reference['floor']
    counts = [sample['steps'] for sample in samples]
    if len(set(counts)) != 1:
        raise ValueError(f'step count differs within one build: {counts}')
    if counts[0] < floor:
        raise ValueError(f'{counts[0]} steps below the MEM-30 floor of {floor}')
    used = max(sample['used'] for sample in samples)
    if reference is not None and used > reference['used']:
        raise ValueError(f"{used} native bytes exceed {reference['used']} evaluator bytes")
    sites = []
    if counts[0]:
        limit = counts[0] // 2
        refused = [run(limit) for _ in range(METER_REPEATS)]
        if any(sample['steps'] > limit for sample in refused):
            raise ValueError(f'injected step limit of {limit} was exceeded')
        sites = [sample['refusal_site'] for sample in refused]
    if sites and (not sites[0] or any(site != sites[0] for site in sites[1:])):
        raise ValueError(f'injected step limit did not consistently refuse at one source point: {sites}')
    return dict(steps=counts, floor=floor, used=used, refusal_sites=sites)


def evaluator_meter(resident, inputs, entry, case, progress, observation):
    def run(limit):
        result = resident.evaluate(inputs, entry, instructions=case.evaluator_instructions,
                                   progress=progress, meter=True, meter_limit=limit, root_limit=case.root_limit)
        report = json.loads(result.stdout)
        if limit is None:
            _, cause = observation(case, result)
            if cause:
                raise ValueError(cause)
        return report

    return check_metering(run)


def native_meter(invoke, validate, environment, report_path, initial, reference):
    def run(limit):
        report_path.unlink(missing_ok=True)
        env = environment.copy()
        if limit is not None:
            env[b'HIRSEL_TEST_METER_LIMIT'] = str(limit).encode()
        result = invoke(env)
        if limit is None and (cause := validate(result)):
            raise ValueError(cause)
        if not report_path.exists():
            raise ValueError('native runtime produced no step report (use the testing runtime archive)')
        return json.loads(report_path.read_text())

    return check_metering(run, first=json.loads(initial), reference=reference)
