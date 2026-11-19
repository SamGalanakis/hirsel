// A guarded copy retains its original scalar partial-failure loop. The fast
// version requires both complete ranges and existing FORM alias scopes.
#include <bits/c++config.h>
#if defined(__GLIBC__) && !__GLIBC_PREREQ(2, 30)
#undef _GLIBCXX_USE_PTHREAD_COND_CLOCKWAIT
#undef _GLIBCXX_USE_PTHREAD_MUTEX_CLOCKLOCK
#undef _GLIBCXX_USE_PTHREAD_RWLOCK_CLOCKLOCK
#endif
#include "pass_cost.h"
#include "forward_reads.h"
#include "loop_version.h"
#include "step_tree.h"
#include <map>
#include <memory>
#include <llvm/ADT/MapVector.h>
#include <llvm/ADT/SetVector.h>
#include <llvm/Analysis/AliasAnalysis.h>
#include <llvm/Analysis/BlockFrequencyInfo.h>
#include <llvm/Analysis/CycleAnalysis.h>
#include <llvm/Analysis/ValueTracking.h>
#include <llvm/Analysis/LoopInfo.h>
#include <llvm/IR/Dominators.h>
#include <llvm/Transforms/Utils/BasicBlockUtils.h>
#include <llvm/Transforms/Utils/Local.h>
#include <llvm/Transforms/Utils/PromoteMemToReg.h>
#include <llvm/Transforms/Utils/Cloning.h>
#include <llvm/Analysis/AssumptionCache.h>
#include <llvm/Analysis/ScalarEvolutionExpressions.h>
#include <llvm/Analysis/TargetLibraryInfo.h>
#include <llvm/IR/IntrinsicInst.h>
#include <llvm/IR/Module.h>
#include <llvm/IR/PatternMatch.h>
#include <llvm/IR/DebugInfo.h>
#include <llvm/Passes/PassBuilder.h>
#include <llvm/Support/Error.h>
#include <llvm/Transforms/Utils/FunctionComparator.h>
using namespace llvm;
using hirsel_llvm::versionLoop;

// Let LLVM use the checked end to discharge redundant guards first. A
// surviving end check for adjacent offsets can use a length limit without
// replacing checks that already share an end offset with the loop.
extern "C" void hirsel_llvm_fixed_width_checks(void *Raw) noexcept {
  using namespace llvm::PatternMatch;
  for (Function &F : *static_cast<Module *>(Raw))
    for (BasicBlock &BB : F) {
      auto *Branch = dyn_cast<CondBrInst>(BB.getTerminator());
      if (!Branch) continue;
      Value *End, *Length, *Sum, *Start;
      Value *Condition = Branch->getCondition();
      auto Within = m_SpecificICmp(ICmpInst::ICMP_ULE, m_Value(End), m_Value(Length));
      auto NoOverflow = m_SpecificICmp(ICmpInst::ICMP_UGE, m_Value(Sum), m_Value(Start));
      if (!match(Condition, m_c_And(Within, NoOverflow)) || End != Sum) continue;

      // InstCombine has flattened (base + start) + width. The two offsets
      // still differ by one word, so their ordered range test can use start.
      Value *Base;
      const APInt *StartOffset, *EndOffset;
      if (!match(End, m_Add(m_Value(Base), m_APInt(EndOffset))) ||
          !match(Start, m_NUWAddLike(m_Specific(Base), m_APInt(StartOffset))) ||
          EndOffset->ule(*StartOffset) || *EndOffset - *StartOffset != 8)
        continue;

      IRBuilder<> B(Branch);
      Value *Width = ConstantInt::get(End->getType(), 8);
      Value *HasWidth = B.CreateICmpUGE(Length, Width);
      Value *Limit = B.CreateSub(Length, Width);
      Branch->setCondition(B.CreateAnd(HasWidth, B.CreateICmpULE(Start, Limit)));
      RecursivelyDeleteTriviallyDeadInstructions(Condition);
    }
}

extern "C" void hirsel_llvm_prepay_step_trees(void *Raw) {
  hirsel_llvm::prepayStepTrees(*static_cast<Module *>(Raw));
}

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

template <typename Run>
static void runLoopPass(TargetMachine *Machine, const Module &M, StringRef Phase, Run RunPass) {
  PassInstrumentationCallbacks Callbacks;
  PassBuilder PB(Machine, PipelineTuningOptions(), std::nullopt, &Callbacks);
  LoopAnalysisManager LAM;
  FunctionAnalysisManager FAM;
  CGSCCAnalysisManager CGAM;
  ModuleAnalysisManager MAM;
  PB.registerModuleAnalyses(MAM);
  PB.registerCGSCCAnalyses(CGAM);
  PB.registerFunctionAnalyses(FAM);
  PB.registerLoopAnalyses(LAM);
  PB.crossRegisterProxies(LAM, FAM, CGAM, MAM);
  hirsel_llvm::PassCost Cost(Phase, M);
  Cost.registerCallbacks(Callbacks);
  RunPass(PB, FAM);
}

extern "C" void hirsel_llvm_guard_copy_loops(void *Raw, void *Machine) noexcept {
  auto &M = *static_cast<Module *>(Raw);
  runLoopPass(static_cast<TargetMachine *>(Machine), M, "checked-copy", [&](PassBuilder &PB, FunctionAnalysisManager &FAM) {
    FunctionPassManager PM;
    PM.addPass(CheckedCopyPass(PB));
    for (Function &F : M) if (!F.isDeclaration()) PM.run(F, FAM);
  });
}

