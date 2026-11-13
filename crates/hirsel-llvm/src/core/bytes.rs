//! Expose equal-length comparison separately so an equality user can discard
//! the ordering-only prefix work after LLVM inlines this total bytewise order.
use super::*;

impl FunctionEmitter<'_> {
    pub(super) fn compare_bytes(&mut self, arguments: &[(String, String)]) -> String {
        self.intrinsics
            .insert("declare i32 @memcmp(ptr, ptr, i64) nounwind memory(argmem: read)".into());
        self.intrinsics.insert(r#"define private signext i8 @core_compare_bytes(ptr %a, i64 %al, ptr %b, i64 %bl) alwaysinline nounwind memory(argmem: read) {
entry:
  %same = icmp eq i64 %al, %bl
  br i1 %same, label %equal_length, label %different_length
equal_length:
  %empty = icmp eq i64 %al, 0
  br i1 %empty, label %empty_equal, label %same_compare
empty_equal:
  ret i8 0
same_compare:
  %cmp = call i32 @memcmp(ptr %a, ptr %b, i64 %al)
  %zero = icmp eq i32 %cmp, 0
  %negative = icmp slt i32 %cmp, 0
  %sign = select i1 %negative, i8 -1, i8 1
  %order = select i1 %zero, i8 0, i8 %sign
  ret i8 %order
different_length:
  %shorter = icmp ult i64 %al, %bl
  %common = select i1 %shorter, i64 %al, i64 %bl
  %length_order = select i1 %shorter, i8 -1, i8 1
  %no_prefix = icmp eq i64 %common, 0
  br i1 %no_prefix, label %length_result, label %prefix_compare
length_result:
  ret i8 %length_order
prefix_compare:
  %prefix = call i32 @memcmp(ptr %a, ptr %b, i64 %common)
  %prefix_zero = icmp eq i32 %prefix, 0
  %prefix_negative = icmp slt i32 %prefix, 0
  %prefix_sign = select i1 %prefix_negative, i8 -1, i8 1
  %prefix_order = select i1 %prefix_zero, i8 %length_order, i8 %prefix_sign
  ret i8 %prefix_order
}"#.into());
        self.calc(format!(
            "call signext i8 @core_compare_bytes({})",
            arguments
                .iter()
                .map(|(t, v)| format!("{t} {v}"))
                .collect::<Vec<_>>()
                .join(", ")
        ))
    }
}
