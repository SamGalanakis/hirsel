// A finite, isolated call tree can spend its exact Step cost up front. Evaluate
// the existing metered IR over constants and private stack objects, recording
// for every call it runs the peak reservation and consumed prefix relative to
// the window at that call. The root and each proved call inside the tree's
// checked copy then take a prepaid copy when the window strictly exceeds the
// peak, and otherwise run checked. An unsupported value, effect or path leaves
// the checked call alone.
#ifndef HIRSEL_LLVM_STEP_TREE_H
#define HIRSEL_LLVM_STEP_TREE_H

#include <llvm/ADT/MapVector.h>
#include <llvm/Analysis/CFG.h>
#include <llvm/Analysis/LoopInfo.h>
#include <llvm/Analysis/ConstantFolding.h>
#include <llvm/Analysis/CycleAnalysis.h>
#include <llvm/Analysis/ValueTracking.h>
#include <llvm/IR/Dominators.h>
#include <llvm/IR/IRBuilder.h>
#include <llvm/IR/IntrinsicInst.h>
#include <llvm/IR/Module.h>
#include <llvm/Transforms/Utils/BasicBlockUtils.h>
#include <llvm/Transforms/Utils/Cloning.h>
#include <llvm/Transforms/Utils/Local.h>
#include <map>
#include <memory>
#include <optional>
#include <vector>

namespace hirsel_llvm {
namespace step_tree {
using namespace llvm;

struct Byte {
  std::optional<uint8_t> Bits;
  bool Meter = false;
};

struct Object {
  explicit Object(size_t Size) : Bytes(Size) {}
  std::vector<Byte> Bytes;
};

struct Address {
  Object *Base = nullptr;
  uint64_t Offset = 0;