namespace {
struct ReadCheck {
  CondBrInst *Branch;
  Value *Bound;
  bool OnTrue;
  SetVector<Instruction *> Reads;
};

// Collect only address arithmetic and ordinary reads reached before this
// check. Each staged check runs after all preceding checks in source order.
static bool collectRead(Value *V, Loop &L, DominatorTree &DT,
                        Instruction *Before, SetVector<Instruction *> &Reads) {
  auto *I = dyn_cast<Instruction>(V);
  if (!I || !L.contains(I)) return DT.dominates(V, L.getLoopPredecessor()->getTerminator());
  if (Reads.contains(I)) return true;
  if (!DT.dominates(I, Before)) return false;
  if (auto *Load = dyn_cast<LoadInst>(I)) {
    if (!Load->isSimple()) return false;
  } else if (!isa<GetElementPtrInst>(I)) {
    return false;
  }
  for (Value *Operand : I->operand_values())
    if (!collectRead(Operand, L, DT, Before, Reads)) return false;
  Reads.insert(I);
  return true;
}

static bool versionReadLoop(Loop &L, LoopInfo &LI, DominatorTree &DT,
                            ScalarEvolution &SE, DenseSet<BasicBlock *> &Done) {
  BasicBlock *Entry = L.getLoopPredecessor(), *H = L.getHeader(), *Latch = L.getLoopLatch();
  if (!Entry || !Latch || !L.isInnermost() || L.getNumBlocks() < 3) return false;
  auto *End = dyn_cast<CondBrInst>(Latch->getTerminator());
  auto *Eq = End ? dyn_cast<ICmpInst>(End->getCondition()) : nullptr;
  if (!Eq || Eq->getPredicate() != ICmpInst::ICMP_EQ ||
      L.contains(End->getSuccessor(0)) || End->getSuccessor(1) != H) return false;
  auto *Next = dyn_cast<BinaryOperator>(Eq->getOperand(0));
  auto *Index = Next ? dyn_cast<PHINode>(Next->getOperand(0)) : nullptr;
  auto *One = Next ? dyn_cast<ConstantInt>(Next->getOperand(1)) : nullptr;
  Value *Limit = Eq->getOperand(1);
  if (!Next || Next->getOpcode() != Instruction::Add || !Next->hasNoUnsignedWrap() ||
      !One || !One->isOne() || !Index || Index->getParent() != H ||
      Index->getNumIncomingValues() != 2 || Index->getIncomingValueForBlock(Latch) != Next ||
      !L.isLoopInvariant(Limit) || !DT.dominates(Limit, Entry->getTerminator())) return false;
  Value *Initial = Index->getIncomingValueForBlock(Entry);
  if (!Initial) return false;

  // No loop store, call into the runtime, volatile read, or other effect can
  // invalidate a staged read or change which state the checked path observes.
  for (BasicBlock *BB : L.blocks()) for (Instruction &I : *BB) {
    if (auto *Load = dyn_cast<LoadInst>(&I)) {
      if (!Load->isSimple()) return false;
    } else if (!I.isTerminator() && (I.mayReadOrWriteMemory() || I.mayHaveSideEffects())) {
      return false;
    }
  }

  SmallVector<ReadCheck, 4> Checks;
  unsigned Bounds = 0;
  for (BasicBlock *BB = H; BB != Latch;) {
    auto *Branch = dyn_cast<CondBrInst>(BB->getTerminator());
    auto *Cmp = Branch ? dyn_cast<ICmpInst>(Branch->getCondition()) : nullptr;
    if (!Cmp) return false;
    bool OnTrue = L.contains(Branch->getSuccessor(0));
    if (OnTrue == L.contains(Branch->getSuccessor(1))) return false;
    Value *Bound = nullptr;
    SetVector<Instruction *> Reads;
    Value *Left = Cmp->getOperand(0), *Right = Cmp->getOperand(1);
    auto Pred = OnTrue ? Cmp->getPredicate() : Cmp->getInversePredicate();
    if (Right == Index) {
      std::swap(Left, Right);
      Pred = ICmpInst::getSwappedPredicate(Pred);
    }
    if (Left == Index) {
      Bound = Right;
      if (Pred == ICmpInst::ICMP_NE) {
        // IndVarSimplify can turn the first unsigned check into equality
        // with max(start, length), or with length itself for a zero start.
        // Limit <= length also proves inequality for every fast iteration.
        auto *Max = dyn_cast<IntrinsicInst>(Bound);
        if (Max && Max->getIntrinsicID() == Intrinsic::umax) {
          if (Max->getArgOperand(0) == Initial) Bound = Max->getArgOperand(1);
          else if (Max->getArgOperand(1) == Initial) Bound = Max->getArgOperand(0);
        }
      } else if (Pred != ICmpInst::ICMP_ULT) {
        return false;
      }
      if (!collectRead(Bound, L, DT, Branch, Reads)) return false;
      ++Bounds;
    } else if (!collectRead(Cmp->getOperand(0), L, DT, Branch, Reads) ||
               !collectRead(Cmp->getOperand(1), L, DT, Branch, Reads)) {
      return false;
    }
    Checks.push_back({Branch, Bound, OnTrue, std::move(Reads)});
    BB = Branch->getSuccessor(OnTrue ? 0 : 1);
    if (Checks.size() >= L.getNumBlocks()) return false;
  }
  if (Bounds < 2 || Checks.size() + 1 != L.getNumBlocks()) return false;

  // SimplifyCFG can merge a preheader with a preceding bounds check. Stage
  // the new loads only on its loop-entry edge, after that check succeeded.
  BasicBlock *PH = L.getLoopPreheader();
  if (!PH) {
    PH = InsertPreheaderForLoop(&L, &DT, &LI, nullptr, /*PreserveLCSSA=*/true);
    if (!PH) return false;
    SE.forgetLoop(&L);
  }
  IRBuilder<> B(PH->getTerminator());
  Value *Nonempty = B.CreateFreeze(B.CreateICmpULT(Initial, Limit));
  versionLoop(L, Nonempty, LI, DT, SE, "read.fast.ph", ".read.checked");
  auto *Route = cast<CondBrInst>(PH->getTerminator());
  BasicBlock *Fast = Route->getSuccessor(0), *Slow = Route->getSuccessor(1);
  Done.insert(H);
  Done.insert(Slow->getSingleSuccessor());
  DenseMap<Value *, Value *> Staged;
  auto substitute = [&](Value *V) { return Staged.lookup(V) ? Staged.lookup(V) : V; };
  for (const ReadCheck &Check : Checks) {
    auto *Guard = BasicBlock::Create(H->getContext(), "read.check", H->getParent(), Fast);
    Route->setSuccessor(0, Guard);
    B.SetInsertPoint(Guard);
    auto *Cmp = cast<ICmpInst>(Check.Branch->getCondition());
    for (Instruction *Read : Check.Reads) {
      if (Staged.contains(Read)) continue;
      auto *Copy = Read->clone();
      for (Use &Operand : Copy->operands()) Operand.set(substitute(Operand.get()));
      Copy->setMetadata(LLVMContext::MD_alias_scope, nullptr);
      Copy->setMetadata(LLVMContext::MD_noalias, nullptr);
      B.Insert(Copy);
      Staged[Read] = Copy;
    }
    Value *Good;
    if (Check.Bound) {
      Good = B.CreateICmpULE(Limit, substitute(Check.Bound));
    } else {
      Good = B.CreateICmp(Cmp->getPredicate(), substitute(Cmp->getOperand(0)),
                          substitute(Cmp->getOperand(1)));
      if (!Check.OnTrue) Good = B.CreateNot(Good);
    }
    Route = B.CreateCondBr(B.CreateFreeze(Good), Fast, Slow);
    Check.Branch->setCondition(B.getInt1(Check.OnTrue));
  }
  for (auto [Original, Copy] : Staged) Original->replaceAllUsesWith(Copy);
  return true;
}
}

