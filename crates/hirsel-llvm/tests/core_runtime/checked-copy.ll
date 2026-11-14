declare i64 @llvm.umax.i64(i64, i64)
define i32 @checked_copy(ptr noalias %dst, ptr noalias %src, i64 %lo, i64 %hi) {
entry:
  %nonempty = icmp ult i64 %lo, %hi
  br i1 %nonempty, label %ph, label %done
ph:
  %dp = getelementptr i8, ptr %dst, i64 8
  %dl = load i64, ptr %dp, align 1, !alias.scope !5, !noalias !9
  %sp = getelementptr i8, ptr %src, i64 8
  %bound = call i64 @llvm.umax.i64(i64 %dl, i64 %lo)
  br label %head
head:
  %i = phi i64 [ %lo, %ph ], [ %next, %body ]
  %bad = icmp eq i64 %i, %bound
  br i1 %bad, label %bad_dst, label %source
source:
  %sl = load i64, ptr %sp, align 1, !alias.scope !6, !noalias !10
  %valid = icmp ult i64 %i, %sl
  br i1 %valid, label %body, label %bad_src
body:
  %d = load ptr, ptr %dst, align 1, !alias.scope !5, !noalias !9
  %s = load ptr, ptr %src, align 1, !alias.scope !6, !noalias !10
  %sa = getelementptr i32, ptr %s, i64 %i
  %da = getelementptr i32, ptr %d, i64 %i
  %x = load i32, ptr %sa, align 1, !alias.scope !7, !noalias !11
  store i32 %x, ptr %da, align 1, !alias.scope !8, !noalias !12
  %next = add nuw i64 %i, 1
  %end = icmp eq i64 %next, %hi
  br i1 %end, label %done, label %head
bad_dst:
  ret i32 1
bad_src:
  ret i32 2
done:
  ret i32 0
}
!0 = distinct !{!0}
!1 = distinct !{!1, !0, !"destination carrier"}
!2 = distinct !{!2, !0, !"source carrier"}
!3 = distinct !{!3, !0, !"source buffer"}
!4 = distinct !{!4, !0, !"destination buffer"}
!5 = !{!1}
!6 = !{!2}
!7 = !{!3}
!8 = !{!4}
!9 = !{!2, !3, !4}
!10 = !{!1, !3, !4}
!11 = !{!1, !2, !4}
!12 = !{!1, !2, !3}
