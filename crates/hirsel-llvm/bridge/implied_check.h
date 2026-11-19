// Hoists an arm's overflow check above the branch into the arm when the check
// can only fail on inputs that take the arm.
#pragma once
#include <llvm/IR/ConstantRange.h>
#include <llvm/IR/Constants.h>
#include <llvm/IR/Dominators.h>
#include <llvm/IR/Instructions.h>
#include <llvm/IR/IntrinsicInst.h>
#include <llvm/IR/Operator.h>
#include <llvm/IR/PassManager.h>
#include <llvm/IR/PatternMatch.h>
namespace hirsel_llvm {
using namespace llvm;
// `if v < 0 { -v } else { v }` puts a checked negation in its own block: the
// arm computes `ssub.with.overflow(0, v)` and branches to the failure on its
// flag. The check keeps the arm a block, so SimplifyCFG cannot turn the
// diamond into a select (awfy Bounce: a data-dependent `js` per reflect where
// Rust has `neg; cmovns`), and the arm's value carries no no-wrap fact, so a
// second negation of the result keeps its own check.
//
// When every input that skips the arm makes the operation overflow-free, the
// flag implies the arm's condition: the check fails exactly when the original
// program would have entered the arm and failed there. Nothing runs between the
// branch and the check, so moving the operation and its failure branch to just
// before the branch leaves which inputs fail, where they fail, and every effect
// before the failure unchanged. The arm keeps only an unconditional branch.
// Its value is then used only on the overflow-free edge, so it is rebuilt as a
// no-wrap operation, which lets InstCombine form `abs` and fold the outer
// negation's check. Arms are limited to the operation, its two extracts and
// nothing else, so no other work is speculated onto the skipping path.
class ImpliedCheckHoistPass : public OptionalPassInfoMixin<ImpliedCheckHoistPass> {
public:
    PreservedAnalyses run(Function &f, FunctionAnalysisManager &) {
        bool changed = false;
        SmallVector<BasicBlock *, 16> arms;
        for (BasicBlock &block : f) arms.push_back(&block);
        for (BasicBlock *arm : arms) changed |= hoist(*arm);
        return changed ? PreservedAnalyses::none() : PreservedAnalyses::all();
    }

private:
    // The constant operand and the variable one, or false.
    static bool split(WithOverflowInst &op, Value *&variable, const APInt *&constant,
                      bool &variableLeft) {
        using namespace PatternMatch;
        if (match(op.getRHS(), m_APInt(constant))) {
            variable = op.getLHS();
            variableLeft = true;
            return !isa<Constant>(variable);
        }
        if (match(op.getLHS(), m_APInt(constant))) {
            variable = op.getRHS();
            variableLeft = false;
            return !isa<Constant>(variable);
        }
        return false;
    }

    // True when no value in `range` for the variable operand overflows.
    static bool overflowFree(WithOverflowInst &op, const ConstantRange &range, const APInt &k,
                             bool variableLeft) {
        ConstantRange constant(k);
        const ConstantRange &l = variableLeft ? range : constant;
        const ConstantRange &r = variableLeft ? constant : range;
        auto never = ConstantRange::OverflowResult::NeverOverflows;
        switch (op.getIntrinsicID()) {
        case Intrinsic::sadd_with_overflow: return l.signedAddMayOverflow(r) == never;
        case Intrinsic::uadd_with_overflow: return l.unsignedAddMayOverflow(r) == never;
        case Intrinsic::ssub_with_overflow: return l.signedSubMayOverflow(r) == never;
        case Intrinsic::usub_with_overflow: return l.unsignedSubMayOverflow(r) == never;
        case Intrinsic::umul_with_overflow: return l.unsignedMulMayOverflow(r) == never;
        case Intrinsic::smul_with_overflow:
            return ConstantRange::makeExactNoWrapRegion(Instruction::Mul, k,
                                                        OverflowingBinaryOperator::NoSignedWrap)
                .contains(range);
        default: return false;
        }
    }