// A private cursor which advances by at most one on a back edge stays behind
// the counted cursor. The exit test bounds that cursor by the same length SSA
// value as the write check, including when the length originally came from memory.
static bool removeBoundedCursorChecks(Function &F, FunctionAnalysisManager &AM) {
  using namespace llvm::PatternMatch;
  auto &LI = AM.getResult<LoopAnalysis>(F);
  auto &SE = AM.getResult<ScalarEvolutionAnalysis>(F);
  SmallVector<CondBrInst *, 4> Redundant;
  for (Loop *L : LI.getLoopsInPreorder()) {
    auto *Entry = L->getLoopPredecessor();
    auto *Latch = L->getLoopLatch();
    auto *Header = L->getHeader();
    if (!Entry || !Latch || !L->isInnermost()) continue;
    auto *Exit = dyn_cast<CondBrInst>(Latch->getTerminator());
    if (!Exit || L->contains(Exit->getSuccessor(0)) || Exit->getSuccessor(1) != Header)
      continue;
    Value *Advance, *Length, *ReadValue;
    if (!match(Exit->getCondition(),
               m_SpecificICmp(ICmpInst::ICMP_EQ, m_Value(Advance), m_Value(Length))) ||
        !match(Advance, m_c_Add(m_Value(ReadValue), m_One())) ||
        !L->isLoopInvariant(Length)) continue;
    auto *Read = dyn_cast<PHINode>(ReadValue);
    if (!Read || Read->getParent() != Header || Read->getNumIncomingValues() != 2 ||
        Read->getIncomingValueForBlock(Latch) != Advance) continue;
    Value *ReadStart = Read->getIncomingValueForBlock(Entry);
    if (!SE.isLoopEntryGuardedByCond(L, ICmpInst::ICMP_ULT,
                                    SE.getSCEV(ReadStart), SE.getSCEV(Length))) continue;

    SmallVector<BasicBlock *, 4> Exiting;
    L->getExitingBlocks(Exiting);
    for (BasicBlock *BB : Exiting) {
      auto *Check = dyn_cast<CondBrInst>(BB->getTerminator());
      Value *WriteValue;
      if (!Check || !L->contains(Check->getSuccessor(0)) ||
          !match(Check->getCondition(),
                 m_SpecificICmp(ICmpInst::ICMP_ULT, m_Value(WriteValue), m_Specific(Length))))
        continue;
      auto *Write = dyn_cast<PHINode>(WriteValue);
      if (!Write || Write->getParent() != Header || Write->getNumIncomingValues() != 2 ||
          !SE.isLoopEntryGuardedByCond(L, ICmpInst::ICMP_ULE,
                                      SE.getSCEV(Write->getIncomingValueForBlock(Entry)),
                                      SE.getSCEV(ReadStart))) continue;
      Value *Back = Write->getIncomingValueForBlock(Latch);
      auto IsUnitStep = [Write](Value *V) {
        return V == Write || match(V, m_c_Add(m_Specific(Write), m_One()));
      };
      auto *Merge = dyn_cast<PHINode>(Back);
      if (IsUnitStep(Back) || (Merge && Merge->getParent() == Latch &&
                             all_of(Merge->incoming_values(), IsUnitStep)))
        Redundant.push_back(Check);
    }
  }
  for (CondBrInst *Check : Redundant)
    Check->setCondition(ConstantInt::getTrue(F.getContext()));
  return !Redundant.empty();
}

using RecordFields = std::map<uint64_t, StoreInst *>;

// A private stack home has just one store per field. Since the stores are
// outside CFG cycles, a field known on an incoming edge keeps its value
// through the join. Any other use can expose or write the slot.
static bool privateRecordFields(AllocaInst &Slot, uint64_t Size,
                                const SmallSetVector<PHINode *, 4> &Joins,
                                CycleInfo &Cycles, RecordFields &Fields) {
  const auto &DL = Slot.getDataLayout();
  auto Allocated = Slot.getAllocationSize(DL);
  if (!Slot.isStaticAlloca() || !Allocated || Allocated->isScalable() ||
      Allocated->getFixedValue() != Size) return false;

  auto AddStore = [&](Use &U, uint64_t Offset) {
    auto *Store = dyn_cast<StoreInst>(U.getUser());
    if (!Store || !Store->isSimple() || &U != &Store->getOperandUse(1) ||
        Cycles.getCycle(Store->getParent())) return false;
    auto *Field = dyn_cast<IntegerType>(Store->getValueOperand()->getType());
    if (!Field || Field->getBitWidth() % 8 ||
        Field->getBitWidth() / 8 > Size - Offset) return false;
    return Fields.try_emplace(Offset, Store).second;
  };
  for (Use &U : Slot.uses()) {
    auto *Phi = dyn_cast<PHINode>(U.getUser());
    if (Phi && Joins.contains(Phi)) continue;
    if (auto *GEP = dyn_cast<GetElementPtrInst>(U.getUser())) {
      APInt Offset(DL.getIndexTypeSizeInBits(GEP->getType()), 0);
      if (&U != &GEP->getOperandUse(0) ||
          !GEP->accumulateConstantOffset(DL, Offset) ||
          Offset.isNegative() || Offset.uge(Size) || !GEP->hasOneUse() ||
          !AddStore(*GEP->use_begin(), Offset.getZExtValue())) return false;
    } else if (!AddStore(U, 0)) {
      return false;
    }
  }
  uint64_t End = 0;
  for (auto [Offset, S] : Fields) {
    if (Offset < End) return false;
    End = Offset + S->getValueOperand()->getType()->getIntegerBitWidth() / 8;
  }
  return !Fields.empty();
}

// Integer PHI cycles preserve the range of their non-PHI inputs.
static bool isBitValue(Value *V) {
  SmallSetVector<Value *, 8> Pending;
  Pending.insert(V);
  for (unsigned I = 0; I < Pending.size(); ++I) {
    if (auto *Phi = dyn_cast<PHINode>(Pending[I])) {
      for (Value *Incoming : Phi->incoming_values()) Pending.insert(Incoming);
    } else if (auto *C = dyn_cast<ConstantInt>(Pending[I])) {
      if (C->getValue().getActiveBits() > 1) return false;
    } else {
      return false;
    }
  }
  return true;
}

// Reuse a tested flag when the matching branch edge determines the field
// on every path that selects a stack home.
static Value *existingRecordFlag(MemCpyInst &Copy, uint64_t Offset, Type *T,
                                 const SmallSetVector<PHINode *, 4> &Joins,
                                 const MapVector<AllocaInst *, RecordFields> &Slots,
                                 CycleInfo &Cycles, DominatorTree &DT) {
  bool HasZero = false;
  bool HasOne = false;
  for (const auto &Entry : Slots) {
    auto *C = dyn_cast<ConstantInt>(Entry.second.at(Offset)->getValueOperand());
    if (!C || C->getValue().getActiveBits() > 1) return nullptr;
    HasZero |= C->isZero();
    HasOne |= C->isOne();
  }
  if (!HasZero || !HasOne) return nullptr;
  for (auto *Node = DT.getNode(Copy.getParent()); Node; Node = Node->getIDom()) {
    BasicBlock *BB = Node->getBlock();
    auto *Branch = dyn_cast<CondBrInst>(BB->getTerminator());
    if (!Branch || Cycles.getCycle(BB)) continue;
    auto *Cmp = dyn_cast<ICmpInst>(Branch->getCondition());
    if (!Cmp || !Cmp->isEquality() || Cmp->getOperand(0)->getType() != T) continue;
    auto *Zero = dyn_cast<ConstantInt>(Cmp->getOperand(1));
    if (!Zero || !Zero->isZero() || !isBitValue(Cmp->getOperand(0))) continue;
    bool Matches = true;
    for (PHINode *Join : Joins) {
      for (unsigned I = 0; I < Join->getNumIncomingValues(); ++I) {
        auto *Slot = dyn_cast<AllocaInst>(Join->getIncomingValue(I));
        if (!Slot) continue;
        auto *C = cast<ConstantInt>(Slots.find(Slot)->second.at(Offset)->getValueOperand());
        bool TrueEdge = C->isZero() == (Cmp->getPredicate() == ICmpInst::ICMP_EQ);
        BasicBlockEdge Edge(BB, Branch->getSuccessor(TrueEdge ? 0 : 1));
        if (!DT.dominates(Edge, Join->getIncomingBlock(I))) Matches = false;
      }
    }
    if (Matches) return Cmp->getOperand(0);
  }
  return nullptr;
}

