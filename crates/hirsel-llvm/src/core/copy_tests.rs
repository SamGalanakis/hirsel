//! Release bridge rewrites must preserve every partial update, failure order and raised cause.
use crate::context::{Context, Module, TargetMachine};
use std::{fmt::Write as _, path::PathBuf, process::Command};

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
fn fixed_width_check_rewrites_all_admitted_widths() {
    let mut source = String::new();
    let widths = [0, 1, 2, 4, 8, 16, 32, 64, 65];
    for width in widths {
        writeln!(
            source,
            "define i1 @width_{width}(i64 %base, i64 %len) {{
               %start = add nuw i64 %base, 11
               %end = add i64 %base, {}
               %within = icmp ule i64 %end, %len
               %no_wrap = icmp uge i64 %end, %start
               %valid = and i1 %within, %no_wrap
               br i1 %valid, label %pass, label %fail
             pass: ret i1 true
             fail: ret i1 false
             }}",
            11 + width
        )
        .unwrap();
    }
    let context = Context::new();
    let module = Module::parse(&context, "fixed-width", &source).unwrap();
    module.verify().unwrap();
    module.fixed_width_checks();
    module.verify().unwrap();
    let ir = module.text();
    for width in widths {
        let (_, tail) = ir.split_once(&format!("@width_{width}(")).unwrap();
        let (body, _) = tail.split_once("\n}").unwrap();
        if (1..=64).contains(&width) {
            assert!(
                body.contains(&format!("icmp uge i64 %len, {width}")),
                "{body}"
            );
            assert!(body.contains(&format!("sub i64 %len, {width}")), "{body}");
            assert!(body.contains("icmp ule i64 %start"), "{body}");
            assert!(!body.contains("%end ="), "{body}");
        } else {
            assert!(body.contains("icmp uge i64 %end, %start"), "{body}");
        }
    }
}

#[test]
fn ordered_counted_read_entry_proves_a_nonwrapping_increment() {
    // These inequality checks admit UINT64_MAX for some bounds. An unsigned
    // less-than check would itself prove the increment cannot wrap.
    let kernel = r#"
define i64 @read_loop(ptr %a, ptr %b, ptr %data, i64 %start, i64 %limit) {
entry:
  %nonempty = icmp ult i64 %start, %limit
  br i1 %nonempty, label %header, label %empty
header:
  %i = phi i64 [ %start, %entry ], [ %next, %body ]
  %sum = phi i64 [ 0, %entry ], [ %total, %body ]
  %len_a = load i64, ptr %a, align 8
  %in_a = icmp ne i64 %i, %len_a
  br i1 %in_a, label %check_b, label %failed_a
check_b:
  %len_b = load i64, ptr %b, align 8
  %in_b = icmp ne i64 %i, %len_b
  br i1 %in_b, label %body, label %failed_b
body:
  %at = getelementptr i64, ptr %data, i64 %i
  %x = load i64, ptr %at, align 8
  %total = add i64 %sum, %x
  %next = add i64 %i, 1
  %finished = icmp eq i64 %next, %limit
  br i1 %finished, label %done, label %header
done:
  ret i64 %total
empty:
  ret i64 0
failed_a:
  ret i64 -1
failed_b:
  ret i64 -2
}
"#;
    let unordered = kernel.replace(
        "br i1 %nonempty, label %header, label %empty",
        "br label %header",
    );
    let context = Context::new();
    let machine = TargetMachine::core(hirsel_core_layout::Target::X86_64Linux, true).unwrap();
    for (source, guarded_entry) in [(kernel, true), (&unordered, false)] {
        let module = Module::parse(&context, "counted-read", source).unwrap();
        module.set_target(&machine);
        module.verify().unwrap();
        module.release_tail(&machine);
        module.verify().unwrap();
        let ir = module.text();
        assert_eq!(ir.contains("read.fast.ph"), guarded_entry, "{ir}");
        if guarded_entry {
            assert!(ir.contains("add nuw i64"), "{ir}");
        }
    }
}

#[test]
fn guarded_copy_preserves_partial_writes_and_first_failure() {
    let (ir, object) = optimized(COPY);
    assert!(ir.contains("copy.first.safe"), "{ir}");
    assert!(ir.contains("vector.body"), "{ir}");
    assert!(ir.contains(".copy.checked"), "{ir}");
    assert_eq!(object, hirsel_llvm_lazy::compile_ir(COPY, true).unwrap());
    let scratch = tempfile::tempdir().unwrap();
    let dir = scratch.path();
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
    let scratch = tempfile::tempdir().unwrap();
    let dir = scratch.path();
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
}

