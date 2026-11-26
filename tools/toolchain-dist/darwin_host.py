"""Build and check the distribution bundle natively on an Apple Silicon Mac.

    tools/toolchain-dist/build --host aarch64-apple-darwin --output DIR [--work DIR]
    tools/toolchain-dist/build --host aarch64-apple-darwin --check-fresh PREFIX

Buck2 does not run on macOS, so a Mac builds with Cargo what Buck2 builds on
Linux, from the same pins and flags:

- the compiler, Cargo's `dist` profile against the pinned LLVM tree
  (tools/buck2/bootstrap_llvm.py), whose ld.lld and libLLVM.23.1.dylib ship;
- the six runtime archives, each a fat-LTO staticlib of hirsel-core-rt against
  the trace-only std (tools/buck2/toolchains/raw_backtrace_std.py), with the
  runtime profile of tools/buck2/profile.bzl;
- the Darwin link kit, the text stubs link_kit.py makes from Hirsel's allowlists
  and the pinned Zig libSystem.tbd;
- the core and std images, the toolchain packages and the helpers, installed by
  the bundle's own compiler (toolchain-programs.sh).

Every Mach-O is built for the macOS floor, MACOSX_DEPLOYMENT_TARGET taken from
link_kit.DARWIN_FLOOR, and carries an ad-hoc signature. The check reads every
shipped Mach-O's load commands by file class (macho_check), runs
`codesign -v --strict` on it, then installs the bundle into an empty prefix and
runs the fresh-machine sequence under `env -i`.
"""
import ast
import hashlib
import json
import os
from pathlib import Path
import platform
import re
import shutil
import struct
import subprocess
import sys
import tarfile
import tempfile
import tomllib
import urllib.request

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / 'tools/buck2/native-toolchain'))
import link_kit  # noqa: E402

TARGET = link_kit.DARWIN
FLOOR = link_kit.DARWIN_FLOOR
# std-product's runtime slots and the hirsel-core-rt features of each
# (tools/buck2/sync.py). std-product checks each archive's features by symbol.
RUNTIMES = {
    'runtime': ['standalone'],
    'recording-runtime': ['standalone', 'record'],
    'prof-runtime': ['standalone', 'prof'],
    'prof-recording-runtime': ['standalone', 'prof', 'record'],
    'observed-runtime': ['standalone', 'charge-observation'],
    'observed-recording-runtime': ['standalone', 'charge-observation', 'record'],
}
SYSTEM = {name: path for name, path in link_kit.DARWIN_LIBRARIES.items()}
LIBSYSTEM = SYSTEM['libSystem']
LIBCXX = '/usr/lib/libc++.1.dylib'
LIBZ = '/usr/lib/libz.1.dylib'
LIBICONV = '/usr/lib/libiconv.2.dylib'
LIBLLVM = 'lib/libLLVM.23.1.dylib'
LIBLLVM_ID = '@rpath/libLLVM.23.1.dylib'