  bool contains(uint64_t Size) const {
    return Base && Offset <= Base->Bytes.size() && Size <= Base->Bytes.size() - Offset;
  }
};

struct Word {
  Constant *Bits = nullptr;
  Address Ptr;
  bool Meter = false;
};

using Values = DenseMap<const Value *, Word>;

static Word value(const Value *V, const Values &Values) {
  if (auto *C = dyn_cast<Constant>(V)) return {const_cast<Constant *>(C), {}, false};
  return Values.lookup(V);
}

static ConstantInt *integer(const Value *V, const Values &Values) {
  return dyn_cast_or_null<ConstantInt>(value(V, Values).Bits);
}

static std::optional<uint64_t> sizeOf(Type *Ty, const DataLayout &DL) {
  if (!Ty->isSized()) return {};
  auto Size = DL.getTypeStoreSize(Ty);
  if (Size.isScalable()) return {};
  return Size.getFixedValue();
}

static std::optional<Word> read(Address Addr, Type *Ty, const DataLayout &DL) {
  auto Count = sizeOf(Ty, DL);
  if (!Ty->isIntegerTy() || Ty->getIntegerBitWidth() % 8 != 0 ||
      !Count || !Addr.contains(*Count)) return {};
  APInt Bits(DL.getTypeSizeInBits(Ty).getFixedValue(), 0);
  bool Known = true, Meter = false;
  for (uint64_t I = 0; I < *Count; ++I) {
    auto &Cell = Addr.Base->Bytes[Addr.Offset + I];
    Meter |= Cell.Meter;
    Known &= Cell.Bits.has_value();
    if (Cell.Bits) Bits |= APInt(Bits.getBitWidth(), *Cell.Bits) << (8 * I);
  }
  return Word{Known ? ConstantInt::get(Ty, Bits) : nullptr, {}, Meter};
}

static bool write(Address Addr, Type *Ty, Word V, const DataLayout &DL) {
  auto Count = sizeOf(Ty, DL);
  if (!Count || !Addr.contains(*Count) || V.Ptr.Base ||
      (Ty->isIntegerTy() && Ty->getIntegerBitWidth() % 8 != 0)) return false;
  auto *Int = dyn_cast_or_null<ConstantInt>(V.Bits);
  for (uint64_t I = 0; I < *Count; ++I) {
    auto &Cell = Addr.Base->Bytes[Addr.Offset + I];
    Cell = {std::nullopt, V.Meter};
    if (Int) {
      const APInt &Bits = Int->getValue();
      Cell.Bits = static_cast<uint8_t>(Bits.extractBitsAsZExtValue(
          std::min<unsigned>(8, Bits.getBitWidth() - 8 * I), 8 * I));
    }
  }
  return true;
}

static bool overflowIntrinsic(Intrinsic::ID Id) {
  switch (Id) {
  case Intrinsic::sadd_with_overflow: case Intrinsic::uadd_with_overflow:
  case Intrinsic::ssub_with_overflow: case Intrinsic::usub_with_overflow:
  case Intrinsic::smul_with_overflow: case Intrinsic::umul_with_overflow:
    return true;
  default:
    return false;
  }
}

static bool inertIntrinsic(Intrinsic::ID Id) {
  return Id == Intrinsic::lifetime_start || Id == Intrinsic::lifetime_end ||
         Id == Intrinsic::experimental_noalias_scope_decl;
}

static MDNode *stepAccess(const Instruction &I) {
  auto *Tag = I.getMetadata(LLVMContext::MD_tbaa);
  auto *Type = Tag && Tag->getNumOperands() == 3
      ? dyn_cast<MDNode>(Tag->getOperand(1)) : nullptr;
  auto *Name = Type && Type->getNumOperands()
      ? dyn_cast<MDString>(Type->getOperand(0)) : nullptr;
  return Name && Name->getString() == "steps" ? Tag : nullptr;
}

struct Directions {
  unsigned Seen = 0;
  bool Meter = false;
};

struct Cost {
  uint64_t Maximum;
  uint64_t Consumed;
  bool operator==(const Cost &Other) const {
    return Maximum == Other.Maximum && Consumed == Other.Consumed;
  }
  bool operator!=(const Cost &Other) const { return !(*this == Other); }
};

// One proved execution of a call: its integer arguments, and its cost relative
// to the window at the call when that cost is exact and worth a guard.
struct Instance {
  SmallVector<std::optional<uint64_t>, 4> Args;
  std::optional<Cost> Charge;
};

// A call site's admission rule. Every run of the site has the same cost, or
// one integer argument determines it. Values with no proved cost run checked.
struct Summary {
  std::optional<Cost> Fixed;
  unsigned Key = 0;
  SmallVector<std::pair<uint64_t, Cost>, 8> Cases;
};

static Summary summarize(ArrayRef<Instance> Runs) {
  Summary S;
  if (Runs.empty()) return S;
  if (llvm::all_of(Runs, [&](const Instance &R) {
        return R.Charge && *R.Charge == *Runs.front().Charge;
      })) {
    S.Fixed = Runs.front().Charge;
    return S;
  }
  for (unsigned Key = 0; Key < Runs.front().Args.size(); ++Key) {
    std::map<uint64_t, std::optional<Cost>> Table;
    bool Determines = true;
    for (const Instance &R : Runs) {
      if (!R.Args[Key]) { Determines = false; break; }
      auto [It, New] = Table.try_emplace(*R.Args[Key], R.Charge);
      if (!New && It->second != R.Charge) { Determines = false; break; }
    }
    if (!Determines) continue;
    S.Key = Key + 1;
    for (auto &[Value, Charge] : Table)
      if (Charge) S.Cases.emplace_back(Value, *Charge);
    return S;
  }
  return S;
}

struct Tree {
  static constexpr uint64_t Slice = 65536;
  static constexpr uint64_t WindowOffset = 16;
  const DataLayout &DL;
  LLVMContext &Context;
  Object Meter{WindowOffset + 8};
  SmallPtrSet<Object *, 4> Inputs;
  SmallPtrSet<Function *, 8> Functions;
  SmallPtrSet<CallInst *, 16> Calls;
  SmallPtrSet<Instruction *, 32> WindowAccesses;
  SmallPtrSet<Instruction *, 32> ProgramAccesses;
  DenseMap<CondBrInst *, Directions> Branches;
  MapVector<CallInst *, std::vector<Instance>> Runs;
  unsigned Work = 2000000;
  uint64_t Maximum = 0;

  Tree(const DataLayout &DL, LLVMContext &Context) : DL(DL), Context(Context) {
    auto *Ty = Type::getInt64Ty(Context);
    write({&Meter, WindowOffset}, Ty, {ConstantInt::get(Ty, Slice), {}, true}, DL);
  }

  bool isWindow(Address Ptr, Type *Ty) const {
    return Ptr.Offset == WindowOffset && Ty->isIntegerTy(64);
  }

