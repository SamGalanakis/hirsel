#!/usr/bin/env python3
"""Keep tests and references to test-only modules out of production sources."""

import argparse
from bisect import bisect_right
from pathlib import Path
import re
import sys
import tomllib

from check_file_lengths import git_paths, snapshot


PROGRAMS = Path('core/programs')
TEST_ONLY = {'store.fixture', 'store.signup', 'runner.fixture'}
# Complexitysmash finding 1: moved by cs-store-split-1935 push B.
ALLOW_LIST = {
    ('runner/runner.hirsel', 'test-attribute'): (
        'journal_test_trigger_cancel journal_test_trigger_granted '
        'journal_test_trigger_snapshot journal_test_trigger_transact test_projection_isolated '
        'test_range_reruns test_retired_without_waiting test_rows_normalized '
        'test_stale_never_released test_subs_main test_subs_reset_keeps_diff '
        'test_subscribe_order trigger_scan_classes'
    ),
    ('runner/runner.hirsel', 'test-function'): (
        'law_caller law_owner_take_wakes law_owner_takes_queued law_projection_isolated '
        'law_range_reruns law_retired_without_waiting law_rows_normalized '
        'law_stale_never_released law_subs_main law_subs_reset_keeps_diff law_subscribe_order '
        'law_trigger_timer_wall_wait test_projection_isolated test_range_reruns '
        'test_retired_without_waiting test_rows_normalized test_stale_never_released '
        'test_subs_main test_subs_reset_keeps_diff test_subscribe_order'
    ),
    ('runner/triggers.hirsel', 'test-attribute'): (
        'journal_test_trigger_revision'
    ),
    ('runner/triggers.hirsel', 'test-function'): (
        'law_directory law_options'
    ),
    ('store/store-layout.hirsel', 'test-attribute'): (
        'disk_test_load_path_layout'
    ),
    ('store/store-locations.hirsel', 'test-attribute'): (
        'disk_test_page_location_closure'
    ),
    ('store/store-manifest.hirsel', 'test-attribute'): (
        'disk_test_format_cutover disk_test_paged_inventory disk_test_paged_manifest'
    ),
    ('store/store-packs.hirsel', 'test-attribute'): (
        'disk_test_blob_threshold disk_test_page_emission disk_test_paged_generation '
        'disk_test_relocation_late_refusal'
    ),
    ('store/store-packs.hirsel', 'test-function'): (
        'law_full_frame_selection law_generation_locations law_leaf_cell law_pack_stamp '
        'law_pointed_payload'
    ),
    ('store/store-pages.hirsel', 'test-attribute'): (
        'test_paged_collection_paths test_paged_row_account test_paged_shrinking_overwrite'
    ),
    ('store/store-pages.hirsel', 'test-function'): (
        'test_paged_collection_paths test_paged_row_account test_paged_shrinking_overwrite'
    ),
    ('store/store-reexec.hirsel', 'test-attribute'): (
        'disk_test_reexecute_info'
    ),
    ('store/store-scrub.hirsel', 'test-attribute'): (
        'disk_test_scrub_lease disk_test_scrub_misdirected'
    ),
    ('store/store.hirsel', 'test-attribute'): (
        'disk_pack_bound disk_test_branches disk_test_db disk_test_db_refusal '
        'disk_test_frames disk_test_migrate disk_test_opening disk_test_publish_refusal '
        'disk_test_read_only_snapshot disk_test_removal_spans disk_test_resident_charge '
        'disk_test_schema_refusal disk_test_typed_error_drops_frame_tail follow_schedules '
        'frame_size_boundary journal_frame_oversize journal_frame_size_boundary '
        'journal_test_ack_channel journal_test_apply journal_test_bootstrap '
        'journal_test_call_vector journal_test_cap_arithmetic journal_test_cap_calls '
        'journal_test_cap_restart journal_test_changes_checkpoint journal_test_db '
        'journal_test_drop journal_test_end journal_test_end_storage journal_test_floor '
        'journal_test_fork_cost journal_test_fork_exports journal_test_fork_history '
        'journal_test_fork_identity journal_test_fork_meta_limit journal_test_fork_pack_lists '
        'journal_test_fork_preparation journal_test_fork_same_roots '
        'journal_test_fork_threshold journal_test_options journal_test_order_workspace '
        'journal_test_payload journal_test_place_idle journal_test_recorded '
        'journal_test_recorded_barrier journal_test_recorded_fork journal_test_recovery '
        'journal_test_rotation journal_test_segment journal_test_step_stop '
        'journal_test_streams journal_test_worker test_abort_budget test_branches '
        'test_call_info test_callback_shares_staged test_catalog_invalid test_changes '
        'test_changes_identity test_changes_rewritten_equal test_close_follow '
        'test_drop_recreate test_follow test_foreign test_fork_old_snapshot test_fork_settle '
        'test_fork_shared_head test_frame_size test_frames test_index_refusal '
        'test_index_writes test_info_mut test_look test_look_faults test_look_task '
        'test_memory_identity test_migrate test_model test_node_ids test_overwrite_account '
        'test_pin_settle test_pinned_delete test_pinned_draft_at_bound test_published_value '
        'test_put_validation test_range_prefix test_read_cover test_read_liveness '
        'test_read_spans test_reader_task test_reads_ranges test_refused_reply '
        'test_request_digest test_reserve_latches test_resident test_retention_keeps_steps '
        'test_rollback_on_overrun test_schema_numbers test_shared_changes test_snapshots '
        'test_staged_index_reads test_step_bounds test_stored_size test_streams '
        'test_typed_foreign test_typed_indexes test_typed_model test_typed_resident '
        'test_typed_snapshot_task'
    ),
    ('store/store.hirsel', 'test-function'): (
        'law_abort_budget law_branches law_call_info law_call_reserve '
        'law_callback_shares_staged law_catalog_invalid law_changes law_changes_identity '
        'law_changes_rewritten_equal law_close_follow law_drop_recreate '
        'law_empty_collection_pages law_empty_collection_resident law_follow law_foreign '
        'law_fork_old_snapshot law_fork_settle law_fork_shared_head law_frame_size '
        'law_frame_size_boundary law_frames law_index_refusal law_index_writes law_info_mut '
        'law_leaf_range_stop law_location_cow_charge law_look law_look_faults law_migrate '
        'law_model law_node_ids law_overwrite_account law_pin_settle law_pinned_delete '
        'law_published_value law_put_validation law_range_prefix law_read_cover '
        'law_read_liveness law_read_spans law_reader_task law_reads_ranges law_refused_reply '
        'law_request_digest law_reserve_latches law_resident law_resident_name '
        'law_resident_scan law_resident_text law_retention_keeps_steps '
        'law_rollback_on_overrun law_schema_numbers law_shared_changes law_snapshots '
        'law_step_bounds law_stored_size law_streams law_typed_foreign law_typed_indexes '
        'law_typed_model law_typed_resident law_view_geometry test_abort_budget test_branches '
        'test_call_info test_callback_shares_staged test_catalog_invalid test_changes '
        'test_changes_identity test_changes_rewritten_equal test_close_follow test_db '
        'test_drop_recreate test_follow test_foreign test_fork_old_snapshot test_fork_settle '
        'test_fork_shared_head test_frame_size test_frames test_index_refusal '
        'test_index_writes test_info_mut test_look test_look_faults test_look_task '
        'test_memory_identity test_migrate test_model test_node_ids test_overwrite_account '
        'test_pin_settle test_pinned_delete test_pinned_draft_at_bound test_published_value '
        'test_put_validation test_range_prefix test_read_cover test_read_liveness '
        'test_read_spans test_reader_task test_reads_ranges test_refused_reply '
        'test_request_digest test_reserve_latches test_resident test_retention_keeps_steps '
        'test_rollback_on_overrun test_schema_numbers test_shared_changes test_snapshots '
        'test_staged_index_reads test_step_bounds test_stored_size test_streams '
        'test_typed_foreign test_typed_indexes test_typed_model test_typed_resident '
        'test_typed_snapshot_task'
    ),
}

