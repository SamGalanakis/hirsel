//! Frozen runtime-prefix fast paths. Only allocation/refill, nonlocal payers,
//! compound resource construction and last-holder cleanup cross the C ABI.
// The shared abi_layout.rs documents the runtime raw-payer equality sequence:
// refund current directly on exact word equality, otherwise resolve the original
// payer. Outstanding bytes pin the original slot across exit/re-entry. Native
// tag bits may cause a safe slow-path false negative. No offset changes.
use super::memory_layout::*;
use super::storage::Payer;
use super::*;

impl<S: sink::Sink> FunctionEmitter<'_, S> {
    pub(super) fn field_pointer(&mut self, p: &str, offset: usize) -> String {
        if offset == 0 {
            p.into()
        } else {
            self.calc(format!("getelementptr i8, ptr {p}, i64 {offset}"))
        }
    }
    pub(super) fn load_native(&mut self, p: &str, offset: usize, ty: &str) -> String {
        let at = self.field_pointer(p, offset);
        self.calc(format!("load {ty}, ptr {at}, align 8"))
    }
    pub(super) fn store_native(&mut self, p: &str, offset: usize, ty: &str, value: &str) {
        let at = self.field_pointer(p, offset);
        self.line(format!("store {ty} {value}, ptr {at}, align 8"));
    }
    fn current_payer(&mut self) -> String {
        self.load_native("%context", CONTEXT_CURRENT_LIMIT, "ptr")
    }
    pub(super) fn likely(&mut self, test: &str, hot: &str, cold: &str) {
        self.intrinsics
            .insert("!0 = !{!\"branch_weights\", i32 2000, i32 1}".into());
        self.line(format!(
            "br i1 {test}, label %{hot}, label %{cold}, !prof !0"
        ));
    }
    pub(super) fn cold_void(&mut self, name: &str, args: &[(String, String)]) {
        self.intrinsics.insert(format!(
            "declare void @{name}({})",
            args.iter()
                .map(|(t, _)| self.c_argument(t))
                .collect::<Vec<_>>()
                .join(", ")
        ));
        self.line(format!(
            "call void @{name}({}) cold",
            args.iter()
                .map(|(t, v)| format!("{} {v}", self.c_argument(t)))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    /// Every inline Arc operation has one immutable context gate. No sink pays
    /// one test; a recorded run calls the context-first hook on the cold branch.
    pub(super) fn arc_event(&mut self, name: &str, arguments: &[(String, String)]) {
        let n = self.next;
        self.next += 1;
        let at = self.field_pointer("%context", CONTEXT_EVENTS);
        let invariant = if self.facts {
            self.intrinsics.insert("!1 = !{}".into());
            ", !invariant.load !1"
        } else {
            ""
        };
        let events = self.calc(format!("load i64, ptr {at}, align 8{invariant}"));
        let empty = self.calc(format!("icmp eq i64 {events}, 0"));
        self.likely(
            &empty,
            &format!("arc_event_done{n}"),
            &format!("arc_event_call{n}"),
        );
        self.sink
            .block(&format!("arc_event_call{n}:").trim_end_matches(':'));
        let mut args = vec![("ptr".into(), "%context".into())];
        args.extend_from_slice(arguments);
        self.cold_void(name, &args);
        self.line(format!("br label %arc_event_done{n}"));
        self.sink
            .block(&format!("arc_event_done{n}:").trim_end_matches(':'));
    }
    fn admission_probe(&mut self, charge: &str) -> String {
        #[cfg(test)]
        {
            self.intrinsic(
                "hirsel_core_test_probe_admission",
                "i1",
                &[
                    ("ptr".into(), "%context".into()),
                    ("i64".into(), charge.into()),
                ],
            )
        }
        #[cfg(not(test))]
        {
            let _ = charge;
            "true".into()
        }
    }
    fn admission_test(&mut self, payer: &str, charge: &str) -> (String, String) {
        let allowed = self.admission_probe(charge);
        let available = self.load_native(payer, LIMIT_AVAILABLE, "i64");
        let fits = self.calc(format!("icmp ule i64 {charge}, {available}"));
        let fits = if cfg!(test) {
            self.calc(format!("and i1 {fits}, {allowed}"))
        } else {
            fits
        };
        (fits, available)
    }
    fn admit_balance(&mut self, payer: &str, charge: &str, available: &str) {
        let left = self.calc(format!("sub i64 {available}, {charge}"));
        self.store_native(payer, LIMIT_AVAILABLE, "i64", &left);
        let outstanding = self.load_native(payer, LIMIT_OUTSTANDING, "i64");
        let used = self.calc(format!("add i64 {outstanding}, {charge}"));
        self.store_native(payer, LIMIT_OUTSTANDING, "i64", &used);
        if self.observe_charge {
            self.cold_void("hirsel_core_observe_charge", &[]);
        }
    }
    /// Empty scopes have no admission or external observer between entry/exit.
    /// Their reservation and refund cancel; the source Capacity check remains.
    pub(super) fn empty_bound(&mut self, charge: &str) -> (String, String) {
        let payer = self.current_payer();
        let (fits, available) = self.admission_test(&payer, charge);
        let admitted = self.typed_cast_llvm(
            sink::Cast::ZExt,
            sink::Type::I1,
            &fits,
            Ty::Scalar(Scalar::U8),
        );
        let room = self.calc(format!("select i1 {fits}, i64 0, i64 {available}"));
        (admitted, room)
    }
    #[allow(clippy::too_many_arguments)]
    pub(super) fn fast_allocate(
        &mut self,
        resource: bool,
        native_kind: u8,
        place: &Payer,
        charge: &str,
        bytes: &str,
        align: u64,
        overflow: Option<ValueId>,
        owned_like: bool,
    ) -> Option<(String, String)> {
        if resource {
            return None;
        }
        if let Payer::Region(region) = place {
            if overflow.is_some() {
                return None;
            }
            return Some(self.fast_region(region, charge, bytes, align));
        }
        if align > 512 {
            return None;
        }
        let prefix = own_prefix(native_kind) as u64;
        let alignment = align.max(8);
        let offset = prefix.next_multiple_of(alignment);
        let maximum = ((i64::MAX as u64) & !(alignment - 1)) - offset;
        let n = self.next;
        self.next += 1;
        let payer = self.current_payer();
        let like = match place {
            Payer::Like(old) => old.clone(),
            _ => "null".into(),
        };
        let overflow = overflow.map_or_else(
            || "false".into(),
            |v| {
                let flag = self.numeric(v);
                self.calc(format!("icmp ne i8 {flag}, 0"))
            },
        );
        let sized = self.calc(format!("icmp ule i64 {bytes}, {maximum}"));
        let eligible = if overflow == "false" {
            sized
        } else {
            let exact = self.calc(format!("xor i1 {overflow}, true"));
            self.calc(format!("and i1 {sized}, {exact}"))
        };
        self.likely(
            &eligible,
            &if matches!(place, Payer::Like(_)) {
                format!("allocation_payer{n}")
            } else {
                format!("admit{n}")
            },
            &format!("allocation_slow{n}"),
        );
        if matches!(place, Payer::Like(_)) {
            self.sink
                .block(&format!("allocation_payer{n}:").trim_end_matches(':'));
            if !owned_like {
                let empty = self.calc(format!("icmp eq ptr {like}, null"));
                self.likely(
                    &empty,
                    &format!("admit{n}"),
                    &format!("allocation_header{n}"),
                );
                self.sink
                    .block(&format!("allocation_header{n}:").trim_end_matches(':'));
            }
            let header = self.calc(format!(
                "getelementptr i8, ptr {like}, i64 -{OWN_PAYER_BACK}"
            ));
            let raw = self.calc(format!("load i64, ptr {header}, align 8"));
            let original = self.calc(format!("and i64 {raw}, {}", !(OWN_TAG_MASK as u64)));
            let current = self.calc(format!("ptrtoint ptr {payer} to i64"));
            let same = self.calc(format!("icmp eq i64 {original}, {current}"));
            self.likely(&same, &format!("admit{n}"), &format!("allocation_slow{n}"));
        }
        self.sink.block(&format!("admit{n}:").trim_end_matches(':'));
        let available = self.load_native(&payer, LIMIT_AVAILABLE, "i64");
        // Unsigned charge-1 < room is exactly charge != 0 && charge <= room,
        // including room=MAX. Zero retains the runtime's payer pinning path.
        let last = self.calc(format!("sub i64 {charge}, 1"));
        let fits = self.calc(format!("icmp ult i64 {last}, {available}"));
        // Real refusal uses the one runtime fallback, retaining Capacity events.
        // Probe only a fitting admission, so tests never count an attempt twice.
        self.likely(
            &fits,
            &if cfg!(test) {
                format!("allocation_probe{n}")
            } else {
                format!("allocate{n}")
            },
            &format!("allocation_slow{n}"),
        );
        // Refusal injection exists only in the test engine. Production needs
        // neither its always-true branch nor its unreachable refusal join.
        if cfg!(test) {
            self.sink
                .block(&format!("allocation_probe{n}:").trim_end_matches(':'));
            let allowed = self.admission_probe(charge);
            self.likely(&allowed, &format!("allocate{n}"), &format!("refused{n}"));
        }
        self.sink
            .block(&format!("allocate{n}:").trim_end_matches(':'));
        self.admit_balance(&payer, charge, &available);
        let zero = self.calc(format!("icmp eq i64 {bytes}, 0"));
        let nonzero = self.calc(format!("select i1 {zero}, i64 1, i64 {bytes}"));
        let native_bytes = self.calc(format!("add i64 {nonzero}, {offset}"));
        let pointer_type = if self.facts { "nonnull ptr" } else { "ptr" };
        let base = if alignment <= 16 {
            self.intrinsic(
                "hirsel_core_native_alloc_ordinary",
                pointer_type,
                &[("i64".into(), native_bytes)],
            )
        } else {
            self.intrinsic(
                "hirsel_core_native_alloc",
                pointer_type,
                &[
                    ("i64".into(), native_bytes),
                    ("i64".into(), alignment.to_string()),
                ],
            )
        };
        // The native allocator returns a base or stops. The admitted native
        // extent contains this payload offset; it cannot yield a null payload.
        let inbounds = if self.facts { " inbounds" } else { "" };
        let payload = self.calc(format!(
            "getelementptr{inbounds} i8, ptr {base}, i64 {offset}"
        ));
        let tag = ((alignment.trailing_zeros() - 3) << 2) | u32::from(native_kind);
        let tagged = self.calc(format!("getelementptr i8, ptr {payer}, i64 {tag}"));
        let header = self.calc(format!(
            "getelementptr i8, ptr {payload}, i64 -{OWN_PAYER_BACK}"
        ));
        self.line(format!("store ptr {tagged}, ptr {header}, align 8"));
        self.line(format!("br label %allocation_done{n}"));
        if cfg!(test) {
            self.sink
                .block(&format!("refused{n}:").trim_end_matches(':'));
            self.line(format!("br label %allocation_done{n}"));
        }
        self.sink
            .block(&format!("allocation_slow{n}:").trim_end_matches(':'));
        let (slow, room) = self.allocation_cold(
            "hirsel_core_alloc",
            &[
                ("ptr".into(), "%context".into()),
                ("ptr".into(), like),
                ("i64".into(), bytes.into()),
                ("i64".into(), align.to_string()),
                ("i64".into(), charge.into()),
                ("i1 zeroext".into(), overflow),
                ("i8 zeroext".into(), native_kind.to_string()),
            ],
        );
        self.line(format!("br label %allocation_done{n}"));
        self.sink
            .block(&format!("allocation_done{n}:").trim_end_matches(':'));
        let refused_pointer = if cfg!(test) {
            format!(", [null, %refused{n}]")
        } else {
            String::new()
        };
        let refused_room = if cfg!(test) {
            format!(", [{available}, %refused{n}]")
        } else {
            String::new()
        };
        let pointer = self.calc(format!(
            "phi ptr [{payload}, %allocate{n}], [{slow}, %allocation_slow{n}]{refused_pointer}"
        ));
        let room = self.calc(format!(
            "phi i64 [0, %allocate{n}], [{room}, %allocation_slow{n}]{refused_room}"
        ));
        Some((pointer, room))
    }
    fn fast_region(
        &mut self,
        region: &str,
        charge: &str,
        bytes: &str,
        align: u64,
    ) -> (String, String) {
        let n = self.next;
        self.next += 1;
        let payer = self.current_payer();
        let saved = self.load_native(region, REGION_PAYER, "ptr");
        let accumulated = self.load_native(region, REGION_CHARGE, "i64");
        let same = self.calc(format!("icmp eq ptr {saved}, {payer}"));
        // A fully trimmed entry no longer pins its payer. Pointer equality alone
        // would let a recycled slot masquerade as this run's live payer.
        let live = self.calc(format!("icmp ne i64 {accumulated}, 0"));
        let positive = self.calc(format!("icmp ne i64 {charge}, 0"));
        let reusable = self.calc(format!("and i1 {same}, {live}"));
        let reusable = self.calc(format!("and i1 {reusable}, {positive}"));
        self.likely(
            &reusable,
            &format!("region_admit{n}"),
            &format!("region_slow{n}"),
        );
        self.sink
            .block(&format!("region_admit{n}:").trim_end_matches(':'));
        let (fits, available) = self.admission_test(&payer, charge);
        self.likely(
            &fits,
            &format!("region_bump{n}"),
            &format!("region_refused{n}"),
        );
        self.sink
            .block(&format!("region_bump{n}:").trim_end_matches(':'));
        self.admit_balance(&payer, charge, &available);
        let total = self.calc(format!("add i64 {accumulated}, {charge}"));
        self.store_native(region, REGION_CHARGE, "i64", &total);
        let cursor = self.load_native(region, REGION_CURSOR, "ptr");
        let end = self.load_native(region, REGION_END, "ptr");
        let address = self.calc(format!("ptrtoint ptr {cursor} to i64"));
        let rounded = self.calc(format!("add i64 {address}, {}", align - 1));
        let start = self.calc(format!("and i64 {rounded}, -{align}"));
        let offset = self.calc(format!("sub i64 {start}, {address}"));
        let result = self.calc(format!("getelementptr i8, ptr {cursor}, i64 {offset}"));
        let zero = self.calc(format!("icmp eq i64 {bytes}, 0"));
        let size = self.calc(format!("select i1 {zero}, i64 1, i64 {bytes}"));
        let next = self.calc(format!("add i64 {start}, {size}"));
        let wrapped = self.calc(format!("icmp uge i64 {next}, {start}"));
        let limit = self.calc(format!("ptrtoint ptr {end} to i64"));
        let room = self.calc(format!("icmp ule i64 {next}, {limit}"));
        let fits = self.calc(format!("and i1 {room}, {wrapped}"));
        self.likely(
            &fits,
            &format!("region_store{n}"),
            &format!("region_refill{n}"),
        );
        self.sink
            .block(&format!("region_store{n}:").trim_end_matches(':'));
        let next_pointer = self.calc(format!("getelementptr i8, ptr {result}, i64 {size}"));
        self.store_native(region, REGION_CURSOR, "ptr", &next_pointer);
        self.line(format!("br label %region_done{n}"));
        self.sink
            .block(&format!("region_refill{n}:").trim_end_matches(':'));
        self.intrinsics
            .insert("declare ptr @hirsel_core_region_refill(ptr, i64, i64)".into());
        let refill = self.calc(format!(
            "call ptr @hirsel_core_region_refill(ptr {region}, i64 {bytes}, i64 {align}) cold"
        ));
        self.line(format!("br label %region_done{n}"));
        self.sink
            .block(&format!("region_refused{n}:").trim_end_matches(':'));
        self.line(format!("br label %region_done{n}"));
        self.sink
            .block(&format!("region_slow{n}:").trim_end_matches(':'));
        let (slow, slow_room) = self.allocation_cold(
            "hirsel_core_region_alloc",
            &[
                ("ptr".into(), "%context".into()),
                ("ptr".into(), region.into()),
                ("i64".into(), bytes.into()),
                ("i64".into(), align.to_string()),
                ("i64".into(), charge.into()),
                ("i1 zeroext".into(), "false".into()),
            ],
        );
        self.line(format!("br label %region_done{n}"));
        self.sink
            .block(&format!("region_done{n}:").trim_end_matches(':'));
        let pointer = self.calc(format!("phi ptr [{result}, %region_store{n}], [{refill}, %region_refill{n}], [null, %region_refused{n}], [{slow}, %region_slow{n}]"));
        let room = self.calc(format!("phi i64 [0, %region_store{n}], [0, %region_refill{n}], [{available}, %region_refused{n}], [{slow_room}, %region_slow{n}]"));
        (pointer, room)
    }
    pub(super) fn free_block(&mut self, pointer: &str, charge: &str) {
        let n = self.next;
        self.next += 1;
        let at = self.calc(format!(
            "getelementptr i8, ptr {pointer}, i64 -{OWN_PAYER_BACK}"
        ));
        let tagged = self.calc(format!("load ptr, ptr {at}, align 8"));
        let word = self.calc(format!("ptrtoint ptr {tagged} to i64"));
        let current = self.current_payer();
        let current_word = self.calc(format!("ptrtoint ptr {current} to i64"));
        // Exact raw equality with the canonical current pointer, optionally its
        // ordinary GRAPH tag, proves both local custody and the native layout.
        let tag = self.calc(format!("xor i64 {word}, {current_word}"));
        let ordinary = self.calc(format!("icmp ule i64 {tag}, {OWN_ORDINARY_MAX_TAG}"));
        let positive = self.calc(format!("icmp ne i64 {charge}, 0"));
        let fast = self.calc(format!("and i1 {ordinary}, {positive}"));
        self.likely(&fast, &format!("free_local{n}"), &format!("free_slow{n}"));
        self.sink
            .block(&format!("free_local{n}:").trim_end_matches(':'));
        let available = self.load_native(&current, LIMIT_AVAILABLE, "i64");
        let restored = self.calc(format!("add i64 {available}, {charge}"));
        self.store_native(&current, LIMIT_AVAILABLE, "i64", &restored);
        let outstanding = self.load_native(&current, LIMIT_OUTSTANDING, "i64");
        let remaining = self.calc(format!("sub i64 {outstanding}, {charge}"));
        self.store_native(&current, LIMIT_OUTSTANDING, "i64", &remaining);
        let plain = self.calc(format!("icmp eq i64 {tag}, {OWN_ORDINARY_PLAIN_TAG}"));
        let offset = self.calc(format!(
            "select i1 {plain}, i64 {PLAIN_PREFIX_BYTES}, i64 {GRAPH_PREFIX_BYTES}"
        ));
        let neg = self.calc(format!("sub i64 0, {offset}"));
        let base = self.calc(format!("getelementptr i8, ptr {pointer}, i64 {neg}"));
        self.runtime_void("hirsel_core_native_free", &[("ptr".into(), base)]);
        self.line(format!("br label %freed{n}"));
        self.sink
            .block(&format!("free_slow{n}:").trim_end_matches(':'));
        let fixed = self.calc(format!("icmp eq i64 {tag}, {NATIVE_STATIC}"));
        let fixed = self.calc(format!("and i1 {fixed}, {positive}"));
        self.line(format!(
            "br i1 {fixed}, label %free_static{n}, label %free_generic{n}"
        ));
        self.sink
            .block(&format!("free_static{n}:").trim_end_matches(':'));
        let available = self.load_native(&current, LIMIT_AVAILABLE, "i64");
        let restored = self.calc(format!("add i64 {available}, {charge}"));
        self.store_native(&current, LIMIT_AVAILABLE, "i64", &restored);
        let outstanding = self.load_native(&current, LIMIT_OUTSTANDING, "i64");
        let remaining = self.calc(format!("sub i64 {outstanding}, {charge}"));
        self.store_native(&current, LIMIT_OUTSTANDING, "i64", &remaining);
        let base = self.calc(format!(
            "getelementptr i8, ptr {pointer}, i64 -{STATIC_PREFIX_BYTES}"
        ));
        self.runtime_void("hirsel_core_native_free", &[("ptr".into(), base)]);
        self.line(format!("br label %freed{n}"));
        self.sink
            .block(&format!("free_generic{n}:").trim_end_matches(':'));

        self.cold_void(
            "hirsel_core_free_value",
            &[
                ("ptr".into(), pointer.into()),
                ("i64".into(), charge.into()),
            ],
        );
        self.line(format!("br label %freed{n}"));
        self.sink.block(&format!("freed{n}:").trim_end_matches(':'));
    }
    pub(super) fn share_node(&mut self, node: &str, atomic: bool) {
        let n = self.next;
        self.next += 1;
        self.line(format!("br label %retain{n}"));
        self.sink
            .block(&format!("retain{n}:").trim_end_matches(':'));
        let count = self.calc(if atomic {
            format!("load atomic i64, ptr {node} monotonic, align 8")
        } else {
            format!("load i64, ptr {node}, align 8")
        });
        let fits = self.calc(format!("icmp ne i64 {count}, -1"));
        self.likely(
            &fits,
            &format!("retain_store{n}"),
            &format!("count_stop{n}"),
        );
        self.sink
            .block(&format!("count_stop{n}:").trim_end_matches(':'));
        self.cold_void(
            "hirsel_core_share",
            &[
                ("ptr".into(), node.into()),
                ("i1 zeroext".into(), atomic.to_string()),
            ],
        );
        self.line("unreachable");
        self.sink
            .block(&format!("retain_store{n}:").trim_end_matches(':'));
        let next = self.calc(format!("add i64 {count}, 1"));
        if atomic {
            let exchange = self.calc(format!(
                "cmpxchg weak ptr {node}, i64 {count}, i64 {next} monotonic monotonic"
            ));
            let changed = self.calc(format!("extractvalue {{i64, i1}} {exchange}, 1"));
            self.likely(&changed, &format!("retained{n}"), &format!("retain{n}"));
        } else {
            self.line(format!("store i64 {next}, ptr {node}, align 8"));
            self.line(format!("br label %retained{n}"));
        }
        self.sink
            .block(&format!("retained{n}:").trim_end_matches(':'));
        if atomic {
            self.arc_event("hirsel_core_arc_shared", &[("ptr".into(), node.into())]);
        }
    }
    pub(super) fn unique_node(&mut self, node: &str, atomic: bool) -> String {
        let count = self.calc(if atomic {
            format!("load atomic i64, ptr {node} acquire, align 8")
        } else {
            format!("load i64, ptr {node}, align 8")
        });
        let unique = self.calc(format!("icmp eq i64 {count}, 1"));
        if atomic {
            self.arc_event(
                "hirsel_core_arc_unwrapped",
                &[("i1 zeroext".into(), unique.clone())],
            );
        }
        unique
    }
    pub(super) fn decrement_node(&mut self, node: &str, atomic: bool) -> String {
        if atomic {
            let old = self.calc(format!("atomicrmw sub ptr {node}, i64 1 release, align 8"));
            self.calc(format!("icmp eq i64 {old}, 1"))
        } else {
            let old = self.load_native(node, NODE_COUNT, "i64");
            let next = self.calc(format!("sub i64 {old}, 1"));
            self.store_native(node, NODE_COUNT, "i64", &next);
            self.calc(format!("icmp eq i64 {next}, 0"))
        }
    }
    pub(super) fn release_fast(
        &mut self,
        node: &str,
        charge: u64,
        recipe: Option<&str>,
        atomic: bool,
    ) {
        let n = self.next;
        self.next += 1;
        let last = self.decrement_node(node, atomic);
        let retained = self.calc(format!("xor i1 {last}, true"));
        self.likely(
            &retained,
            &format!("release_nonlast{n}"),
            &format!("release_last{n}"),
        );
        self.sink
            .block(&format!("release_nonlast{n}:").trim_end_matches(':'));
        if atomic {
            self.arc_event(
                "hirsel_core_arc_released",
                &[("i1 zeroext".into(), "false".into())],
            );
        }
        self.line(format!("br label %released{n}"));
        self.sink
            .block(&format!("release_last{n}:").trim_end_matches(':'));
        if atomic {
            self.line("fence acquire");
            self.arc_event(
                "hirsel_core_arc_released",
                &[("i1 zeroext".into(), "true".into())],
            );
        }
        if let Some(recipe) = recipe {
            self.cold_void(
                "hirsel_core_release_static_last",
                &[
                    ("ptr".into(), "%context".into()),
                    ("ptr".into(), node.into()),
                    ("ptr".into(), recipe.into()),
                ],
            );
        } else {
            self.free_block(node, &charge.to_string());
        }
        self.line(format!("br label %released{n}"));
        self.sink
            .block(&format!("released{n}:").trim_end_matches(':'));
    }
}
