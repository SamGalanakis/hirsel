declare double @llvm.copysign.f64(double, double)
declare void @sink(double)
define double @returned(double %x) {
entry:
 %n = fcmp uno double %x, %x
 %m = select i1 %n, double 0x7FF8000000000000, double %x
 ret double %m
}
define void @bits(double %x, ptr %out) {
entry:
 %n = fcmp uno double %x, %x
 %m = select i1 %n, double 0x7FF8000000000000, double %x
 %bits = bitcast double %m to i64
 store i64 %bits, ptr %out
 ret void
}
define void @typed_store(double %x, ptr %out) {
entry:
 %n = fcmp uno double %x, %x
 %m = select i1 %n, double 0x7FF8000000000000, double %x
 store double %m, ptr %out
 ret void
}
define void @conversion(double %x, ptr %out) {
entry:
 %n = fcmp uno double %x, %x
 %m = select i1 %n, double 0x7FF8000000000000, double %x
 %int = fptosi double %m to i64
 store i64 %int, ptr %out
 ret void
}
define void @call(double %x) {
entry:
 %n = fcmp uno double %x, %x
 %m = select i1 %n, double 0x7FF8000000000000, double %x
 call void @sink(double %m)
 ret void
}
define {double, double} @packing(double %x) {
entry:
 %n = fcmp uno double %x, %x
 %m = select i1 %n, double 0x7FF8000000000000, double %x
 %pair = insertvalue {double, double} zeroinitializer, double %m, 0
 ret {double, double} %pair
}
define void @copy_sign(double %x, ptr %out) {
entry:
 %n = fcmp uno double %x, %x
 %m = select i1 %n, double 0x7FF8000000000000, double %x
 %v = call double @llvm.copysign.f64(double 1.0, double %m)
 store double %v, ptr %out
 ret void
}
define void @cycle_store(double %x, ptr %out, i1 %done) {
entry:
 %n = fcmp uno double %x, %x
 %m = select i1 %n, double 0x7FF8000000000000, double %x
 br label %loop
loop:
 %p = phi double [%m, %entry], [%q, %loop]
 %q = fadd double %p, 1.0
 br i1 %done, label %exit, label %loop
exit:
 store double %q, ptr %out
 ret void
}
define void @two_maps_cycle(double %x, ptr %out, i1 %done) {
entry:
 %n = fcmp uno double %x, %x
 %m = select i1 %n, double 0x7FF8000000000000, double %x
 br label %loop
loop:
 %p = phi double [%m, %entry], [%m2, %loop]
 %q = fadd double %p, 1.0
 %n2 = fcmp uno double %q, %q
 %m2 = select i1 %n2, double 0x7FF8000000000000, double %q
 br i1 %done, label %exit, label %loop
exit:
 store double %p, ptr %out
 ret void
}
; Only one of two successive maps may be removed when each is the other's
; barrier and their SCC reaches memory after the backedge.
define void @successive_cycle(double %x, ptr %out, i1 %done) {
entry:
 br label %loop
loop:
 %p = phi double [%x, %entry], [%m2, %loop]
 %n = fcmp uno double %p, %p
 %m = select i1 %n, double 0x7FF8000000000000, double %p
 %n2 = fcmp uno double %m, %m
 %m2 = select i1 %n2, double 0x7FF8000000000000, double %m
 br i1 %done, label %exit, label %loop
exit:
 store double %m2, ptr %out
 ret void
}
define void @spectral(double %x, ptr %out, i1 %done) {
entry:
 %n = fcmp uno double %x, %x
 %m = select i1 %n, double 0x7FF8000000000000, double %x
 %product = fmul double %m, 2.0
 br label %loop
loop:
 %p = phi double [%product, %entry], [%sum, %loop]
 %sum = fadd double %p, 1.0
 br i1 %done, label %exit, label %loop
exit:
 %n2 = fcmp uno double %sum, %sum
 %m2 = select i1 %n2, double 0x7FF8000000000000, double %sum
 store double %m2, ptr %out
 ret void
}
define i1 @compare(double %x) {
entry:
 %n = fcmp uno double %x, %x
 %m = select i1 %n, double 0x7FF8000000000000, double %x
 %cmp = fcmp olt double %m, 1.0
 ret i1 %cmp
}
define i1 @fast_math(double %x) {
entry:
 %n = fcmp uno double %x, %x
 %m = select i1 %n, double 0x7FF8000000000000, double %x
 %v = fadd nnan double %m, 1.0
 %cmp = fcmp olt double %v, 1.0
 ret i1 %cmp
}
define i1 @fast_condition(double %x) {
entry:
 %n = fcmp nnan uno double %x, %x
 %m = select i1 %n, double 0x7FF8000000000000, double %x
 %cmp = fcmp olt double %m, 1.0
 ret i1 %cmp
}