IDENT = r'[A-Za-z_][A-Za-z_0-9]*'
IGNORED = re.compile(r'(?P<comment>//[^\n]*)|\b(?:br|r)(?P<hashes>\#*)"[^\n]*?"(?P=hashes)'
                     r'''|"(?:\\[^\n]|[^"\\\n])*"|'(?:\\[^\n]|[^'\\\n])*' ''', re.VERBOSE)
FUNCTION = re.compile(rf'\bfn\s+((?:test_|law_)[A-Za-z_0-9]*)\b')
ATTRIBUTE = re.compile(rf'@\s*test\b(?:\s+(?:pub\s+)?fn\s+({IDENT}))?')
REFERENCE = re.compile(rf'(?P<field>(?<!\.)\.(?!\.)\s*)?\b{IDENT}(?:\s*\.\s*{IDENT})+')


def source_violations(source, modules):
    # Literal bodies are opaque tokens. Comments are whitespace, even inside a dotted path.
    source = IGNORED.sub(lambda match: (' ' if match['comment'] else '\0') * len(match[0]), source)
    newlines = [match.start() for match in re.finditer('\n', source)]
    for match in FUNCTION.finditer(source):
        yield bisect_right(newlines, match.start()) + 1, 'test-function', match[1]
    for match in ATTRIBUTE.finditer(source):
        yield bisect_right(newlines, match.start()) + 1, 'test-attribute', match[1] or '@test'
    for match in REFERENCE.finditer(source):
        if match['field']:
            continue
        path = tuple(re.split(r'\s*\.\s*', match[0]))
        for size in range(len(path), 0, -1):
            module = path[:size]
            if module in modules:
                if modules[module]:
                    yield bisect_right(newlines, match.start()) + 1, 'test-only-module', '.'.join(module)
                break