  std::optional<Word> run(Function &F, ArrayRef<Word> Args, unsigned Depth) {
    if (F.isDeclaration() || !F.hasLocalLinkage() || F.isVarArg() ||
        Depth == 64 || Args.size() != F.arg_size()) return {};
    Functions.insert(&F);
    Values V;
    for (auto &A : F.args()) V[&A] = Args[A.getArgNo()];
    std::vector<std::unique_ptr<Object>> Locals;
    BasicBlock *Prev = nullptr, *Block = &F.getEntryBlock();
    while (Block) {
      if (Work == 0) return {};
      SmallVector<std::pair<PHINode *, Word>, 4> Pending;
      for (PHINode &Phi : Block->phis()) {
        if (!Prev) return {};
        Pending.emplace_back(&Phi, value(Phi.getIncomingValueForBlock(Prev), V));
      }
      for (auto &[Phi, In] : Pending) V[Phi] = In;
      for (Instruction &I : *Block) {
        if (Work == 0) return {};
        --Work;
        if (isa<PHINode>(I) || I.isTerminator()) continue;
        auto Result = instruction(I, V, Locals, Depth);
        if (!Result) return {};
        V[&I] = *Result;
      }
      auto *End = Block->getTerminator();
      if (auto *R = dyn_cast<ReturnInst>(End)) {
        Word Result = R->getReturnValue() ? value(R->getReturnValue(), V) : Word{};
        if (Result.Meter || Result.Ptr.Base) return {};
        return Result;
      }
      Prev = Block;
      if (auto *Jump = dyn_cast<UncondBrInst>(End)) {
        Block = Jump->getSuccessor();
      } else if (auto *Branch = dyn_cast<CondBrInst>(End)) {
        auto *Cond = integer(Branch->getCondition(), V);
        if (!Cond) return {};
        unsigned Choice = Cond->isZero() ? 1 : 0;
        auto &Direction = Branches[Branch];
        Direction.Seen |= 1 << Choice;
        Direction.Meter |= value(Branch->getCondition(), V).Meter;
        Block = Branch->getSuccessor(Choice);
      } else {
        return {};
      }
    }
    return {};
  }

