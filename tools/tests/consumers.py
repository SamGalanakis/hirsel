#!/usr/bin/env python3
"""Consumer selection follows real launch bodies, source uses and shard assignment."""
import importlib.machinery
import importlib.util
import contextlib
import io
import json
import subprocess
import re
from pathlib import Path
import tempfile
import unittest
import sys
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[2]
loader = importlib.machinery.SourceFileLoader('consumers', str(ROOT / 'tools/consumers'))
consumers = importlib.util.module_from_spec(importlib.util.spec_from_loader('consumers', loader))
loader.exec_module(consumers)
sys.path.insert(0, str(ROOT / 'tools/buck2'))
import consumer_results  # noqa: E402


def graph_for(inventories):
    return {f'root//:{binary}-shard-{index + 1}':
            dict(test_binary=binary, shard_index=index, shards=3, costs=None)
            for binary in inventories for index in range(3)}


class Consumers(unittest.TestCase):
    def test_direct_sources_precede_transitive_shards_and_data_does_not_own_sources(self):
        path = 'crates/hirsel-check/src/lib.rs'
        attrs = {'root//:hirsel-check-unittest-harness': {'srcs': ['root///' + path]},
                 'root//:ships-source-as-data': {}}
        with patch.object(consumers, 'buck', return_value=json.dumps(attrs)):
            owners = consumers.direct_owners(ROOT, [path])
        self.assertEqual(owners, {'root//:hirsel-check-unittest-harness': 'direct source owner'})
        graph = {'root//:a-transitive': {'test_binary': 'a-transitive'},
                 'root//:hirsel-check-unittest': {'test_binary': 'hirsel-check-unittest'}}
        reasons = {}
        selected = consumers.select(ROOT, [path], graph, {}, owners, reasons)
        self.assertEqual(selected[0], 'root//:hirsel-check-unittest')
        self.assertEqual(reasons[selected[0]], 'direct source owner')
        self.assertEqual(reasons['root//:a-transitive'], 'transitive shard')

    def test_changed_test_input_owns_its_harness(self):
        target = 'root//:hirsel-check-test-law'
        graph = {target: {'test_binary': 'hirsel-check-test-law'}}
        reasons = {}
        selected = consumers.select(ROOT, ['crates/hirsel-check/tests/law.rs'], graph, {},
                                    {target + '-harness'}, reasons)
        self.assertEqual(selected, [target])
        self.assertEqual(reasons[target], 'direct test owner')

    def test_tool_consumers_keep_graph_dependent_scripts(self):
        graph = {'root//:direct-script': {'script': 'root///tools/tests/consumers.py'},
                 'root//:dependent-script': {'script': 'root///tools/tests/buck2_sync.py'},
                 'root//:direct': {'harness': 'root//:direct-script'},
                 'root//:dependent': {'harness': 'root//:dependent-script'}}
        reasons = {}
        selected = consumers.select(ROOT, ['tools/tests/consumers.py', 'tools/consumers'], graph, {},
                                    {'root//:direct-script': 'direct test owner'}, reasons)
        self.assertEqual(selected, ['root//:direct', 'root//:dependent'])
        self.assertEqual(reasons['root//:dependent'], 'transitive shard')

    def test_data_dependency_resolves_a_harness_outside_the_reverse_graph(self):
        target = 'root//:dependent'
        graph = {target: {'harness': target + '-script'}}
        harness = {target + '-script': {'script': 'root///tools/tests/buck2_sync.py'}}
        with patch.object(consumers, 'buck', side_effect=[json.dumps(graph), json.dumps(harness)]):
            found = consumers.test_graph(ROOT, ['owner("tools/buck2/sync.py")'])
        selected = consumers.select(ROOT, ['tools/buck2/sync.py'], found, {})
        self.assertEqual(selected, [target])

    def test_many_direct_owners_cannot_evict_mandatory_launches(self):
        inventories = {binary: set(cases) for binary, cases in consumers.MUST.items()}
        inventories['hirsel-cli-test-free'] = {'native_returned_rest_completion_replays_across_engines'}
        graph = graph_for(inventories)
        owners = set()
        for index in range(consumers.LIMIT + 1):
            binary = f'a-direct-{index:03}'
            graph['root//:' + binary] = dict(test_binary=binary)
            owners.add('root//:' + binary + '-harness')
        reasons = {}
        selected = consumers.select(ROOT, ['crates/hirsel-rt/src/pool.rs'], graph, inventories, owners, reasons)
        mandatory = {target for target, reason in reasons.items() if reason == 'mandatory set'}
        self.assertTrue(mandatory)
        self.assertLessEqual(mandatory, set(selected[:consumers.LIMIT]))

    def test_the_cap_keeps_the_nearest_owner_and_reports_the_cut(self):
        # place-store-681: 100 of 4,261 kept by name, with no word on what was cut.
        graph = {'root//:lib': {}, 'root//:mid': {'buck.deps': ['root//:lib']},
                 'root//:z-near-harness': {'buck.deps': ['root//:lib']},
                 'root//:a-far-harness': {'buck.deps': ['root//:mid']}}
        for name in ('z-near', 'a-far'):
            graph[f'root//:{name}'] = dict(test_binary=name, harness=f'root//:{name}-harness',
                                           **{'buck.deps': [f'root//:{name}-harness']})
        cut = []
        selected = consumers.select(ROOT, ['crates/hirsel-check/src/lib.rs'], graph, {},
                                    {'root//:lib': 'direct source owner'}, {}, 1, cut)
        self.assertEqual((selected, cut), (['root//:z-near'], ['root//:a-far']))

    def test_cpu_pool_change_selects_native_launches_and_replay_before_other_shards(self):
        inventories = {binary: set(cases) for binary, cases in consumers.MUST.items()}
        inventories['hirsel-cli-test-free'] = {
            'native_returned_rest_completion_replays_across_engines',
            'every_f0_trace_replays_to_its_live_projection_at_every_cut_gap_interpreter',
        }
        graph = graph_for(inventories)
        cpu_targets = {f'root//:hirsel-cli-test-cpu_pool_{width}' for width in (1, 4, 8, 16)}
        graph |= {target: dict(test_binary=target.split(':')[1]) for target in cpu_targets}
        assignments = consumers.assignments(ROOT, graph, inventories)
        expected = cpu_targets | {assignments[binary, case] for binary, cases in consumers.MUST.items() for case in cases}
        expected.add(assignments['hirsel-cli-test-free', 'native_returned_rest_completion_replays_across_engines'])
        expected |= {'root//:hirsel-cli-test-cli-consumer-native-http',
                     'root//:hirsel-cli-test-cli-consumer-native-http-concurrent'}
        for path in ('crates/hirsel-rt/src/pool.rs', 'crates/hirsel-rt/src/free_launch.rs',
                     'crates/hirsel-native/src/kit.rs', 'crates/hirsel-llvm/src/c_entry.rs',
                     'crates/hirsel-engine/src/queries/native_records.rs',
                     'tools/buck2/native-toolchain/link_kit.py'):
            with self.subTest(path=path):
                selected = consumers.select(ROOT, [path], graph, inventories)
                self.assertLessEqual(expected, set(selected[:consumers.LIMIT]))

    def test_std_module_selects_the_conformance_shard_that_uses_it(self):
        path = 'stdlib/std@1/toml.hir'
        corpus = 'conformance/SEM/SEM-3-toml-edit-cargo-run.hir'
        inventories = {'hirsel-driver-test-it-stdlib': {'toml_upstream_25::generated_00'}}
        graph = graph_for(inventories)
        selected = consumers.select(ROOT, [path], graph, inventories)
        mapped = consumers.assignments(ROOT, graph, inventories)
        self.assertIn(mapped['hirsel-driver-test-it-stdlib', 'toml_upstream_25::generated_00'], selected)
        from sync import conformance_plan
        index, = [i for i, part in enumerate(conformance_plan(), 1)
                  if consumers.corpus_shard(corpus, part['count']) == part['index']]
        target = f'root//:gate-conformance-shard-{index}'
        self.assertIn(target, selected)
        self.assertNotIn('root//:hirsel-llvm-unittest', selected)

    def test_module_uses_propagate_through_other_std_modules(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            std = root / 'stdlib/std@1'
            std.mkdir(parents=True)
            (std / 'a.hir').write_text('pub fn a() -> () {}')
            (std / 'b.hir').write_text('pub fn b() -> () { a.a() }')
            (std / 'c.hir').write_text('pub fn c() -> () { b.b() }')
            self.assertEqual(consumers.module_users(root, ['stdlib/std@1/a.hir']), {'a', 'b', 'c'})

    def test_case_source_follows_helpers_and_loaded_corpus_without_string_braces(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / 'loaded.hir').write_text('toml.document("a=1")')
            (root / 'main.rs').write_text('''
fn unrelated() { let text = "fn regression() { }"; }
fn fixture() { let text = r###" } { "###; include_str!("loaded.hir"); }
#[test]
fn regression() { fixture(); }
''')
            source = consumers.Sources(root, 'main.rs')
            self.assertEqual(source.matching({'regression', 'unrelated'}, re.compile(r'\btoml\.')),
                             {'regression'})

    def test_isolated_body_selects_its_dedicated_target_after_balancing(self):
        binary = 'hirsel-cli-test-free'
        inventories = {binary: {'isolated', 'ordinary'}}
        graph = graph_for(inventories)
        before = consumers.assignments(ROOT, graph, inventories)
        graph['dedicated'] = dict(test_binary=binary, extra_env={'HIRSEL_TEST_DEDICATED_CASE': 'isolated'})
        after = consumers.assignments(ROOT, graph, inventories)
        self.assertEqual(after[binary, 'isolated'], 'dedicated')
        self.assertEqual(after[binary, 'ordinary'], before[binary, 'ordinary'])

    def test_cost_assignment_matches_the_existing_launcher_vector(self):
        # Same rule as buck2_launcher's behavioral test: table rows longest
        # first (heap, then smoke and prefix on the lighter shard 2); other
        # and host by name hash, both even: shard 1.
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / 'costs').write_text('heap\t9\nprefix\t1\nsmoke\t2\n')
            binary = 'fixture'
            graph = {f'shard-{index + 1}': dict(test_binary=binary, shard_index=index,
                                              shards=2, costs='root//:costs') for index in range(2)}
            result = consumers.assignments(root, graph, {binary: {'heap', 'prefix', 'smoke', 'other', 'host'}}, widths={})
            self.assertEqual({case: target for (_, case), target in result.items()},
                             dict(heap='shard-1', prefix='shard-2', smoke='shard-2',
                                  other='shard-1', host='shard-1'))
            # A declared width group leaves the measured deal; it has its own shards.
            graph['wide-shard-1'] = dict(test_binary=binary, shard_index=0, shards=1, exact_cpus=4, costs='root//:costs')
            result = consumers.assignments(root, graph, {binary: {'heap', 'prefix', 'smoke', 'other', 'host'}},
                                           widths={binary: {'heap': 4}})
            self.assertEqual({case: target for (_, case), target in result.items()},
                             dict(heap='wide-shard-1', host='shard-1', other='shard-1',
                                  smoke='shard-1', prefix='shard-2'))


