"""Build and check the distribution bundle natively on an Apple Silicon Mac.

    tools/toolchain-dist/build --host aarch64-apple-darwin --output DIR --runtime-archives DIR [--work DIR]
    tools/toolchain-dist/build --host aarch64-apple-darwin --check-fresh PREFIX

The Mac builds the compiler and Exec helper with Cargo and packages:

- the compiler, Cargo's `dist` profile against the pinned LLVM tree
  (tools/buck2/bootstrap_llvm.py), whose ld.lld and libLLVM.23.1.dylib ship;
- the Darwin runtime archives exported from the Linux bundle;
- the Darwin link kit, the text stubs link_kit.py makes from Hirsel's allowlists
  and the pinned Zig libSystem.tbd;
- the core and std images, the toolchain packages and the helpers, installed by
  the bundle's own compiler (toolchain_programs.py).

Every Mach-O is built for the floor in the macOS target record and carries an
ad-hoc signature. The check reads every
shipped Mach-O's load commands by file class (macho_check), runs
`codesign -v --strict` on it, then installs the bundle into an empty prefix and
runs the fresh-machine sequence under `env -i`.
"""
import hashlib
import json
import os
from pathlib import Path
import platform
import re
import shlex
import shutil
import struct
import subprocess
import sys
import tempfile
import tomllib
import urllib.request

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / 'tools/buck2/native-toolchain'))
sys.path.insert(0, str(ROOT / 'tools/buck2'))
from apple_records import AppleRecords, SOURCE  # noqa: E402
import link_kit  # noqa: E402

APPLE = AppleRecords((ROOT / SOURCE).read_text()).rows['macos']
TARGET = APPLE['triple']
FLOOR = APPLE['floor']
PRODUCT = tomllib.loads(Path(__file__).with_name('product.toml').read_text())
SYSTEM = {name: path for name, path in link_kit.DARWIN_LIBRARIES.items()}
LIBSYSTEM = SYSTEM['libSystem']
LIBCXX = '/usr/lib/libc++.1.dylib'
LIBZ = '/usr/lib/libz.1.dylib'
LIBICONV = '/usr/lib/libiconv.2.dylib'
LIBLLVM = PRODUCT['hosts'][TARGET]['llvm_library']
LIBLLVM_ID = '@rpath/' + Path(LIBLLVM).name


def run(command, **options):
    print('dist:', ' '.join(map(str, command)), flush=True)
    subprocess.run(command, check=True, **options)


def require_host():
    if platform.system() != 'Darwin' or platform.machine() != 'arm64':
        raise SystemExit(f'--host {TARGET} runs on an Apple Silicon Mac')


# --- Mach-O load commands -------------------------------------------------

MH_MAGIC_64 = 0xfeedfacf
MACHO_MAGICS = {b'\xcf\xfa\xed\xfe', b'\xce\xfa\xed\xfe', b'\xfe\xed\xfa\xcf', b'\xfe\xed\xfa\xce',
                b'\xca\xfe\xba\xbe', b'\xbe\xba\xfe\xca'}
CPU_ARM64, CPU_SUBTYPE_ARM64_ALL = 0x0100000c, 0
MH_OBJECT, MH_EXECUTE, MH_DYLIB, MH_DSYM = 1, 2, 6, 10
LC_SEGMENT_64, LC_UUID, LC_CODE_SIGNATURE = 0x19, 0x1b, 0x1d
LC_ID_DYLIB, LC_BUILD_VERSION, LC_VERSION_MIN_MACOSX = 0xd, 0x32, 0x24
LC_RPATH = 0x8000001c
DYLIB_LOADS = {0xc: 'load', 0x80000018: 'weak', 0x8000001f: 'reexport', 0x20: 'lazy', 0x80000023: 'upward'}
S_MOD_INIT_FUNC_POINTERS, S_INIT_FUNC_OFFSETS = 0x9, 0x16
VM_PROT_WRITE, VM_PROT_EXECUTE = 2, 4