static bool forwardRecordJoin(MemCpyInst &Copy, FunctionAnalysisManager &AM) {
  auto *Root = dyn_cast<PHINode>(Copy.getRawSource());
  auto *Length = dyn_cast<ConstantInt>(Copy.getLength());
  if (!Root || Root->getParent() != Copy.getParent() || Copy.isVolatile() ||
      !Length || Length->isZero() || Length->getValue().getActiveBits() > 64)
    return false;

  SmallSetVector<PHINode *, 4> Joins;
  Joins.insert(Root);
  MapVector<AllocaInst *, RecordFields> Slots;
  for (unsigned I = 0; I < Joins.size(); ++I) {
    for (Value *Incoming : Joins[I]->incoming_values()) {
      if (auto *Join = dyn_cast<PHINode>(Incoming)) Joins.insert(Join);
      else if (auto *Slot = dyn_cast<AllocaInst>(Incoming)) Slots.try_emplace(Slot);
      else return false;
    }
  }
  if (Slots.size() < 2) return false;
  for (PHINode *Join : Joins)
    for (Use &U : Join->uses()) {
      if (U.getUser() == &Copy && &U == &Copy.getArgOperandUse(1)) continue;
      auto *User = dyn_cast<PHINode>(U.getUser());
      if (!User || !Joins.contains(User)) return false;
    }

  Function &F = *Copy.getFunction();
  auto &Cycles = AM.getResult<CycleAnalysis>(F);
  for (PHINode *Join : Joins)
    if (Cycles.getCycle(Join->getParent())) return false;
  auto &DT = AM.getResult<DominatorTreeAnalysis>(F);
  const auto &Shape = Slots.front().second;
  for (auto &[Slot, Fields] : Slots) {
    if (!privateRecordFields(*Slot, Length->getZExtValue(), Joins, Cycles, Fields) ||
        Fields.size() != Shape.size()) return false;
    for (auto [Offset, Store] : Fields) {
      auto Pos = Shape.find(Offset);
      if (Pos == Shape.end() ||
          Pos->second->getValueOperand()->getType() != Store->getValueOperand()->getType())
        return false;
    }
  }
  for (PHINode *Join : Joins)
    for (unsigned I = 0; I < Join->getNumIncomingValues(); ++I)
      if (auto *Slot = dyn_cast<AllocaInst>(Join->getIncomingValue(I)))
        for (auto [Offset, Store] : Slots.find(Slot)->second)
          if (!DT.dominates(Store, Join->getIncomingBlock(I)->getTerminator()))
            return false;

  // Materialize at the original copy, including its uninitialized bytes.
  IRBuilder<> B(&Copy);
  B.SetCurrentDebugLocation(Copy.getDebugLoc());
  uint64_t Covered = 0;
  auto FillGap = [&](uint64_t End) {
    if (Covered == End) return;
    Value *Dest = B.CreateConstGEP1_64(B.getInt8Ty(), Copy.getRawDest(), Covered);
    B.CreateMemSet(Dest, UndefValue::get(B.getInt8Ty()), End - Covered, Align(1));
  };
  for (auto [Offset, Store] : Shape) {
    FillGap(Offset);
    Covered = Offset + Store->getValueOperand()->getType()->getIntegerBitWidth() / 8;
    Type *T = Store->getValueOperand()->getType();
    Value *Dest = B.CreateConstGEP1_64(B.getInt8Ty(), Copy.getRawDest(), Offset);
    if (Value *Flag = existingRecordFlag(Copy, Offset, T, Joins, Slots, Cycles, DT)) {
      B.CreateAlignedStore(Flag, Dest, Align(1));
      continue;
    }
    DenseMap<PHINode *, PHINode *> Fields;
    for (PHINode *Join : Joins)
      Fields[Join] = PHINode::Create(T, Join->getNumIncomingValues(), "record.field",
                                    Join->getIterator());
    for (PHINode *Join : Joins)
      for (unsigned I = 0; I < Join->getNumIncomingValues(); ++I) {
        Value *Incoming = Join->getIncomingValue(I);
        Value *Field;
        if (auto *Phi = dyn_cast<PHINode>(Incoming)) {
          Field = Fields.lookup(Phi);
        } else {
          auto &IncomingFields = Slots.find(cast<AllocaInst>(Incoming))->second;
          Field = IncomingFields.at(Offset)->getValueOperand();
        }
        Fields.lookup(Join)->addIncoming(Field, Join->getIncomingBlock(I));
      }
    B.CreateAlignedStore(Fields.lookup(Root), Dest, Align(1));
  }
  FillGap(Length->getZExtValue());
  Copy.eraseFromParent();
  for (PHINode *Join : Joins) Join->dropAllReferences();
  for (PHINode *Join : Joins) Join->eraseFromParent();
  for (auto &Entry : Slots)
    for (auto &Field : Entry.second) Field.second->eraseFromParent();
  return true;
}

static bool forwardRecordJoins(Function &F, FunctionAnalysisManager &AM) {
  bool Changed = false;
  for (BasicBlock &BB : F)
    for (Instruction &I : make_early_inc_range(BB))
      if (auto *Copy = dyn_cast<MemCpyInst>(&I))
        Changed |= forwardRecordJoin(*Copy, AM);
  return Changed;
}