/// Closed stack memory in `counter-range` (counter_range.h): hand IR for cell
/// rules no Hirsel lowering reaches. Each function fills or copies an alloca so
/// that a cell holds a value the rule must not lose, then checks a load of it in
/// a counted loop; the check must still raise, with its own site. The oracles
/// come from the constants: a0 reads bytes 00 00 00 ff (-16777216) and
/// subtracts 2130706433, past i32::MIN; a1 reads o+0 = s+4 = i32::MAX; a2
/// stores i32::MAX through o + 12k + (2^63 - 8) = o + 8 (mod 2^64) for
/// k = (2^63 + 16) / 12 and reads +8; a8 reads FLT_MAX's bits 0x7f7fffff at +40
/// and adds 8388609, past i32::MAX.
#[test]
fn closed_cells_keep_admitted_failures() {
    let source = r#"
target triple = "x86_64-unknown-linux-gnu"
declare void @hirsel_core_raise_builtin(ptr, i8, i64)
declare void @llvm.memset.p0.i64(ptr, i8, i64, i1)
declare void @llvm.memcpy.p0.p0.i64(ptr, ptr, i64, i1)
declare { i32, i1 } @llvm.sadd.with.overflow.i32(i32, i32)
declare { i32, i1 } @llvm.ssub.with.overflow.i32(i32, i32)

define void @a0(ptr %context, i64 %j, i64 %n) {
entry:
  %o = alloca [16 x i8], align 8
  call void @llvm.memset.p0.i64(ptr align 8 %o, i8 -1, i64 16, i1 false)
  call void @llvm.memset.p0.i64(ptr align 8 %o, i8 0, i64 3, i1 false)
  br label %loop
loop:
  %i = phi i64 [ 0, %entry ], [ %next, %ok ]
  %more = icmp ult i64 %i, %n
  br i1 %more, label %body, label %done
body:
  %x = add i64 %j, %i
  %p = getelementptr [16 x i8], ptr %o, i64 %x
  %v = load i32, ptr %p, align 4
  %r = call { i32, i1 } @llvm.ssub.with.overflow.i32(i32 %v, i32 2130706433)
  %f = extractvalue { i32, i1 } %r, 1
  br i1 %f, label %bad, label %ok
bad:
  call void @hirsel_core_raise_builtin(ptr %context, i8 1, i64 10)
  ret void
ok:
  %next = add i64 %i, 1
  br label %loop
done:
  ret void
}

define void @a1(ptr %context, i64 %j, i64 %k, i64 %n) {
entry:
  %s = alloca [24 x i8], align 8
  %o = alloca [16 x i8], align 8
  call void @llvm.memset.p0.i64(ptr align 8 %s, i8 0, i64 24, i1 false)
  call void @llvm.memset.p0.i64(ptr align 8 %o, i8 0, i64 16, i1 false)
  %sk = getelementptr [8 x i8], ptr %s, i64 %k
  %sk4 = getelementptr i8, ptr %sk, i64 4
  store i32 2147483647, ptr %sk4, align 4
  %s4 = getelementptr i8, ptr %s, i64 4
  call void @llvm.memcpy.p0.p0.i64(ptr align 8 %o, ptr align 4 %s4, i64 16, i1 false)
  br label %loop
loop:
  %i = phi i64 [ 0, %entry ], [ %next, %ok ]
  %more = icmp ult i64 %i, %n
  br i1 %more, label %body, label %done
body:
  %x = add i64 %j, %i
  %p = getelementptr [8 x i8], ptr %o, i64 %x
  %v = load i32, ptr %p, align 4
  %r = call { i32, i1 } @llvm.sadd.with.overflow.i32(i32 %v, i32 1)
  %f = extractvalue { i32, i1 } %r, 1
  br i1 %f, label %bad, label %ok
bad:
  call void @hirsel_core_raise_builtin(ptr %context, i8 1, i64 11)
  ret void
ok:
  %next = add i64 %i, 1
  br label %loop
done:
  ret void
}

define void @a2(ptr %context, i64 %j, i64 %k, i64 %n) {
entry:
  %o = alloca [48 x i8], align 8
  call void @llvm.memset.p0.i64(ptr align 8 %o, i8 0, i64 48, i1 false)
  %ok12 = getelementptr nuw [12 x i8], ptr %o, i64 %k
  %w = getelementptr i8, ptr %ok12, i64 9223372036854775800
  store i32 2147483647, ptr %w, align 4
  br label %loop
loop:
  %i = phi i64 [ 0, %entry ], [ %next, %ok ]
  %more = icmp ult i64 %i, %n
  br i1 %more, label %body, label %done
body:
  %x = add i64 %j, %i
  %e = getelementptr [12 x i8], ptr %o, i64 %x
  %p = getelementptr i8, ptr %e, i64 8
  %v = load i32, ptr %p, align 4
  %r = call { i32, i1 } @llvm.sadd.with.overflow.i32(i32 %v, i32 1)
  %f = extractvalue { i32, i1 } %r, 1
  br i1 %f, label %bad, label %ok
bad:
  call void @hirsel_core_raise_builtin(ptr %context, i8 1, i64 12)
  ret void
ok:
  %next = add i64 %i, 1
  br label %loop
done:
  ret void
}

define void @a8(ptr %context, i64 %j, i64 %k, i64 %n) {
entry:
  %o = alloca [48 x i8], align 8
  call void @llvm.memset.p0.i64(ptr align 8 %o, i8 0, i64 48, i1 false)
  %ok12 = getelementptr [12 x i8], ptr %o, i64 %k
  %w = getelementptr i8, ptr %ok12, i64 4
  store float 0x47EFFFFFE0000000, ptr %w, align 4
  br label %loop
loop:
  %i = phi i64 [ 0, %entry ], [ %next, %ok ]
  %more = icmp ult i64 %i, %n
  br i1 %more, label %body, label %done
body:
  %x = add i64 %j, %i
  %e = getelementptr [16 x i8], ptr %o, i64 %x
  %p = getelementptr i8, ptr %e, i64 8
  %v = load i32, ptr %p, align 4
  %r = call { i32, i1 } @llvm.sadd.with.overflow.i32(i32 %v, i32 8388609)
  %f = extractvalue { i32, i1 } %r, 1
  br i1 %f, label %bad, label %ok
bad:
  call void @hirsel_core_raise_builtin(ptr %context, i8 1, i64 13)
  ret void
ok:
  %next = add i64 %i, 1
  br label %loop
done:
  ret void
}
"#;
    let (_, object) = optimized(source);
    let scratch = tempfile::tempdir().unwrap();
    let dir = scratch.path();
    std::fs::write(dir.join("cells.o"), object).unwrap();
    std::fs::write(
        dir.join("test.c"),
        r#"
#include <stdint.h>
#include <stdio.h>
static uint64_t site; static int raised;
void hirsel_core_raise_builtin(void *context, uint8_t kind, uint64_t s) { site = s; ++raised; }
extern void a0(void *, uint64_t, uint64_t);
extern void a1(void *, uint64_t, uint64_t, uint64_t);
extern void a2(void *, uint64_t, uint64_t, uint64_t);
extern void a8(void *, uint64_t, uint64_t, uint64_t);
#define CASE(name, call, want) \
    raised = 0; site = 0; call; \
    if (raised != 1 || site != want) { printf(name " raised %d at %llu\n", raised, (unsigned long long)site); failed = 1; }
int main(void) {
    int failed = 0;
    const uint64_t k = 768614336404564652ull;
    CASE("a0", a0(&raised, 0, 1), 10)
    CASE("a1", a1(&raised, 0, 0, 1), 11)
    CASE("a2", a2(&raised, 0, k, 1), 12)
    CASE("a8", a8(&raised, 2, 3, 1), 13)
    return failed;
}
"#,
    )
    .unwrap();
    let prefix = PathBuf::from(std::env::var_os("LLVM_SYS_231_PREFIX").unwrap());
    let result = Command::new(prefix.join("bin/clang"))
        .arg(format!("--ld-path={}", prefix.join("bin/ld.lld").display()))
        .args(["-std=c11", "-O2", "-no-pie"])
        .arg(dir.join("test.c"))
        .arg(dir.join("cells.o"))
        .arg("-o")
        .arg(dir.join("test"))
        .output()
        .unwrap();
    assert!(result.status.success(), "{result:?}");
    let result = Command::new(dir.join("test")).output().unwrap();
    assert!(result.status.success(), "{result:?}");
}
