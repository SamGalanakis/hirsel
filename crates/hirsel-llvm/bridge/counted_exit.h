// Splits a loop latch that branches on a counter compare fused with another
// condition, so the counter keeps an exit SCEV can count.
#pragma once
#include <llvm/ADT/DepthFirstIterator.h>
#include <llvm/Analysis/LoopInfo.h>
#include <llvm/Analysis/ScalarEvolution.h>
#include <llvm/Analysis/ScalarEvolutionExpressions.h>
#include <llvm/Analysis/ValueTracking.h>
#include <llvm/IR/Instructions.h>
#include <llvm/IR/PassManager.h>
#include <llvm/IR/PatternMatch.h>
namespace hirsel_llvm {
using namespace llvm;
// SimplifyCFG folds a latch's short-circuit `i < n && x <= limit` into one
// branch on `select i1 %counted, i1 %x, false`. Codegen splits it back into
// two branches, but the loop now has one exit whose count SCEV cannot compute,
// so loop strength reduction keeps the counter and its compare instead of
// counting down to zero (langarena Mandelbrot's escape loop: one more
// instruction per iteration than the same loop written with two exits). A
// latch branch on a logical and (or) of a counter compare and one other
// condition becomes two branches again: the other condition leaves first, then
// a new latch leaves on the counter. With the counted exit last, loop rotation
// keeps the shape; with it first, rotation moves it to the latch and carries
// the other condition into the next iteration as a flag. The release pipeline
// runs it before indvars and the release tail runs it last, after the final
// simplifycfg has fused the branches again.
class CountedExitSplitPass : public OptionalPassInfoMixin<CountedExitSplitPass> {
public:
    PreservedAnalyses run(Function &f, FunctionAnalysisManager &analyses) {
        using namespace PatternMatch;
        LoopInfo &loops = analyses.getResult<LoopAnalysis>(f);
        ScalarEvolution &evolution = analyses.getResult<ScalarEvolutionAnalysis>(f);
        auto counted = [&](Value *v, Loop *loop) {
            auto *cmp = dyn_cast<ICmpInst>(v);
            if (!cmp || !loop->contains(cmp)) return false;
            for (int side = 0; side < 2; side++) {
                Value *iv = cmp->getOperand(side), *bound = cmp->getOperand(1 - side);
                auto *rec = dyn_cast<SCEVAddRecExpr>(evolution.getSCEV(iv));
                if (rec && rec->getLoop() == loop && rec->isAffine() &&
                    loop->isLoopInvariant(bound))
                    return true;
            }
            return false;
        };
        SmallVector<BranchInst *, 4> latches;
        for (Loop *top : loops)
            for (Loop *loop : depth_first(top))
                if (BasicBlock *latch = loop->getLoopLatch())
                    if (auto *branch = dyn_cast<BranchInst>(latch->getTerminator()))
                        if (branch->isConditional() && loop->getExitingBlock() == latch)
                            latches.push_back(branch);
        bool changed = false;
        for (BranchInst *branch : latches) {
            Loop *loop = loops.getLoopFor(branch->getParent());
            Value *first, *second;
            bool both;  // stay in the loop when both hold, else leave when either holds
            if (match(branch->getCondition(), m_LogicalAnd(m_Value(first), m_Value(second))))
                both = true;
            else if (match(branch->getCondition(), m_LogicalOr(m_Value(first), m_Value(second))))
                both = false;
            else
                continue;
            BasicBlock *stay = branch->getSuccessor(both ? 0 : 1);
            BasicBlock *leave = branch->getSuccessor(both ? 1 : 0);
            if (stay != loop->getHeader() || loop->contains(leave)) continue;
            if (counted(first, loop) == counted(second, loop)) continue;
            Value *counter = counted(first, loop) ? first : second;
            Value *other = counter == first ? second : first;
            auto *fused = dyn_cast<Instruction>(branch->getCondition());
            if (!fused || !fused->hasOneUse()) continue;
            // A select's second operand may be poison where the first decides alone.
            if (!isGuaranteedNotToBePoison(other))
                other = new FreezeInst(other, "", branch->getIterator());
            BasicBlock *latch = branch->getParent();
            BasicBlock *rest = BasicBlock::Create(f.getContext(), "", &f, leave);
            BranchInst::Create(both ? stay : leave, both ? leave : stay, counter, rest);
            for (PHINode &phi : leave->phis())
                phi.addIncoming(phi.getIncomingValueForBlock(latch), rest);
            for (PHINode &phi : stay->phis())
                phi.setIncomingBlock(phi.getBasicBlockIndex(latch), rest);
            branch->setCondition(other);
            branch->setSuccessor(both ? 0 : 1, rest);
            branch->setMetadata(LLVMContext::MD_prof, nullptr);
            fused->eraseFromParent();
            loop->addBasicBlockToLoop(rest, loops);
            changed = true;
        }
        if (!changed) return PreservedAnalyses::all();
        evolution.forgetAllLoops();
        return PreservedAnalyses::none();
    }
};
} // namespace hirsel_llvm