def bzl_list(path, name):
    """A list constant of a Starlark file, which Python cannot import."""
    match = re.search(rf'^{name} = (\[.*?\])$', path.read_text(), re.M | re.S)
    return ast.literal_eval(match[1])


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
PLATFORM_MACOS = 1
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
            if platform_id == PLATFORM_MACOS:
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
    if relative == 'bin/ld.lld':
        return dict(filetype=MH_EXECUTE, dylibs={LIBSYSTEM, LIBCXX, LIBZ, LIBLLVM_ID},
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
    return dict(os.environ, MACOSX_DEPLOYMENT_TARGET=FLOOR, LLVM_SYS_231_PREFIX=str(llvm),
                CARGO_TARGET_DIR=str(work / 'target'), SOURCE_DATE_EPOCH='0')


def trace_sysroot(work, env):
    """Rustup's pinned sysroot with the trace-only std in place of its libstd."""
    run(['rustup', 'component', 'add', 'rust-src'], cwd=ROOT, env=env)
    sysroot = Path(subprocess.check_output(['rustc', '--print', 'sysroot'], cwd=ROOT, env=env, text=True).strip())
    libraries = Path('lib/rustlib') / TARGET / 'lib'
    tree = work / 'sysroot'
    (tree / libraries).mkdir(parents=True)
    for library in (sysroot / libraries).iterdir():
        if not library.name.startswith('libstd-'):
            (tree / libraries / library.name).symlink_to(library)
    run([sys.executable, ROOT / 'tools/buck2/toolchains/raw_backtrace_std.py', sysroot,
         sysroot / 'lib/rustlib/src/rust/library', ROOT / 'tools/buck2/toolchains/panic_image_darwin.rs',
         TARGET, tree / libraries / 'libstd-hirsel-trace.rlib'], cwd=ROOT, env=env)
    return tree


def runtimes(work, env):
    """The six runtime archives, with tools/buck2/profile.bzl's runtime flags."""
    sysroot = trace_sysroot(work, env)
    profile = 'profile.hirsel-runtime'
    settings = [f'{profile}.inherits="release"', f'{profile}.opt-level=3', f'{profile}.debug=0',
                f'{profile}.debug-assertions=false', f'{profile}.overflow-checks=false',
                f'{profile}.codegen-units=1', f'{profile}.lto="fat"', f'{profile}.panic="abort"']
    settings += [f'{profile}.package.{crate}.opt-level="s"'
                 for crate in bzl_list(ROOT / 'tools/buck2/profile.bzl', 'COLD_RUNTIME_CRATES')]
    env = dict(env, RUSTC_BOOTSTRAP='1',
               RUSTFLAGS=f'--sysroot {sysroot} -Cembed-bitcode=yes -Crelocation-model=pic')
    archives = {}
    for slot, features in RUNTIMES.items():
        run(['cargo', 'rustc', '--locked', '--lib', '-p', 'hirsel-core-rt', '--target', TARGET,
             '--profile', 'hirsel-runtime', '--crate-type', 'staticlib', '--features', ','.join(features),
             *[option for setting in settings for option in ('--config', setting)],
             '--', '--cfg', 'hirsel_runtime_size', '-Zcrate-attr=feature(optimize_attribute)'],
            cwd=ROOT, env=env)
        archive = work / 'runtimes' / slot / 'libhirsel_core_rt.a'
        archive.parent.mkdir(parents=True)
        shutil.copyfile(work / 'target' / TARGET / 'hirsel-runtime/libhirsel_core_rt.a', archive)
        run(['/usr/bin/strip', '-S', archive])
        archives[slot] = archive
    return archives


def zig_libsystem(work):
    pin = json.loads((ROOT / 'tools/native-toolchain/native.lock.json').read_text())['link_kit']['libSystem-tbd']
    with urllib.request.urlopen(urllib.request.Request(pin['url'], headers={'User-Agent': 'hirsel-dist'}),
                                timeout=120) as response:
        data = response.read()
    if len(data) != pin['size'] or hashlib.sha256(data).hexdigest() != pin['sha256']:
        raise SystemExit(f"{pin['url']}: size or SHA-256 differs from native.lock.json")
    return data.decode()


def link_kit_tree(work):
    kit = work / 'kit'
    kit.mkdir()
    allowlists = {library: ROOT / f'tools/native-toolchain/darwin/{library}.symbols'
                  for library in link_kit.DARWIN_LIBRARIES}
    link_kit.write_manifest(kit, {'targets': {TARGET: link_kit.darwin_kit(kit, allowlists, zig_libsystem(work))}})
    return kit


def build(output, work, release_tag):
    require_host()
    work.mkdir(parents=True, exist_ok=True)
    llvm = work / 'llvm'
    run([sys.executable, ROOT / 'tools/buck2/bootstrap_llvm.py', '--output', llvm], cwd=ROOT)
    env = dict(cargo_env(work, llvm), HIRSEL_RELEASE_VERSION=release_tag)
    run(['cargo', 'build', '--locked', '--profile', 'dist', '-p', 'hirsel-core-driver', '--bin', 'hirsel'],
        cwd=ROOT, env=env)
    archives = runtimes(work, env)
    kit = link_kit_tree(work)
    compiler = work / 'target/dist/hirsel'
    scratch = work / 'install'
    scratch.mkdir()
    install = dict(env, TMPDIR=str(scratch), HIRSEL_JOBS='1')
    run([compiler, 'core-product', '--install', output], env=install, stdout=subprocess.DEVNULL)
    run([compiler, 'std-product', '--install', output,
         *[option for slot, archive in archives.items() for option in (f'--{slot}', archive)],
         '--target', TARGET, '--link-kit', kit], env=install, stdout=subprocess.DEVNULL)
    programs = ROOT / 'tools/toolchain-dist/toolchain-programs.sh'
    packages = tomllib.loads((ROOT / 'core/programs/toolchain-packages.toml').read_text())['packages']
    run(['/bin/sh', programs, 'pack', compiler, ROOT / 'core/programs', output / 'core-packages', scratch / 'packs',
         *packages], env=install)
    run(['/bin/sh', programs, 'helpers', output, ROOT / 'core/programs/platform', scratch], env=install)
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
        with tarfile.open(bundle, 'w') as archive:
            archive.add(prefix, arcname='.')
        fresh = scratch / 'prefix'
        fresh.mkdir()
        with tarfile.open(bundle) as archive:
            archive.extractall(fresh, filter='tar')
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