def is_macho(path):
    with open(path, 'rb') as stream:
        return stream.read(4) in MACHO_MAGICS


def load_commands(data):
    """The facts the structure rules read from one thin arm64 Mach-O."""
    magic, cpu, subtype, filetype, count, _, _, _ = struct.unpack_from('<IiiIIIII', data, 0)
    if magic != MH_MAGIC_64 or cpu != CPU_ARM64 or subtype & 0xffffff != CPU_SUBTYPE_ARM64_ALL:
        raise ValueError('not a thin little-endian arm64 (ALL) Mach-O')
    facts = dict(filetype=filetype, dylibs=[], rpaths=[], id=None, minos=None, uuid=None,
                 signature=False, writable_code=[], init=[], version_min=False)
    def string(offset, base, size):
        return data[base + offset:base + size].split(b'\0', 1)[0].decode()
    at = 32
    for _ in range(count):
        command, size = struct.unpack_from('<II', data, at)
        if command == LC_SEGMENT_64:
            name = data[at + 8:at + 24].rstrip(b'\0').decode()
            maxprot, _, sections, _ = struct.unpack_from('<iiII', data, at + 56)
            if maxprot & VM_PROT_WRITE and maxprot & VM_PROT_EXECUTE:
                facts['writable_code'].append(name)
            for index in range(sections):
                section = at + 72 + 80 * index
                sectname = data[section:section + 16].rstrip(b'\0').decode()
                length, = struct.unpack_from('<Q', data, section + 40)
                flags, = struct.unpack_from('<I', data, section + 64)
                if (flags & 0xff in (S_MOD_INIT_FUNC_POINTERS, S_INIT_FUNC_OFFSETS)
                        or sectname in ('__mod_init_func', '__init_offsets')) and length:
                    facts['init'].append(f'{name},{sectname}')
        elif command in DYLIB_LOADS:
            facts['dylibs'].append(string(struct.unpack_from('<I', data, at + 8)[0], at, size))
        elif command == LC_ID_DYLIB:
            facts['id'] = string(struct.unpack_from('<I', data, at + 8)[0], at, size)
        elif command == LC_RPATH:
            facts['rpaths'].append(string(struct.unpack_from('<I', data, at + 8)[0], at, size))
        elif command == LC_BUILD_VERSION:
            platform_id, minos = struct.unpack_from('<II', data, at + 8)
            if platform_id == APPLE['macho_platform']:
                facts['minos'] = f'{minos >> 16}.{minos >> 8 & 0xff}'
                if minos & 0xff:
                    facts['minos'] += f'.{minos & 0xff}'
        elif command == LC_VERSION_MIN_MACOSX:
            facts['version_min'] = True
        elif command == LC_UUID:
            facts['uuid'] = data[at + 8:at + 24]
        elif command == LC_CODE_SIGNATURE:
            facts['signature'] = struct.unpack_from('<I', data, at + 12)[0] != 0
        at += size
    return facts


DSYM = re.compile(r'(?P<program>(?:libexec/hirsel|witness)/[^/]+)\.dSYM/Contents/Resources/DWARF/[^/]+')
STD_OBJECT = re.compile(r'core-packages/[0-9a-f]{64}/native/o/[0-9a-f]{2}/[0-9a-f]+')


