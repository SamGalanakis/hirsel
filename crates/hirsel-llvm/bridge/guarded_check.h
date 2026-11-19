// If-converts a checked step that runs under a guard inside a loop.
#pragma once
#include <llvm/ADT/STLExtras.h>
#include <llvm/ADT/SmallPtrSet.h>
#include <llvm/ADT/SmallVector.h>
#include <llvm/Analysis/LoopInfo.h>
#include <llvm/Analysis/ValueTracking.h>
#include <llvm/IR/Dominators.h>
#include <llvm/IR/IRBuilder.h>
#include <llvm/IR/Instructions.h>
#include <llvm/IR/Module.h>
namespace hirsel_llvm {
using namespace llvm;
// `if primes[k] == 1 { count += 1 }` lowers to a guard branch into a block that
// adds and branches to the overflow raise, then a join that picks the old or
// the new count. On a data-dependent guard every iteration pays a mispredicted
// branch (langarena Sieve's count, 1.4x the cycles of the same loop counting
// with wrapping_add). The guarded block's arithmetic now runs unconditionally,
// the join takes it through a select on the guard, and one branch raises when
// both the guard and the block's own failure condition hold. The raise block,
// its cause record and therefore its failure site are unchanged; the guard
// branch is gone. Fires only on a triangle inside a loop: the guarded block has
// the guard as its single predecessor, at most four instructions that touch no
// memory and cannot trap, and ends in a branch to the join or to a block that
// raises. A guard the emitter already weighted is left alone. Runs after the
// last IR cleanup and before failure edges are weighted, so the new raise
// branch gets the 2000:1 weight like every other.
inline bool reachesRaise(BasicBlock *block) {
  for (int hop = 0; block && hop < 4; hop++) {
    for (Instruction &i : *block)
      if (auto *call = dyn_cast<CallInst>(&i))
        if (Function *callee = call->getCalledFunction())
          if (callee->getName().starts_with("hirsel_core_raise")) return true;
    auto *branch = dyn_cast<BranchInst>(block->getTerminator());
    if (!branch || branch->isConditional()) return false;
    block = branch->getSuccessor(0);
  }
  return false;
}

struct GuardedCheck {
  BranchInst *guard;     // in A: br c, B, J (or br c, J, B)
  BasicBlock *body;      // B
  BasicBlock *join;      // J
  BasicBlock *raise;     // T
};

inline bool matchGuardedCheck(BasicBlock &a, GuardedCheck &out) {
  auto *guard = dyn_cast<BranchInst>(a.getTerminator());
  if (!guard || !guard->isConditional() || guard->hasMetadata(LLVMContext::MD_prof))
    return false;
  for (unsigned side = 0; side < 2; side++) {
    BasicBlock *body = guard->getSuccessor(side), *join = guard->getSuccessor(1 - side);
    if (body == join || body == &a || body->getSinglePredecessor() != &a) continue;
    auto *check = dyn_cast<BranchInst>(body->getTerminator());
    if (!check || !check->isConditional()) continue;
    BasicBlock *raise = nullptr;
    if (check->getSuccessor(0) == join) raise = check->getSuccessor(1);
    else if (check->getSuccessor(1) == join) raise = check->getSuccessor(0);
    if (!raise || raise == join || raise == body || !reachesRaise(raise) ||
        reachesRaise(join))
      continue;
    unsigned count = 0;
    bool simple = true;
    for (Instruction &i : *body) {
      if (&i == check) break;
      if (++count > 4 || isa<PHINode>(i) || i.mayReadOrWriteMemory() ||
          !isSafeToSpeculativelyExecute(&i)) {
        simple = false;
        break;
      }
    }
    if (!simple) continue;
    out = {guard, body, join, raise};
    return true;
  }
  return false;
}

inline void ifConvertGuardedCheck(const GuardedCheck &m) {
  BasicBlock *a = m.guard->getParent();
  auto *check = cast<BranchInst>(m.body->getTerminator());
  IRBuilder<> builder(m.guard);
  Value *c = m.guard->getCondition();
  bool enter_on_true = m.guard->getSuccessor(0) == m.body;
  // The body's values now run whatever the guard says; a poison result is only
  // ever chosen by a select or a logical and whose guard rejects it.
  while (&m.body->front() != check) m.body->front().moveBefore(m.guard->getIterator());
  Value *fails = check->getCondition();
  if (check->getSuccessor(0) != m.raise) fails = builder.CreateNot(fails);
  Value *raise_now = enter_on_true ? builder.CreateLogicalAnd(c, fails)
                                   : builder.CreateLogicalAnd(builder.CreateNot(c), fails);
  for (PHINode &phi : m.join->phis()) {
    Value *skipped = phi.getIncomingValueForBlock(a);
    Value *taken = phi.getIncomingValueForBlock(m.body);
    Value *merged = skipped == taken ? taken
        : enter_on_true ? builder.CreateSelect(c, taken, skipped)
                        : builder.CreateSelect(c, skipped, taken);
    phi.removeIncomingValue(m.body, /*DeletePHIIfEmpty=*/false);
    phi.setIncomingValue(phi.getBasicBlockIndex(a), merged);
  }
  for (PHINode &phi : m.raise->phis())
    phi.setIncomingBlock(phi.getBasicBlockIndex(m.body), a);
  BranchInst *branch = builder.CreateCondBr(raise_now, m.raise, m.join);
  branch->setDebugLoc(check->getDebugLoc());
  m.guard->eraseFromParent();
  check->eraseFromParent();
  m.body->eraseFromParent();
}

inline void ifConvertGuardedChecks(Function &f) {
  SmallVector<BasicBlock *, 4> candidates;
  for (BasicBlock &a : f) {
    GuardedCheck m;
    if (matchGuardedCheck(a, m)) candidates.push_back(&a);
  }
  if (candidates.empty()) return;
  {
    DominatorTree tree(f);
    LoopInfo loops(tree);
    llvm::erase_if(candidates, [&](BasicBlock *a) { return !loops.getLoopFor(a); });
  }
  // Match again: an earlier conversion may have removed a candidate or
  // changed its triangle.
  SmallPtrSet<BasicBlock *, 4> removed;
  for (BasicBlock *a : candidates) {
    GuardedCheck m;
    if (removed.contains(a) || !matchGuardedCheck(*a, m)) continue;
    removed.insert(m.body);
    ifConvertGuardedCheck(m);
  }
}
} // namespace hirsel_llvm
