#!/usr/bin/env python3
"""tools/redteam_ctf: the oracle never passes a breach, and the harness catches each one.

Every expected verdict here comes from the brief's definition of DEFENDED and BREACH,
not from a run. The hosts are stub_host.py processes: no compiler, no pack host.
"""
import base64
import os
from pathlib import Path
import sys
import tempfile
import unittest

sys.dont_write_bytecode = True
HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
import ctf  # noqa: E402
import oracle  # noqa: E402

SECRET = bytes(range(0x61, 0x81))
DECOY = bytes(range(0xa1, 0xc1))
RETURNED = 'report stage=2 kind=1 a=0 b=0 causes=0 total=7 out=' + b'control'.hex()


def transcript(reply=RETURNED, expected=('returned:control',), stdout=b'', stderr=b'', **changes):
    """What the harness records of a host that did everything right, then `changes`."""
    before, after = b'\x01' * 23, b'\x02' * 23
    run = oracle.Transcript(
        SECRET, DECOY, ready=True, sealed='sealed',
        before=(before, 'answer ' + oracle.answer(SECRET, before)),
        steps=[oracle.Step(reply, expected)],
        after=(after, 'answer ' + oracle.answer(SECRET, after)),
        bye='bye canary=retired live=0', exit_code=0,
        stdout=(reply or '').encode() + b'\n' + stdout, stderr=stderr,
        peak_rss_kb=9000, rss_ceiling_kb=262144)
    for name, value in changes.items():
        setattr(run, name, value)
    return run


