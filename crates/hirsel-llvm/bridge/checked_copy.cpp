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
#include <llvm/Analysis/ValueTracking.h>
#include <llvm/Transforms/Utils/PromoteMemToReg.h>
#include <llvm/Transforms/Utils/Cloning.h>
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

// The step word is private runtime storage. Once calls have been inlined,
// carry it through the resulting CFG and materialize it only where the
// remaining call ABI or an exit can observe it. Program pointers never name it.
static bool stepWord(Instruction &I, Value *Context, const DataLayout &DL) {
  auto *Tag = I.getMetadata(LLVMContext::MD_tbaa);
  auto *Type = Tag && Tag->getNumOperands() ? dyn_cast<MDNode>(Tag->getOperand(0)) : nullptr;
  auto *Name = Type && Type->getNumOperands() ? dyn_cast<MDString>(Type->getOperand(0)) : nullptr;
  if (!Name || Name->getString() != "steps") return false;
  Value *P = nullptr;
  if (auto *L = dyn_cast<LoadInst>(&I)) P = L->getPointerOperand();
  if (auto *S = dyn_cast<StoreInst>(&I)) P = S->getPointerOperand();
  if (!P) return false;
  int64_t Offset = 0;
  return GetPointerBaseWithConstantOffset(P, Offset, DL) == Context && Offset == 16;
}
// Metering expands tiny helpers with checked slow paths before the ordinary
// inliner prices them. Revisit direct loop calls after O3 has simplified those
// paths, sharing one finite expansion budget across introduced helper calls.
// The whole expansion budget prices each helper too: a separate small-callee
// cutoff would still reject the hot body of a metered structured helper.
static void inlineLoopHelpers(Module &M) {
  for (Function &F : M) {
    if (F.isDeclaration()) continue;
    DominatorTree DT(F);
    LoopInfo LI(DT);
    SmallVector<CallBase *, 16> Pending;
    for (BasicBlock &BB : F) if (LI.getLoopFor(&BB))
      for (Instruction &I : BB) if (auto *C = dyn_cast<CallInst>(&I))
        if (C->getCalledFunction()) Pending.push_back(C);
    unsigned Budget = 2048;
    while (!Pending.empty()) {
      CallBase *Call = Pending.pop_back_val();
      Function *Target = Call->getCalledFunction();
      if (!Target || Target->isDeclaration() || Target == &F ||
          Target->hasFnAttribute(Attribute::NoInline) ||
          Call->hasFnAttr(Attribute::NoInline) ||
          Target->getInstructionCount() > Budget) continue;
      unsigned Cost = Target->getInstructionCount();
      InlineFunctionInfo Info;
      if (InlineFunction(*Call, Info, true).isSuccess()) {
        Budget -= Cost;
        Pending.append(Info.InlinedCallSites.begin(), Info.InlinedCallSites.end());
      }
    }
  }
}
extern "C" void hirsel_llvm_promote_step_window(void *ModuleRef) {
  auto &M = *unwrap(static_cast<LLVMModuleRef>(ModuleRef));
  inlineLoopHelpers(M);
  for (Function &F : M) {
    if (F.isDeclaration() || F.arg_empty() || !F.getArg(0)->getType()->isPointerTy()) continue;
    SmallVector<Instruction *, 16> Accesses;
    SmallVector<CallInst *, 16> Calls;
    SmallVector<ReturnInst *, 8> Returns;
    bool Supported = true;
    for (BasicBlock &BB : F) for (Instruction &I : BB) {
      if (stepWord(I, F.getArg(0), M.getDataLayout())) Accesses.push_back(&I);
      if (auto *C = dyn_cast<CallBase>(&I)) {
        if (auto *CI = dyn_cast<CallInst>(C)) {
          if (CI->isMustTailCall()) Supported = false;
          else if (!isa<IntrinsicInst>(CI) &&
                   !CI->hasFnAttr("hirsel-step-neutral")) Calls.push_back(CI);
        } else Supported = false;
      }
      if (auto *R = dyn_cast<ReturnInst>(&I)) Returns.push_back(R);
    }
    if (Accesses.empty() || !Supported) continue;
    auto *Tag = Accesses.front()->getMetadata(LLVMContext::MD_tbaa);
    IRBuilder<> B(&*F.getEntryBlock().getFirstInsertionPt());
    auto *Word = B.getInt64Ty();
    auto *Counter = B.CreateAlloca(Word, nullptr, "step.window");
    auto *Field = B.CreateConstGEP1_64(B.getInt8Ty(), F.getArg(0), 16, "step.field");
    auto Read = [&]() { auto *V = B.CreateLoad(Word, Field); V->setMetadata(LLVMContext::MD_tbaa, Tag); return V; };
    auto Write = [&]() { auto *S = B.CreateStore(B.CreateLoad(Word, Counter), Field); S->setMetadata(LLVMContext::MD_tbaa, Tag); };
    B.CreateStore(Read(), Counter);
    for (Instruction *I : Accesses) {
      if (auto *L = dyn_cast<LoadInst>(I)) L->setOperand(0, Counter);
      else cast<StoreInst>(I)->setOperand(1, Counter);
      I->setMetadata(LLVMContext::MD_tbaa, nullptr);
    }
    for (CallInst *C : Calls) {
      B.SetInsertPoint(C); Write();
      B.SetInsertPoint(C->getNextNode()); B.CreateStore(Read(), Counter);
    }
    for (ReturnInst *R : Returns) { B.SetInsertPoint(R); Write(); }
    DominatorTree DT(F);
    PromoteMemToReg({Counter}, DT);
  }
}

