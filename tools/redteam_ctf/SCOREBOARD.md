# Red-team containment CTF scoreboard

Written by `tools/redteam_ctf/ctf.py --write-scoreboard`. Do not edit by hand.

Base: `b65d4c34ee9967b5aa6ec24cf708f900ef2fbc17`. 68 DEFENDED, 0 BREACH, 0 UNEXPECTED, 0 ERROR of 68 cases.

DEFENDED means what README.md says it means and no more: one owner per pack pool,
no speculative or timing channels, and only the attacks in this corpus.

| challenge | class | engine | verdict | ended by | ms | peak RSS KiB |
|---|---|---|---|---|---:|---:|
| `cap_ambient_clock` | capability-forgery | cranelift | DEFENDED | `refused:CAP-1` | 14 | 9236 |
| `cap_ambient_clock` | capability-forgery | llvm | DEFENDED | `refused:CAP-1` | 21 | 9544 |
| `cap_forged_net` | capability-forgery | cranelift | DEFENDED | `refused:PROG-2` | 18 | 9380 |
| `cap_forged_net` | capability-forgery | llvm | DEFENDED | `refused:PROG-2` | 17 | 9488 |
| `cap_hidden_wait` | capability-forgery | cranelift | DEFENDED | `refused:LANG-8` | 19 | 9284 |
| `cap_hidden_wait` | capability-forgery | llvm | DEFENDED | `refused:LANG-8` | 22 | 9240 |
| `cap_provider_import` | capability-forgery | cranelift | DEFENDED | `load-refused:Import` | 17 | 9080 |
| `cap_provider_import` | capability-forgery | llvm | DEFENDED | `load-refused:Import` | 135 | 9572 |
| `cap_waiting_entry` | capability-forgery | cranelift | DEFENDED | `load-refused:NoEntry` | 26 | 9372 |
| `cap_waiting_entry` | capability-forgery | llvm | DEFENDED | `load-refused:NoEntry` | 139 | 9400 |
| `control_benign` | control | cranelift | DEFENDED | `returned:control` | 17 | 9284 |
| `control_benign` | control | llvm | DEFENDED | `returned:control` | 128 | 9608 |
| `heap_list_growth` | heap-exhaustion | cranelift | DEFENDED | `bounded:Capacity` | 28 | 10112 |
| `heap_list_growth` | heap-exhaustion | llvm | DEFENDED | `bounded:Capacity` | 145 | 9860 |
| `heap_string_growth` | heap-exhaustion | cranelift | DEFENDED | `bounded:Capacity` | 35 | 11020 |
| `heap_string_growth` | heap-exhaustion | llvm | DEFENDED | `bounded:Capacity` | 145 | 10968 |
| `heap_zst_list_capacity` | heap-exhaustion | cranelift | DEFENDED | `returned:zst-capacity-held` | 27 | 9444 |
| `heap_zst_list_capacity` | heap-exhaustion | llvm | DEFENDED | `returned:zst-capacity-held` | 145 | 9488 |
| `held_forged_handle` | held-confusion | cranelift | DEFENDED | `refused:PACK-16` | 14 | 9404 |
| `held_forged_handle` | held-confusion | llvm | DEFENDED | `refused:PACK-16` | 19 | 9600 |
| `held_lookalike_type` | held-confusion | cranelift | DEFENDED | `returned:TypeMismatch` | 19 | 9300 |
| `held_lookalike_type` | held-confusion | llvm | DEFENDED | `returned:TypeMismatch` | 141 | 9668 |
| `held_signature_confusion` | held-confusion | cranelift | DEFENDED | `load-refused:NoEntry` | 18 | 9272 |
| `held_signature_confusion` | held-confusion | llvm | DEFENDED | `load-refused:NoEntry` | 136 | 9644 |
| `held_structural_guesses` | held-confusion | cranelift | DEFENDED | `returned:every-guess-mismatched` | 24 | 9536 |
| `held_structural_guesses` | held-confusion | llvm | DEFENDED | `returned:every-guess-mismatched` | 149 | 9440 |
| `literal_grow_and_write` | literal-custody | cranelift | DEFENDED | `returned:Loaded-literal!tail-grown` | 21 | 9552 |
| `literal_grow_and_write` | literal-custody | llvm | DEFENDED | `returned:Loaded-literal!tail-grown` | 149 | 9796 |
| `literal_outlives_image` | literal-custody | cranelift | DEFENDED | `returned:outlives-its-image` | 23 | 9536 |
| `literal_outlives_image` | literal-custody | llvm | DEFENDED | `returned:outlives-its-image` | 141 | 9628 |
| `mod_altered_bytes` | malformed-module | cranelift | DEFENDED | `raw-refused:Malformed, raw-refused:Malformed, raw-refused:Mismatch, raw-refused:NotAdmitted, raw-refused:NotAdmitted` | 16 | 9476 |
| `mod_altered_bytes` | malformed-module | llvm | DEFENDED | `raw-refused:Malformed, raw-refused:Malformed, raw-refused:Mismatch, raw-refused:NotAdmitted, raw-refused:NotAdmitted` | 130 | 9532 |
| `mod_oversized` | malformed-module | cranelift | DEFENDED | `raw-refused:Malformed, raw-refused:Malformed, raw-refused:Malformed, raw-refused:Malformed` | 198 | 58556 |
| `mod_oversized` | malformed-module | llvm | DEFENDED | `raw-refused:Malformed, raw-refused:Malformed, raw-refused:Malformed, raw-refused:Malformed` | 313 | 58572 |
| `mod_truncated` | malformed-module | cranelift | DEFENDED | `raw-refused:Malformed, raw-refused:Malformed, raw-refused:Malformed, raw-refused:Malformed, raw-refused:Malformed` | 17 | 9356 |
| `mod_truncated` | malformed-module | llvm | DEFENDED | `raw-refused:Malformed, raw-refused:Malformed, raw-refused:Malformed, raw-refused:Malformed, raw-refused:Malformed` | 131 | 9396 |
| `own_alias_mut` | ownership | cranelift | DEFENDED | `refused:OWN-9` | 14 | 8992 |
| `own_alias_mut` | ownership | llvm | DEFENDED | `refused:OWN-9` | 17 | 9120 |
| `own_frozen_write` | ownership | cranelift | DEFENDED | `refused:OWN-10` | 14 | 9256 |
| `own_frozen_write` | ownership | llvm | DEFENDED | `refused:OWN-10` | 18 | 9464 |
| `own_use_after_move` | ownership | cranelift | DEFENDED | `refused:OWN-5` | 17 | 8972 |
| `own_use_after_move` | ownership | llvm | DEFENDED | `refused:OWN-5` | 18 | 9256 |
| `own_view_escape` | ownership | cranelift | DEFENDED | `refused:OWN-10` | 13 | 9384 |
| `own_view_escape` | ownership | llvm | DEFENDED | `refused:OWN-10` | 18 | 9540 |
| `ports_bound_churn` | port-exhaustion | cranelift | DEFENDED | `bounded:OutOfTime` | 960 | 9880 |
| `ports_bound_churn` | port-exhaustion | llvm | DEFENDED | `bounded:OutOfTime` | 1139 | 10060 |
| `ports_hoard` | port-exhaustion | cranelift | DEFENDED | `bounded:Capacity` | 22 | 10328 |
| `ports_hoard` | port-exhaustion | llvm | DEFENDED | `bounded:Capacity` | 141 | 10416 |
| `spin_clone_compare` | metering | cranelift | DEFENDED | `bounded:OutOfTime` | 25 | 9620 |
| `spin_clone_compare` | metering | llvm | DEFENDED | `bounded:OutOfTime` | 148 | 9600 |
| `spin_tight_loop` | metering | cranelift | DEFENDED | `bounded:OutOfTime` | 19 | 9560 |
| `spin_tight_loop` | metering | llvm | DEFENDED | `bounded:OutOfTime` | 127 | 9464 |
| `spin_unpaid_repeat` | metering | cranelift | DEFENDED | `bounded:OutOfTime` | 109 | 13420 |
| `spin_unpaid_repeat` | metering | llvm | DEFENDED | `bounded:OutOfTime` | 227 | 13568 |
| `stack_deep_recursion` | stack-exhaustion | cranelift | DEFENDED | `bounded:Stack` | 22 | 16772 |
| `stack_deep_recursion` | stack-exhaustion | llvm | DEFENDED | `bounded:Stack` | 142 | 16156 |
| `stack_glue_deep_chain` | stack-exhaustion | cranelift | DEFENDED | `returned:glue-survived` | 157 | 48092 |
| `stack_glue_deep_chain` | stack-exhaustion | llvm | DEFENDED | `returned:glue-survived` | 274 | 48312 |
| `stack_mutual_recursion` | stack-exhaustion | cranelift | DEFENDED | `bounded:Stack` | 25 | 16320 |
| `stack_mutual_recursion` | stack-exhaustion | llvm | DEFENDED | `bounded:Stack` | 142 | 15692 |
| `type_doubling_alias` | type-expansion | cranelift | DEFENDED | `refused:resource` | 12 | 9268 |
| `type_doubling_alias` | type-expansion | llvm | DEFENDED | `refused:resource` | 17 | 9388 |
| `type_generic_instantiation` | type-expansion | cranelift | DEFENDED | `refused:resource` | 20 | 9108 |
| `type_generic_instantiation` | type-expansion | llvm | DEFENDED | `refused:resource` | 23 | 9536 |
| `type_inferred_doubling` | type-expansion | cranelift | DEFENDED | `refused:resource` | 255 | 9344 |
| `type_inferred_doubling` | type-expansion | llvm | DEFENDED | `refused:resource` | 260 | 9548 |
| `type_inline_nesting` | type-expansion | cranelift | DEFENDED | `refused:resource` | 13 | 8988 |
| `type_inline_nesting` | type-expansion | llvm | DEFENDED | `refused:resource` | 16 | 9448 |
