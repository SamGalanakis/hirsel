#ifndef HIRSEL_AGGREGATE_PHI_H
#define HIRSEL_AGGREGATE_PHI_H

#include <llvm-c/Core.h>

// Project scalar fields before the late cleanup round. Inlining can merge a
// value and its failure byte into one aggregate PHI; projection then hides the
// constant status on each predecessor from ordinary branch simplification.
static inline void hirsel_scalarize_aggregate_phis(LLVMModuleRef module,
                                                  LLVMContextRef context) {
  LLVMBuilderRef builder = LLVMCreateBuilderInContext(context);
  for (LLVMValueRef f = LLVMGetFirstFunction(module); f;
       f = LLVMGetNextFunction(f)) {
    for (LLVMBasicBlockRef block = LLVMGetFirstBasicBlock(f); block;
         block = LLVMGetNextBasicBlock(block)) {
      LLVMValueRef phi = LLVMGetFirstInstruction(block);
      while (phi && LLVMIsAPHINode(phi)) {
        LLVMValueRef next = LLVMGetNextInstruction(phi);
        if (next && !LLVMIsAPHINode(next))
          next = NULL;
        if (LLVMGetTypeKind(LLVMTypeOf(phi)) != LLVMStructTypeKind) {
          phi = next;
          continue;
        }
        int eligible = 1;
        for (LLVMUseRef use = LLVMGetFirstUse(phi); use;
             use = LLVMGetNextUse(use)) {
          LLVMValueRef user = LLVMGetUser(use);
          if (!LLVMIsAExtractValueInst(user) || LLVMGetNumIndices(user) != 1 ||
              LLVMGetTypeKind(LLVMTypeOf(user)) == LLVMStructTypeKind ||
              LLVMGetTypeKind(LLVMTypeOf(user)) == LLVMArrayTypeKind) {
            eligible = 0;
            break;
          }
        }
        // CoreIR branches have no edge-defined values. Do not insert a use
        // before an invoke or another terminator that defines its incoming value.
        for (unsigned i = 0; eligible && i < LLVMCountIncoming(phi); ++i) {
          LLVMOpcode op = LLVMGetInstructionOpcode(
              LLVMGetBasicBlockTerminator(LLVMGetIncomingBlock(phi, i)));
          eligible = op == LLVMUncondBr || op == LLVMCondBr || op == LLVMSwitch;
        }
        if (eligible) {
          while (LLVMGetFirstUse(phi)) {
            LLVMValueRef extract = LLVMGetUser(LLVMGetFirstUse(phi));
            unsigned index = LLVMGetIndices(extract)[0];
            LLVMPositionBuilderBefore(builder, phi);
            LLVMValueRef scalar =
                LLVMBuildPhi(builder, LLVMTypeOf(extract), "return.field");
            for (unsigned i = 0; i < LLVMCountIncoming(phi); ++i) {
              LLVMBasicBlockRef predecessor = LLVMGetIncomingBlock(phi, i);
              LLVMPositionBuilderBefore(
                  builder, LLVMGetBasicBlockTerminator(predecessor));
              LLVMValueRef value = LLVMBuildExtractValue(
                  builder, LLVMGetIncomingValue(phi, i), index, "return.edge");
              LLVMAddIncoming(scalar, &value, &predecessor, 1);
            }
            LLVMReplaceAllUsesWith(extract, scalar);
            LLVMInstructionEraseFromParent(extract);
          }
          LLVMInstructionEraseFromParent(phi);
        }
        phi = next;
      }
    }
  }
  LLVMDisposeBuilder(builder);
}

#endif