def file_class(relative):
    """The structure rules of a bundle path. A Mach-O outside these is refused.
    An image is signed and built for the floor; `signed=False` marks a file the
    loader never maps."""
    if STD_OBJECT.fullmatch(relative):
        # The std's native object cache (std-product, and the helpers' builds):
        # unlinked objects, whose platform version the final link sets.
        return dict(filetype=MH_OBJECT, dylibs=set(), rpaths=[], init=None, signed=False)
    if match := DSYM.fullmatch(relative):
        # A program's debug companion, which `hirsel build` writes beside it.
        return dict(filetype=MH_DSYM, dylibs=set(), rpaths=[], init=None, signed=False,
                    uuid_of=match['program'])
    if relative == 'hirsel':
        # Rust's std links libiconv on Apple targets; the compiler uses no name from it.
        return dict(filetype=MH_EXECUTE, dylibs={LIBSYSTEM, SYSTEM['CoreFoundation'], SYSTEM['Security'], LIBICONV},
                    rpaths=[], init=None)
    if re.fullmatch(rf'runtime/{re.escape(TARGET)}/[0-9a-f]{{64}}/hirsel-exec-helper', relative):
        # Signed nested code. A sandboxed app signs its own copy with the App
        # Sandbox inherit entitlement (EMB-3).
        return dict(filetype=MH_EXECUTE, dylibs={*SYSTEM.values(), LIBICONV},
                    subset=True, rpaths=[], init=None)
    if relative == 'bin/ld.lld':
        return dict(filetype=MH_EXECUTE, dylibs={LIBSYSTEM, LIBCXX, LIBZ, LIBLLVM_ID},
                    rpaths=['@loader_path/../lib'], init=None)
    if relative in {'bin/llvm-ar', 'bin/llvm-objcopy'}:
        return dict(filetype=MH_EXECUTE, dylibs={LIBSYSTEM, LIBCXX, LIBLLVM_ID},
                    rpaths=['@loader_path/../lib'], init=None)
    if relative == LIBLLVM:
        return dict(filetype=MH_DYLIB, dylibs={LIBSYSTEM, LIBCXX, LIBZ}, rpaths=[], init=None, id=LIBLLVM_ID)
    if relative.startswith('libexec/hirsel/') or relative.startswith('witness/'):
        # A program the bundle's compiler links: the link policy of
        # hirsel-core-driver's native_debug::macho_structure (the kit's system
        # libraries only, no search path, no initializers), which the driver's
        # laws hold for the images they link. The bundle's other files have
        # their own rules, so this check reads every class here.
        return dict(filetype=MH_EXECUTE, dylibs=set(SYSTEM.values()), subset=True, rpaths=[], init=[])
    return None


def macho_check(root, relatives):
    """Check each Mach-O by its class; return the failures."""
    failures = []
    for relative in relatives:
        rules = file_class(relative)
        if rules is None:
            failures.append(f'{relative}: Mach-O of no shipped class')
            continue
        try:
            facts = load_commands((root / relative).read_bytes())
        except (ValueError, struct.error, UnicodeDecodeError) as error:
            failures.append(f'{relative}: {error}')
            continue
        problems = []
        image = rules.get('signed', True)
        if facts['filetype'] != rules['filetype']:
            problems.append(f"file type {facts['filetype']}")
        if facts['version_min'] or image and facts['minos'] != FLOOR:
            problems.append(f"minos {facts['minos']}, not the floor {FLOOR}")
        if image and not facts['uuid']:
            problems.append('no LC_UUID')
        if image and not facts['signature']:
            problems.append('no LC_CODE_SIGNATURE')
        if image and facts['writable_code']:
            problems.append(f"W+X segments {facts['writable_code']}")
        if 'uuid_of' in rules:
            program = root / rules['uuid_of']
            if not program.is_file() or facts['uuid'] is None or load_commands(program.read_bytes())['uuid'] != facts['uuid']:
                problems.append(f"UUID is not {rules['uuid_of']}'s")
        dylibs = set(facts['dylibs'])
        if len(dylibs) != len(facts['dylibs']):
            problems.append(f"repeated dylibs {facts['dylibs']}")
        if rules.get('subset') and not dylibs <= rules['dylibs']:
            problems.append(f"dylibs off the kit {sorted(dylibs - rules['dylibs'])}")
        if not rules.get('subset') and dylibs != rules['dylibs']:
            problems.append(f"dylibs {sorted(dylibs)}, expected {sorted(rules['dylibs'])}")
        if facts['rpaths'] != rules['rpaths']:
            problems.append(f"LC_RPATH {facts['rpaths']}, expected {rules['rpaths']}")
        if rules['init'] is not None and facts['init'] != rules['init']:
            problems.append(f"initializer sections {facts['init']}")
        if facts['id'] != rules.get('id'):
            problems.append(f"LC_ID_DYLIB {facts['id']}")
        failures.extend(f'{relative}: {problem}' for problem in problems)
        if rules['filetype'] == MH_OBJECT and not problems:
            continue
        print(f"dist: {relative}: {'FAIL' if problems else 'ok'} minos {facts['minos']}, "
              f"dylibs {sorted(dylibs)}, rpaths {facts['rpaths']}, init {facts['init']}", flush=True)
    return failures


