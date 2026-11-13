//! Frozen runtime-prefix fast paths. Only allocation/refill, nonlocal payers,
//! compound resource construction and last-holder cleanup cross the C ABI.
// The shared abi_layout.rs documents the runtime raw-payer equality sequence:
// refund current directly on exact word equality, otherwise resolve the original
// payer. Outstanding bytes pin the original slot across exit/re-entry. Native
// tag bits may cause a safe slow-path false negative. No offset changes.
use super::memory_layout::*;
use super::*;

impl FunctionEmitter<'_> {
    pub(super) fn field_pointer(&mut self, p: &str, offset: usize) -> String {
        if offset == 0 {
            p.into()
        } else {
            self.calc(format!("getelementptr i8, ptr {p}, i64 {offset}"))
        }
    }
    fn load_native(&mut self, p: &str, offset: usize, ty: &str) -> String {
        let at = self.field_pointer(p, offset);
        self.calc(format!("load {ty}, ptr {at}, align 8"))
    }
    fn store_native(&mut self, p: &str, offset: usize, ty: &str, value: &str) {
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
                .map(|(t, _)| t.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ));
        self.line(format!(
            "call void @{name}({}) cold",
            args.iter()
                .map(|(t, v)| format!("{t} {v}"))
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
        writeln!(self.code, "arc_event_call{n}:").unwrap();
        let mut args = vec![("ptr".into(), "%context".into())];
        args.extend_from_slice(arguments);
        self.cold_void(name, &args);
        self.line(format!("br label %arc_event_done{n}"));
        writeln!(self.code, "arc_event_done{n}:").unwrap();
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
        (self.calc(format!("and i1 {fits}, {allowed}")), available)
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
        let admitted = self.calc(format!("zext i1 {fits} to i8"));
        let room = self.calc(format!("select i1 {fits}, i64 0, i64 {available}"));
        (admitted, room)
    }
    pub(super) fn fast_allocate(
        &mut self,
        kind: AllocKind,
        place: AllocPlace,
        charge: &str,
        bytes: &str,
        align: u64,
        overflow: Option<ValueId>,
    ) -> Option<(String, String)> {
        if matches!(kind, AllocKind::Resource { .. }) {
            return None;
        }
        if let AllocPlace::Region(region) = place {
            if overflow.is_some() {
                return None;
            }
            let region = self.get(region);
            return Some(self.fast_region(&region, charge, bytes, align));
        }
        if !matches!(place, AllocPlace::Current | AllocPlace::Like(_)) || align > 512 {
            return None;
        }
        let graph = kind != AllocKind::Plain;
        let prefix = if graph {
            GRAPH_PREFIX_BYTES
        } else {
            PLAIN_PREFIX_BYTES
        } as u64;
        let alignment = align.max(8);
        let offset = prefix.next_multiple_of(alignment);
        let maximum = ((i64::MAX as u64) & !(alignment - 1)) - offset;
        let n = self.next;
        self.next += 1;
        let payer = self.current_payer();
        let like = match place {
            AllocPlace::Like(v) => self.get(v),
            _ => "null".into(),
        };
        let overflow = overflow.map_or_else(
            || "false".into(),
            |v| {
                let flag = self.get(v);
                self.calc(format!("icmp ne i8 {flag}, 0"))
            },
        );
        let exact = self.calc(format!("xor i1 {overflow}, true"));
        let positive = self.calc(format!("icmp ne i64 {charge}, 0"));
        let sized = self.calc(format!("icmp ule i64 {bytes}, {maximum}"));
        let eligible = self.calc(format!("and i1 {positive}, {sized}"));
        let eligible = self.calc(format!("and i1 {eligible}, {exact}"));
        self.likely(
            &eligible,
            &format!("allocation_payer{n}"),
            &format!("allocation_slow{n}"),
        );
        writeln!(self.code, "allocation_payer{n}:").unwrap();
        if matches!(place, AllocPlace::Like(_)) {
            let empty = self.calc(format!("icmp eq ptr {like}, null"));
            self.likely(
                &empty,
                &format!("admit{n}"),
                &format!("allocation_header{n}"),
            );
            writeln!(self.code, "allocation_header{n}:").unwrap();
            let header = self.calc(format!(
                "getelementptr i8, ptr {like}, i64 -{OWN_PAYER_BACK}"
            ));
            let raw = self.calc(format!("load i64, ptr {header}, align 8"));
            let original = self.calc(format!("and i64 {raw}, {}", !(OWN_TAG_MASK as u64)));
            let current = self.calc(format!("ptrtoint ptr {payer} to i64"));
            let same = self.calc(format!("icmp eq i64 {original}, {current}"));
            self.likely(&same, &format!("admit{n}"), &format!("allocation_slow{n}"));
        } else {
            self.line(format!("br label %admit{n}"));
        }
        writeln!(self.code, "admit{n}:").unwrap();
        let available = self.load_native(&payer, LIMIT_AVAILABLE, "i64");
        let fits = self.calc(format!("icmp ule i64 {charge}, {available}"));
        // Real refusal uses the one runtime fallback, retaining Capacity events.
        // Probe only a fitting admission, so tests never count an attempt twice.
        self.likely(
            &fits,
            &format!("allocation_probe{n}"),
            &format!("allocation_slow{n}"),
        );
        writeln!(self.code, "allocation_probe{n}:").unwrap();
        let allowed = self.admission_probe(charge);
        self.likely(&allowed, &format!("allocate{n}"), &format!("refused{n}"));
        writeln!(self.code, "allocate{n}:").unwrap();
        self.admit_balance(&payer, charge, &available);
        let zero = self.calc(format!("icmp eq i64 {bytes}, 0"));
        let nonzero = self.calc(format!("select i1 {zero}, i64 1, i64 {bytes}"));
        let native_bytes = self.calc(format!("add i64 {nonzero}, {offset}"));
        let base = self.intrinsic(
            "hirsel_core_native_alloc",
            "ptr",
            &[
                ("i64".into(), native_bytes),
                ("i64".into(), alignment.to_string()),
            ],
        );
        let payload = self.calc(format!("getelementptr i8, ptr {base}, i64 {offset}"));
        let tag = ((alignment.trailing_zeros() - 3) << 2) | u32::from(graph);
        let tagged = self.calc(format!("getelementptr i8, ptr {payer}, i64 {tag}"));
        let header = self.calc(format!(
            "getelementptr i8, ptr {payload}, i64 -{OWN_PAYER_BACK}"
        ));
        self.line(format!("store ptr {tagged}, ptr {header}, align 8"));
        self.line(format!("br label %allocation_done{n}"));
        writeln!(self.code, "refused{n}:").unwrap();
        self.line(format!("br label %allocation_done{n}"));
        writeln!(self.code, "allocation_slow{n}:").unwrap();
        let (slow, room) = self.allocation_cold(
            "hirsel_core_alloc",
            &[
                ("ptr".into(), "%context".into()),
                ("ptr".into(), like),
                ("i64".into(), bytes.into()),
                ("i64".into(), align.to_string()),
                ("i64".into(), charge.into()),
                ("i1 zeroext".into(), overflow),
                ("i8 zeroext".into(), u8::from(graph).to_string()),
            ],
        );
        self.line(format!("br label %allocation_done{n}"));
        writeln!(self.code, "allocation_done{n}:").unwrap();
        let pointer = self.calc(format!(
            "phi ptr [{payload}, %allocate{n}], [null, %refused{n}], [{slow}, %allocation_slow{n}]"
        ));
        let room = self.calc(format!(
            "phi i64 [0, %allocate{n}], [{available}, %refused{n}], [{room}, %allocation_slow{n}]"
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
        writeln!(self.code, "region_admit{n}:").unwrap();
        let (fits, available) = self.admission_test(&payer, charge);
        self.likely(
            &fits,
            &format!("region_bump{n}"),
            &format!("region_refused{n}"),
        );
        writeln!(self.code, "region_bump{n}:").unwrap();
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
        writeln!(self.code, "region_store{n}:").unwrap();
        let next_pointer = self.calc(format!("getelementptr i8, ptr {result}, i64 {size}"));
        self.store_native(region, REGION_CURSOR, "ptr", &next_pointer);
        self.line(format!("br label %region_done{n}"));
        writeln!(self.code, "region_refill{n}:").unwrap();
        self.intrinsics
            .insert("declare ptr @hirsel_core_region_refill(ptr, i64, i64)".into());
        let refill = self.calc(format!(
            "call ptr @hirsel_core_region_refill(ptr {region}, i64 {bytes}, i64 {align}) cold"
        ));
        self.line(format!("br label %region_done{n}"));
        writeln!(self.code, "region_refused{n}:").unwrap();
        self.line(format!("br label %region_done{n}"));
        writeln!(self.code, "region_slow{n}:").unwrap();
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
        writeln!(self.code, "region_done{n}:").unwrap();
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
        writeln!(self.code, "free_local{n}:").unwrap();
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
        writeln!(self.code, "free_slow{n}:").unwrap();
        self.cold_void(
            "hirsel_core_free_value",
            &[
                ("ptr".into(), pointer.into()),
                ("i64".into(), charge.into()),
            ],
        );
        self.line(format!("br label %freed{n}"));
        writeln!(self.code, "freed{n}:").unwrap();
    }
    pub(super) fn share_node(&mut self, node: &str, atomic: bool) {
        let n = self.next;
        self.next += 1;
        self.line(format!("br label %retain{n}"));
        writeln!(self.code, "retain{n}:").unwrap();
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
        writeln!(self.code, "count_stop{n}:").unwrap();
        self.cold_void(
            "hirsel_core_share",
            &[
                ("ptr".into(), node.into()),
                ("i1 zeroext".into(), atomic.to_string()),
            ],
        );
        self.line("unreachable");
        writeln!(self.code, "retain_store{n}:").unwrap();
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
        writeln!(self.code, "retained{n}:").unwrap();
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
    pub(super) fn release_fast(&mut self, node: &str, charge: u64, recipe: &str, atomic: bool) {
        let n = self.next;
        self.next += 1;
        let last = self.decrement_node(node, atomic);
        let retained = self.calc(format!("xor i1 {last}, true"));
        self.likely(
            &retained,
            &format!("release_nonlast{n}"),
            &format!("release_last{n}"),
        );
        writeln!(self.code, "release_nonlast{n}:").unwrap();
        if atomic {
            self.arc_event(
                "hirsel_core_arc_released",
                &[("i1 zeroext".into(), "false".into())],
            );
        }
        self.line(format!("br label %released{n}"));
        writeln!(self.code, "release_last{n}:").unwrap();
        if atomic {
            self.line("fence acquire");
            self.arc_event(
                "hirsel_core_arc_released",
                &[("i1 zeroext".into(), "true".into())],
            );
        }
        self.cold_void(
            "hirsel_core_release_last",
            &[
                ("ptr".into(), "%context".into()),
                ("ptr".into(), node.into()),
                ("i64".into(), charge.to_string()),
                ("ptr".into(), recipe.into()),
            ],
        );
        self.line(format!("br label %released{n}"));
        writeln!(self.code, "released{n}:").unwrap();
    }
}
