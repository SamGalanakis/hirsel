#[test]
fn shared_foreign_thunk_links_sealed_code_and_runs_from_each_backend() {
    shared_foreign_thunk_fixture(false);
}

fn shared_foreign_thunk_fixture(profile: bool) {
    use crate::code_store::ObjectKind;
    use crate::foreign_archive::LinkArchives;
    use hirsel_core_ir::decode::Debit;
    use hirsel_core_ir::release_recipe::{ForeignFunction, ForeignLeaf, ObjectRef};
    use hirsel_core_layout::ObjectFormat;

    let dir = tempfile::tempdir().unwrap();
    let path = |name| dir.path().join(name);
    let host = crate::host::HOST_TARGET;
    let target = host.triple();
    let mut options = native_options(Backend::Cranelift, dir.path());
    if profile {
        options.runtime = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .join(std::env::var_os("HIRSEL_CORE_RT_PROF_ARCHIVE").unwrap());
    }
    options.foreign_grants.insert("ffi_laws@1/laws/Laws".into());
    let bin = PathBuf::from(std::env::var_os("LLVM_SYS_231_PREFIX").unwrap()).join("bin");
    let adapter = path("adapter.c");
    std::fs::write(
        &adapter,
        r#"
#include <stdint.h>
static uint8_t cell;
static uint64_t released;
#ifdef HFL_PROFILE
#define MEASURE_EFFECT " stack "
#ifdef __MACH__
/* The Darwin sampler walks frame pointers without copying the stack. */
#define FRAME_BYTES 2048
#else
#define FRAME_BYTES (20 * 1024)
#endif
__attribute__((noinline)) static void hfl_profile_hot(volatile uint8_t *frame) {
    volatile uint64_t value = frame[0];
    for (uint64_t i = 0; i < 80000000; i++) value = value * 5 + i;
    frame[FRAME_BYTES - 1] = (uint8_t)value;
}
#else
#define MEASURE_EFFECT " waits "
#endif
uint64_t hfl_measure(int8_t s, float f) {
#ifdef HFL_PROFILE
    volatile uint8_t frame[FRAME_BYTES];
    /* perf's stack copy cannot page in untouched anonymous stack pages. */
    for (unsigned long i = 4096; i < sizeof(frame); i += 4096) frame[i] = 0;
    frame[0] = (uint8_t)s;
    hfl_profile_hot(frame);
    __asm__ volatile("" : : "r"(&frame) : "memory");
#endif
    return s == -3 && f == 1.25f ? 42 : 0;
}
void *hfl_make(void) { return &cell; }
void hfl_free(void *p) { if (p == &cell) released++; }
uint64_t hfl_freed(void) { return released; }
#define ABI "hfl_measure" MEASURE_EFFECT "(i8, f32) -> u64\n" \
            "hfl_make stack () -> new ffi_laws@1/laws/Obj\n" \
            "hfl_free waits (take ffi_laws@1/laws/Obj) -> ()\n" \
            "hfl_freed stack () -> u64\n"
#ifdef __MACH__
#define ABI_SECTION "__TEXT,__hirsel_abi"
#else
#define ABI_SECTION ".hirsel_abi"
#endif
__attribute__((used, section(ABI_SECTION)))
static const char abi[sizeof(ABI) - 1] = ABI;
"#,
    )
    .unwrap();
    command(
        Command::new(bin.join("clang"))
            .args(["-std=c11", "-O2", "-fPIC", "-c"])
            .arg(format!("--target={target}"))
            .args(profile.then_some("-DHFL_PROFILE"))
            .args(profile.then_some("-fasynchronous-unwind-tables"))
            .args(profile.then_some("-fno-omit-frame-pointer"))
            .arg(adapter)
            .arg("-o")
            .arg(path("adapter.o")),
    )
    .unwrap();
    command(
        Command::new(bin.join("llvm-ar"))
            .arg(match host.object_format() {
                ObjectFormat::Elf => "--format=gnu",
                ObjectFormat::MachO => "--format=darwin",
            })
            .arg("rcs")
            .arg(path("adapter.a"))
            .arg(path("adapter.o")),
    )
    .unwrap();
    let bytes = std::fs::read(path("adapter.a")).unwrap();
    let archive = ObjectRef {
        hash: ObjectKind::ForeignArchive.hash(&bytes),
        len: bytes.len() as u64,
    };
    let entry = DefPath::from_full("ffi_laws@1/laws/main").unwrap();
    let identity = Identity::plain(entry.clone());
    let measure = if profile {
        "(core@1/core/Foreign[ffi_laws@1/laws/Laws],i8,f32)->u64"
    } else {
        "(core@1/core/Foreign[ffi_laws@1/laws/Laws],i8,f32)->u64 waits"
    };
    let leaf = ForeignLeaf {
        module: DefPath::from_full("ffi_laws@1/native").unwrap(),
        symbols: "hfl_".into(),
        code: hirsel_core_ir::release_recipe::ForeignCode::Native(vec![]),
        functions: [
            ("measure", measure),
            ("make", "(core@1/core/Foreign[ffi_laws@1/laws/Laws])->core@1/core/Option[core@1/foreign/Ptr[ffi_laws@1/laws/Obj]]"),
            ("free", "(core@1/core/Foreign[ffi_laws@1/laws/Laws],take core@1/foreign/Ptr[ffi_laws@1/laws/Obj])->()waits"),
            ("freed", "(core@1/core/Foreign[ffi_laws@1/laws/Laws])->u64"),
        ].into_iter().map(|(name, header)| ForeignFunction {
            identity: Identity::plain(DefPath::from_full(&format!("ffi_laws@1/native/{name}")).unwrap()),
            header: header.into(),
        }).collect(),
    };
    let mut archives = LinkArchives::default();
    archives
        .admit(archive, &leaf, target, None, &Debit::unlimited(), || {
            Ok(bytes)
        })
        .unwrap();
    let mut module = hirsel_core_ir::text::parse(
        r#"
core v1
type t0 = own "Result" | Ok { } | Error { }
type t1 = own "core@1/foreign/Ptr[ffi_laws@1/laws/Obj]" value u64 { handle: u64 }
type t2 = own "core@1/core/Option[core@1/foreign/Ptr[ffi_laws@1/laws/Obj]]" | None { } | Some { p: t1 }
type t3 = resource "ffi_laws@1/laws/Owner" drop f4 { p: t2 }
sig g0 = (u64) -> () dest t0
sig g1 = (u64, i8, f32) -> (u64)
sig g2 = (u64) -> () dest t2
sig g3 = (u64, u64) -> ()
sig g4 = (ptr) -> ()
sig g5 = (u64) -> (u64)
inst n0 = ffi_laws@1/laws/main [] []
inst n1 = ffi_laws@1/native/measure [] []
inst n2 = ffi_laws@1/native/make [] []
inst n3 = ffi_laws@1/native/free [] []
inst n4 = ffi_laws@1/laws/finalize [] []
inst n5 = ffi_laws@1/native/freed [] []
fn f0 = n0 g0 {
 slot s0 = t3
 addr a0 = v0 : t0
 addr a1 = s0 : t3
 addr a2 = s0 : ptr
 addr a3 = v9 : t3
 addr a4 = a3 field 0 : t2
b0(v0: ptr, v1: u64):
 address a0
 v2: i8 = const -3
 v3: f32 = const 0x3fa00000
 v4: u64 = call f1 g1 (v1, v2, v3)
 v5: u64 = const 42
 v6: bool = eq v4, v5
 branch v6, b1, b3
b1:
 v7: u64 = const 16
 v8: u64 = const 48
 v9: ptr, v10: u64 = alloc resource grant 0 v7 align 8 charge v8
 v11: ptr = const 0x0
 v12: bool = ne v9, v11
 branch v12, b2, b3
b2:
 live s0
 address a1
 address a2
 address a3
 address a4
 call f2 g2 (&a4, v1)
 store a2, v9
 drop a1 t3
 dead s0
 v13: u64 = call f5 g5 (v1)
 v14: u64 = const 1
 v15: bool = eq v13, v14
 branch v15, b4, b3
b3:
 set_case a0, 1
 return
b4:
 set_case a0, 0
 return
}
fn f1 = n1 g1 extern
fn f2 = n2 g2 extern
fn f3 = n3 g3 extern
fn f4 = n4 g4 {
b0(v0: ptr):
 return
}
fn f5 = n5 g5 extern
"#,
    )
    .unwrap();
    let mut sources = hirsel_core_ir::source::SourceMap::default();
    if profile {
        use hirsel_core_ir::{
            Anchor, Inst, ItemId,
            source::{ItemSource, SourceFile},
        };
        module.items.push(entry.clone());
        let item = ItemId((module.items.len() - 1) as u32);
        let body = module.functions[0].body.as_mut().unwrap();
        let at = body
            .insts
            .iter()
            .position(|inst| matches!(inst, Inst::Call { call } if call.index() == 0))
            .unwrap();
        body.inst_anchors.resize(body.insts.len(), None);
        body.inst_anchors[at] = Some(Anchor {
            item,
            start: 5,
            end: 6,
        });
        sources.files.push(SourceFile {
            path: "fixture.hirsel".into(),
            text: "fixture call".into(),
        });
        sources.items.insert(
            entry.clone(),
            ItemSource {
                file: 0,
                file_offset: 3,
            },
        );
    }
    let plan_abi = EntryAbi {
        recipe: hirsel_core_layout::Recipe::clone(
            &hirsel_core_layout::compute(&module.types).unwrap()[0],
        ),
        exit_offset: None,
        error_text: None,
        parameters: vec![EntryParameter {
            ty: "core@1/core/Foreign[ffi_laws@1/laws/Laws]".into(),
            name: "f".into(),
        }],
    };
    let (_, recipe) =
        crate::tool_inputs::toolchain(&options.link, target, &options.runtime).unwrap();
    for backend in [Backend::Cranelift, Backend::Llvm] {
        options.backend = backend;
        let object = emit_unit(
            &module,
            || module.link_symbols(),
            &options,
            ContentHash::default(),
            &identity,
        )
        .unwrap();
        let plan = crate::native_entry::Plan::new(
            identity.symbol(),
            entry.to_string(),
            &plan_abi,
            None,
            false,
            &options,
        )
        .unwrap()
        .with_foreign([&leaf], None);
        let startup = plan
            .emit(
                target,
                &sources,
                ContentHash::default(),
                &[0; 32],
                &[0; 32],
                &[],
                (None, false, (&[], &[])),
            )
            .unwrap();
        std::fs::write(path("unit.o"), object.bytes).unwrap();
        std::fs::write(path("startup.o"), startup).unwrap();
        let mut inputs = vec![path("startup.o"), path("unit.o")];
        archives
            .append_link_inputs(&mut inputs, &[], &options.runtime, &recipe)
            .unwrap();
        inputs.push(options.runtime.clone());
        let output = path("program");
        direct_link(&recipe, &inputs, None, &output, false, false, false).unwrap();
        if profile {
            let profile_path = dir.path().join(format!("foreign-{backend:?}.hprof"));
            let raw = tempfile::NamedTempFile::new_in(dir.path()).unwrap();
            let report = capture_native_profile(&output, &profile_path, Some(raw.path()));
            let db = report.connection();
            let host: i64 = db
                .query_row(
                    "SELECT count(*) FROM cpu_sample s JOIN callsite c ON c.id=s.callsite_id
                 JOIN frame fr ON fr.id=c.frame_id JOIN function f ON f.id=fr.function_id
                 WHERE f.name='hfl_profile_hot' AND s.stack_root='host' AND s.unwind_status='ok'",
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            let attributed: i64 = db
                .query_row(
                    "SELECT count(*) FROM cpu_sample s JOIN callsite c ON c.id=s.callsite_id
                 JOIN frame fr ON fr.id=c.frame_id JOIN function f ON f.id=fr.function_id
                 JOIN host_stack_interval h ON h.id=s.host_stack_interval_id
                 JOIN task t ON t.id=s.task_id
                 WHERE f.name='hfl_profile_hot' AND s.stack_root='host' AND
                       s.unwind_status='ok' AND t.runtime_id=0 AND h.task_id=t.id AND h.site=9",
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(
                attributed, host,
                "{backend:?}: host interval pairs the root with the call at site 1 + 3 + 5"
            );
            if host == 0 {
                let counts: String = db.query_row(
                    "SELECT COALESCE(group_concat(unwind_status || '/' || stack_root || '=' || n), '')
                     FROM (SELECT unwind_status, stack_root, count(*) AS n FROM cpu_sample
                           GROUP BY unwind_status, stack_root)",
                    [], |row| row.get(0),
                ).unwrap();
                panic!(
                    "{backend:?}: expected Foreign callback samples on the host fiber. {counts}"
                );
            }
        } else {
            run_clean(&output, 0);
        }
    }
}