def check_signatures(root, relatives):
    failures = []
    for relative in relatives:
        checked = subprocess.run(['/usr/bin/codesign', '-v', '--strict', root / relative],
                                 capture_output=True, text=True)
        if checked.returncode:
            failures.append(f'{relative}: codesign -v --strict: {checked.stderr.strip()}')
        signed = subprocess.run(['/usr/bin/codesign', '-dv', root / relative], capture_output=True, text=True)
        if 'Signature=adhoc' not in signed.stderr:
            failures.append(f'{relative}: not ad-hoc signed')
    return failures


def check_machos(root, relatives):
    images = [relative for relative in relatives if (file_class(relative) or {}).get('signed', True)]
    failures = macho_check(root, relatives) + check_signatures(root, images)
    if failures:
        raise SystemExit('\n'.join(failures))
    print(f'dist: {len(relatives)} Mach-O files passed their classes, {len(images)} images codesign -v --strict',
          flush=True)


def shipped_machos(prefix):
    return [path.relative_to(prefix).as_posix() for path in sorted(prefix.rglob('*'))
            if path.is_file() and not path.is_symlink() and is_macho(path)]


# --- Build ------------------------------------------------------------------

def cargo_env(work, llvm):
    return {**os.environ, APPLE['deployment_env']: FLOOR,
            'LLVM_SYS_231_PREFIX': str(llvm), 'CARGO_TARGET_DIR': str(work / 'target'),
            'SOURCE_DATE_EPOCH': '0'}


def zig_libsystem(work):
    pin = json.loads((ROOT / 'tools/native-toolchain/native.lock.json').read_text())['link_kit']['libSystem-tbd']
    cache_dir = work / 'cache'
    cached = cache_dir / f"{pin['sha256']}.libSystem.tbd"
    if cached.is_file():
        data = cached.read_bytes()
        if len(data) == pin['size'] and hashlib.sha256(data).hexdigest() == pin['sha256']:
            return data.decode()
    with urllib.request.urlopen(urllib.request.Request(pin['url'], headers={'User-Agent': 'hirsel-dist'}),
                                timeout=120) as response:
        data = response.read()
    if len(data) != pin['size'] or hashlib.sha256(data).hexdigest() != pin['sha256']:
        raise SystemExit(f"{pin['url']}: size or SHA-256 differs from native.lock.json")
    cache_dir.mkdir(parents=True, exist_ok=True)
    tmp = cached.with_suffix('.tmp')
    tmp.write_bytes(data)
    tmp.replace(cached)
    return data.decode()


def link_kit_tree(work):
    kit = work / 'kit'
    if kit.exists():
        shutil.rmtree(kit)
    kit.mkdir(parents=True)
    allowlists = {library: ROOT / f'tools/native-toolchain/darwin/{library}.symbols'
                  for library in link_kit.DARWIN_LIBRARIES}
    row = link_kit.darwin_kit(kit, allowlists, zig_libsystem(work), APPLE)
    link_kit.write_manifest(kit, {'targets': {TARGET: row}})
    return kit