    static bool hoist(BasicBlock &arm) {
        using namespace PatternMatch;
        auto *pred = arm.getSinglePredecessor();
        auto *guard = pred ? dyn_cast<BranchInst>(pred->getTerminator()) : nullptr;
        if (!guard || !guard->isConditional() || guard->getSuccessor(0) == guard->getSuccessor(1))
            return false;
        auto *check = dyn_cast<BranchInst>(arm.getTerminator());
        if (!check || !check->isConditional()) return false;
        // The arm is exactly the operation and its extracts.
        WithOverflowInst *op = nullptr;
        ExtractValueInst *value = nullptr, *flag = nullptr;
        for (Instruction &i : arm) {
            if (&i == check || isa<DbgInfoIntrinsic>(i)) continue;
            if (auto *o = dyn_cast<WithOverflowInst>(&i); o && !op) { op = o; continue; }
            auto *e = dyn_cast<ExtractValueInst>(&i);
            if (!e || !op || e->getAggregateOperand() != op || e->getNumIndices() != 1) return false;
            ExtractValueInst *&slot = e->getIndices()[0] ? flag : value;
            if (slot) return false;
            slot = e;
        }
        if (!op || !flag || check->getCondition() != flag) return false;
        BasicBlock *fail = check->getSuccessor(0), *ok = check->getSuccessor(1);
        if (fail == &arm || ok == &arm || fail == ok || fail == pred) return false;
        // Only an arm that rejoins the guard's other edge, directly or through
        // one block, so the hoist leaves a select. A loop guard's body arm
        // (`while k != 0 { k + 1 ... }`) would only gain a check on the exit
        // path (LangArena Fannkuchredux: +2% instructions).
        BasicBlock *other = guard->getSuccessor(guard->getSuccessor(0) == &arm ? 1 : 0);
        if (other != ok && other->getSingleSuccessor() != ok) return false;
        // The failure edge carries no value made in the arm, and the failure
        // block has no edge from the guard yet.
        for (PHINode &phi : fail->phis()) {
            Value *in = phi.getIncomingValueForBlock(&arm);
            if (auto *i = dyn_cast<Instruction>(in); i && i->getParent() == &arm) return false;
            if (phi.getBasicBlockIndex(pred) >= 0) return false;
        }
        if (value)
            for (User *user : value->users())
                if (cast<Instruction>(user)->getParent() == fail) return false;
        Value *variable;
        const APInt *k;
        bool variableLeft;
        if (!split(*op, variable, k, variableLeft)) return false;
        // The guard compares the variable operand with a constant.
        CmpPredicate predicate;
        const APInt *bound;
        if (!match(guard->getCondition(), m_ICmp(predicate, m_Specific(variable), m_APInt(bound))))
            return false;
        ConstantRange taken = ConstantRange::makeExactICmpRegion(predicate, *bound);
        ConstantRange skipping = guard->getSuccessor(0) == &arm ? taken.inverse() : taken;
        if (!overflowFree(*op, skipping, *k, variableLeft)) return false;

        // pred: ... op; br flag, fail, rest
        // rest: br cond, arm | other
        // arm:  br ok
        BasicBlock *rest = pred->splitBasicBlock(guard->getIterator(), pred->getName() + ".checked");
        Instruction *at = pred->getTerminator();
        op->moveBefore(at->getIterator());
        flag->moveBefore(at->getIterator());
        if (value) value->moveBefore(at->getIterator());
        BranchInst::Create(fail, rest, flag, at->getIterator());
        at->eraseFromParent();
        for (PHINode &phi : fail->phis()) {
            int index = phi.getBasicBlockIndex(&arm);
            if (index >= 0) phi.setIncomingBlock(index, pred);
        }
        BranchInst::Create(ok, check->getIterator());
        check->eraseFromParent();
        // Past the check the value cannot have wrapped.
        if (value && !value->use_empty()) {
            Instruction::BinaryOps opcode;
            bool sign;
            switch (op->getIntrinsicID()) {
            case Intrinsic::sadd_with_overflow: opcode = Instruction::Add; sign = true; break;
            case Intrinsic::uadd_with_overflow: opcode = Instruction::Add; sign = false; break;
            case Intrinsic::ssub_with_overflow: opcode = Instruction::Sub; sign = true; break;
            case Intrinsic::usub_with_overflow: opcode = Instruction::Sub; sign = false; break;
            case Intrinsic::smul_with_overflow: opcode = Instruction::Mul; sign = true; break;
            default: opcode = Instruction::Mul; sign = false; break;
            }
            auto *wrapless = BinaryOperator::Create(opcode, op->getLHS(), op->getRHS(),
                                                    value->getName(), rest->getFirstNonPHIIt());
            if (sign) wrapless->setHasNoSignedWrap();
            else wrapless->setHasNoUnsignedWrap();
            value->replaceAllUsesWith(wrapless);
            value->eraseFromParent();
        }
        return true;
    }
};
} // namespace hirsel_llvm
