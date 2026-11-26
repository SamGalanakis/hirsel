use super::*;

pub(super) fn cancelled_child_inside_isolate_fixture(target: Target) {
    let mut source = include_str!("../../../../hirsel-core-ir/samples/custody.core").to_owned();
    source = source.replace(
        "sig g12 = (ptr) -> (ptr)",
        "sig g12 = (ptr) -> (ptr)\nsig g13 = () -> (bool, u64)\nsig g14 = () -> ()\nsig g15 = () -> (ptr)\nsig g16 = (ptr) -> ()",
    );
    let child_start = source.find("fn f1 = n1 g1 {").unwrap();
    let child_end = source[child_start..].find("fn f2 = n2 g5 {").unwrap() + child_start;
    source.replace_range(
        child_start..child_end,
        r#"fn f1 = n1 g1 {
  addr a0 = v2 : t1
b0(v0: ptr, v1: ptr, v2: ptr):
  address a0
  v3: bool, v4: u64 = call rt.isolate_enter g13 ()
  branch v3, b1, b2
b1:
  call rt.check_cancel g4 () fail b3
  call rt.isolate_leave g14 ()
  drop a0 t1
  return
b2:
  drop a0 t1
  return
b3:
  v5: ptr = call rt.isolate_catch g15 ()
  drop a0 t1
  propagate
}
"#,
    );
    source = source.replace(
        "  v4: ptr, v5: u64 = call rt.spawn[t1, t2] g2 (v1, &a1, v3)   ; spawn cannot fail",
        "  v4: ptr, v5: u64 = call rt.spawn[t1, t2] g2 (v1, &a1, v3)   ; spawn cannot fail\n  call rt.cancel g16 (v4)",
    );
    let module = text::parse(&source).unwrap();
    wf::module(&module).unwrap();
    let object = match target {
        Target::X86_64Linux | Target::Aarch64Linux => {
            emit_object_for_target(&module, target, Optimization::None, false, &module.sites())
                .unwrap()
        }
        Target::Aarch64Darwin => {
            // Mach-O has no link-time --wrap. Redirect this fixture's generated
            // call to the same C wrapper, which calls the actual runtime.
            let ir = emit_ir_for_target(&module, target, false, &module.sites()).unwrap();
            assert!(ir.contains("@hirsel_core_spawn("));
            let wrapped = ir.replace("@hirsel_core_spawn(", "@__wrap_hirsel_core_spawn(");
            hirsel_llvm_lazy::compile_ir_for_target(&wrapped, false, target).unwrap()
        }
    };
    let scratch = tempfile::tempdir().unwrap();
    let dir = scratch.path();
    fs::write(dir.join("input.o"), object).unwrap();
    let body = symbol(&module, FuncId(2));
    let real_spawn = match target {
        Target::X86_64Linux | Target::Aarch64Linux => "__real_hirsel_core_spawn",
        Target::Aarch64Darwin => "hirsel_core_spawn",
    };
    let c = format!(
        r#"
#include <stdint.h>
#include <stdlib.h>
#include "hirsel_core_rt.h"
static uint64_t traces;
void hirsel_core_test_drop_trace(uint64_t n) {{ (void)n; traces++; }}
HirselCoreSpawn {real_spawn}(HirselCoreContext *,uint64_t,uint64_t,void *,HirselCoreTaskBody,HirselCoreTaskLayout,uint64_t);
HirselCoreSpawn __wrap_hirsel_core_spawn(HirselCoreContext *ctx,uint64_t tasks,uint64_t bytes,void *input,HirselCoreTaskBody body,HirselCoreTaskLayout layout,uint64_t site) {{
  HirselCoreSpawn spawn={real_spawn}(ctx,tasks,bytes,input,body,layout,site);
  if(spawn.task) hirsel_core_cancel(ctx,spawn.task);
  return spawn;
}}
struct R {{ uint64_t status, value, available, native_status; }};
extern struct R {body}(void *,uint64_t,uint64_t,uint64_t);
extern HirselCoreAllocation hirsel_core_task_start(HirselCoreContext *);
extern uint64_t hirsel_core_test_available(const HirselCoreContext *);
extern uint8_t hirsel_core_test_run(HirselCoreContext *,HirselCoreTaskBody,void *,void *,size_t,size_t);
static uint8_t entry(HirselCoreContext *ctx,void *out,void *input) {{ *(struct R *)out={body}(ctx,7,1000,0); return 0; }}
int main(void) {{
  HirselCoreContext *ctx=hirsel_core_context_new(UINT64_C(1000000));
  if(!hirsel_core_task_start(ctx).pointer) return 6;
  hirsel_core_task_bind(ctx);
  uint64_t before=hirsel_core_test_available(ctx);
  struct R result={{0}};
  if(hirsel_core_test_run(ctx,entry,0,&result,0,sizeof result)) return 5;
  if(result.native_status || result.status!=2) return 1;
  if(hirsel_core_test_available(ctx)!=before-40) return 2;
  hirsel_core_task_bind(NULL);
  hirsel_core_context_free(ctx);
  return 0;
}}
"#,
        body = body,
    );
    fs::write(dir.join("test.c"), c).unwrap();
    let mut compiler = c_linker(target, false);
    match target {
        Target::X86_64Linux | Target::Aarch64Linux => {
            compiler.arg("-Wl,--wrap=hirsel_core_spawn");
        }
        Target::Aarch64Darwin => {}
    }
    output(
        link_runtime(
            compiler.arg(dir.join("test.c")).arg(dir.join("input.o")),
            target,
        )
        .arg("-o")
        .arg(dir.join("test")),
    );
    output(&mut c_executable(target, false, &dir.join("test")));
}