_RELINK_WRAPPER = """#!{python}
import json
import os
from pathlib import Path
import shlex
import shutil
import subprocess
import sys

relink = Path(os.environ['HIRSEL_RELINK_DIR']).resolve()
raw_tmp = os.environ['HIRSEL_RUSTC_TMP']
tmp = Path(raw_tmp).resolve()
args = sys.argv[1:]
saved_tmp = relink / 'tmp'
saved_objs = relink / 'objs'
if saved_tmp.exists():
    shutil.rmtree(saved_tmp)
if saved_objs.exists():
    shutil.rmtree(saved_objs)
if tmp.exists():
    shutil.copytree(tmp, saved_tmp)
else:
    saved_tmp.mkdir(parents=True)
saved_objs.mkdir(parents=True)

tmp_prefixes = tuple(dict.fromkeys([str(tmp), raw_tmp]))
saved_tmp_str = str(saved_tmp)
copied = {{}}


def rewrite_tmp(text):
    for prefix in tmp_prefixes:
        if prefix in text:
            text = text.replace(prefix, saved_tmp_str)
    return text


def preserve(path_text):
    rewritten = rewrite_tmp(path_text)
    if rewritten != path_text:
        return rewritten
    if path_text in copied:
        return copied[path_text]
    candidate = Path(path_text)
    if candidate.is_file() and (
        candidate.suffix in ('.o', '.a', '.rlib')
        or any(part.startswith('rustc') for part in candidate.parts)
    ):
        target = saved_objs / f'{{len(copied)}}-' / candidate.name
        target.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(candidate, target)
        copied[path_text] = str(target)
        return copied[path_text]
    return path_text


def save_aux_file(path):
    rewritten = rewrite_tmp(str(path))
    if rewritten != str(path):
        return Path(rewritten)
    key = str(path)
    if key in copied:
        return Path(copied[key])
    saved = saved_objs / f'aux-{{len(copied)}}-' / path.name
    saved.parent.mkdir(parents=True, exist_ok=True)
    shutil.copy2(path, saved)
    copied[key] = str(saved)
    return saved


def rewrite_filelist(path):
    lines = path.read_text(encoding='utf-8').splitlines()
    path.write_text('\\n'.join(preserve(line) for line in lines) + ('\\n' if lines else ''), encoding='utf-8')


def rewrite_response_file(path):
    raw = path.read_text(encoding='utf-8')
    tokens = shlex.split(raw)
    rewritten = []
    index = 0
    while index < len(tokens):
        token = tokens[index]
        if token == '-filelist' and index + 1 < len(tokens):
            saved = save_aux_file(Path(tokens[index + 1]))
            if saved.is_file():
                rewrite_filelist(saved)
            rewritten.extend([token, str(saved)])
            index += 2
            continue
        if token.startswith('-Wl,-filelist,'):
            saved = save_aux_file(Path(token.split(',', 2)[2]))
            if saved.is_file():
                rewrite_filelist(saved)
            rewritten.append('-Wl,-filelist,' + str(saved))
            index += 1
            continue
        rewritten.append(preserve(token))
        index += 1
    path.write_text('\\n'.join(shlex.quote(token) for token in rewritten) + '\\n', encoding='utf-8')


rewritten_args = []
for arg in args:
    if arg.startswith('@'):
        saved_resp = save_aux_file(Path(arg[1:]))
        if saved_resp.is_file():
            rewrite_response_file(saved_resp)
        rewritten_args.append('@' + str(saved_resp))
    elif arg.startswith('-Wl,@'):
        saved_resp = save_aux_file(Path(arg[5:]))
        if saved_resp.is_file():
            rewrite_response_file(saved_resp)
        rewritten_args.append('-Wl,@' + str(saved_resp))
    elif arg.startswith('-Wl,-filelist,'):
        saved_list = save_aux_file(Path(arg.split(',', 2)[2]))
        if saved_list.is_file():
            rewrite_filelist(saved_list)
        rewritten_args.append('-Wl,-filelist,' + str(saved_list))
    else:
        rewritten_args.append(preserve(arg))

(relink / 'link.json').write_text(
    json.dumps({{'argv': rewritten_args, 'cwd': os.getcwd()}}, indent=2) + '\\n',
    encoding='utf-8',
)
raise SystemExit(subprocess.run(['cc', *args]).returncode)
"""