// Late argument promotion can expose a codec's function pointer only after the
// ordinary inliner ran. Remember loop callbacks before constant propagation;
// give their resolved bodies and newly exposed helpers one loop-call budget
// before lifting fuel. The budget belongs to the whole expansion, including recursion.
extern "C" void hirsel_llvm_mark_loop_callbacks(void *Raw) {
  auto &M = *static_cast<Module *>(Raw);
  for (Function &F : M) {
    if (F.isDeclaration()) continue;
    DominatorTree DT(F);
    LoopInfo LI(DT);
    for (BasicBlock &BB : F) if (LI.getLoopFor(&BB))
      for (Instruction &I : BB) if (auto *C = dyn_cast<CallInst>(&I))
        if (C->isIndirectCall() && !C->isInlineAsm())
          C->setMetadata("hirsel.loop.callback", MDNode::get(M.getContext(), {}));
  }
}
extern "C" void hirsel_llvm_inline_loop_callbacks(void *Raw) {
  auto &M = *static_cast<Module *>(Raw);
  SmallVector<CallInst *, 16> Calls;
  for (Function &F : M) for (BasicBlock &BB : F) for (Instruction &I : BB)
    if (auto *C = dyn_cast<CallInst>(&I))
      if (C->getMetadata("hirsel.loop.callback")) Calls.push_back(C);
  for (CallInst *C : Calls) {
    C->setMetadata("hirsel.loop.callback", nullptr);
    unsigned Budget = 1024;
    SmallVector<CallBase *, 8> Pending{C};
    while (!Pending.empty()) {
      CallBase *Call = Pending.pop_back_val();
      Function *Target = Call->getCalledFunction();
      if (!Target || Target->isDeclaration() || Target == Call->getFunction() ||
          Target->hasFnAttribute(Attribute::NoInline) ||
          Call->hasFnAttr(Attribute::NoInline) || Target->getInstructionCount() > Budget)
        continue;
      unsigned Cost = Target->getInstructionCount();
      InlineFunctionInfo Info;
      if (InlineFunction(*Call, Info, true).isSuccess()) {
        Budget -= Cost;
        Pending.append(Info.InlinedCallSites.begin(), Info.InlinedCallSites.end());
      }
    }
  }
}