  std::optional<Word> instruction(Instruction &I, Values &V,
      std::vector<std::unique_ptr<Object>> &Locals, unsigned Depth) {
    if (auto *A = dyn_cast<AllocaInst>(&I)) {
      auto Size = A->getAllocationSize(DL);
      if (!A->isStaticAlloca() || !Size || Size->isScalable() ||
          Size->getFixedValue() > 4096) return {};
      Locals.push_back(std::make_unique<Object>(Size->getFixedValue()));
      return Word{nullptr, {Locals.back().get(), 0}, false};
    }
    if (auto *G = dyn_cast<GetElementPtrInst>(&I)) {
      auto Base = value(G->getPointerOperand(), V);
      APInt Offset(DL.getIndexTypeSizeInBits(G->getType()), 0);
      bool Fixed = cast<GEPOperator>(G)->accumulateConstantOffset(DL, Offset,
          [&V](Value &Index, APInt &Result) {
            auto Word = value(&Index, V);
            auto *C = dyn_cast_or_null<ConstantInt>(Word.Bits);
            if (!C || Word.Meter) return false;
            Result = C->getValue();
            return true;
          });
      if (!Fixed || !Base.Ptr.Base || Base.Meter) return {};
      auto &P = Base.Ptr;
      P.Offset += Offset.getZExtValue();
      if (!P.contains(0)) return {};
      return Base;
    }
    if (auto *L = dyn_cast<LoadInst>(&I)) {
      auto Ptr = value(L->getPointerOperand(), V).Ptr;
      if (!L->isSimple()) return {};
      if (Ptr.Base == &Meter) {
        if (!isWindow(Ptr, L->getType()) || !stepAccess(*L) || ProgramAccesses.contains(L))
          return {};
        WindowAccesses.insert(L);
      } else {
        if (WindowAccesses.contains(L)) return {};
        ProgramAccesses.insert(L);
      }
      return read(Ptr, L->getType(), DL);
    }
    if (auto *S = dyn_cast<StoreInst>(&I)) {
      auto Ptr = value(S->getPointerOperand(), V).Ptr;
      auto Stored = value(S->getValueOperand(), V);
      auto *Ty = S->getValueOperand()->getType();
      if (!S->isSimple() || (Stored.Meter && Ptr.Base != &Meter)) return {};
      if (Ptr.Base == &Meter) {
        if (!isWindow(Ptr, Ty) || !stepAccess(*S) || !Stored.Meter || ProgramAccesses.contains(S))
          return {};
        WindowAccesses.insert(S);
      } else {
        if (WindowAccesses.contains(S)) return {};
        ProgramAccesses.insert(S);
      }
      if (!write(Ptr, Ty, Stored, DL)) return {};
      return Word{};
    }
    if (auto *C = dyn_cast<CallInst>(&I)) {
      auto *F = C->getCalledFunction();
      if (!F || C->hasOperandBundles() || C->isMustTailCall()) return {};
      if (isa<DbgInfoIntrinsic>(C) || F->getIntrinsicID() == Intrinsic::experimental_noalias_scope_decl)
        return Word{};
      if (F->getIntrinsicID() == Intrinsic::lifetime_start ||
          F->getIntrinsicID() == Intrinsic::lifetime_end) {
        auto Ptr = value(C->getArgOperand(0), V).Ptr;
        if (!Ptr.Base || Ptr.Offset || Ptr.Base == &Meter || Inputs.contains(Ptr.Base)) return {};
        std::fill(Ptr.Base->Bytes.begin(), Ptr.Base->Bytes.end(), Byte{});
        return Word{};
      }
      if (auto *Set = dyn_cast<MemSetInst>(C)) {
        auto Ptr = value(Set->getDest(), V).Ptr;
        auto *Count = integer(Set->getLength(), V);
        auto *Fill = integer(Set->getValue(), V);
        if (Set->isVolatile() || Ptr.Base == &Meter || !Count || !Fill ||
            !Ptr.contains(Count->getZExtValue())) return {};
        if (value(Set->getLength(), V).Meter || value(Set->getValue(), V).Meter) return {};
        std::fill_n(Ptr.Base->Bytes.begin() + Ptr.Offset, Count->getZExtValue(),
                    Byte{static_cast<uint8_t>(Fill->getZExtValue()), false});
        return Word{};
      }
      if (!F->isIntrinsic()) {
        SmallVector<Word, 4> Args;
        for (auto &Use : C->args()) {
          Word W = value(Use.get(), V);
          if (W.Meter) return {};
          Args.push_back(W);
        }
        if (Args.empty() || Args.front().Ptr.Base != &Meter || Args.front().Ptr.Offset != 0)
          return {};
        Instance Run;
        for (auto &Arg : ArrayRef(Args).drop_front()) {
          if (Arg.Ptr.Base == &Meter) return {};
          auto *Int = dyn_cast_or_null<ConstantInt>(Arg.Bits);
          Run.Args.push_back(Int && Int->getBitWidth() <= 64
              ? std::optional<uint64_t>(Int->getZExtValue()) : std::nullopt);
        }
        Calls.insert(C);
        auto Entry = window();
        auto Outer = std::exchange(Maximum, 0);
        auto Result = run(*F, Args, Depth + 1);
        auto Inner = std::exchange(Maximum, std::max(Outer, Maximum));
        if (!Result) return {};
        if (Entry) Run.Charge = charge(*Entry, window(), Inner);
        Runs[C].push_back(std::move(Run));
        return Result;
      }
      if (!overflowIntrinsic(F->getIntrinsicID())) return {};
    } else if (!isa<BinaryOperator, ICmpInst, CastInst, ExtractValueInst, InsertValueInst,
                    SelectInst>(I)) {
      return {};
    }
    SmallVector<Constant *, 4> Args;
    bool Tainted = false;
    for (auto &Use : I.operands()) {
      if (auto *C = dyn_cast<CallInst>(&I); C && C->isCallee(&Use)) continue;
      Word W = value(Use.get(), V);
      if (W.Ptr.Base) return {};
      Tainted |= W.Meter;
      Args.push_back(W.Bits);
    }
    if (llvm::any_of(Args, [](Constant *C) { return !C; }))
      return Tainted ? std::nullopt : std::optional<Word>(Word{});
    auto *Folded = ConstantFoldInstOperands(&I, Args, DL, nullptr, false);
    if (Tainted && !proveWindowUse(I, V, Folded)) return {};
    return Word{Folded, {}, Tainted};
  }