extern "C" void hirsel_llvm_finish_reads(void *Raw, void *Machine) noexcept {
  auto &M = *static_cast<Module *>(Raw);
  runLoopPass(static_cast<TargetMachine *>(Machine), M, "finish-reads", [&](PassBuilder &PB, FunctionAnalysisManager &FAM) {
    FunctionPassManager Clean;
    cantFail(PB.parsePassPipeline(Clean,
        "simplifycfg,loop-mssa(licm),instcombine<no-verify-fixpoint>,simplifycfg"));
    for (Function &F : M) if (!F.isDeclaration()) {
      DenseSet<BasicBlock *> Done;
      for (;;) {
        auto &LI = FAM.getResult<LoopAnalysis>(F);
        auto &DT = FAM.getResult<DominatorTreeAnalysis>(F);
        auto &SE = FAM.getResult<ScalarEvolutionAnalysis>(F);
        bool Changed = false;
        for (Loop *L : LI.getLoopsInPreorder())
          if (!Done.contains(L->getHeader()) && versionReadLoop(*L, LI, DT, SE, Done)) {
            Changed = true;
            break;
          }
        if (!Changed) break;
        FAM.invalidate(F, PreservedAnalyses::none());
      }
      const bool BoundedCursors = removeBoundedCursorChecks(F, FAM);
      if (BoundedCursors) FAM.invalidate(F, PreservedAnalyses::none());
      const bool Forwarded = hirsel_llvm::forwardSnapshotReads(F, FAM);
      if (Forwarded) FAM.invalidate(F, PreservedAnalyses::none());
      const bool Records = forwardRecordJoins(F, FAM);
      if (Records) FAM.invalidate(F, PreservedAnalyses::none());
      if (Records || BoundedCursors || Forwarded || !Done.empty()) Clean.run(F, FAM);
    }
  });
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
// A full copy into a private slot can disappear once SROA sees the read-only
// call's field loads. Limit early inlining to slots dedicated to that call.
static bool hasSnapshotArgument(CallInst &Call, const DataLayout &DL) {
  Function &Target = *Call.getCalledFunction();
  for (Argument &Arg : Target.args()) {
    unsigned I = Arg.getArgNo();
    if (!Arg.getType()->isPointerTy() || !Arg.onlyReadsMemory()) continue;
    auto *Slot = dyn_cast<AllocaInst>(Call.getArgOperand(I));
    if (!Slot || !Slot->isStaticAlloca() || !Slot->hasNUses(2)) continue;
    auto Bytes = Slot->getAllocationSize(DL);
    // Leave small copies to the ordinary inliner.
    if (!Bytes || Bytes->isScalable() || Bytes->getFixedValue() < 32) continue;
    for (User *U : Slot->users()) {
      auto *Copy = dyn_cast<MemCpyInst>(U);
      if (!Copy || Copy->getRawDest() != Slot || Copy->isVolatile() ||
          Copy->getParent() != Call.getParent() || !Copy->comesBefore(&Call)) continue;
      auto *Length = dyn_cast<ConstantInt>(Copy->getLength());
      if (Length && Length->equalsInt(Bytes->getFixedValue())) return true;
    }
  }
  return false;
}

extern "C" void hirsel_llvm_inline_snapshot_calls(void *Raw) {
  auto &M = *static_cast<Module *>(Raw);
  for (Function &F : M) {
    if (F.isDeclaration()) continue;
    DominatorTree DT(F);
    LoopInfo LI(DT);
    SmallVector<CallInst *, 8> Calls;
    for (BasicBlock &BB : F) if (LI.getLoopFor(&BB))
      for (Instruction &I : BB) if (auto *Call = dyn_cast<CallInst>(&I)) {
        Function *Target = Call->getCalledFunction();
        if (!Target || Target == &F || Target->isDeclaration() ||
            Call->hasFnAttr(Attribute::NoInline) ||
            Target->hasFnAttribute(Attribute::NoInline) ||
            !hasSnapshotArgument(*Call, M.getDataLayout())) continue;
        Calls.push_back(Call);
      }
    unsigned Budget = 2048;
    for (CallInst *Call : Calls) {
      unsigned Cost = Call->getCalledFunction()->getInstructionCount();
      if (Cost > Budget) continue;
      InlineFunctionInfo Info;
      if (InlineFunction(*Call, Info, true).isSuccess()) Budget -= Cost;
    }
  }
}

// Select an immediate multiply for byte strides that otherwise require an
// x86 LEA and a shift. Keep address arithmetic visible until after IR passes.
extern "C" void hirsel_llvm_lower_x86_index_strides(void *Raw) {
  auto &M = *static_cast<Module *>(Raw);
  if (Triple(M.getTargetTriple()).getArch() != Triple::x86_64) return;
  const auto &DL = M.getDataLayout();
  if (DL.getIndexSizeInBits(0) != 64) return;
  for (Function &F : M) for (BasicBlock &BB : F) {
    for (auto I = BB.begin(); I != BB.end();) {
      auto *GEP = dyn_cast<GetElementPtrInst>(&*I++);
      if (!GEP || GEP->getPointerAddressSpace() != 0 || GEP->getType()->isVectorTy()) continue;
      SmallMapVector<Value *, APInt, 4> Terms;
      APInt Offset(64, 0);
      if (!GEP->collectOffset(DL, 64, Terms, Offset) || Terms.size() != 1) continue;
      auto [Index, Scale] = Terms.front();
      if (!Index->getType()->isIntegerTy(64)) continue;
      if (Scale.isOne()) {
        auto *Mul = dyn_cast<BinaryOperator>(Index);
        if (!Mul || Mul->getOpcode() != Instruction::Mul) continue;
        auto *C = dyn_cast<ConstantInt>(Mul->getOperand(1));
        if (!C) continue;
        Index = Mul->getOperand(0);
        Scale = C->getValue();
      }
      uint64_t Stride = Scale.getZExtValue();
      if (Stride < 48 || Stride > INT32_MAX || Stride % 3 ||
          !isPowerOf2_64(Stride / 3)) continue;
      IRBuilder<> B(GEP);
      B.SetCurrentDebugLocation(GEP->getDebugLoc());
      auto *Word = B.getInt64Ty();
      auto *Ty = FunctionType::get(Word, {Word}, false);
      auto *Multiply = InlineAsm::get(Ty, "imulq $$" + std::to_string(Stride) + ", $1, $0",
                                      "=r,r,~{flags}", false);
      Value *Bytes = B.CreateCall(Multiply, {Index});
      if (!Offset.isZero()) Bytes = B.CreateAdd(Bytes, ConstantInt::get(M.getContext(), Offset));
      auto *Address = B.CreateGEP(B.getInt8Ty(), GEP->getPointerOperand(), Bytes);
      GEP->replaceAllUsesWith(Address);
      GEP->eraseFromParent();
    }
  }
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
extern "C" void hirsel_llvm_promote_development_slots(void *ModuleRef) {
  auto &M = *unwrap(static_cast<LLVMModuleRef>(ModuleRef));
  for (Function &F : M) {
    if (F.isDeclaration() || F.hasOptNone()) continue;
    SmallVector<AllocaInst *, 16> Slots;
    auto collect = [&] {
      Slots.clear();
      for (Instruction &I : F.getEntryBlock()) {
        auto *Slot = dyn_cast<AllocaInst>(&I);
        // The Step counter already has one home across all call sites. Fast
        // register allocation can spill each promoted counter value separately.
        if (Slot && Slot->getName() != "hirsel.step.counter" && isAllocaPromotable(Slot))
          Slots.push_back(Slot);
      }
    };
    collect();
    if (Slots.empty()) continue;
    DominatorTree DT(F);
    AssumptionCache AC(F);
    do {
      PromoteMemToReg(Slots, DT, &AC);
      collect();
    } while (!Slots.empty());
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


// Inline the constant-argument leaf of a directly recursive function. Only
// inspect branches on that argument. All effects, including failing prefixes,
// stay in the cloned leaf and execute after dispatch.
static CondBrInst *leafBranch(BasicBlock &Block, Argument &Arg, ConstantInt &Value,
                              unsigned &Direction) {
  auto *Branch = dyn_cast<CondBrInst>(Block.getTerminator());
  auto *Cmp = Branch ? dyn_cast<ICmpInst>(Branch->getCondition()) : nullptr;
  if (!Cmp || Cmp->getOperand(0) != &Arg) return nullptr;
  auto *Rhs = dyn_cast<ConstantInt>(Cmp->getOperand(1));
  if (!Rhs) return nullptr;
  Direction = ICmpInst::compare(Value.getValue(), Rhs->getValue(), Cmp->getPredicate()) ? 0 : 1;
  return Branch;
}

static bool hasSmallLeaf(Function &F, Argument &Arg, ConstantInt &Value,
                         ArrayRef<CallInst *> Recursions) {
  SmallPtrSet<BasicBlock *, 32> Visited;
  SmallVector<BasicBlock *, 16> Pending{&F.getEntryBlock()};
  unsigned Budget = 1024;
  while (!Pending.empty()) {
    auto *Block = Pending.pop_back_val();
    if (!Visited.insert(Block).second) continue;
    if (Block->size() > Budget || Block->hasAddressTaken() ||
        llvm::any_of(*Block, [](Instruction &I) {
          auto *Call = dyn_cast<CallBase>(&I);
          return Call && (Call->cannotDuplicate() || Call->isConvergent());
        }))
      return false;
    Budget -= Block->size();
    unsigned Direction;
    if (auto *Branch = leafBranch(*Block, Arg, Value, Direction))
      Pending.push_back(Branch->getSuccessor(Direction));
    else
      Pending.append(succ_begin(Block), succ_end(Block));
  }
  return llvm::none_of(Recursions, [&](CallInst *C) { return Visited.contains(C->getParent()); });
}

static void inlineRecursiveLeaf(CallInst &Call, Function &Leaf, unsigned Index,
                                ConstantInt &Value) {
  IRBuilder<> B(&Call);
  auto *Arg = B.CreateFreeze(Call.getArgOperand(Index));
  auto *IsLeaf = B.CreateICmpEQ(Arg, &Value);
  Instruction *LeafEnd, *RecursiveEnd;
  SplitBlockAndInsertIfThenElse(IsLeaf, &Call, &LeafEnd, &RecursiveEnd);
  auto *LeafCall = cast<CallInst>(Call.clone());
  LeafCall->setCalledFunction(&Leaf);
  LeafCall->setArgOperand(Index, &Value);
  LeafCall->setTailCallKind(CallInst::TCK_None);
  LeafCall->insertBefore(LeafEnd->getIterator());
  if (!Call.getType()->isVoidTy()) {
    auto *Result = PHINode::Create(Call.getType(), 2, "recursive.result", Call.getIterator());
    Call.replaceAllUsesWith(Result);
    Result->addIncoming(LeafCall, LeafEnd->getParent());
    Result->addIncoming(&Call, RecursiveEnd->getParent());
  }
  Call.moveBeforePreserving(RecursiveEnd->getIterator());
  Call.setTailCallKind(CallInst::TCK_None);
  InlineFunctionInfo Info;
  if (!InlineFunction(*LeafCall, Info, true).isSuccess()) {
    LeafCall->setCalledFunction(Call.getCalledFunction());
    LeafCall->setArgOperand(Index, Call.getArgOperand(Index));
  }
}

extern "C" void hirsel_llvm_inline_recursive_leaves(void *Raw) {
  auto &M = *static_cast<Module *>(Raw);
  for (auto &F : make_early_inc_range(M)) {
    if (F.isDeclaration() || !F.hasExactDefinition() || F.hasOptNone() || F.hasFnAttribute(Attribute::NoInline))
      continue;
    SmallVector<CallInst *, 4> Recursions;
    for (auto &Use : F.uses())
      if (auto *Call = dyn_cast<CallInst>(Use.getUser()))
        if (Call->isCallee(&Use) && Call->getFunction() == &F)
          Recursions.push_back(Call);
    if (Recursions.empty() || Recursions.size() > 4 ||
        llvm::any_of(Recursions, [](CallInst *C) {
          return C->isMustTailCall() || C->hasFnAttr(Attribute::NoInline) ||
                 C->cannotDuplicate() || C->hasOperandBundles();
        }))
      continue;
    SmallVector<std::pair<Argument *, ConstantInt *>, 4> Candidates;
    for (auto &Block : F) {
      auto *Branch = dyn_cast<CondBrInst>(Block.getTerminator());
      auto *Cmp = Branch ? dyn_cast<ICmpInst>(Branch->getCondition()) : nullptr;
      if (!Cmp || !Cmp->isEquality()) continue;
      auto *Arg = dyn_cast<Argument>(Cmp->getOperand(0));
      auto *Value = dyn_cast<ConstantInt>(Cmp->getOperand(1));
      if (Arg && Value && !llvm::is_contained(Candidates, std::make_pair(Arg, Value)))
        Candidates.emplace_back(Arg, Value);
    }
    for (auto [Arg, Value] : Candidates) {
      if (!hasSmallLeaf(F, *Arg, *Value, Recursions)) continue;
      ValueToValueMapTy Map;
      auto *Leaf = CloneFunction(&F, Map);
      Leaf->setLinkage(GlobalValue::InternalLinkage);
      Leaf->setComdat(nullptr);
      auto &LeafArg = *Leaf->getArg(Arg->getArgNo());
      for (auto &Block : *Leaf) {
        unsigned Direction;
        if (auto *Branch = leafBranch(Block, LeafArg, *Value, Direction))
          Branch->setCondition(ConstantInt::getBool(M.getContext(), Direction == 0));
        ConstantFoldTerminator(&Block);
      }
      removeUnreachableBlocks(*Leaf);
      LeafArg.replaceAllUsesWith(Value);
      for (auto *Call : Recursions)
        inlineRecursiveLeaf(*Call, *Leaf, Arg->getArgNo(), *Value);
      Leaf->eraseFromParent();
      break;
    }
  }
}

// Outlined Step edges can differ only in the site forwarded to the runtime.
// Keep their existing call boundaries and share the rest of the outlined body.
static void shareStepWindows(Module &M, Function &Step) {
  struct Window {
    CallInst *Call;
    Constant *Site;
  };
  GlobalNumberState Numbers;
  auto Less = [&](Function *Left, Function *Right) {
    return FunctionComparator(Left, Right, &Numbers).compare() < 0;
  };
  std::map<Function *, SmallVector<Window, 4>, decltype(Less)> Groups(Less);
  for (auto &F : M) {
    if (!F.hasLocalLinkage() || !F.hasFnAttribute(Attribute::Cold) || F.hasAddressTaken() ||
        F.size() != 1 || F.arg_size() != 3 || F.getCallingConv() != CallingConv::Fast ||
        F.isVarArg() || !F.getReturnType()->isIntegerTy(1) ||
        !F.getArg(0)->getType()->isPointerTy() || !F.getArg(1)->getType()->isIntegerTy(64) ||
        !F.getArg(2)->getType()->isPointerTy() || F.getEntryBlock().hasAddressTaken())
      continue;
    CallInst *Call = nullptr;
    for (auto &I : F.getEntryBlock()) {
      auto *C = dyn_cast<CallInst>(&I);
      if (!C || isa<IntrinsicInst>(C)) continue;
      if (Call || C->getCalledFunction() != &Step || C->hasOperandBundles()) {
        Call = nullptr;
        break;
      }
      Call = C;
    }
    if (!Call) continue;
    auto *Site = dyn_cast<Constant>(Call->getArgOperand(1));
    if (!Site || (!isa<ConstantInt>(Site) && !isa<ConstantExpr>(Site))) continue;
    // Compare using the same placeholder. Restore the real site at every exit.
    Call->setArgOperand(1, PoisonValue::get(Site->getType()));
    Groups[&F].push_back({Call, Site});
  }
  for (auto &[F, Windows] : Groups) {
    // The new shared body needs one frame and one unwind entry of its own.
    if (Windows.size() < 4) {
      for (const auto &Window : Windows) Window.Call->setArgOperand(1, Window.Site);
      continue;
    }
    auto &First = Windows.front();
    SmallVector<Type *, 4> Args(F->getFunctionType()->params());
    Args.push_back(First.Site->getType());
    auto *Type = FunctionType::get(F->getReturnType(), Args, false);
    auto *Shared = Function::Create(Type, GlobalValue::InternalLinkage, "hirsel.step.window", M);
    ValueToValueMapTy Map;
    for (auto &Arg : F->args()) Map[&Arg] = Shared->getArg(Arg.getArgNo());
    SmallVector<ReturnInst *, 1> Returns;
    CloneFunctionInto(Shared, F, Map, CloneFunctionChangeType::LocalChangesOnly, Returns);
    Shared->setCallingConv(F->getCallingConv());
    cast<CallInst>(Map[First.Call])->setArgOperand(1, Shared->getArg(F->arg_size()));
    stripDebugInfo(*Shared);
    for (const auto &Window : Windows) {
      auto *Original = Window.Call->getFunction();
      Original->getEntryBlock().eraseFromParent();
      IRBuilder<> Builder(BasicBlock::Create(M.getContext(), "", Original));
      auto *Tail = Builder.CreateCall(
          Shared, {Original->getArg(0), Original->getArg(1), Original->getArg(2), Window.Site});
      Tail->setCallingConv(Shared->getCallingConv());
      Tail->setAttributes(Shared->getAttributes());
      Tail->setTailCallKind(CallInst::TCK_Tail);
      Builder.CreateRet(Tail);
    }
  }
}

// A built-in cause is a constant: zero words around its kind and site. SysV
// passes it by value, so every raise copies 32 bytes onto the outgoing stack.
// Pass the kind and site in registers instead, after the IR pipeline: a
// cheaper raise seen by O3 changes inlining and unswitching, and Etc::Sieve's
// marking loop then spills its index (+51% instructions). Raises inside a loop
// keep the copy; their argument registers change greedy allocation of the hot
// loop around them (StoragePool's fill counter moves to a spilled r8, +0.68%).
static void scalarRaises(Module &M) {
  auto *Raise = M.getFunction("hirsel_core_raise");
  if (!Raise) return;
  auto &Context = M.getContext();
  auto *Word = Type::getInt64Ty(Context);
  auto *Kind = Type::getInt8Ty(Context);
  FunctionCallee Builtin = M.getOrInsertFunction(
      "hirsel_core_raise_builtin",
      FunctionType::get(Type::getVoidTy(Context), {Raise->getArg(0)->getType(), Kind, Word}, false));
  SmallVector<CallInst *, 64> Calls;
  for (auto &Use : Raise->uses())
    if (auto *Call = dyn_cast<CallInst>(Use.getUser()); Call && Call->isCallee(&Use))
      Calls.push_back(Call);
  SmallPtrSet<GlobalVariable *, 32> Causes;
  DenseMap<Function *, std::unique_ptr<LoopInfo>> Loops;
  for (auto *Call : Calls) {
    auto &Info = Loops[Call->getFunction()];
    if (!Info) Info = std::make_unique<LoopInfo>(DominatorTree(*Call->getFunction()));
    if (Info->getLoopFor(Call->getParent())) continue;
    auto *Cause = dyn_cast<GlobalVariable>(Call->getArgOperand(1));
    if (!Cause || !Cause->isConstant() || !Cause->hasInitializer() ||
        Cause->isExternallyInitialized())
      continue;
    auto *Init = dyn_cast<ConstantStruct>(Cause->getInitializer());
    if (!Init || Init->getNumOperands() != 3 || !Init->getOperand(0)->isNullValue()) continue;
    auto *KindWord = dyn_cast<ConstantInt>(Init->getOperand(1));
    auto *Site = Init->getOperand(2);
    if (!KindWord || Site->getType() != Word) continue;
    IRBuilder<> Builder(Call);
    auto *Scalar = Builder.CreateCall(
        Builtin, {Call->getArgOperand(0),
                  ConstantInt::get(Kind, KindWord->getZExtValue() & 0xff), Site});
    Scalar->setCallingConv(Call->getCallingConv());
    Scalar->setAttributes(Call->getAttributes().removeParamAttributes(Context, 1));
    Scalar->addParamAttr(1, Attribute::ZExt);
    Scalar->copyMetadata(*Call);
    Call->eraseFromParent();
    Causes.insert(Cause);
  }
  for (auto *Cause : Causes)
    if (Cause->use_empty()) Cause->eraseFromParent();
}

// Inlining and Step versioning can copy the same source Step many times. On
// x86-64 a relocated source position costs a ten-byte movabs at each copy.
// Share that load on the slow edge once it pays for a separate function.
extern "C" void hirsel_llvm_share_cold_sites(void *Raw) {
  auto &M = *static_cast<Module *>(Raw);
  if (Triple(M.getTargetTriple()).getArch() != Triple::x86_64) return;
  scalarRaises(M);
  auto *Step = M.getFunction("hirsel_core_step_out");
  if (!Step) return;
  shareStepWindows(M, *Step);
  MapVector<Constant *, SmallVector<CallInst *, 4>> Sites;
  for (auto &F : M)
    for (auto &B : F)
      for (auto &I : B) {
        auto *Call = dyn_cast<CallInst>(&I);
        if (!Call || Call->getCalledFunction() != Step) continue;
        auto *Site = Call->getArgOperand(1);
        if (auto *Word = dyn_cast<ConstantInt>(Site)) {
          if (Word->getValue().isSignedIntN(32)) continue;
        } else if (!isa<ConstantExpr>(Site)) {
          continue;
        }
        Sites[cast<Constant>(Site)].push_back(Call);
      }
  auto *Type = FunctionType::get(Step->getReturnType(), {Step->getArg(0)->getType()}, false);
  auto &Context = M.getContext();
  for (auto &[Site, Calls] : Sites) {
    // Four saved loads pay for the thunk, its alignment and bookkeeping.
    if (Calls.size() < 4) continue;
    auto *Shared = Function::Create(Type, GlobalValue::InternalLinkage, "hirsel.step.out", M);
    Shared->addFnAttr(Attribute::Cold);
    IRBuilder<> Body(BasicBlock::Create(Context, "", Shared));
    auto *Slow = Body.CreateCall(Step, {Shared->getArg(0), Site});
    Slow->setTailCallKind(CallInst::TCK_Tail);
    Body.CreateRet(Slow);
    for (auto *Call : Calls) {
      IRBuilder<> Builder(Call);
      auto *Replacement = Builder.CreateCall(Shared, {Call->getArgOperand(0)});
      auto Attrs = Call->getAttributes();
      Replacement->setAttributes(AttributeList::get(
          Context, Attrs.getFnAttrs(), Attrs.getRetAttrs(), {Attrs.getParamAttrs(0)}));
      Replacement->copyMetadata(*Call);
      Call->replaceAllUsesWith(Replacement);
      Call->eraseFromParent();
    }
  }
}

// A spilled Step window is flushed and reloaded around every refill. Share
// that sequence only where final block frequencies put it off the hot path.
// Leave functions with loops alone: even replacing a loop-external refill
// changes Base64's register allocation in its hot loop.
// Outline the same context-relative counter address and pass the original
// site through, so each failure still reports the Step that exhausted it.
struct StepSite {
  StoreInst *Store;
  CallInst *Call;
  LoadInst *Load;
};

static void shareColdStepSites(Module &M, FunctionAnalysisManager &FAM) {
  auto *Step = M.getFunction("hirsel_core_step_out");
  if (!Step || !Step->isDeclaration()) return;

  MapVector<uint64_t, SmallVector<StepSite, 8>> Groups;
  for (Use &U : Step->uses()) {
    auto *Call = dyn_cast<CallInst>(U.getUser());
    if (!Call || !Call->isCallee(&U) ||
        Call->getFunctionType() != Step->getFunctionType() ||
        Call->getCallingConv() != CallingConv::C || Call->hasOperandBundles() ||
        Call->isMustTailCall() || Call->isConvergent())
      continue;
    auto *Store = dyn_cast_or_null<StoreInst>(Call->getPrevNode());
    auto *Load = dyn_cast_or_null<LoadInst>(Call->getNextNode());
    if (!Store || !Load || !Store->isSimple() || !Load->isSimple() ||
        !Load->getType()->isIntegerTy(64) ||
        !Store->getValueOperand()->getType()->isIntegerTy(64) ||
        Load->getPointerOperand() != Store->getPointerOperand() ||
        Store->getAlign() < Align(8) || Load->getAlign() < Align(8))
      continue;
    auto *Address = dyn_cast<GetElementPtrInst>(Store->getPointerOperand());
    if (!Address || Address->getPointerOperand() != Call->getArgOperand(0) ||
        Address->getNumIndices() != 1 || !Address->getSourceElementType()->isIntegerTy(8))
      continue;
    auto &F = *Call->getFunction();
    auto *B = Call->getParent();
    if (!FAM.getResult<LoopAnalysis>(F).empty()) continue;
    const auto &Freq = FAM.getResult<BlockFrequencyAnalysis>(F);
    if (Freq.getBlockFreq(B).getFrequency() > Freq.getEntryFreq().getFrequency() / 100)
      continue;
    if (auto *Offset = dyn_cast<ConstantInt>(Address->getOperand(1));
        Offset && Offset->getBitWidth() == 64)
      Groups[Offset->getZExtValue()].push_back({Store, Call, Load});
  }

  for (auto &[Offset, Sites] : Groups) {
    if (Sites.size() < 4) continue;
    auto &C = M.getContext();
    auto *Word = Type::getInt64Ty(C);
    auto *Status = Step->getReturnType();
    auto *Result = StructType::get(Word, Status);
    auto *Type = FunctionType::get(Result, {Step->getArg(0)->getType(), Word, Word}, false);
    auto *Shared = Function::Create(Type, GlobalValue::InternalLinkage, "hirsel.step.sync", M);
    Shared->addFnAttr(Attribute::Cold);
    IRBuilder<> Builder(BasicBlock::Create(C, "", Shared));
    auto *Address = Builder.CreateConstGEP1_64(Type::getInt8Ty(C), Shared->getArg(0), Offset);
    Builder.CreateAlignedStore(Shared->getArg(2), Address, Align(8));
    auto *Outcome = Builder.CreateCall(Step, {Shared->getArg(0), Shared->getArg(1)});
    auto *Remaining = Builder.CreateAlignedLoad(Word, Address, Align(8));
    auto *Pair = Builder.CreateInsertValue(PoisonValue::get(Result), Remaining, 0);
    Builder.CreateRet(Builder.CreateInsertValue(Pair, Outcome, 1));

    for (auto &[Store, Call, Load] : Sites) {
      Builder.SetInsertPoint(Call);
      Builder.SetCurrentDebugLocation(Call->getDebugLoc());
      auto *New = Builder.CreateCall(Shared, {
          Call->getArgOperand(0), Call->getArgOperand(1), Store->getValueOperand()});
      Call->replaceAllUsesWith(Builder.CreateExtractValue(New, 1));
      Load->replaceAllUsesWith(Builder.CreateExtractValue(New, 0));
      Store->eraseFromParent();
      Call->eraseFromParent();
      Load->eraseFromParent();
    }
  }
}

extern "C" void hirsel_llvm_share_cold_step_sync(void *Raw, void *Machine) {
  auto &M = *static_cast<Module *>(Raw);
  if (Triple(M.getTargetTriple()).getArch() != Triple::x86_64) return;
  runLoopPass(static_cast<TargetMachine *>(Machine), M, "cold-step-sync",
              [&](PassBuilder &, FunctionAnalysisManager &FAM) {
                shareColdStepSites(M, FAM);
              });
}

// Every exact Step decrements a runtime window far from the signed limit.
// Tell x86 instruction selection about that range after IR optimization so it
// can fold the comparison into the decrement without changing the CFG.
static bool exactStepRefill(BasicBlock &Slow, BinaryOperator &After,
                            Function &Refill, const DataLayout &DL) {
  auto *Body = Slow.getParent();
  if (Body->arg_empty() || !Body->getArg(0)->getType()->isPointerTy()) return false;
  auto *Context = Body->getArg(0);
  bool Stored = false;
  for (Instruction &I : Slow) {
    if (auto *Call = dyn_cast<CallInst>(&I); Call && !isa<IntrinsicInst>(Call))
      return Stored && Call->getCalledFunction() == &Refill && Call->getArgOperand(0) == Context;
    if (auto *Store = dyn_cast<StoreInst>(&I); Store && stepWord(*Store, Context, DL))
      Stored = Store->getValueOperand() == &After;
  }
  return false;
}

extern "C" void hirsel_llvm_reuse_step_flags(void *Raw) {
  auto &M = *static_cast<Module *>(Raw);
  if (Triple(M.getTargetTriple()).getArch() != Triple::x86_64) return;
  auto *Refill = M.getFunction("hirsel_core_step_out");
  if (!Refill) return;
  auto *Word = Type::getInt64Ty(M.getContext());
  for (Function &F : M) for (BasicBlock &BB : F) {
    auto *Branch = dyn_cast<CondBrInst>(BB.getTerminator());
    auto *Cmp = Branch ? dyn_cast<ICmpInst>(Branch->getCondition()) : nullptr;
    if (!Cmp || !Cmp->hasOneUse()) continue;
    auto *After = dyn_cast<BinaryOperator>(Cmp->getOperand(0));
    auto *Limit = dyn_cast<ConstantInt>(Cmp->getOperand(1));
    if (!After || After->getParent() != &BB || After->getType() != Word || !Limit) continue;
    unsigned SlowEdge;
    if (Cmp->getPredicate() == ICmpInst::ICMP_SGT && Limit->isZero()) SlowEdge = 1;
    else if (Cmp->getPredicate() == ICmpInst::ICMP_SLT && Limit->isOne()) SlowEdge = 0;
    else continue;
    auto *Delta = dyn_cast<ConstantInt>(After->getOperand(1));
    if (!Delta || !((After->getOpcode() == Instruction::Add && Delta->isMinusOne()) ||
                    (After->getOpcode() == Instruction::Sub && Delta->isOne()))) continue;
    auto *Slow = Branch->getSuccessor(SlowEdge);
    auto *Ready = Branch->getSuccessor(1 - SlowEdge);
    if (Slow == Ready || Slow->getSinglePredecessor() != &BB ||
        !exactStepRefill(*Slow, *After, *Refill, M.getDataLayout())) continue;
    After->setHasNoSignedWrap();
  }
}
