# Red-team containment CTF scoreboard

Written by `tools/redteam_ctf/ctf.py --write-scoreboard`. Do not edit by hand.

Base: `d6b702921340dadcae82bd2295809ebb15663fbf`. 2 BREACH, 65 DEFENDED, 0 ERROR, 1 UNEXPECTED of 68 cases.

DEFENDED means what README.md says it means and no more: one owner per pack pool,
no speculative or timing channels, and only the attacks in this corpus.

## Not defended

- **BREACH** `ports_bound_churn` on cranelift: canary:probe; canary:retire; rss; ended by none, not by one of: bounded:OutOfTime. The harness ended the host at 270760 KiB resident.
- **BREACH** `ports_bound_churn` on llvm: canary:probe; canary:retire; rss; ended by none, not by one of: bounded:OutOfTime. The harness ended the host at 267940 KiB resident.
- **UNEXPECTED** `stack_glue_deep_chain` on llvm: ended by compile-unsupported, not by one of: returned:glue-survived; PC32 is not a RIP-relative displacement.

`ports_bound_churn`: Each pass opens a small byte bound, makes twenty virtual ports of five kinds inside it and lets the bound end. MEM-24: the ports are withdrawn with the limit that paid for them, so no pass leaves a charge or host memory behind and only the step bound ends the loop. A leak shows as Capacity or as host memory past the ceiling.

    tools/redteam_ctf/ctf.py --build --only ports_bound_churn --verbose

`stack_glue_deep_chain`: A Box chain 200,000 links deep, then cloned, compared and dropped. FAIL-17: generated glue uses constant stack whatever the value's depth, so none of the three may raise Stack or stop the run; the module returns its marker.

    tools/redteam_ctf/ctf.py --build --only stack_glue_deep_chain --verbose

