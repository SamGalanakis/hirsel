// Forward reads through a stack copy whose address a cold call also needs.
#pragma once
#include <llvm/ADT/SetVector.h>
#include <llvm/IR/PassManager.h>
#include <llvm/IR/Dominators.h>
#include <llvm/IR/IntrinsicInst.h>
#include <llvm/IR/IRBuilder.h>
#include <llvm/IR/Module.h>

namespace hirsel_llvm {
using namespace llvm;

struct SnapshotUses {
  DenseMap<LoadInst *, uint64_t> Loads;
  CallBase *Cold = nullptr;
};

static bool snapshotUses(AllocaInst &Scratch, uint64_t Size,
                         MemCpyInst &Copy, const DataLayout &DL,
                         SnapshotUses &Uses) {
  SmallVector<std::pair<Value *, uint64_t>, 8> Work{{&Scratch, 0}};
  while (!Work.empty()) {
    auto [Ptr, Offset] = Work.pop_back_val();
    for (Use &U : Ptr->uses()) {
      auto *User = dyn_cast<Instruction>(U.getUser());
      if (User == &Copy && &U == &Copy.getArgOperandUse(0))
        continue;
      if (auto *GEP = dyn_cast_or_null<GetElementPtrInst>(User)) {
        APInt Delta(DL.getIndexTypeSizeInBits(GEP->getType()), 0);
        if (&U != &GEP->getOperandUse(0) ||
            !GEP->accumulateConstantOffset(DL, Delta) ||
            Delta.isNegative() || Delta.ugt(Size - Offset))
          return false;
        Work.emplace_back(GEP, Offset + Delta.getZExtValue());
        continue;
      }
      if (auto *Load = dyn_cast_or_null<LoadInst>(User)) {
        TypeSize Width = DL.getTypeStoreSize(Load->getType());
        if (!Load->isSimple() || Width.isScalable() || Width.getFixedValue() > Size - Offset)
          return false;
        Uses.Loads[Load] = Offset;
        continue;
      }
      auto *Call = dyn_cast_or_null<CallBase>(User);
      if (Offset != 0 || !Call || Uses.Cold || !Call->isArgOperand(&U) ||
          !Call->hasFnAttr(Attribute::Cold))
        return false;
      unsigned Arg = Call->getArgOperandNo(&U);
      if (!Call->doesNotCapture(Arg) || !Call->onlyReadsMemory(Arg))
        return false;
      Uses.Cold = Call;
    }
  }
  return Uses.Cold && !Uses.Loads.empty();
}

// Order the acyclic part of the CFG from the copy to its readers. A back edge
// into the copy itself starts another snapshot and needs no input from this one.
static bool snapshotBlocks(MemCpyInst &Copy, const SnapshotUses &Uses,
                           DominatorTree &DT, SmallVectorImpl<BasicBlock *> &Order,
                           DenseMap<BasicBlock *, unsigned> &Readers) {
  SmallSetVector<BasicBlock *, 16> Region;
  BasicBlock *Entry = Copy.getParent();
  auto Add = [&](Instruction *Use) {
    if (!DT.dominates(&Copy, Use)) return false;
    ++Readers[Use->getParent()];
    Region.insert(Use->getParent());
    return true;
  };
  if (!Add(Uses.Cold)) return false;
  for (auto [Load, Offset] : Uses.Loads)
    if (!Add(Load)) return false;
  for (unsigned I = 0; I < Region.size(); ++I) {
    BasicBlock *BB = Region[I];
    if (!isa<CondBrInst, UncondBrInst, SwitchInst, ReturnInst, UnreachableInst>(
            BB->getTerminator()))
      return false;
    if (BB == Entry) continue;
    for (BasicBlock *Pred : predecessors(BB)) {
      if (!DT.dominates(&Copy, Pred->getTerminator())) return false;
      Region.insert(Pred);
    }
  }
  DenseMap<BasicBlock *, unsigned> Incoming;
  for (BasicBlock *BB : Region) {
    Readers.try_emplace(BB, 0);
    if (BB != Entry) Incoming[BB] = pred_size(BB);
  }
  Order.push_back(Entry);
  for (unsigned I = 0; I < Order.size(); ++I)
    for (BasicBlock *Next : successors(Order[I]))
      if (Next != Entry && Incoming.contains(Next) && --Incoming[Next] == 0)
        Order.push_back(Next);
  return Order.size() == Region.size();
}

static bool snapshotBarrier(Instruction &I) {
  if (auto *Intrinsic = dyn_cast<IntrinsicInst>(&I))
    if (Intrinsic->getIntrinsicID() == Intrinsic::experimental_noalias_scope_decl)
      return false;
  return !I.isTerminator() &&
         (I.mayHaveSideEffects() || I.maySynchronize() || I.isLifetimeStartOrEnd() ||
          (isa<CallBase>(I) && !isa<IntrinsicInst>(I)));
}

static bool forwardSnapshot(MemCpyInst &Copy, DominatorTree &DT) {
  auto *Scratch = dyn_cast<AllocaInst>(Copy.getRawDest());
  auto *Length = dyn_cast<ConstantInt>(Copy.getLength());
  if (!Scratch || !Scratch->isStaticAlloca() || !Length || Length->isZero() ||
      Copy.isVolatile() || Copy.getSourceAddressSpace() != Scratch->getAddressSpace())
    return false;
  const DataLayout &DL = Copy.getModule()->getDataLayout();
  auto Size = Scratch->getAllocationSize(DL);
  if (!Size || Size->isScalable() || Length->getValue() != Size->getFixedValue())
    return false;
  SnapshotUses Uses;
  if (!snapshotUses(*Scratch, Size->getFixedValue(), Copy, DL, Uses))
    return false;
  SmallVector<BasicBlock *, 16> Order;
  DenseMap<BasicBlock *, unsigned> Readers;
  if (!snapshotBlocks(Copy, Uses, DT, Order, Readers)) return false;

  IRBuilder<> AtCopy(&Copy);
  Value *Original = AtCopy.CreateFreeze(Copy.getRawSource(), "snapshot.original");
  DenseMap<BasicBlock *, Value *> SourceAtExit;
  BasicBlock *Entry = Copy.getParent();
  for (BasicBlock *BB : Order) {
    auto It = BB == Entry ? std::next(Copy.getIterator()) : BB->begin();
    Value *Source = Original;
    if (BB != Entry) {
      auto Preds = predecessors(BB);
      Source = SourceAtExit.lookup(*Preds.begin());
      if (!llvm::all_of(Preds, [&](BasicBlock *P) { return SourceAtExit.lookup(P) == Source; })) {
        auto *Phi = PHINode::Create(Source->getType(), pred_size(BB),
                                    "snapshot.source", BB->begin());
        for (BasicBlock *P : Preds) Phi->addIncoming(SourceAtExit.lookup(P), P);
        Source = Phi;
      }
    }
    unsigned Remaining = Readers.lookup(BB);
    bool LiveOut = llvm::any_of(successors(BB), [&](BasicBlock *Next) {
      return Next != Entry && Readers.contains(Next);
    });
    while (It != BB->end() && (Remaining || LiveOut)) {
      Instruction &I = *It++;
      if (auto *Load = dyn_cast<LoadInst>(&I); Load && Uses.Loads.contains(Load)) {
        IRBuilder<> B(Load);
        B.SetCurrentDebugLocation(Load->getDebugLoc());
        // Memory metadata for the stack slot need not apply to the source.
        Value *At = B.CreateConstGEP1_64(B.getInt8Ty(), Source, Uses.Loads.lookup(Load));
        auto *Forwarded = B.CreateAlignedLoad(Load->getType(), At, Align(1));
        Load->replaceAllUsesWith(Forwarded);
        Load->eraseFromParent();
        --Remaining;
      } else if (snapshotBarrier(I) || &I == Uses.Cold) {
        // The source may change or go out of scope. Keep the original bytes
        // in the slot before that can happen, or before the callee reads it.
        if (Source != Scratch) {
          IRBuilder<> B(&I);
          B.CreateMemMove(Scratch, Copy.getDestAlign(), Source, Align(1),
                          Copy.getLength());
          Source = Scratch;
        }
        if (&I == Uses.Cold) --Remaining;
      }
    }
    SourceAtExit[BB] = Source;
  }
  Copy.eraseFromParent();
  return true;
}

static bool forwardSnapshotReads(Function &F, FunctionAnalysisManager &AM) {
  SmallVector<MemCpyInst *, 4> Copies;
  for (BasicBlock &BB : F)
    for (Instruction &I : BB)
      if (auto *Copy = dyn_cast<MemCpyInst>(&I); Copy && isa<AllocaInst>(Copy->getRawDest()))
        Copies.push_back(Copy);
  if (Copies.empty()) return false;
  auto &DT = AM.getResult<DominatorTreeAnalysis>(F);
  bool Changed = false;
  for (MemCpyInst *Copy : Copies) Changed |= forwardSnapshot(*Copy, DT);
  return Changed;
}
}
