//! Release bridge rewrites must preserve every partial update, failure order and raised cause.
use crate::context::{Context, Module, TargetMachine};
use std::{path::PathBuf, process::Command};

const COPY: &str = include_str!("../../tests/core_runtime/checked-copy.ll");

fn optimized(source: &str) -> (String, Vec<u8>) {
    let context = Context::new();
    let module = Module::parse(&context, "hirsel-core", source).unwrap();
    let machine = TargetMachine::core(hirsel_core_layout::Target::X86_64Linux, true).unwrap();
    module.set_target(&machine);
    module.verify().unwrap();
    module.run_core_passes(&machine).unwrap();
    module.verify().unwrap();
    (module.text(), module.emit_object(&machine).unwrap())
}

#[test]
fn guarded_copy_preserves_partial_writes_and_first_failure() {
    let (ir, object) = optimized(COPY);
    assert!(ir.contains("copy.first.safe"), "{ir}");
    assert!(ir.contains("vector.body"), "{ir}");
    assert!(ir.contains(".copy.checked"), "{ir}");
    assert_eq!(object, hirsel_llvm_lazy::compile_ir(COPY, true).unwrap());
    let dir = std::env::temp_dir().join(format!("hirsel-checked-copy-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("copy.o"), object).unwrap();
    std::fs::write(
        dir.join("test.c"),
        r#"
#include <assert.h>
#include <stdint.h>
#include <string.h>
typedef struct { uint32_t *data; uint64_t len; } Carrier;
extern int checked_copy(Carrier *, Carrier *, uint64_t, uint64_t);
int main(void) {
    const uint64_t bounds[] = {0, 1, 3, 4, 7, 8, 15, 16, 31, 63, 64, 65};
    uint32_t src[64], dst[64], expected[64];
    for (unsigned i=0; i<64; ++i) src[i]=100+i;
    for (unsigned l=0; l<12; ++l) for (unsigned h=0; h<12; ++h)
    for (unsigned d=0; d<11; ++d) for (unsigned s=0; s<11; ++s) {
        uint64_t lo=bounds[l], hi=bounds[h], dl=bounds[d], sl=bounds[s];
        for (unsigned i=0; i<64; ++i) dst[i]=expected[i]=0xdeadbeef;
        int status=0;
        for (uint64_t i=lo; i<hi; ++i) {
            if (i>=dl) { status=1; break; }
            if (i>=sl) { status=2; break; }
            expected[i]=src[i];
        }
        Carrier out={dst,dl}, in={src,sl};
        assert(checked_copy(&out,&in,lo,hi)==status);
        assert(memcmp(dst,expected,sizeof dst)==0);
    }
    Carrier empty={0,0}, out={dst,64};
    assert(checked_copy(&empty,0,0,16)==1);
    assert(checked_copy(&out,&empty,0,16)==2);
    assert(checked_copy(0,0,1,1)==0);
    return 0;
}
"#,
    )
    .unwrap();
    let prefix = PathBuf::from(std::env::var_os("LLVM_SYS_231_PREFIX").unwrap());
    let result = Command::new(prefix.join("bin/clang"))
        .arg(format!("--ld-path={}", prefix.join("bin/ld.lld").display()))
        .args(["-std=c11", "-O2", "-no-pie"])
        .arg(dir.join("test.c"))
        .arg(dir.join("copy.o"))
        .arg("-o")
        .arg(dir.join("test"))
        .output()
        .unwrap();
    assert!(result.status.success(), "{result:?}");
    let result = Command::new(dir.join("test")).output().unwrap();
    assert!(result.status.success(), "{result:?}");
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn guarded_copy_requires_buffer_and_carrier_alias_proofs() {
    for noalias in [
        ", !alias.scope !7, !noalias !11",
        ", !alias.scope !6, !noalias !10",
    ] {
        let source = COPY.replace(noalias, "").replace("ptr noalias", "ptr");
        let (ir, _) = optimized(&source);
        assert!(!ir.contains("copy.first.safe"), "{noalias}: {ir}");
    }
}

/// A built-in cause built in a stack temporary (an inherited site) reaches the
/// runtime with the kind and site its bytes hold: two zero words, the kind word
/// `1 << 63 | kind`, then the site, whichever raise entry the release passes
/// leave in place.
#[test]
fn stack_built_cause_keeps_its_kind_and_site() {
    let source = r#"
target triple = "x86_64-unknown-linux-gnu"
declare void @hirsel_core_raise(ptr, ptr byval([32 x i8]) align 8)
define void @raise_at(ptr %context, i64 %site) {
  %t = alloca [32 x i8], align 8
  call void @llvm.memset.p0.i64(ptr align 8 %t, i8 0, i64 16, i1 false)
  %kind = getelementptr inbounds i8, ptr %t, i64 16
  store i64 -9223372036854775802, ptr %kind, align 8
  %at = getelementptr inbounds i8, ptr %t, i64 24
  store i64 %site, ptr %at, align 8
  call void @hirsel_core_raise(ptr %context, ptr byval([32 x i8]) align 8 %t)
  ret void
}
declare void @llvm.memset.p0.i64(ptr, i8, i64, i1)
"#;
    let (_, object) = optimized(source);
    let dir = std::env::temp_dir().join(format!("hirsel-stack-cause-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("raise.o"), object).unwrap();
    std::fs::write(
        dir.join("test.c"),
        r#"
#include <assert.h>
#include <stdint.h>
typedef struct { uint64_t data, len, kind, site; } Cause;
static uint8_t kind; static uint64_t site; static int raised;
void hirsel_core_raise(void *context, Cause cause) {
    assert(cause.data == 0 && cause.len == 0 && cause.kind >> 63);
    kind = (uint8_t)cause.kind; site = cause.site; ++raised;
}
void hirsel_core_raise_builtin(void *context, uint8_t k, uint64_t s) { kind = k; site = s; ++raised; }
extern void raise_at(void *, uint64_t);
int main(void) {
    const uint64_t sites[] = {0, 1, 0x500000002dd, UINT64_MAX};
    for (unsigned i = 0; i < 4; ++i) {
        raised = 0; kind = 0; site = ~sites[i];
        raise_at(&raised, sites[i]);
        assert(raised == 1 && kind == 6 && site == sites[i]);
    }
    return 0;
}
"#,
    )
    .unwrap();
    let prefix = PathBuf::from(std::env::var_os("LLVM_SYS_231_PREFIX").unwrap());
    let result = Command::new(prefix.join("bin/clang"))
        .arg(format!("--ld-path={}", prefix.join("bin/ld.lld").display()))
        .args(["-std=c11", "-O2", "-no-pie"])
        .arg(dir.join("test.c"))
        .arg(dir.join("raise.o"))
        .arg("-o")
        .arg(dir.join("test"))
        .output()
        .unwrap();
    assert!(result.status.success(), "{result:?}");
    let result = Command::new(dir.join("test")).output().unwrap();
    assert!(result.status.success(), "{result:?}");
    std::fs::remove_dir_all(dir).unwrap();
}