def _replace_output_arg(argv, destination):
    """Replace `-o <path>` in argv (or inside an @response file) and return the new argv."""
    result = list(argv)
    for index, token in enumerate(result):
        if token == '-o' and index + 1 < len(result):
            result[index + 1] = str(destination)
            return result
        if token.startswith('-o') and len(token) > 2:
            result[index] = '-o' + str(destination)
            return result
    for index, token in enumerate(result):
        if not token.startswith('@'):
            continue
        resp = Path(token[1:])
        tokens = shlex.split(resp.read_text(encoding='utf-8'))
        for inner, item in enumerate(tokens):
            if item == '-o' and inner + 1 < len(tokens):
                tokens[inner + 1] = str(destination)
                patched = resp.with_name(resp.name + '.relink')
                patched.write_text('\n'.join(shlex.quote(t) for t in tokens) + '\n', encoding='utf-8')
                result[index] = '@' + str(patched)
                return result
    raise ValueError('captured linker command had no -o output argument')


def _relink_with_host(relink_dir, host, destination, env):
    record = json.loads((relink_dir / 'link.json').read_text(encoding='utf-8'))
    argv = _replace_output_arg(record['argv'], destination)
    argv.extend(['-Wl,-sectcreate,__TEXT,__hirsel_host', '-Xlinker', str(host)])
    run(['cc', *argv], cwd=record['cwd'], env=env)
    if not destination.is_file():
        raise RuntimeError(f'relink did not produce {destination}')


def build(output, work, release_tag, archives):
    require_host()
    work.mkdir(parents=True, exist_ok=True)
    llvm = work / 'llvm'
    run([sys.executable, ROOT / 'tools/buck2/bootstrap_llvm.py', '--output', llvm], cwd=ROOT)
    env = dict(cargo_env(work, llvm), HIRSEL_RELEASE_VERSION=release_tag)
    driver = ['--locked', '--profile', 'dist', '-p', 'hirsel-core-driver', '--bin', 'hirsel']
    relink_dir = (work / 'relink').resolve()
    rustc_tmp = (work / 'rustc-tmp').resolve()
    for path in (relink_dir, rustc_tmp):
        if path.exists():
            shutil.rmtree(path)
        path.mkdir(parents=True)
    wrapper = relink_dir / 'hirsel-cc-wrapper'
    wrapper.write_text(_RELINK_WRAPPER.format(python=sys.executable), encoding='utf-8')
    wrapper.chmod(0o755)
    driver_env = dict(env, TMPDIR=str(rustc_tmp), HIRSEL_RELINK_DIR=str(relink_dir),
                      HIRSEL_RUSTC_TMP=str(rustc_tmp))
    run(['cargo', 'rustc', *driver, '--', '-C', f'linker={wrapper}'], cwd=ROOT, env=driver_env)
    helper = 'hirsel-exec-helper'
    run(['cargo', 'build', '--locked', '--profile', 'dist', '--target', TARGET,
         '--jobs', '8', '-p', helper, '--bin', helper], cwd=ROOT, env=env)
    helper_image = work / 'target' / TARGET / 'dist' / helper
    run(['/usr/bin/codesign', '--force', '--sign', '-', helper_image])
    kit = link_kit_tree(work)
    compiler = work / 'target/dist/hirsel'
    scratch = work / 'install'
    if scratch.exists():
        shutil.rmtree(scratch)
    scratch.mkdir(parents=True)
    jobs = os.environ.get('HIRSEL_JOBS') or str(os.cpu_count() or 1)
    install = dict(env, TMPDIR=str(scratch), XDG_CACHE_HOME=str(scratch / 'cache'), HIRSEL_JOBS=jobs)
    run([compiler, 'core-product', '--install', output], env=install, stdout=subprocess.DEVNULL)
    run([compiler, 'std-product', '--install', output,
         *[option for slot, archive in archives.items() for option in (f'--{slot}', archive)],
         '--target', TARGET, '--link-kit', kit, '--exec-helper', helper_image],
        env=install, stdout=subprocess.DEVNULL)
    programs = ROOT / 'tools/toolchain-dist/toolchain_programs.py'
    packages = tomllib.loads((ROOT / 'core/programs/toolchain-packages.toml').read_text())['packages']
    run([sys.executable, programs, 'pack', compiler, ROOT / 'core/programs', output / 'core-packages', scratch / 'packs',
         *packages], env=install)
    host = scratch / 'platform_main'
    run([sys.executable, programs, 'helpers', Path(__file__).with_name('product.toml'),
         output, ROOT / 'core/programs/platform', scratch, host], env=install)
    run(['/usr/bin/codesign', '-v', '--strict', host])
    # Embed the platform host into the signed compiler image. Reuse the
    # captured link inputs from the first pass to avoid a second fat-LTO pass.
    if (relink_dir / 'link.json').is_file():
        relinked = relink_dir / 'hirsel'
        _relink_with_host(relink_dir, host, relinked, env)
        shutil.copy2(relinked, output / 'hirsel')
    else:
        run(['cargo', 'rustc', *driver, '--', '-C', 'link-arg=-Wl,-sectcreate,__TEXT,__hirsel_host',
             '-C', 'link-arg=-Xlinker', '-C', f'link-arg={host}'], cwd=ROOT, env=env)
        shutil.copy2(compiler, output / 'hirsel')
    shutil.rmtree(relink_dir, ignore_errors=True)
    shutil.rmtree(rustc_tmp, ignore_errors=True)
    return llvm