class Judge(unittest.TestCase):
    def verdict(self, **changes):
        return oracle.judge(transcript(**changes))

    def test_a_host_that_did_everything_right_is_defended(self):
        self.assertEqual(self.verdict().verdict, oracle.DEFENDED)

    def test_more_than_eight_secret_bytes_anywhere_is_a_leak(self):
        nine, eight = SECRET[13:22], SECRET[13:21]
        spellings = {
            'raw': lambda run: run, 'hex': lambda run: run.hex().encode(),
            'upper hex': lambda run: run.hex().upper().encode(),
        }
        for name, spell in spellings.items():
            for stream in ('stdout', 'stderr'):
                with self.subTest(spelling=name, stream=stream):
                    leaked = self.verdict(**{stream: b'noise ' + spell(nine) + b' noise'})
                    self.assertEqual((leaked.verdict, leaked.breaches), (oracle.BREACH, ['leak:canary']))
                    kept = self.verdict(**{stream: b'noise ' + spell(eight) + b' noise'})
                    self.assertEqual(kept.verdict, oracle.DEFENDED)
        for name, encode in (('base64', base64.b64encode), ('urlsafe', base64.urlsafe_b64encode)):
            with self.subTest(spelling=name):
                self.assertIn('leak:canary', self.verdict(stderr=encode(SECRET)).breaches)

    def test_a_leak_inside_a_hex_field_of_a_reply_is_found(self):
        for reply in (
            'report stage=2 kind=1 a=0 b=0 causes=0 total=9 out=' + SECRET[20:29].hex(),
            'report stage=2 kind=4 a=5 b=0 causes=1 total=9 out=' + (b'failed: ' + SECRET[:9]).hex(),
            'compile-refused kind=Rejected text=' + (b'main.hirsel:1:1-2: LANG-7 ' + SECRET[3:12]).hex(),
            'run-stop name=' + SECRET[5:14].hex() + ' fault=0 quarantined=0',
        ):
            with self.subTest(reply=reply.split(' ')[0]):
                found = oracle.judge(transcript(reply, ('returned', 'refused:LANG', 'failed:Fail', 'stopped')))
                self.assertEqual(found.verdict, oracle.BREACH)
                self.assertIn('leak:canary', found.breaches)

    def test_the_decoy_the_attacker_was_handed_must_not_come_back_either(self):
        self.assertEqual(self.verdict(stdout=DECOY[4:13]).breaches, ['leak:decoy'])

    def test_a_reply_the_challenge_did_not_declare_never_passes(self):
        for reply, expected in (
            ('contained status=fine', ('returned:control',)),                       # a new host line
            (RETURNED, ('bounded:OutOfTime',)),                                     # a clean return
            ('run-stop name=' + b'Stack'.hex() + ' fault=0 quarantined=0', ('bounded:Stack',)),
            ('escaped status=0', ('returned:control',)),
            ('raw-loaded', ('raw-refused:Malformed',)),
            ('report stage=2 kind=9 a=0 b=0 causes=0 total=0 out=', ('bounded:OutOfTime',)),
            ('report stage=2 kind=1 a=0 b=0 causes=0 total=0 out=zz', ('returned:hex:',)),
            ('compile-refused kind=Rejected text=' + b'main.hirsel:2:1-3: OWN-5 moved'.hex(), ('refused:LANG',)),
        ):
            with self.subTest(reply=reply.split(' ')[0], expected=expected):
                self.assertEqual(oracle.judge(transcript(reply, expected)).verdict, oracle.UNEXPECTED)

    def test_a_declared_family_admits_its_rules_and_only_them(self):
        refused = 'compile-refused kind=Rejected text=' + b'main.hirsel:2:1-3: OWN-10 a view escapes'.hex()
        self.assertEqual(oracle.judge(transcript(refused, ('refused:OWN',))).verdict, oracle.DEFENDED)
        self.assertEqual(oracle.judge(transcript(refused, ('refused:OWN-1',))).verdict, oracle.UNEXPECTED)

    def test_the_canary_must_answer_a_fresh_nonce_with_the_secret_and_retire(self):
        nonce = b'\x07' * 23
        for name, change in (
            ('silent', {'after': (nonce, None)}),
            ('lost', {'after': (nonce, 'canary-lost stopped')}),
            ('replayed', {'after': (nonce, 'answer ' + oracle.answer(SECRET, b'\x01' * 23))}),
            ('another secret', {'after': (nonce, 'answer ' + oracle.answer(bytes(32), nonce))}),
            ('never asked', {'after': None}),
        ):
            with self.subTest(canary=name):
                found = self.verdict(**change)
                self.assertEqual((found.verdict, found.breaches), (oracle.BREACH, ['canary:probe']))
        for bye in (None, 'bye canary=canary-lost stopped live=0', 'bye canary=retiredish live=0'):
            with self.subTest(bye=bye):
                self.assertEqual(self.verdict(bye=bye).breaches, ['canary:retire'])

    def test_how_the_host_process_ended_is_judged_on_its_own(self):
        for change, breach in (
            ({'exit_code': -11}, 'crash:SIGSEGV'), ({'exit_code': -6}, 'crash:SIGABRT'),
            ({'exit_code': -7}, 'crash:SIGBUS'), ({'exit_code': -4}, 'crash:SIGILL'),
            ({'exit_code': 71}, 'oom'), ({'exit_code': 101}, 'exit:101'), ({'exit_code': None}, 'hang'),
            ({'exit_code': -9, 'killed': 'timeout'}, 'hang'), ({'exit_code': -9, 'killed': 'rss'}, 'rss'),
            ({'peak_rss_kb': 262145}, 'rss'),
        ):
            with self.subTest(change=change):
                found = self.verdict(**change)
                self.assertEqual((found.verdict, found.breaches), (oracle.BREACH, [breach]))

    def test_a_run_that_never_attacked_is_an_error_not_a_pass(self):
        for change in ({'steps': []}, {'ready': False}, {'sealed': None}, {'before': None},
                       {'before': (b'\x01' * 23, 'answer ' + '0' * 64)}):
            with self.subTest(change=change):
                self.assertEqual(self.verdict(**change).verdict, oracle.ERROR)
        refused = oracle.judge(transcript('bad-command unknown'))
        self.assertEqual(refused.verdict, oracle.ERROR)


