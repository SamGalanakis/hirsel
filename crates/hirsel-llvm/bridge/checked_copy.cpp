// A guarded copy retains its original scalar partial-failure loop. The fast
// version requires both complete ranges and existing FORM alias scopes.
#include <bits/c++config.h>
#if defined(__GLIBC__) && !__GLIBC_PREREQ(2, 30)
#undef _GLIBCXX_USE_PTHREAD_COND_CLOCKWAIT
#undef _GLIBCXX_USE_PTHREAD_MUTEX_CLOCKLOCK
#undef _GLIBCXX_USE_PTHREAD_RWLOCK_CLOCKLOCK
#endif
#include "loop_version.h"
#include <llvm/Analysis/AliasAnalysis.h>
#include <llvm/Analysis/AssumptionCache.h>
#include <llvm/Analysis/ScalarEvolutionExpressions.h>
#include <llvm/Analysis/TargetLibraryInfo.h>
#include <llvm/IR/IntrinsicInst.h>
#include <llvm/IR/Module.h>
#include <llvm/Passes/PassBuilder.h>
#include <llvm/Support/Error.h>
using namespace llvm;
using hirsel_llvm::versionLoop;

static bool versionCopy(Loop &L, LoopInfo &LI, DominatorTree &DT,
                        ScalarEvolution &SE, AAResults &AA) {
  auto *PH = L.getLoopPreheader();
  auto *H = L.getHeader();
  auto *Latch = L.getLoopLatch();
  if (!PH || !Latch || !L.isInnermost() || L.getNumBlocks() != 3) return false;
  auto *End = dyn_cast<CondBrInst>(Latch->getTerminator());
  auto *EndCmp = End ? dyn_cast<ICmpInst>(End->getCondition()) : nullptr;
  if (!EndCmp || EndCmp->getPredicate() != ICmpInst::ICMP_EQ ||
      L.contains(End->getSuccessor(0)) || End->getSuccessor(1) != H) return false;
  auto *Next = dyn_cast<BinaryOperator>(EndCmp->getOperand(0));
  auto *Index = Next ? dyn_cast<PHINode>(Next->getOperand(0)) : nullptr;
  auto *One = Next ? dyn_cast<ConstantInt>(Next->getOperand(1)) : nullptr;
  Value *Limit = EndCmp->getOperand(1);
  if (!Next || Next->getOpcode() != Instruction::Add || !One || !One->isOne() ||
      !Index || Index->getParent() != H || !Index->getType()->isIntegerTy(64) ||
      Index->getNumIncomingValues() != 2 ||
      Index->getIncomingValueForBlock(Latch) != Next ||
      !L.isLoopInvariant(Limit) || !DT.dominates(Limit, PH->getTerminator()))
    return false;
  Value *Initial = Index->getIncomingValueForBlock(PH);
  if (!Initial) return false;
  auto *First = dyn_cast<CondBrInst>(H->getTerminator());
  auto *FirstCmp = First ? dyn_cast<ICmpInst>(First->getCondition()) : nullptr;
  if (!FirstCmp || FirstCmp->getOperand(0) != Index) return false;
  BasicBlock *Middle = nullptr;
  Value *FirstLength = FirstCmp->getOperand(1);
  if (FirstCmp->getPredicate() == ICmpInst::ICMP_ULT &&
      L.contains(First->getSuccessor(0)) && !L.contains(First->getSuccessor(1))) {
    Middle = First->getSuccessor(0);
  } else if (FirstCmp->getPredicate() == ICmpInst::ICMP_EQ &&
      !L.contains(First->getSuccessor(0)) && L.contains(First->getSuccessor(1))) {
    auto *Max = dyn_cast<IntrinsicInst>(FirstLength);
    if (!Max || Max->getIntrinsicID() != Intrinsic::umax) return false;
    if (Max->getArgOperand(0) == Initial) FirstLength = Max->getArgOperand(1);
    else if (Max->getArgOperand(1) == Initial) FirstLength = Max->getArgOperand(0);
    else return false;
    Middle = First->getSuccessor(1);
  } else return false;
  if (Middle == H || Middle == Latch || !L.isLoopInvariant(FirstLength) ||
      !DT.dominates(FirstLength, PH->getTerminator())) return false;
  auto *Second = dyn_cast<CondBrInst>(Middle->getTerminator());
  auto *SecondCmp = Second ? dyn_cast<ICmpInst>(Second->getCondition()) : nullptr;
  auto *Length = SecondCmp ? dyn_cast<LoadInst>(SecondCmp->getOperand(1)) : nullptr;
  if (!SecondCmp || SecondCmp->getPredicate() != ICmpInst::ICMP_ULT ||
      SecondCmp->getOperand(0) != Index || Second->getSuccessor(0) != Latch ||
      L.contains(Second->getSuccessor(1)) || !Length || !Length->isSimple() ||
      Length->getParent() != Middle || !Length->getType()->isIntegerTy(64) ||
      !L.isLoopInvariant(Length->getPointerOperand()) ||
      !DT.dominates(Length->getPointerOperand(), PH->getTerminator())) return false;

  StoreInst *Store = nullptr;
  SmallVector<LoadInst *, 4> Loads;
  for (BasicBlock *BB : L.blocks()) for (Instruction &I : *BB) {
    if (auto *S = dyn_cast<StoreInst>(&I)) {
      if (Store || !S->isSimple() || BB != Latch) return false;
      Store = S;
    } else if (auto *Read = dyn_cast<LoadInst>(&I)) {
      if (!Read->isSimple()) return false;
      Loads.push_back(Read);
    } else if (I.mayReadOrWriteMemory() || I.mayHaveSideEffects()) {
      if (!I.isTerminator()) return false;
    }
  }
  auto *Data = Store ? dyn_cast<LoadInst>(Store->getValueOperand()) : nullptr;
  if (!Data || !Data->getType()->isIntegerTy() || Data->getParent() != Latch || Loads.size() != 4 ||
      AA.alias(MemoryLocation::get(Data), MemoryLocation::get(Store)) != AliasResult::NoAlias)
    return false;
  const DataLayout &DL = H->getModule()->getDataLayout();
  SmallVector<LoadInst *, 2> Pointers;
  for (Value *V : {Data->getPointerOperand(), Store->getPointerOperand()}) {
    auto *G = dyn_cast<GetElementPtrInst>(V);
    auto *P = G ? dyn_cast<LoadInst>(G->getPointerOperand()) : nullptr;
    if (!G || G->getNumIndices() != 1 || G->idx_begin()->get() != Index ||
        DL.getTypeAllocSize(G->getSourceElementType()) != DL.getTypeStoreSize(Data->getType()) ||
        !P || P->getParent() != Latch || !P->getType()->isPointerTy() ||
        !L.isLoopInvariant(P->getPointerOperand()) ||
        !DT.dominates(P->getPointerOperand(), PH->getTerminator())) return false;
    Pointers.push_back(P);
  }
  if (Pointers[0] == Pointers[1]) return false;
  for (LoadInst *Read : Loads) {
    if (Read == Data) continue;
    if (Read != Length && Read != Pointers[0] && Read != Pointers[1]) return false;
    if (isModSet(AA.getModRefInfo(Store, MemoryLocation::get(Read)))) return false;
  }

  // The source length is read only if the original first bounds check must
  // succeed. Pointer reads move only into the path where both ranges fit.
  // Thus no guard speculates a read across an original failure exit.
  IRBuilder<> B(PH->getTerminator());
  auto *FirstSafe = B.CreateFreeze(B.CreateAnd(B.CreateICmpULT(Initial, Limit),
                              B.CreateICmpULE(Limit, FirstLength)), "copy.first.safe");
  auto *SavedLength = cast<LoadInst>(Length->clone());
  SavedLength->setName("copy.source.length");
  SavedLength->insertBefore(PH->getTerminator()->getIterator());
  auto *Safe = B.CreateFreeze(B.CreateICmpULE(Limit, SavedLength), "copy.safe");
  versionLoop(L, Safe, LI, DT, SE, "copy.fast.ph", ".copy.checked");
  BasicBlock *FastPH = L.getLoopPreheader();
  BasicBlock *SlowPH = PH->getTerminator()->getSuccessor(1);
  BasicBlock *CheckSource = SplitBlock(PH, SavedLength, static_cast<DominatorTree *>(nullptr), nullptr, nullptr,
                                      "copy.source.check");
  auto *Old = PH->getTerminator();
  CondBrInst::Create(FirstSafe, CheckSource, SlowPH, Old->getIterator());
  Old->eraseFromParent();
  FirstCmp->replaceAllUsesWith(B.getInt1(FirstCmp->getPredicate() == ICmpInst::ICMP_ULT));
  SecondCmp->replaceAllUsesWith(B.getTrue());
  Length->replaceAllUsesWith(SavedLength);
  Length->eraseFromParent();
  for (LoadInst *P : Pointers) P->moveBefore(FastPH->getTerminator()->getIterator());
  return true;
}
class CheckedCopyPass : public PassInfoMixin<CheckedCopyPass> {
  PassBuilder &PB;
public:
  explicit CheckedCopyPass(PassBuilder &PB) : PB(PB) {}
  PreservedAnalyses run(Function &F, FunctionAnalysisManager &AM) {
    // Avoid loop/alias analyses unless there is a unit-stride load/store pair.
    bool Candidate = false;
    for (BasicBlock &BB : F) for (Instruction &I : BB) {
      auto *S = dyn_cast<StoreInst>(&I);
      auto *D = S ? dyn_cast<LoadInst>(S->getValueOperand()) : nullptr;
      auto *G = D ? dyn_cast<GetElementPtrInst>(D->getPointerOperand()) : nullptr;
      Candidate |= G && G->getNumIndices() == 1 && isa<PHINode>(G->idx_begin()->get());
    }
    if (!Candidate) return PreservedAnalyses::all();
    auto &DT = AM.getResult<DominatorTreeAnalysis>(F);
    auto &LI = AM.getResult<LoopAnalysis>(F);
    auto &SE = AM.getResult<ScalarEvolutionAnalysis>(F);
    auto &AA = AM.getResult<AAManager>(F);
    // One guarded copy per function bounds the additional optimization work.
    for (Loop *L : LI.getLoopsInPreorder()) if (versionCopy(*L, LI, DT, SE, AA)) {
      AM.invalidate(F, PreservedAnalyses::none());
      FunctionPassManager Clean;
      cantFail(PB.parsePassPipeline(Clean,
          "instcombine<no-verify-fixpoint>,simplifycfg,loop-simplify,lcssa,loop-vectorize,instcombine<no-verify-fixpoint>,simplifycfg"));
      Clean.run(F, AM);
      return PreservedAnalyses::none();
    }
    return PreservedAnalyses::all();
  }
};

extern "C" void hirsel_llvm_guard_copy_loops(void *Raw, void *Machine) noexcept {
  auto &M = *static_cast<Module *>(Raw);
  PassBuilder PB(static_cast<TargetMachine *>(Machine));
  LoopAnalysisManager LAM;
  FunctionAnalysisManager FAM;
  CGSCCAnalysisManager CGAM;
  ModuleAnalysisManager MAM;
  PB.registerModuleAnalyses(MAM);
  PB.registerCGSCCAnalyses(CGAM);
  PB.registerFunctionAnalyses(FAM);
  PB.registerLoopAnalyses(LAM);
  PB.crossRegisterProxies(LAM, FAM, CGAM, MAM);
  FunctionPassManager PM;
  PM.addPass(CheckedCopyPass(PB));
  for (Function &F : M) if (!F.isDeclaration()) PM.run(F, FAM);
}