class ConsumerResults(unittest.TestCase):
    # Hand-derived panic and deadline records use test_report.py's real libtest
    # and deadline markers. The ledger deliberately records every assertion
    # value: a matching family or assertion header alone cannot suppress a red.
    panic = """running 1 tests
HIRSEL_TEST_STARTED laws::receipt
test laws::receipt ... FAILED
thread 'laws::receipt' panicked at src/laws.rs:8:5:
assertion `left == right` failed: cap=20s
  left: 3
 right: 4

test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out;
"""
    reason = ('Failed laws::receipt. src/laws.rs:8:5: assertion `left == right` failed: cap=20s '
              'left: 3 right: 4')
    deadline = """HIRSEL_TEST_STARTED laws::receipt
HIRSEL_TEST_INNER_DEADLINE after 570s; completed=0 running=1; action cap=600s
HIRSEL_TEST_RUNNING laws::receipt elapsed=570.001s
"""

    def classify(self, details=None, rows=None, status=2):
        rows = {'fixture::laws::receipt': self.reason} if rows is None else rows
        rows = {key: consumer_results.KnownFailure(reason, 'fixture ledger') for key, reason in rows.items()}
        return consumer_results.classify('fixture-shard-1', dict(status=status, details=details or self.panic),
                                         rows)[0]

    def test_exact_selector_diagnostic_body_and_cap_is_known(self):
        self.assertEqual(self.classify(), 'known')

    def test_current_rust_panic_thread_id_preserves_diagnostic_matching(self):
        self.assertEqual(self.classify(self.panic.replace("' panicked", "' (149) panicked")), 'known')

    def test_new_exact_body_is_new_even_when_another_shard_has_a_row(self):
        self.assertEqual(self.classify(rows={'fixture-shard-2': self.reason}), 'new')

    def test_changed_assertion_value_is_uncertain(self):
        self.assertEqual(self.classify(self.panic.replace('left: 3', 'left: 5')), 'uncertain')

    def test_historical_assertion_cannot_override_the_current_record(self):
        reason = self.reason.replace('left: 3', 'left: 5') + ' Prior: ' + self.reason
        self.assertEqual(self.classify(rows={'fixture::laws::receipt': reason}), 'uncertain')

    def test_target_row_cannot_borrow_another_bodys_diagnostic(self):
        self.assertEqual(self.classify(rows={'fixture-shard-1': self.reason}), 'known')
        reason = self.reason.replace('left: 3', 'left: 5') + ' Failed laws::other. ' + self.reason
        self.assertEqual(self.classify(rows={'fixture-shard-1': reason}), 'uncertain')

    def test_changed_cap_is_uncertain(self):
        self.assertEqual(self.classify(self.panic.replace('cap=20s', 'cap=21s')), 'uncertain')

    def test_missing_cap_is_uncertain(self):
        self.assertEqual(self.classify(self.panic.replace('cap=20s', 'unchanged cap')), 'uncertain')

    def test_unlocalized_deadline_and_unknown_body_are_uncertain(self):
        details = 'HIRSEL_TEST_INNER_DEADLINE after 570s; completed=0 running=0; action cap=600s'
        self.assertEqual(self.classify(details, {'fixture-shard-1': details}), 'uncertain')
        self.assertEqual(self.classify('HIRSEL_TEST_STARTED setup\nFAILURE', {}), 'uncertain')

    def test_localized_deadline_requires_matching_body_and_both_caps(self):
        row = self.deadline.replace('\n', ' ')
        self.assertEqual(self.classify(self.deadline, {'fixture::laws::receipt': row}), 'known')
        self.assertEqual(self.classify(self.deadline.replace('cap=600s', 'cap=601s'),
                                       {'fixture::laws::receipt': row}), 'uncertain')

    def test_zero_body_and_all_ignored_are_neither_known_nor_pass(self):
        for details in ('running 0 tests\ntest result: ok. 0 passed; 0 failed;',
                        'HIRSEL_TEST_STARTED skipped\ntest skipped ... ignored\n'
                        'test result: ok. 0 passed; 0 failed; 1 ignored;'):
            for status in (1, 2):
                self.assertEqual(self.classify(details, {'fixture-shard-1': details}, status), 'uncertain')

    def test_corpus_slices_are_judged_by_the_runners_receipts(self):
        # testsmash-apply-703: 670 executed cases in 32 tools/corpus slices read
        # as 32 "zero executed bodies".
        counts = 'whole corpus: {} validated / {} passing / {} failing / 0 not implemented\n'
        failing = ('conformance/LANG/LANG-4-std-duplicate-bad.hir: expected Reject { rule: RuleId("LANG-4"), '
                   'line: 1 }; got accept')
        red = failing + '\n--path requires every selected test to pass\n' + counts.format(20, 19, 1)
        self.assertEqual(self.classify(counts.format(21, 21, 0), {}, 1), 'pass')
        self.assertEqual(self.classify(counts.format(0, 0, 0), {}, 1), 'uncertain')
        self.assertEqual(self.classify(red, {}), 'new')
        self.assertEqual(self.classify(red, {'fixture::conformance/LANG/LANG-4-std-duplicate-bad.hir': failing}),
                         'known')
        self.assertEqual(self.classify(red.replace('got accept', 'got LANG-9'),
                                       {'fixture::conformance/LANG/LANG-4-std-duplicate-bad.hir': failing}),
                         'uncertain')
        native = ('native: conformance/STORE/STORE-12-serve-run.hir under x86_64/regions/elide/reuse/flat-walk/heap: '
                  'ready: no native layout for Inbox (native-unavailable)\n{\n  "configuration_attempts": 2,\n}\n')
        self.assertEqual(self.classify(native, {}), 'new')
        self.assertEqual(self.classify(native.split('\n', 1)[1], {}, 1), 'pass')

    def test_known_body_does_not_hide_new_failure_in_same_target(self):
        details = self.panic + '\nHIRSEL_TEST_STARTED laws::new\ntest laws::new ... FAILED\n'
        self.assertEqual(self.classify(details), 'new')

    def test_only_positive_completed_body_evidence_can_pass(self):
        details = 'test laws::receipt ... ok\ntest result: ok. 1 passed; 0 failed; 0 ignored;'
        self.assertEqual(self.classify(details, {}, 1), 'pass')
        self.assertEqual(self.classify('HIRSEL_TEST_STARTED laws::receipt\n'
                                       'test result: ok. 1 passed; 0 failed;', {}, 1), 'uncertain')

    def test_nested_child_summary_does_not_replace_the_final_suite_census(self):
        child = 'test child ... ok\ntest result: ok. 1 passed; 0 failed; 0 ignored;\n'
        details = child + 'test parent ... ok\ntest result: ok. 2 passed; 0 failed; 0 ignored;\n'
        self.assertEqual(self.classify(details, {}, 1), 'pass')
        self.assertEqual(self.classify(child + 'test result: ok. 0 passed; 0 failed; 0 ignored;\n', {}, 1),
                         'uncertain')

    def test_python_script_body_counts_exclude_skips(self):
        self.assertEqual(self.classify('Ran 3 tests in 0.001s\n\nOK (skipped=1)\n', {}, 1), 'pass')
        self.assertEqual(self.classify('Ran 3 tests in 0.001s\n\nOK (skipped=3)\n', {}, 1), 'uncertain')
        self.assertEqual(self.classify('Ran 0 tests in 0.001s\n\nOK\n', {}, 1), 'uncertain')
        child = 'Ran 1 test in 0.001s\n\nOK\n'
        self.assertEqual(self.classify(child + 'Ran 0 tests in 0.001s\n\nOK\n', {}, 1), 'uncertain')
        self.assertEqual(self.classify(child + 'Ran 3 tests in 0.001s\n\nOK (skipped=3)\n', {}, 1), 'uncertain')

    def test_unfinished_second_body_prevents_known_classification(self):
        self.assertEqual(self.classify(self.panic + '\nHIRSEL_TEST_STARTED unknown\n'), 'uncertain')

    def test_completed_failure_does_not_localize_a_later_deadline(self):
        self.assertEqual(self.classify(self.panic, status=6), 'uncertain')

    def test_buck_results_are_read_without_skipping_any_result(self):
        with tempfile.TemporaryDirectory() as directory:
            folder = Path(directory)
            table = folder / 'known.tsv'
            table.write_text('fixture::laws::receipt\t' + self.reason + '\towner\t2026-11-04\n')
            log = folder / 'results.jsonl'
            results = [dict(name='fixture-shard-1', status=2, details=self.panic),
                       dict(name='fixture-shard-2', status=2, details=self.panic.replace('laws::receipt', 'laws::new')),
                       dict(name='empty', status=1, details='running 0 tests')]
            log.write_text('\n'.join(json.dumps({'Event': {'data': {'Instant': {'data': {'TestResult': result}}}}})
                                     for result in results))
            decoded = consumer_results.event_logs.test_results(log, history=consumer_results.event_logs.NoTestHistory())
            rows = consumer_results.ledger(table)
            self.assertEqual([consumer_results.classify(target, result, rows)[0] for target, result in decoded.items()],
                             ['known', 'new', 'uncertain'])
            with contextlib.redirect_stdout(io.StringIO()), contextlib.redirect_stderr(io.StringIO()):
                self.assertEqual(consumer_results.report([log], table), 1)

    def report_events(self, events, reasons):
        with tempfile.TemporaryDirectory() as directory:
            folder = Path(directory)
            table = folder / 'known.tsv'
            table.write_text(''.join(f'{selector}\t{reason}\towner\t2026-11-06\n'
                                     for selector, reason in reasons.items()))
            log = folder / 'results.jsonl'
            log.write_text('\n'.join(map(json.dumps, events)))
            stdout, stderr = io.StringIO(), io.StringIO()
            with contextlib.redirect_stdout(stdout), contextlib.redirect_stderr(stderr):
                status = consumer_results.report([log], table)
            return status, stdout.getvalue(), stderr.getvalue()

    @staticmethod
    def result_event(name, details, status=2):
        return {'Event': {'data': {'Instant': {'data': {'TestResult':
                dict(name=name, status=status, details=details)}}}}}

    def test_440_native_proof_results_do_not_create_zero_body_observations(self):
        # 440g/h TestRun uses a label object; TestResult uses the qualified name.
        cases = {
            'root-plan-proof': 'frozen_root_plans_run_both_backends_without_live_linker_inputs',
            'root-frozen-kit-proof': 'admitted_root_plans_bind_the_kit_before_codegen',
        }
        events = []
        for target, case in cases.items():
            label = dict(package='root//tools/buck2/native-toolchain', name=target)
            events.append({'Event': {'data': {'SpanEnd': {'data': {'TestRun': {
                'suite': {'suite_name': target, 'target_label': {'label': label}},
                'command_report': {'status': {'Success': {}}, 'details': {'signed_exit_code': 0}},
            }}}}}})
            details = ('---- STDOUT ----\n\nrunning 1 test\n'
                       f'test native_compilation_tasks_19::{case} ... ok\n\n'
                       'test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; '
                       '1906 filtered out; finished in 33.02s\n\n---- STDERR ----\n')
            events.append(self.result_event(label['package'] + ':' + target, details, 1))
        status, stdout, stderr = self.report_events(events, {})
        self.assertEqual((status, stdout), (0, ''))
        self.assertIn('2 passed', stderr)
        self.assertIn('0 uncertain', stderr)

    def configctl_observation(self):
        bodies = ['configctl_8_cli_arguments', 'configctl_8_data_validation', 'configctl_8_record_replay']
        diagnostic = 'namespace capture exceeds its retained admission'
        details = 'running 3 tests\nHIRSEL_TEST_LIMITS inner570 action cap=600s\n'
        reasons = {}
        for body in bodies:
            details += (f'HIRSEL_TEST_STARTED {body}\ntest {body} ... FAILED\n'
                        f"thread '{body}' panicked at crates/hirsel-cli/tests/configctl_8.rs:166:5:\n"
                        f'{diagnostic}\n\n')
            reasons['hirsel-cli-test-configctl_8::' + body] = (
                f'Failed {body}. crates/hirsel-cli/tests/configctl_8.rs:166:5: {diagnostic}; '
                'inner570 RE600. Evidence mutex-borrow-416/REPORT.md and free-heap-report-421/REPORT.md.')
        details += 'test result: FAILED. 0 passed; 3 failed; 0 ignored;\n'
        return details, reasons

    def test_configctl_exact_evidence_is_reused_and_printed(self):
        details, reasons = self.configctl_observation()
        status, stdout, stderr = self.report_events([
            self.result_event('hirsel-cli-test-configctl_8', details)], reasons)
        self.assertEqual((status, stdout), (0, ''))
        self.assertIn('known\thirsel-cli-test-configctl_8', stderr)
        for selector in reasons:
            self.assertIn(selector, stderr)
        self.assertIn('namespace capture exceeds its retained admission', stderr)
        self.assertIn('inner=570s', stderr)
        self.assertIn('action=600s', stderr)
        self.assertRegex(stderr, r'known\.tsv:[123]')
        self.assertIn('mutex-borrow-416/REPORT.md', stderr)

    def test_configctl_changed_diagnostic_stays_uncertain_with_observed_evidence(self):
        details, reasons = self.configctl_observation()
        details = details.replace('namespace capture exceeds its retained admission',
                                  'installed integrity metadata exceeds request transport admission')
        status, stdout, stderr = self.report_events([
            self.result_event('hirsel-cli-test-configctl_8', details)], reasons)
        self.assertEqual(status, 1)
        self.assertIn('uncertain\thirsel-cli-test-configctl_8', stdout)
        self.assertIn('installed integrity metadata exceeds request transport admission', stdout)
        self.assertIn('known.tsv:', stdout)
        self.assertIn('0 known', stderr)

    def test_original_batch_failure_survives_passing_isolated_controls(self):
        passed = 'test laws::receipt ... ok\ntest result: ok. 1 passed; 0 failed;'
        events = [self.result_event('fixture-shard-1', self.panic),
                  self.result_event('fixture-shard-1', passed, 1),
                  self.result_event('fixture-shard-1', passed, 1)]
        status, stdout, stderr = self.report_events(events, {})
        self.assertEqual(status, 1)
        self.assertIn('new\tfixture-shard-1', stdout)
        self.assertIn('1 new', stderr)
        self.assertIn('2 passed', stderr)

    def test_json_libtest_failure_keeps_the_assertion_values(self):
        details = '\n'.join(json.dumps(event) for event in [
            dict(type='test', event='started', name='laws::receipt'),
            dict(type='test', event='failed', name='laws::receipt',
                 stdout='src/laws.rs:8:5: assertion `left == right` failed: cap=20s left: 3 right: 4')])
        self.assertEqual(self.classify(details), 'known')


if __name__ == '__main__':
    unittest.main()
