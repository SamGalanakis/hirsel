// Shared LLVM loop versioning with all recoverable failure exits preserved.
#pragma once
#include <llvm/Analysis/LoopInfo.h>
#include <llvm/Analysis/ScalarEvolution.h>
#include <llvm/IR/Dominators.h>
#include <llvm/IR/IRBuilder.h>
#include <llvm/Transforms/Utils/LCSSA.h>
#include <llvm/Transforms/Utils/LoopUtils.h>
#include <llvm/Transforms/Utils/BasicBlockUtils.h>
#include <llvm/Transforms/Utils/Cloning.h>
namespace hirsel_llvm {
using namespace llvm;
// Keep the checked loop and every partial-failure exit as the fallback.
inline void versionLoop(Loop &L, Value *Safe, LoopInfo &LI, DominatorTree &DT,
                        ScalarEvolution &SE, StringRef Preheader,
                        StringRef Suffix) {
  BasicBlock *PH = L.getLoopPreheader();
  formLCSSA(L, DT, &LI, &SE);
  BasicBlock *Check = PH;
  PH = SplitBlock(Check, Check->getTerminator(), &DT, &LI, nullptr, Preheader);
  ValueToValueMapTy Map;
  SmallVector<BasicBlock *, 16> Cloned;
  Loop *Slow = cloneLoopWithPreheader(PH, Check, &L, Map, Suffix, &LI,
                                      &DT, Cloned);
  remapInstructionsInBlocks(Cloned, Map);
  // LCSSA already funnels all outside uses through exit PHIs. Clone each
  // incoming edge, preserving the precise partially updated state on failure.
  SmallVector<BasicBlock *, 8> Exits;
  L.getUniqueExitBlocks(Exits);
  for (BasicBlock *Exit : Exits) for (PHINode &P : Exit->phis()) {
    unsigned N = P.getNumIncomingValues();
    for (unsigned I = 0; I < N; ++I) {
      BasicBlock *From = P.getIncomingBlock(I);
      if (!L.contains(From)) continue;
      Value *V = P.getIncomingValue(I);
      P.addIncoming(Map.count(V) ? static_cast<Value *>(Map[V]) : V,
                    cast<BasicBlock>(Map[From]));
    }
  }
  Instruction *OldBranch = Check->getTerminator();
  CondBrInst::Create(Safe, PH, Slow->getLoopPreheader(), OldBranch->getIterator());
  OldBranch->eraseFromParent();
}

}