  // Every accepted Step word is the initial window minus a known prefix.
  // A true credit comparison contributes its absolute threshold. Include
  // intermediate debits too, so refunds cannot hide the peak charge.
  bool proveWindowUse(Instruction &I, const Values &V, Constant *Folded) {
    if (I.getNumOperands() != 2 || !value(I.getOperand(0), V).Meter ||
        value(I.getOperand(1), V).Meter) return false;
    auto *Left = integer(I.getOperand(0), V);
    auto *Right = integer(I.getOperand(1), V);
    auto *Result = dyn_cast_or_null<ConstantInt>(Folded);
    if (!Left || Left->getBitWidth() != 64 || !Right ||
        Left->isZero() || Left->uge(Slice + 1) || !Result) return false;
    if (auto *Cmp = dyn_cast<ICmpInst>(&I)) {
      if (Cmp->getPredicate() != ICmpInst::ICMP_SGT || Right->isNegative() ||
          Result->isZero()) return false;
      Maximum = std::max(Maximum, Slice - Left->getZExtValue() + Right->getZExtValue());
      return true;
    }
    auto *Op = dyn_cast<BinaryOperator>(&I);
    if (!Op || (Op->getOpcode() != Instruction::Add && Op->getOpcode() != Instruction::Sub) ||
        (Op->hasNoUnsignedWrap() && Right->isNegative()) ||
        Result->isZero() || Result->uge(Slice + 1)) return false;
    Maximum = std::max(Maximum, Slice - Result->getZExtValue());
    return true;
  }

  std::optional<uint64_t> window() const {
    auto Left = read({const_cast<Object *>(&Meter), WindowOffset}, Type::getInt64Ty(Context), DL);
    auto *Count = Left ? dyn_cast_or_null<ConstantInt>(Left->Bits) : nullptr;
    if (!Count || Count->isZero() || Count->getValue().ugt(Slice)) return {};
    return Count->getZExtValue();
  }

  // Maximum is absolute against a Slice-sized starting window; an invocation
  // entered at Entry needs only the part above what was consumed before it.
  // A guard pays for itself only over a few Steps.
  static std::optional<Cost> charge(uint64_t Entry, std::optional<uint64_t> Exit,
                                    uint64_t Inner) {
    if (!Exit || *Exit > Entry || Entry - *Exit < 16) return {};
    auto Consumed = Entry - *Exit;
    auto Before = Slice - Entry;
    return Cost{std::max(Inner > Before ? Inner - Before : 0, Consumed), Consumed};
  }

