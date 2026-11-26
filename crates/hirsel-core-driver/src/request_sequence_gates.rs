// The request sequences' predicted gate classes (request_sequences.rs).

/// Each sequence's predicted gate class (see [`Gate`]), sorted by name. Unmarked
/// rows are l-body-review-1039's settled column (programs/*-v3.md, NEW-SEQUENCES.md).
#[rustfmt::skip]
const GATES: &[(&str, Gate)] = &[
    ("alias_cycle_added", Gate::Fallback),
    ("alias_cycle_body", Gate::Fallback), // 1039: fast; a module of A is refused (v3.1 condition 4)
    ("alias_target", Gate::Fallback),
    ("assertion_value", Gate::Fallback), // comptime-phase2-2693: b's two assertions read a.V; the second step refuses one
    ("backend_llvm", Gate::Fast),
    ("body_parse_refusal", Gate::Fallback),
    ("body_test_reference", Gate::Fast),
    ("bound_transitive", Gate::Fallback),
    ("check_to_build", Gate::Fallback),
    ("checkout_relocated", Gate::Fast),
    ("clone_added", Gate::Fallback),
    ("clone_body", Gate::Fast),
    ("clone_winner_order", Gate::Fallback), // 1039: fast; a module of A is refused (v3.1 condition 4)
    ("clone_winner_removed", Gate::Fallback),
    ("code_identity_body", Gate::Fast),
    ("codec_context_added", Gate::Fallback),
    ("codec_default", Gate::Fallback),
    ("codec_field_changed", Gate::Fallback),
    ("codec_maker_parse_refusal", Gate::Fallback),
    ("codec_owner_body", Gate::Fast),
    ("comptime_absent", Gate::Fallback),
    ("constant_call_absent", Gate::Fast),
    ("constant_call_equal", Gate::Fast),
    ("constant_call_unrelated", Gate::Fallback),
    ("constant_call_unrelated_body", Gate::Fast),
    ("constant_cycle", Gate::Fallback),
    ("constant_dfs_order", Gate::Fallback), // 1039: fast; a module of A is refused (v3.1 condition 4)
    ("constant_parts_forward", Gate::Fast),
    ("constant_projection", Gate::Fallback),
    ("constant_ready_queue", Gate::Fallback), // 1039: fast; a module of A is refused (v3.1 condition 4)
    ("constant_value", Gate::Fallback),
    ("constant_with_current_owner", Gate::Fast),
    ("constructor_body", Gate::Fallback),
    ("constructor_body_fixed", Gate::Fallback), // not in 1039: b gains a type, shape changes
    ("constructor_const", Gate::Fallback),
    ("constructor_const_fixed", Gate::Fallback), // not in 1039: b gains a type, shape changes
    ("constructor_pattern", Gate::Fallback),
    ("core_source_edit", Gate::Fast),
    ("cycle_two_current", Gate::Fast),
    ("debug_lines", Gate::Fast),
    ("decl_count_fallback", Gate::Fallback),
    ("declaration_pass_order", Gate::Fallback), // 1039: fast; a module of A is refused (v3.1 condition 4)
    ("declared_second_file", Gate::Fast),
    ("default_argument_absent", Gate::Fallback),
    ("dependency_order", Gate::Fast),
    ("dependency_version", Gate::Fallback),
    ("doc_trivia", Gate::Fallback), // 1039: fast; a module of A is refused (v3.1 condition 4)
    ("duplicate_copy_body", Gate::Fallback), // not in 1039: A refuses the duplicate
    ("entry_warning_interleave", Gate::Fast), // v3.2: fast; C has A's context and shape, and A' published A's record
    ("extension_candidate", Gate::Fallback),
    ("extension_new_ambiguous", Gate::Fallback),
    ("failure_body", Gate::Fast),
    ("finalizer_body", Gate::Fallback),
    ("finalizer_body_edit", Gate::Fallback), // v3.2: a finalizer body is its type's header text
    ("finalizer_cross_module", Gate::Fast),
    ("foreign_duplicate_span", Gate::Fallback), // 1039: fast; a module of A is refused (v3.1 condition 4)
    ("foreign_private_span", Gate::Fallback), // 1039: fast; a module of A is refused (v3.1 condition 4)
    ("full_to_reach", Gate::Fallback),
    ("generic_body", Gate::Fast),
    ("generic_body_result", Gate::Fast),
    ("generic_cross_clone", Gate::Fast),
    ("generic_instance_new_type", Gate::Fast),
    ("generic_refused_then_demanded", Gate::Fallback), // 1039: fast; a module of A is refused (v3.1 condition 4)
    ("generic_transitive", Gate::Fast),
    ("global_summary_refusal", Gate::Fallback), // 1039: fast; a module of A is refused (v3.1 condition 4)
    ("header_alternates", Gate::Fast), // not in 1039: B's shape falls back and does not publish; A' published A's generation
    ("header_respaced", Gate::Fast), // v3.2: the same tokens, so the same shape
    ("header_type_before_function", Gate::Fallback), // 1039: fast; a module of A is refused (v3.1 condition 4)
    ("header_type_before_function_order", Gate::Fallback), // 1039: fast; a module of A is refused (v3.1 condition 4)
    ("holds_transitive", Gate::Fallback),
    ("image_body_edit", Gate::Fast),
    ("image_library_read_added", Gate::Fast),
    ("image_library_read_dropped", Gate::Fast),
    ("implicit_capacity", Gate::Fallback),
    ("index_builtin", Gate::Fallback),
    ("index_duplicate", Gate::Fallback),
    ("inline_cycle", Gate::Fallback),
    ("keyword_module", Gate::Fallback),
    ("lang12_direct", Gate::Fallback), // 1039: fast; a module of A is refused (v3.1 condition 4)
    ("lang12_mutual_break", Gate::Fallback),
    ("lang12_mutual_growth", Gate::Fallback), // 1039: fast; a module of A is refused (v3.1 condition 4)
    ("lang12_permutation", Gate::Fast),
    ("lang12_reset", Gate::Fast),
    ("lang12_unused_parameter", Gate::Fallback), // 1039: fast; a module of A is refused (v3.1 condition 4)
    ("lang12_wrapper_module", Gate::Fallback), // 1039: fast; a module of A is refused (v3.1 condition 4)
    ("lang12_wrapper_own", Gate::Fallback), // 1039: fast; a module of A is refused (v3.1 condition 4)
    ("lang12_wrapper_package", Gate::Fallback), // 1039: fast; a module of A is refused (v3.1 condition 4)
    ("layout_embedded", Gate::Fallback),
    ("layout_failure_removed", Gate::Fallback),
    ("layout_first_failure_order", Gate::Fallback), // 1039: fast; a module of A is refused (v3.1 condition 4)
    ("layout_global_failure", Gate::Fallback),
    ("macro_absent", Gate::Fallback),
    ("manifest_permuted", Gate::Fast),
    ("module_split", Gate::Fallback),
    ("native_const", Gate::Fallback),
    ("native_foreign_body", Gate::Fast),
    ("native_reorder", Gate::Fast),
    ("negative_module_head", Gate::Fallback),
    ("new_body_refusal", Gate::Fast),
    ("opaque_private_field", Gate::Fallback),
    ("optimization", Gate::Fast),
    ("parameter_anchor_swapped", Gate::Fallback),
    ("parse_mode_changed", Gate::Fallback),
    ("prelude_negative_type", Gate::Fallback),
    ("private_alias", Gate::Fallback),
    ("private_function_body", Gate::Fast),
    ("product_order", Gate::Fallback), // 1036: fallback, the source tuple reorders
    ("publish_body_third", Gate::Fast),
    ("publish_check_build_check", Gate::Fast), // 1039 cold history: fallback; here A' published the check generation and the build does not replace it
    ("publish_refused_second", Gate::Fast),
    ("publish_uncommitted_second", Gate::Fast),
    ("reach_to_full", Gate::Fallback),
    ("reexport_alias", Gate::Fallback),
    ("referenced_type_current_owner", Gate::Fast),
    ("repair_one_body_refusal", Gate::Fallback), // 1039: fast; a module of A is refused (v3.1 condition 4)
    ("ring", Gate::Fast),
    ("root_limit", Gate::Fast),
    ("root_missing", Gate::Fast), // v3.2: fast; the declared record does not live under the check root
    ("same_module_file_tie", Gate::Fallback),
    ("same_shape_refused_constant", Gate::Fallback), // 1039: fast; a module of A is refused (v3.1 condition 4)
    ("source_moves_module", Gate::Fast), // v3.2: fast; a file name is not in the shape, and declaration reads no file name
    ("std_dependency_added", Gate::Fallback),
    ("symbol_body_insertion", Gate::Fast),
    ("symlink_directory", Gate::Fast),
    ("test_added", Gate::Fallback),
    ("test_body_added", Gate::Fallback), // 1039: fast; a module of A is refused (v3.1 condition 4)
    ("test_header_flip", Gate::Fallback),
    ("test_signature_before_reference_order", Gate::Fallback), // 1039: fast; a module of A is refused (v3.1 condition 4)
    ("test_signature_reference_order", Gate::Fallback), // 1039: fast; a module of A is refused (v3.1 condition 4)
    ("trailing_line_added", Gate::Fallback),
    ("two_body_refusals_order", Gate::Fallback), // 1039: fast; a module of A is refused (v3.1 condition 4)
    ("unchanged_body_refusal", Gate::Fallback), // 1039: fast; a module of A is refused (v3.1 condition 4)
    ("unpublished_temp", Gate::Fast),
    ("unreachable_body_refusal", Gate::Fallback), // 1039: fast; a module of A is refused (v3.1 condition 4)
    ("via_conversion_body", Gate::Fast),
    ("via_representation", Gate::Fallback),
    ("waits_body", Gate::Fallback), // 1039: fast; a module of A is refused (v3.1 condition 4)
    ("waits_header", Gate::Fallback),
];
