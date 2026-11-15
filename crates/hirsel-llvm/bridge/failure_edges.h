#ifndef HIRSEL_FAILURE_EDGES_H
#define HIRSEL_FAILURE_EDGES_H

#include <llvm-c/Core.h>
#include <string.h>

// Every failure edge is cold, for code generation only. Runs after the IR
// pipeline: the inliner and the loop passes still see the unhinted module.
// Under IR-level hints the inliner takes call sites out of its cold-call-site
// bucket, and the larger loops then miss non-trivial unswitching
// (Template::Parse +25%, Etc::Sieve +24%). Block placement and register
// allocation see the hints: raise calls are `cold`, and a two-way branch
// whose one side reaches a raise is weighted 2000:1 away from it.
static inline int hirsel_failure_is_raise(LLVMValueRef call) {
  LLVMValueRef callee = LLVMGetCalledValue(call);
  size_t len = 0;
  const char *name = callee ? LLVMGetValueName2(callee, &len) : "";
  return len >= 17 && !strncmp(name, "hirsel_core_raise", 17);
}

// A raise in the block, or within four unconditional hops of it.
static inline int hirsel_failure_reaches_raise(LLVMBasicBlockRef block) {
  for (int hop = 0; block && hop < 4; hop++) {
    for (LLVMValueRef i = LLVMGetFirstInstruction(block); i;
         i = LLVMGetNextInstruction(i))
      if (LLVMGetInstructionOpcode(i) == LLVMCall && hirsel_failure_is_raise(i))
        return 1;
    LLVMValueRef end = LLVMGetBasicBlockTerminator(block);
    if (!end || LLVMGetInstructionOpcode(end) != LLVMUncondBr)
      return 0;
    block = LLVMGetSuccessor(end, 0);
  }
  return 0;
}

static inline void hirsel_cold_failure_edges(LLVMModuleRef module,
                                             LLVMContextRef context) {
  unsigned prof = LLVMGetMDKindIDInContext(context, "prof", 4);
  LLVMAttributeRef cold = LLVMCreateEnumAttribute(
      context, LLVMGetEnumAttributeKindForName("cold", 4), 0);
  LLVMTypeRef i32 = LLVMInt32TypeInContext(context);
  LLVMMetadataRef tag = LLVMMDStringInContext2(context, "branch_weights", 14);
  LLVMMetadataRef hot = LLVMValueAsMetadata(LLVMConstInt(i32, 2000, 0));
  LLVMMetadataRef rare = LLVMValueAsMetadata(LLVMConstInt(i32, 1, 0));
  LLVMMetadataRef first[3] = {tag, hot, rare}, second[3] = {tag, rare, hot};
  LLVMValueRef to_first =
      LLVMMetadataAsValue(context, LLVMMDNodeInContext2(context, first, 3));
  LLVMValueRef to_second =
      LLVMMetadataAsValue(context, LLVMMDNodeInContext2(context, second, 3));
  for (LLVMValueRef f = LLVMGetFirstFunction(module); f;
       f = LLVMGetNextFunction(f))
    for (LLVMBasicBlockRef b = LLVMGetFirstBasicBlock(f); b;
         b = LLVMGetNextBasicBlock(b))
      for (LLVMValueRef i = LLVMGetFirstInstruction(b); i;
           i = LLVMGetNextInstruction(i)) {
        LLVMOpcode op = LLVMGetInstructionOpcode(i);
        if (op == LLVMCall && hirsel_failure_is_raise(i)) {
          LLVMAddCallSiteAttribute(i, LLVMAttributeFunctionIndex, cold);
        } else if (op == LLVMCondBr) {
          int a = hirsel_failure_reaches_raise(LLVMGetSuccessor(i, 0));
          int z = hirsel_failure_reaches_raise(LLVMGetSuccessor(i, 1));
          if (a != z)
            LLVMSetMetadata(i, prof, z ? to_first : to_second);
        }
      }
}

#endif
