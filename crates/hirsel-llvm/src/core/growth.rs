//! One threshold test and the classic triple per site. Large buffers call the
//! shared runtime replacement; no accounting/realloc implementation is cloned.
use super::memory_layout::*;
use super::storage::Payer;
use super::*;
use hirsel_core_layout::{FieldTy, field_size};

impl<S: sink::Sink> FunctionEmitter<'_, S> {
    #[allow(clippy::too_many_arguments)]
    pub(super) fn grow_buffer(
        &mut self,
        edge: FieldTy,
        at: &str,
        old: &str,
        charge: &str,
        size: &str,
        align: u64,
        overflow: Option<ValueId>,
        fail: Option<BlockId>,
        capacity: &str,
        moved: &str,
        refund: &str,
    ) -> (String, String) {
        let FieldTy::Seq(item) = &edge else {
            unreachable!("grow sequence")
        };
        let stride = field_size(&self.module.types, self.recipes, item).0;
        let dispatch = stride != 0;
        let n = self.next;
        self.next += 1;
        let native = if dispatch {
            let cap = self.load_native(at, 16, "i64");
            let large = self.calc(format!(
                "icmp uge i64 {cap}, {}",
                GROW_REALLOC_MIN_PAYLOAD_BYTES.div_ceil(stride)
            ));
            self.line(format!(
                "br i1 {large}, label %grow_native{n}, label %grow_classic{n}"
            ));
            self.sink.block(&format!("grow_native{n}:").trim_end_matches(':'));
            let kind =
                match hirsel_core_layout::cleanup_class(&self.module.types, self.recipes, &edge) {
                    Some(hirsel_core_layout::Cleanup::Terminal) => NATIVE_PLAIN,
                    _ => NATIVE_GRAPH,
                };
            let over = overflow.map_or_else(
                || "false".into(),
                |v| {
                    let flag = self.numeric(v);
                    self.calc(format!("icmp ne i8 {flag}, 0"))
                },
            );
            // The runtime borrows only this call-lifetime carrier. Keep the
            // source carrier's address out of the call so local homes promote.
            let shadow = self.temp("[3 x i64]");
            self.runtime_void("llvm.lifetime.start.p0", &[("ptr".into(), shadow.clone())]);
            let length = self.load_native(at, 8, "i64");
            self.store_native(&shadow, 0, "ptr", old);
            self.store_native(&shadow, 8, "i64", &length);
            self.store_native(&shadow, 16, "i64", &cap);
            let pair = self.intrinsic(
                "hirsel_core_grow",
                "{ ptr, i64 }",
                &[
                    ("ptr".into(), "%context".into()),
                    ("ptr".into(), shadow.clone()),
                    ("i64".into(), size.into()),
                    ("i64".into(), align.to_string()),
                    ("i64".into(), charge.into()),
                    ("i64".into(), capacity.into()),
                    ("i64".into(), moved.into()),
                    ("i64".into(), refund.into()),
                    ("i1 zeroext".into(), over),
                    ("i8 zeroext".into(), kind.to_string()),
                ],
            );
            let block = self.calc(format!("extractvalue {{ ptr, i64 }} {pair}, 0"));
            let room = self.calc(format!("extractvalue {{ ptr, i64 }} {pair}, 1"));
            let present = self.calc(format!("icmp ne ptr {block}, null"));
            self.likely(
                &present,
                &format!("grow_native_copy{n}"),
                &format!("grow_native_refused{n}"),
            );
            self.sink.block(&format!("grow_native_copy{n}:").trim_end_matches(':'));
            // Read the helper's carrier word to avoid threading the requested
            // operand through loop-carried values; publish #93's fact on success.
            let grown_capacity = self.load_native(&shadow, 16, "i64");
            if self.facts {
                let exact = self.calc(format!("icmp eq i64 {grown_capacity}, {capacity}"));
                self.runtime_void("llvm.assume", &[("i1".into(), exact)]);
            }
            self.store_native(at, 0, "ptr", &block);
            self.store_native(at, 16, "i64", &grown_capacity);
            self.runtime_void("llvm.lifetime.end.p0", &[("ptr".into(), shadow.clone())]);
            self.line(format!("br label %grow_done{n}"));
            self.sink.block(&format!("grow_native_refused{n}:").trim_end_matches(':'));
            self.runtime_void("llvm.lifetime.end.p0", &[("ptr".into(), shadow)]);
            self.line(format!("br label %grow_refused{n}"));
            self.sink.block(&format!("grow_classic{n}:").trim_end_matches(':'));
            Some((block, room))
        } else {
            None
        };
        let (block, room) = self.allocate_raw(
            &AllocKind::Own { edge },
            &Payer::Like(old.into()),
            charge,
            size,
            align,
            overflow,
            dispatch,
        );
        self.line(format!("br label %grow_allocated{n}"));
        self.sink.block(&format!("grow_allocated{n}:").trim_end_matches(':'));
        let present = self.calc(format!("icmp ne ptr {block}, null"));
        self.line(format!(
            "br i1 {present}, label %grow_copy{n}, label %grow_refused{n}"
        ));
        self.sink.block(&format!("grow_copy{n}:").trim_end_matches(':'));
        if self.facts {
            let distinct = self.calc(format!("icmp ne ptr {block}, {old}"));
            self.runtime_void("llvm.assume", &[("i1".into(), distinct)]);
        }
        self.runtime_void(
            "llvm.memcpy.p0.p0.i64",
            &[
                ("ptr".into(), block.clone()),
                ("ptr".into(), old.into()),
                ("i64".into(), moved.into()),
                ("i1".into(), "false".into()),
            ],
        );
        self.store_native(at, 0, "ptr", &block);
        self.store_native(at, 16, "i64", capacity);
        self.free_block(old, refund);
        self.line(format!("br label %grow_copied{n}"));
        self.sink.block(&format!("grow_copied{n}:").trim_end_matches(':'));
        self.line(format!("br label %grow_done{n}"));
        self.sink.block(&format!("grow_refused{n}:").trim_end_matches(':'));
        let mut refused = format!("[{room}, %grow_allocated{n}]");
        if let Some((_, room)) = &native {
            write!(refused, ", [{room}, %grow_native_refused{n}]").unwrap();
        }
        let room = self.calc(format!("phi i64 {refused}"));
        self.intrinsics
            .insert("declare i8 @hirsel_core_allocation_status(ptr, ptr)".into());
        let status =
            self.calc("call i8 @hirsel_core_allocation_status(ptr %context, ptr null) cold");
        let failed = self.calc(format!("icmp ne i8 {status}, 0"));
        self.branch_failure(&failed, fail, Some(6));
        self.line(format!("br label %grow_refused_done{n}"));
        self.sink.block(&format!("grow_refused_done{n}:").trim_end_matches(':'));
        self.line(format!("br label %grow_done{n}"));
        self.sink.block(&format!("grow_done{n}:").trim_end_matches(':'));
        let mut pointers = format!("[{block}, %grow_copied{n}], [null, %grow_refused_done{n}]");
        let mut rooms = format!("[0, %grow_copied{n}], [{room}, %grow_refused_done{n}]");
        if let Some((block, _)) = native {
            write!(pointers, ", [{block}, %grow_native_copy{n}]").unwrap();
            write!(rooms, ", [0, %grow_native_copy{n}]").unwrap();
        }
        let block = self.calc(format!("phi ptr {pointers}"));
        let room = self.calc(format!("phi i64 {rooms}"));
        (block, room)
    }
}