def violations(root, staged=False):
    if staged:
        paths = git_paths(root, 'ls-files', '-z', '--', str(PROGRAMS))
        manifest_paths = {path for path in paths if Path(path).parent.parent == PROGRAMS
                          and Path(path).name == 'hirsel.toml'}
    else:
        manifest_paths = {str(path.relative_to(root)) for path in (root / PROGRAMS).glob('*/hirsel.toml')}
    revision = ':0' if staged else None
    manifests = []
    qualified = {}
    source_paths = set()
    for path, data in sorted(snapshot(root, revision, manifest_paths).items()):
        manifest = tomllib.loads(data.decode())
        package = manifest['package']['name']
        sources = manifest.get('sources', {})
        test_sources = manifest.get('tests', {}).get('sources', {})
        local = {tuple(module.split('.')): True for module in test_sources.values()}
        local.update((tuple(module.split('.')), False) for module in sources.values())
        crash_package = Path(path).parent.name == 'store-crash'
        if crash_package:
            qualified[package,] = True
        for module, test_only in local.items():
            external = module if module == (package,) else (package, *module)
            qualified[external] = test_only or crash_package
        paths = {str(Path(path).parent / source) for source in sources}
        manifests.append((package, paths, local))
        source_paths.update(paths)

    qualified.update((tuple(module.split('.')), True) for module in TEST_ONLY)
    sources = snapshot(root, revision, source_paths)
    for package, paths, local in manifests:
        modules = qualified | {(package,): False} | local
        modules.update((tuple(module.split('.')[1:]), True) for module in TEST_ONLY
                       if module.startswith(f'{package}.'))
        for path in sorted(paths):
            if path not in sources:
                raise ValueError(f'{path}: missing [sources] file')
            for line, rule, symbol in source_violations(sources[path].decode(), modules):
                yield path, line, rule, symbol


def check(root, staged=False):
    remaining = {(str(PROGRAMS / path), rule, symbol)
                 for (path, rule), symbols in ALLOW_LIST.items() for symbol in symbols.split()}
    ok = True
    try:
        for path, line, rule, symbol in sorted(violations(root, staged)):
            key = path, rule, symbol
            if key in remaining:
                remaining.remove(key)
                continue
            print(f'{path}:{line}: {rule}: {symbol} in [sources]. Move to [tests.sources].',
                  file=sys.stderr)
            ok = False
    except (ValueError, KeyError, OSError) as error:
        print(f'check_boundaries: {error}', file=sys.stderr)
        return False
    for path, rule, symbol in sorted(remaining):
        print(f'{path}: {rule}: {symbol} no longer violates the boundary. '
              'Remove it from ALLOW_LIST in tools/check_boundaries.py.', file=sys.stderr)
    return ok and not remaining


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--staged', action='store_true', help='check the Git index')
    sys.exit(0 if check(Path(__file__).resolve().parents[1], parser.parse_args().staged) else 1)
