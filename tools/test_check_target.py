"""Seeded sources for the target check. Run with python3 tools/test_check_target.py."""

import unittest

import check_target


def rules(source, path='crates/hirsel-llvm/src/core/x.rs'):
    return sorted(check_target.source_violations(path, source))


def floor_texts(link='26.0', buck='26.0', driver='26 << 16', installer='26.0'):
    return {
        check_target.LINK_KIT: f'DARWIN_FLOOR = "{link}"\n',
        check_target.TOOLCHAIN_BUCK: f'DARWIN_FLOOR = "{buck}"\n',
        check_target.DRIVER_MACHO: f'const MACOS_FLOOR: u32 = {driver};\n',
        check_target.INSTALL_SH: f'macos_floor={installer}\n',
    }


class TargetDecisions(unittest.TestCase):
    def test_hidden_target_decisions_are_red(self):
        red = {
            'equality': 'fn f(t: Target) -> bool { t == Target::Aarch64Linux }',
            'qualified fact': 'fn f(t: Target) -> bool {\n    t.os() != hirsel_core_layout::Os::Linux\n}',
            'from_triple': 'let x86 = Target::from_triple(s)\n    == Some(Target::X86_64Linux);',
            'triple string': 'if target == "aarch64-unknown-linux-gnu" { arm() }',
            'matches!': 'let arm = matches!(\n    target,\n    Target::Aarch64Linux | Target::Aarch64Darwin\n);',
            'let else': 'let CAbi::SysV64 = target.c_abi() else { return };',
            'wildcard arm': ('let cpu = match target {\n    Target::X86_64Linux => "x86-64",\n'
                             '    _ => "generic",\n};'),
            'wildcard on triple': ('match triple {\n    "x86_64-unknown-linux-gnu" => {\n        x()\n    }\n'
                                   '    _ => return None,\n}'),
            'guarded wildcard': 'match t.arch() {\n    Arch::X86_64 => 1,\n    _ if fast => 2,\n    _ => 3,\n}',
        }
        for name, source in red.items():
            with self.subTest(name):
                self.assertEqual({rule for _, rule in rules(source)}, {'target-match'}, source)

    def test_exhaustive_decisions_are_green(self):
        green = [
            'match target.arch() {\n    Arch::X86_64 => a(),\n    Arch::Aarch64 => {\n        b()\n    }\n}',
            # A `_` inside an arm's pattern or body is not a wildcard arm.
            'match (code, target.arch()) {\n    (true, Arch::X86_64) => 32,\n    (false, _) | (true, Arch::Aarch64) => 64,\n}',
            'match target {\n    Target::X86_64Linux => match op { _ => 1 },\n'
            '    Target::Aarch64Linux | Target::Aarch64Darwin => 2,\n}',
            # Data and prose, not decisions.
            'for target in [Target::X86_64Linux, Target::Aarch64Linux] { run(target) }',
            'assert_eq!(Target::from_triple(t), Some(Target::Aarch64Darwin));',
            '// t == Target::Aarch64Linux used to hide Darwin\nlet s = "x == Target::X86_64Linux";',
            'match kind {\n    Kind::A => 1,\n    _ => 2,\n}',
        ]
        for source in green:
            with self.subTest(source):
                self.assertEqual(rules(source), [])

    def test_darwin_floor_copies_track_the_link_kit(self):
        self.assertEqual(check_target.darwin_floor_errors(floor_texts()), [])
        self.assertEqual(check_target.darwin_floor_errors(floor_texts(
            link='26.1', buck='26.1', driver='(26 << 16) | (1 << 8)', installer='26.1')), [])

        buck = floor_texts(buck='27.0')
        self.assertEqual([path for path, _, _ in check_target.darwin_floor_errors(buck)],
                         [check_target.TOOLCHAIN_BUCK])
        driver = floor_texts(driver='27 << 16')
        self.assertEqual([path for path, _, _ in check_target.darwin_floor_errors(driver)],
                         [check_target.DRIVER_MACHO])
        installer = floor_texts(installer='27.0')
        self.assertEqual([path for path, _, _ in check_target.darwin_floor_errors(installer)],
                         [check_target.INSTALL_SH])
        source = floor_texts(link='27.0')
        self.assertEqual({path for path, _, _ in check_target.darwin_floor_errors(source)},
                         {check_target.TOOLCHAIN_BUCK, check_target.DRIVER_MACHO, check_target.INSTALL_SH})

    def test_darwin_floor_minor_versions_are_compared(self):
        errors = check_target.darwin_floor_errors(floor_texts(link='26.1', buck='26.1', installer='26.1'))
        self.assertEqual([path for path, _, _ in errors], [check_target.DRIVER_MACHO])

    def test_host_cfg_outside_a_seam_is_red(self):
        red = [
            '#[cfg(target_os = "linux")]\nfn memory_file() {}',
            '#[cfg(all(\n    test,\n    target_os = "linux",\n))]\nmod t {}',
            '#[cfg_attr(not(target_os = "linux"), allow(dead_code))]\nfn f() {}',
            'let path = if cfg!(target_os = "linux") { a } else { b };',
            'let machine = if cfg!(target_arch = "x86_64") { 62 } else { 183 };',
        ]
        for source in red:
            with self.subTest(source):
                self.assertEqual({rule for _, rule in rules(source)}, {'host-cfg'})
        self.assertEqual(rules(red[1]), [(1, 'host-cfg')])

    def test_host_cfg_rules_and_seams(self):
        green = [
            # Arch cfgs compile host SIMD and are outside the rule; so is a cfg_attr's body.
            '#[cfg(target_arch = "x86_64")]\nuse std::arch::x86_64::*;',
            '#[cfg_attr(test, doc = "target_os")]\nfn f() {}',
            '/* #[cfg(target_os = "linux")] */ fn f() {}',
        ]
        for source in green:
            with self.subTest(source):
                self.assertEqual(rules(source), [])
        seam = '#[cfg(target_os = "macos")]\nmod darwin;'
        self.assertEqual(rules(seam, 'crates/hirsel-core-rt/src/host/mod.rs'), [])
        self.assertEqual(rules(seam, next(iter(check_target.HOST_FILES))), [])

    def test_proc_paths_outside_a_seam_are_red(self):
        red = [
            'let s = std::fs::read_to_string("/proc/self/statm");',
            'let gone = !Path::new("/proc").join(pid).exists();',
            'let maps = format!("/proc/{pid}/maps");',
            'let status = c"/proc/self/status";',
            'const C: &str = r#"DIR *d = opendir("/proc/self/fd");"#;',
            'const SH: &str = "read -r pid name < /proc/$$/stat";',
        ]
        for source in red:
            with self.subTest(source):
                self.assertEqual(rules(source), [(1, 'host-proc')])
        green = [
            '/// Linux reads /proc/self/statm.\nfn f() {}',
            '// "/proc/self/fd"\nlet d = "/dev/fd";',
            'let p = "/usr/proc/x"; let q = "a/proc/b"; let r = "/process";',
        ]
        for source in green:
            with self.subTest(source):
                self.assertEqual(rules(source), [])
        seam = 'let s = std::fs::read_to_string("/proc/self/statm");'
        self.assertEqual(rules(seam, 'crates/hirsel-core-driver/src/host/linux/mod.rs'), [])


if __name__ == '__main__':
    unittest.main()