# --- Check ------------------------------------------------------------------

def check(prefix, sequence=None, accept=None, relocation=None):
    """Structure-check and signature-check every shipped Mach-O, then, given the
    fresh-machine sequence, run it in an empty prefix the bundle is unpacked into."""
    require_host()
    check_machos(prefix, shipped_machos(prefix))
    print('dist: every shipped Mach-O passed its structure and codesign checks', flush=True)
    if sequence is None:
        return
    with tempfile.TemporaryDirectory(prefix='hirsel-fresh-') as raw:
        scratch = Path(raw)
        bundle = scratch / 'bundle.tar'
        run(['/usr/bin/tar', '-cf', bundle, '-C', prefix, '.'])
        fresh = scratch / 'prefix'
        fresh.mkdir()
        run(['/usr/bin/tar', '-xf', bundle, '-C', fresh])
        bundle.unlink()
        work = scratch / 'work'
        work.mkdir()
        # The prefix is read-only, as the Linux witness binds it.
        run(['/bin/chmod', '-R', 'a-w', fresh])
        try:
            env = dict(PATH=f'{fresh}:/usr/bin:/bin', HOME=str(work), TMPDIR=str(work),
                       XDG_CACHE_HOME=str(work / 'cache'))
            passed = subprocess.run(['/usr/bin/env', '-i', *[f'{name}={value}' for name, value in env.items()],
                                     '/bin/sh', '-c', sequence], cwd=work, capture_output=True, text=True)
            accept(passed, fresh)
            # Each relocation build also runs with exactly `env`, no inherited variable.
            built = relocation(fresh / 'hirsel', work, env)
            outputs = work / 'witness'
            outputs.mkdir()
            for path in [work / 'demo/program', *built]:
                shutil.copyfile(path, outputs / path.name)
            check_machos(work, [f'witness/{path.name}' for path in sorted(outputs.iterdir())])
        finally:
            run(['/bin/chmod', '-R', 'u+w', fresh])
    print('dist: Darwin fresh-root acceptance passed', flush=True)