| challenge | class | engine | verdict | ended by | ms | peak RSS KiB |
|---|---|---|---|---|---:|---:|
| `cap_ambient_clock` | capability-forgery | cranelift | DEFENDED | `refused:CAP-1` | 11 | 8896 |
| `cap_ambient_clock` | capability-forgery | llvm | DEFENDED | `refused:CAP-1` | 14 | 8924 |
| `cap_forged_net` | capability-forgery | cranelift | DEFENDED | `refused:PROG-2` | 10 | 8896 |
| `cap_forged_net` | capability-forgery | llvm | DEFENDED | `refused:PROG-2` | 14 | 8828 |
| `cap_hidden_wait` | capability-forgery | cranelift | DEFENDED | `refused:LANG-8` | 12 | 8552 |
| `cap_hidden_wait` | capability-forgery | llvm | DEFENDED | `refused:LANG-8` | 15 | 8936 |
| `cap_provider_import` | capability-forgery | cranelift | DEFENDED | `load-refused:Import` | 13 | 8884 |
| `cap_provider_import` | capability-forgery | llvm | DEFENDED | `load-refused:Import` | 111 | 8752 |
| `cap_waiting_entry` | capability-forgery | cranelift | DEFENDED | `load-refused:NoEntry` | 17 | 8708 |
| `cap_waiting_entry` | capability-forgery | llvm | DEFENDED | `load-refused:NoEntry` | 110 | 8952 |
| `control_benign` | control | cranelift | DEFENDED | `returned:control` | 13 | 8808 |
| `control_benign` | control | llvm | DEFENDED | `returned:control` | 110 | 8824 |
| `heap_list_growth` | heap-exhaustion | cranelift | DEFENDED | `bounded:Capacity` | 19 | 9180 |
| `heap_list_growth` | heap-exhaustion | llvm | DEFENDED | `bounded:Capacity` | 126 | 9276 |
| `heap_string_growth` | heap-exhaustion | cranelift | DEFENDED | `bounded:Capacity` | 26 | 9996 |
| `heap_string_growth` | heap-exhaustion | llvm | DEFENDED | `bounded:Capacity` | 125 | 9956 |
| `heap_zst_list_capacity` | heap-exhaustion | cranelift | DEFENDED | `returned:zst-capacity-held` | 22 | 9044 |
| `heap_zst_list_capacity` | heap-exhaustion | llvm | DEFENDED | `returned:zst-capacity-held` | 128 | 9116 |
| `held_forged_handle` | held-confusion | cranelift | DEFENDED | `refused:PACK-16` | 11 | 8796 |
| `held_forged_handle` | held-confusion | llvm | DEFENDED | `refused:PACK-16` | 15 | 8892 |
| `held_lookalike_type` | held-confusion | cranelift | DEFENDED | `returned:TypeMismatch` | 14 | 9068 |
| `held_lookalike_type` | held-confusion | llvm | DEFENDED | `returned:TypeMismatch` | 117 | 9208 |
| `held_signature_confusion` | held-confusion | cranelift | DEFENDED | `load-refused:NoEntry` | 15 | 8520 |
| `held_signature_confusion` | held-confusion | llvm | DEFENDED | `load-refused:NoEntry` | 119 | 8960 |
| `held_structural_guesses` | held-confusion | cranelift | DEFENDED | `returned:every-guess-mismatched` | 21 | 8968 |
| `held_structural_guesses` | held-confusion | llvm | DEFENDED | `returned:every-guess-mismatched` | 123 | 9212 |
| `literal_grow_and_write` | literal-custody | cranelift | DEFENDED | `returned:Loaded-literal!tail-grown` | 18 | 9044 |
| `literal_grow_and_write` | literal-custody | llvm | DEFENDED | `returned:Loaded-literal!tail-grown` | 123 | 8940 |
| `literal_outlives_image` | literal-custody | cranelift | DEFENDED | `returned:outlives-its-image` | 18 | 8912 |
| `literal_outlives_image` | literal-custody | llvm | DEFENDED | `returned:outlives-its-image` | 125 | 9112 |
| `mod_altered_bytes` | malformed-module | cranelift | DEFENDED | `raw-refused:Malformed, raw-refused:Malformed, raw-refused:Mismatch, raw-refused:NotAdmitted, raw-refused:NotAdmitted` | 13 | 8568 |
| `mod_altered_bytes` | malformed-module | llvm | DEFENDED | `raw-refused:Malformed, raw-refused:Malformed, raw-refused:Mismatch, raw-refused:NotAdmitted, raw-refused:NotAdmitted` | 116 | 9000 |
| `mod_oversized` | malformed-module | cranelift | DEFENDED | `raw-refused:Malformed, raw-refused:Malformed, raw-refused:Malformed, raw-refused:Malformed` | 157 | 57892 |
| `mod_oversized` | malformed-module | llvm | DEFENDED | `raw-refused:Malformed, raw-refused:Malformed, raw-refused:Malformed, raw-refused:Malformed` | 259 | 58084 |
| `mod_truncated` | malformed-module | cranelift | DEFENDED | `raw-refused:Malformed, raw-refused:Malformed, raw-refused:Malformed, raw-refused:Malformed, raw-refused:Malformed` | 14 | 8668 |
| `mod_truncated` | malformed-module | llvm | DEFENDED | `raw-refused:Malformed, raw-refused:Malformed, raw-refused:Malformed, raw-refused:Malformed, raw-refused:Malformed` | 116 | 8652 |
| `own_alias_mut` | ownership | cranelift | DEFENDED | `refused:OWN-9` | 13 | 8492 |
| `own_alias_mut` | ownership | llvm | DEFENDED | `refused:OWN-9` | 14 | 8928 |
| `own_frozen_write` | ownership | cranelift | DEFENDED | `refused:OWN-10` | 11 | 8768 |
| `own_frozen_write` | ownership | llvm | DEFENDED | `refused:OWN-10` | 15 | 8820 |
| `own_use_after_move` | ownership | cranelift | DEFENDED | `refused:OWN-5` | 13 | 8796 |
| `own_use_after_move` | ownership | llvm | DEFENDED | `refused:OWN-5` | 15 | 8928 |
| `own_view_escape` | ownership | cranelift | DEFENDED | `refused:OWN-10` | 13 | 8744 |
| `own_view_escape` | ownership | llvm | DEFENDED | `refused:OWN-10` | 15 | 8892 |
| `ports_bound_churn` | port-exhaustion | cranelift | BREACH | `none` | 522 | 270760 |
| `ports_bound_churn` | port-exhaustion | llvm | BREACH | `none` | 591 | 267940 |
| `ports_hoard` | port-exhaustion | cranelift | DEFENDED | `bounded:Capacity` | 22 | 9316 |
| `ports_hoard` | port-exhaustion | llvm | DEFENDED | `bounded:Capacity` | 140 | 9544 |
| `spin_clone_compare` | metering | cranelift | DEFENDED | `bounded:OutOfTime` | 21 | 9044 |
| `spin_clone_compare` | metering | llvm | DEFENDED | `bounded:OutOfTime` | 126 | 9264 |
| `spin_tight_loop` | metering | cranelift | DEFENDED | `bounded:OutOfTime` | 16 | 8944 |
| `spin_tight_loop` | metering | llvm | DEFENDED | `bounded:OutOfTime` | 115 | 8952 |
| `spin_unpaid_repeat` | metering | cranelift | DEFENDED | `bounded:OutOfTime` | 96 | 12932 |
| `spin_unpaid_repeat` | metering | llvm | DEFENDED | `bounded:OutOfTime` | 201 | 12908 |
| `stack_deep_recursion` | stack-exhaustion | cranelift | DEFENDED | `bounded:Stack` | 20 | 15376 |
| `stack_deep_recursion` | stack-exhaustion | llvm | DEFENDED | `bounded:Stack` | 122 | 15472 |
| `stack_glue_deep_chain` | stack-exhaustion | cranelift | DEFENDED | `returned:glue-survived` | 134 | 47148 |
| `stack_glue_deep_chain` | stack-exhaustion | llvm | UNEXPECTED | `compile-unsupported` | 122 | 8756 |
| `stack_mutual_recursion` | stack-exhaustion | cranelift | DEFENDED | `bounded:Stack` | 20 | 15568 |
| `stack_mutual_recursion` | stack-exhaustion | llvm | DEFENDED | `bounded:Stack` | 153 | 15796 |
| `type_doubling_alias` | type-expansion | cranelift | DEFENDED | `refused:resource` | 11 | 8896 |
| `type_doubling_alias` | type-expansion | llvm | DEFENDED | `refused:resource` | 13 | 8884 |
| `type_generic_instantiation` | type-expansion | cranelift | DEFENDED | `compile-limit:Memory` | 6800 | 8676 |
| `type_generic_instantiation` | type-expansion | llvm | DEFENDED | `compile-limit:Memory` | 6631 | 8704 |
| `type_inferred_doubling` | type-expansion | cranelift | DEFENDED | `refused:resource` | 218 | 8676 |
| `type_inferred_doubling` | type-expansion | llvm | DEFENDED | `refused:resource` | 217 | 8872 |
| `type_inline_nesting` | type-expansion | cranelift | DEFENDED | `refused:resource` | 10 | 8796 |
| `type_inline_nesting` | type-expansion | llvm | DEFENDED | `refused:resource` | 13 | 8928 |