  // A metered branch that went both ways means some run refilled or stopped.
  bool exact() const {
    for (auto &[Branch, Direction] : Branches)
      if (Direction.Meter && Direction.Seen == 3) return false;
    return true;
  }
};

// Recover only initialization of caller-local storage whose address has never
// escaped before this call. Every reaching write must dominate the call.
struct Write {
  Instruction *Inst;
  uint64_t Offset;
  uint64_t Size;
  bool Fixed;
};

static bool initialize(AllocaInst &Root, Instruction &At, DominatorTree &DT,
                       LoopInfo &LI, CycleInfo &CI, Object &Out, const DataLayout &DL,
                       unsigned Copies = 0) {
  if (Copies == 8) return false;
  SmallVector<Value *, 8> Pending{&Root};
  SmallPtrSet<Value *, 8> Seen;
  SmallVector<Write, 8> Writes;
  while (!Pending.empty()) {
    Value *P = Pending.pop_back_val();
    if (!Seen.insert(P).second) continue;
    for (Use &U : P->uses()) {
      auto *I = dyn_cast<Instruction>(U.getUser());
      if (!I) return false;
      if (isa<GetElementPtrInst>(I)) {
        Pending.push_back(I);
        continue;
      }
      if (I == &At || !isPotentiallyReachable(I, &At, nullptr, &DT)) continue;
      if (auto *L = dyn_cast<LoadInst>(I); L && L->isSimple()) continue;
      if (auto *C = dyn_cast<IntrinsicInst>(I); C && inertIntrinsic(C->getIntrinsicID())) {
        if (C->getIntrinsicID() != Intrinsic::experimental_noalias_scope_decl)
          Writes.push_back({C, 0, Out.Bytes.size(), false});
        continue;
      }
      std::optional<uint64_t> Count;
      if (auto *S = dyn_cast<StoreInst>(I); S && S->isSimple() && S->getPointerOperand() == P)
        Count = sizeOf(S->getValueOperand()->getType(), DL);
      else if (auto *Transfer = dyn_cast<MemIntrinsic>(I); Transfer && !Transfer->isVolatile()) {
        if (Transfer->getDest() != P) {
          auto *Copy = dyn_cast<MemTransferInst>(Transfer);
          if (Copy && Copy->getSource() == P) continue;
          return false;
        }
        if (auto *Length = dyn_cast<ConstantInt>(Transfer->getLength()))
          Count = Length->getLimitedValue();
      } else {
        return false;
      }
      if (auto *S = dyn_cast<StoreInst>(I); S && S->getValueOperand() == P) return false;
      int64_t Offset = 0;
      bool Fixed = GetPointerBaseWithConstantOffset(P, Offset, DL) == &Root &&
                   Offset >= 0 && Count && Address{&Out, uint64_t(Offset)}.contains(*Count);
      Writes.push_back({I, Fixed ? uint64_t(Offset) : 0, Fixed ? *Count : Out.Bytes.size(), Fixed});
    }
  }
  // A loop may initialize an unrelated part of the record. Treat an unknown
  // write as covering the whole object, then recover bytes from a later
  // dominating write with no possible intervening write to those bytes.
  for (Write &W : Writes) {
    if (!W.Fixed || !DT.dominates(W.Inst, &At) || CI.getCycle(W.Inst->getParent()))
      continue;
    Object Defined(W.Size);
    if (auto *S = dyn_cast<StoreInst>(W.Inst)) {
      auto *C = dyn_cast<Constant>(S->getValueOperand());
      if (!C || !write({&Defined, 0}, C->getType(), {C, {}, false}, DL)) continue;
    } else if (auto *Set = dyn_cast<MemSetInst>(W.Inst)) {
      auto *Fill = dyn_cast<ConstantInt>(Set->getValue());
      if (!Fill) continue;
      std::fill(Defined.Bytes.begin(), Defined.Bytes.end(), Byte{
          static_cast<uint8_t>(Fill->getZExtValue()), false});
    } else if (auto *Copy = dyn_cast<MemTransferInst>(W.Inst)) {
      int64_t Offset = 0;
      auto *Source = dyn_cast<AllocaInst>(
          GetPointerBaseWithConstantOffset(Copy->getSource(), Offset, DL));
      if (!Source || !Source->isStaticAlloca() || Offset < 0) continue;
      auto Size = Source->getAllocationSize(DL);
      if (!Size || Size->isScalable() || Size->getFixedValue() > 4096) continue;
      Object Input(Size->getFixedValue());
      Address From{&Input, uint64_t(Offset)};
      if (!From.contains(W.Size) || !initialize(*Source, *Copy, DT, LI, CI, Input, DL, Copies + 1))
        continue;
      std::copy_n(Input.Bytes.begin() + From.Offset, W.Size, Defined.Bytes.begin());
    }
    for (Write &Other : Writes) {
      if (Other.Inst == W.Inst || Other.Offset >= W.Offset + W.Size ||
          W.Offset >= Other.Offset + Other.Size ||
          !isPotentiallyReachable(W.Inst, Other.Inst, nullptr, &DT, &LI, &CI))
        continue;
      auto Start = std::max(W.Offset, Other.Offset) - W.Offset;
      auto End = std::min(W.Offset + W.Size, Other.Offset + Other.Size) - W.Offset;
      std::fill(Defined.Bytes.begin() + Start, Defined.Bytes.begin() + End, Byte{});
    }
    for (uint64_t I = 0; I < W.Size; ++I)
      if (Defined.Bytes[I].Bits) Out.Bytes[W.Offset + I] = Defined.Bytes[I];
  }
  return true;
}

// Run At through Fast when the window strictly exceeds its proved peak, and
// through Slow otherwise. Equality stays checked so the last Step can yield.
static void admit(CallInst &At, const Summary &S, Function *Fast, Function *Slow,
                  MDNode *Tag) {
  At.setTailCallKind(CallInst::TCK_None);
  if (!S.Fixed && S.Cases.empty()) {
    At.setCalledFunction(Slow);
    return;
  }
  auto &Context = At.getContext();
  auto *Word = Type::getInt64Ty(Context);
  auto *Original = At.getParent();
  auto *Join = Original->splitBasicBlock(At.getNextNode(), "step.tree.join");
  auto *Checked = Original->splitBasicBlock(At.getIterator(), "step.tree.checked");
  auto *F = At.getFunction();
  auto *Test = BasicBlock::Create(Context, "step.tree.test", F, Checked);
  auto *Prepaid = BasicBlock::Create(Context, "step.tree.prepaid", F, Checked);
  IRBuilder<> B(Test);
  B.SetCurrentDebugLocation(At.getDebugLoc());
  Value *Peak, *Consumed;
  Original->getTerminator()->eraseFromParent();
  if (S.Fixed) {
    IRBuilder<>(Original).CreateBr(Test);
    Peak = B.getInt64(S.Fixed->Maximum);
    Consumed = B.getInt64(S.Fixed->Consumed);
  } else {
    auto *PeakPhi = B.CreatePHI(Word, S.Cases.size());
    auto *ConsumedPhi = B.CreatePHI(Word, S.Cases.size());
    auto *Selector = At.getArgOperand(S.Key);
    auto *Switch = SwitchInst::Create(Selector, Checked, S.Cases.size(), Original);
    for (auto &[Value, Charge] : S.Cases) {
      auto *Case = BasicBlock::Create(Context, "step.tree.case", F, Test);
      IRBuilder<>(Case).CreateBr(Test);
      Switch->addCase(ConstantInt::get(cast<IntegerType>(Selector->getType()), Value), Case);
      PeakPhi->addIncoming(B.getInt64(Charge.Maximum), Case);
      ConsumedPhi->addIncoming(B.getInt64(Charge.Consumed), Case);
    }
    Peak = PeakPhi;
    Consumed = ConsumedPhi;
  }
  auto *Field = B.CreateConstGEP1_64(B.getInt8Ty(), At.getArgOperand(0), Tree::WindowOffset);
  auto *Before = B.CreateAlignedLoad(Word, Field, Align(8));
  Before->setMetadata(LLVMContext::MD_tbaa, Tag);
  B.CreateCondBr(B.CreateICmpSGT(Before, Peak), Prepaid, Checked);
  B.SetInsertPoint(Prepaid);
  auto *After = B.CreateSub(Before, Consumed, "", false, true);
  B.CreateAlignedStore(After, Field, Align(8))->setMetadata(LLVMContext::MD_tbaa, Tag);
  auto *New = cast<CallInst>(At.clone());
  New->setCalledFunction(Fast);
  New->addFnAttr(Attribute::get(Context, "hirsel-step-neutral"));
  B.Insert(New);
  B.CreateBr(Join);
  At.setCalledFunction(Slow);
  if (!At.getType()->isVoidTy()) {
    auto *Result = PHINode::Create(At.getType(), 2, "", Join->begin());
    At.replaceAllUsesWith(Result);
    Result->addIncoming(&At, Checked);
    Result->addIncoming(New, Prepaid);
  }
}

// Every proved function gets a prepaid copy, without window accesses or the
// branches no proved run took, and a checked copy reachable only from the
// root's checked edge. That copy repeats the proved runs exactly (the refill
// changes no program state), so each of its proved call sites can admit the
// run it is about to make by the summary of the same site.
static void replace(CallInst &At, Tree &Proof, Cost Charge) {
  struct Copy {
    Function *Prepaid;
    Function *Checked;
    std::unique_ptr<ValueToValueMapTy> PrepaidMap;
    std::unique_ptr<ValueToValueMapTy> CheckedMap;
  };
  MapVector<Function *, Copy> Copies;
  auto &M = *At.getModule();
  auto StepNeutral = Attribute::get(M.getContext(), "hirsel-step-neutral");
  auto *Tag = stepAccess(**Proof.WindowAccesses.begin());
  for (Function &F : M) {
    if (!Proof.Functions.contains(&F)) continue;
    Copy C{nullptr, nullptr, std::make_unique<ValueToValueMapTy>(),
           std::make_unique<ValueToValueMapTy>()};
    C.Prepaid = CloneFunction(&F, *C.PrepaidMap);
    C.Prepaid->setName(F.getName() + ".step_tree");
    C.Checked = CloneFunction(&F, *C.CheckedMap);
    C.Checked->setName(F.getName() + ".step_tree.checked");
    Copies.try_emplace(&F, std::move(C));
  }
  SmallVector<std::pair<CallInst *, CallInst *>, 16> Sites;
  for (auto &[F, Copy] : Copies) {
    for (BasicBlock &B : *F) {
      for (Instruction &I : B) {
        auto *Mapped = cast<Instruction>((*Copy.PrepaidMap)[&I]);
        auto *Call = dyn_cast<CallInst>(&I);
        if (Call && Proof.Calls.contains(Call)) {
          auto *Nested = cast<CallInst>(Mapped);
          Nested->setCalledFunction(Copies.find(Call->getCalledFunction())->second.Prepaid);
          Nested->addFnAttr(StepNeutral);
          Sites.emplace_back(Call, cast<CallInst>((*Copy.CheckedMap)[&I]));
        } else if (Proof.WindowAccesses.contains(&I)) {
          if (isa<LoadInst>(I)) Mapped->replaceAllUsesWith(PoisonValue::get(Mapped->getType()));
          Mapped->eraseFromParent();
        }
      }
      if (auto *Branch = dyn_cast<CondBrInst>(B.getTerminator())) {
        auto It = Proof.Branches.find(Branch);
        if (It != Proof.Branches.end() && It->second.Seen != 3) {
          auto *Mapped = cast<CondBrInst>((*Copy.PrepaidMap)[Branch]);
          Mapped->setCondition(ConstantInt::getBool(M.getContext(), It->second.Seen == 1));
        }
      }
    }
  }
  for (auto &[F, Copy] : Copies) removeUnreachableBlocks(*Copy.Prepaid);
  for (auto &[Call, Nested] : Sites) {
    auto &Callee = Copies.find(Call->getCalledFunction())->second;
    admit(*Nested, summarize(Proof.Runs.lookup(Call)), Callee.Prepaid, Callee.Checked, Tag);
  }
  auto &Root = Copies.find(At.getCalledFunction())->second;
  Summary Whole;
  Whole.Fixed = Charge;
  admit(At, Whole, Root.Prepaid, Root.Checked, Tag);
}

static void tryCall(CallInst &At) {
  auto &Caller = *At.getFunction();
  if (At.arg_empty() || Caller.arg_empty() || At.getArgOperand(0) != Caller.getArg(0) ||
      At.isMustTailCall() || At.hasOperandBundles() || At.hasFnAttr("hirsel-step-neutral")) return;
  auto &M = *At.getModule();
  const auto &DL = M.getDataLayout();
  DominatorTree DT(Caller);
  LoopInfo LI(DT);
  CycleInfo CI;
  CI.compute(Caller);
  if (CI.getCycle(At.getParent())) return;
  Tree Proof(DL, M.getContext());
  MapVector<AllocaInst *, std::unique_ptr<Object>> Objects;
  SmallVector<Word, 4> Args{{nullptr, {&Proof.Meter, 0}, false}};
  for (auto &U : llvm::drop_begin(At.args())) {
    Value *V = U.get();
    if (V->getType()->isPointerTy()) {
      int64_t Offset = 0;
      auto *Root = dyn_cast<AllocaInst>(GetPointerBaseWithConstantOffset(V, Offset, DL));
      if (!Root || !Root->isStaticAlloca() || Offset < 0) return;
      auto Size = Root->getAllocationSize(DL);
      if (!Size || Size->isScalable() || Size->getFixedValue() > 4096) return;
      auto &Slot = Objects[Root];
      if (!Slot) {
        Slot = std::make_unique<Object>(Size->getFixedValue());
        if (!initialize(*Root, At, DT, LI, CI, *Slot, DL)) return;
        Proof.Inputs.insert(Slot.get());
      }
      if (uint64_t(Offset) > Slot->Bytes.size()) return;
      Args.push_back({nullptr, {Slot.get(), static_cast<uint64_t>(Offset)}, false});
    } else {
      auto *C = dyn_cast<Constant>(V);
      // The hidden caller site can be dynamic. It can be forwarded through
      // the proof, but any use that needs its value stops interpretation.
      Args.push_back({C, {}, false});
    }
  }
  if (!Proof.run(*At.getCalledFunction(), Args, 0) || !Proof.exact()) return;
  if (auto Cost = Tree::charge(Tree::Slice, Proof.window(), Proof.Maximum))
    replace(At, Proof, *Cost);
}

} // namespace step_tree

static void prepayStepTrees(llvm::Module &M) {
  using namespace llvm;
  auto *Refill = M.getFunction("hirsel_core_step_out");
  if (!Refill || Refill->use_empty() || !M.getDataLayout().isLittleEndian()) return;
  SmallVector<CallInst *, 8> Roots;
  for (Function &F : M) {
    if (!F.hasLocalLinkage() || F.isDeclaration()) continue;
    bool Recursive = false;
    SmallVector<CallInst *, 4> Entries;
    for (Use &U : F.uses()) {
      auto *C = dyn_cast<CallInst>(U.getUser());
      if (!C || !C->isCallee(&U)) continue;
      if (C->getFunction() == &F) Recursive = true;
      else if (llvm::any_of(C->args(), [](const Use &Arg) { return isa<ConstantInt>(Arg.get()); }))
        Entries.push_back(C);
    }
    if (Recursive) Roots.append(Entries);
  }
  for (CallInst *C : Roots) step_tree::tryCall(*C);
}

} // namespace hirsel_llvm
#endif