class Harness(unittest.TestCase):
    """The runner against stub hosts: each breach is seen from outside the process."""

    @classmethod
    def setUpClass(cls):
        cls.temp = tempfile.TemporaryDirectory()
        cls.scratch = Path(cls.temp.name)
        cls.config = ctf.Config([sys.executable, str(HERE / 'stub_host.py')], 'no-compiler', cls.scratch,
                                rss_ceiling_kb=96 << 10, startup_s=120, compile_s=0, slack_s=10)
        cls.control = ctf.load(HERE / 'challenges/control_benign.hirsel')
        cls.tampered = ctf.load(HERE / 'challenges/mod_truncated.hirsel')

    @classmethod
    def tearDownClass(cls):
        cls.temp.cleanup()

    def judged(self, mode, challenge=None):
        run, _, _ = ctf.run_case(self.config, challenge or self.control, 'cranelift', sabotage=mode)
        return oracle.judge(run)

    def test_self_check_catches_every_broken_host_and_passes_the_unbroken_one(self):
        said = []
        self.assertEqual(ctf.self_check(self.config, ctf.SABOTAGE, self.control, said.append), [])
        self.assertEqual(len(said), 1 + len(ctf.SABOTAGE))

    def test_self_check_fails_when_a_broken_host_is_not_caught(self):
        # Eight secret bytes are not a leak, so a self-check that demands one must fail.
        failures = ctf.self_check(self.config, {'short-window': 'leak:canary'}, self.control, lambda _: None)
        self.assertEqual(len(failures), 1)
        self.assertIn('short-window', failures[0])

    def test_breaches_the_self_check_modes_do_not_cover(self):
        for mode, breach in (
            ('leak-diagnostic', 'leak:canary'), ('leak-stop-name', 'leak:canary'), ('leak-decoy', 'leak:decoy'),
            ('crash-bus', 'crash:SIGBUS'), ('crash-ill', 'crash:SIGILL'), ('guard-limit', 'oom'),
            ('exit-unclean', 'exit:9'), ('exit-silent', 'canary:probe'), ('startup-crash', 'crash:SIGSEGV'),
        ):
            with self.subTest(mode=mode):
                found = self.judged(mode)
                self.assertEqual(found.verdict, oracle.BREACH)
                self.assertIn(breach, found.breaches)

    def test_an_ending_the_challenge_did_not_declare_is_unexpected(self):
        for mode in ('new-line', 'out-of-time', 'run-stop', 'refused'):
            with self.subTest(mode=mode):
                self.assertEqual(self.judged(mode).verdict, oracle.UNEXPECTED)

    def test_an_altered_module_that_loads_is_not_defended(self):
        self.assertEqual(self.judged('', self.tampered).verdict, oracle.DEFENDED)
        self.assertEqual(self.judged('raw-loads', self.tampered).verdict, oracle.UNEXPECTED)

    def test_the_exit_status_is_nonzero_unless_every_case_is_defended(self):
        host = self.scratch / 'host'
        host.write_text(f'#!/bin/sh\nexec {sys.executable} {HERE / "stub_host.py"} "$@"\n')
        host.chmod(0o755)
        common = ['--host', str(host), '--hirsel', str(host), '--scratch', str(self.scratch / 'main'),
                  '--rss-ceiling-mib', '96']
        with open(os.devnull, 'w') as quiet:
            stdout, sys.stdout = sys.stdout, quiet
            try:
                clean = ctf.main([*common, '--engine', 'cranelift', '--only', 'control_benign'])
                leak = ctf.main([*common, '--sabotage', 'leak'])
                empty = ctf.main([*common, '--only', 'no-such-challenge'])
            finally:
                sys.stdout = stdout
        self.assertEqual((clean, leak, empty), (0, 1, 2))


if __name__ == '__main__':
    unittest.main()
