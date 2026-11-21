// Drops overflow and failure checks on values that depend on a loop counter
// when a range fixed point proves they can never fire, including checks on
// counters that a reset keeps in range.
#pragma once
#include <llvm/ADT/DenseMap.h>
#include <llvm/ADT/SmallPtrSet.h>
#include <llvm/Analysis/LoopInfo.h>
#include <llvm/IR/ConstantRange.h>
#include <llvm/IR/Constants.h>
#include <llvm/IR/Dominators.h>
#include <llvm/IR/Instructions.h>
#include <llvm/IR/IntrinsicInst.h>
#include <llvm/IR/Operator.h>
#include <llvm/IR/PassManager.h>
#include <llvm/IR/PatternMatch.h>
#include <queue>
#include <vector>
namespace hirsel_llvm {
using namespace llvm;
// A counter that starts at 0, counts up and is reset to 0 when it reaches 8
// stays in [0, 7], so `1u8 << (7u8 - pos)` and `pos += 1` can never fail
// (LangArena HuffEncode's per-bit loop). LLVM does not see it: LVI and
// ConstraintElimination stop at the loop-carried phi, and SCEV has no
// recurrence for a counter with a reset. Rust with overflow checks keeps the
// same checks.
//
// The rule: the checks are the overflow intrinsics and the compares that
// branch into a failure whose operands depend on a loop header's integer phi.
// Every integer value those operands read, transitively, gets a range that
// holds for each value it ever takes; any other value has its type's whole
// range. A phi's range is the union of its incoming values from reachable
// blocks, each one narrowed by the conditions that hold on its edge (branch
// conditions on the chain of single-predecessor blocks above the edge, and a
// select's condition for its arms); any other instruction applies its
// operation to its operands' ranges. The ranges grow from empty to a fixed
// point. A check whose operands' ranges cannot make it fail is removed: an
// overflow flag becomes false, and the condition of a branch into a failure
// becomes its non-failing constant. Nothing else changes, so the failures
// that remain keep their sites and causes, and the control flow graph is
// unchanged.
//
// Soundness. Arithmetic uses ConstantRange's modular operations, which cover
// wrapping, and the value an overflow intrinsic returns is the wrapped one.
// No-wrap flags only narrow where a violation would be poison. A condition
// narrows a value only when neither the value nor the compared operands are
// defined in a block between the branch and the edge, so they are the values
// the branch tested. Termination. A phi only grows (its new range is joined
// with its old one); from its third change on it widens to the nearest
// thresholds (c-1, c and c+1 for each constant c that a value of its width in
// the slice is compared with), and after 2 + T changes, T being the number of
// those thresholds, to its type's whole range, so it changes at most 3 + T
// times. Every cycle of values passes through a phi, so the worklist, which
// re-evaluates only the readers of a changed value, empties; the widened
// ranges stay sound because they only grow, and the fixed point satisfies
// every value's equation, so each range covers every value it takes.
class CounterRangePass : public OptionalPassInfoMixin<CounterRangePass> {
    using Report = void (*)(void *, const char *, size_t, unsigned);
    Report report;
    void *receiver;

public:
    CounterRangePass(Report report, void *receiver) : report(report), receiver(receiver) {}

    PreservedAnalyses run(Function &f, FunctionAnalysisManager &analyses) {
        if (f.isDeclaration()) return PreservedAnalyses::all();
        LoopInfo &loops = analyses.getResult<LoopAnalysis>(f);
        if (loops.empty()) return PreservedAnalyses::all();
        State state(f, analyses.getResult<DominatorTreeAnalysis>(f));
        if (!state.collect(loops)) return PreservedAnalyses::all();
        state.solve();
        unsigned removed = 0;
        if (!state.fold(removed)) return PreservedAnalyses::all();
        if (report && removed && f.hasName())
            report(receiver, f.getName().data(), f.getName().size(), removed);
        PreservedAnalyses kept;
        kept.preserveSet<CFGAnalyses>();
        return kept;
    }

private:
    // A raise in the block, or within four unconditional hops of it
    // (failure_edges.h's test, on the same IR).
    static bool reachesRaise(BasicBlock *block) {
        for (int hop = 0; block && hop < 4; hop++) {
            for (Instruction &i : *block)
                if (auto *call = dyn_cast<CallInst>(&i))
                    if (Function *callee = call->getCalledFunction();
                        callee && callee->getName().starts_with("hirsel_core_raise"))
                        return true;
            auto *branch = dyn_cast<UncondBrInst>(block->getTerminator());
            if (!branch) return false;
            block = branch->getSuccessor();
        }
        return false;
    }

    struct State {
        Function &f;
        DominatorTree &tree;
        std::vector<WithOverflowInst *> overflows;
        std::vector<CondBrInst *> guards;
        // The values the checks read, transitively, operands first; their
        // ranges; and for each one the values whose evaluation read it.
        std::vector<Instruction *> items;
        DenseMap<Value *, unsigned> index;
        std::vector<ConstantRange> ranges;
        std::vector<unsigned> changes;
        mutable std::vector<SmallVector<unsigned, 2>> readers;
        mutable unsigned reader = ~0u;
        DenseMap<unsigned, std::vector<APInt>> thresholds;
        mutable DenseMap<std::pair<PHINode *, unsigned>, SmallVector<std::pair<Value *, bool>, 2>> edges;

        State(Function &f, DominatorTree &tree) : f(f), tree(tree) {}

        bool reachable(Instruction *i) const { return tree.isReachableFromEntry(i->getParent()); }

        // The checks on values that depend on a loop header's integer phi,
        // and the values they read.
        bool collect(LoopInfo &loops) {
            SmallVector<Instruction *, 32> stack;
            SmallPtrSet<Instruction *, 32> seen;
            for (Loop *loop : loops.getLoopsInPreorder())
                for (PHINode &phi : loop->getHeader()->phis())
                    if (phi.getType()->isIntegerTy() && seen.insert(&phi).second) stack.push_back(&phi);
            if (stack.empty()) return false;
            while (!stack.empty()) {
                Instruction *i = stack.pop_back_val();
                for (User *user : i->users()) {
                    auto *u = dyn_cast<Instruction>(user);
                    if (!u || !reachable(u) || !seen.insert(u).second) continue;
                    if (auto *op = dyn_cast<WithOverflowInst>(u)) {
                        if (op->getType()->getStructElementType(0)->isVectorTy()) continue;
                        overflows.push_back(op);
                    } else if (auto *compare = dyn_cast<ICmpInst>(u)) {
                        if (!compare->getOperand(0)->getType()->isIntegerTy()) continue;
                        for (User *use : compare->users())
                            if (auto *branch = dyn_cast<CondBrInst>(use);
                                branch && reachesRaise(branch->getSuccessor(0)) !=
                                              reachesRaise(branch->getSuccessor(1)))
                                guards.push_back(branch);
                        continue;
                    } else if (!u->getType()->isIntegerTy() || u->getType()->isIntegerTy(1)) {
                        continue;
                    }
                    stack.push_back(u);
                }
            }
            if (overflows.empty() && guards.empty()) return false;
            for (WithOverflowInst *op : overflows) {
                include(op->getLHS());
                include(op->getRHS());
            }
            for (CondBrInst *guard : guards) {
                auto *compare = cast<ICmpInst>(guard->getCondition());
                include(compare->getOperand(0));
                include(compare->getOperand(1));
            }
            for (Instruction *i : items) {
                for (User *user : i->users()) {
                    using namespace PatternMatch;
                    CmpPredicate predicate;
                    const APInt *c;
                    if (!match(user, m_ICmp(predicate, m_Value(), m_APInt(c)))) continue;
                    auto &list = thresholds[c->getBitWidth()];
                    list.push_back(*c - 1);
                    list.push_back(*c);
                    list.push_back(*c + 1);
                }
            }
            for (auto &entry : thresholds) {
                auto &list = entry.second;
                llvm::sort(list, [](const APInt &a, const APInt &b) { return a.ult(b); });
                list.erase(std::unique(list.begin(), list.end()), list.end());
            }
            return true;
        }

        // The values `i`'s range is computed from.
        void inputs(Instruction *i, SmallVectorImpl<Value *> &out) const {
            if (auto *phi = dyn_cast<PHINode>(i)) {
                for (unsigned k = 0; k < phi->getNumIncomingValues(); k++)
                    if (tree.isReachableFromEntry(phi->getIncomingBlock(k)))
                        out.push_back(phi->getIncomingValue(k));
            } else if (auto *extract = dyn_cast<ExtractValueInst>(i)) {
                auto *op = dyn_cast<WithOverflowInst>(extract->getAggregateOperand());
                if (op && extract->getNumIndices() == 1 && extract->getIndices()[0] == 0) {
                    out.push_back(op->getLHS());
                    out.push_back(op->getRHS());
                }
            } else {
                for (Value *operand : i->operands()) out.push_back(operand);
            }
        }

        // Adds `root` and the integer values it reads to `items`, operands
        // before their readers where no cycle intervenes.
        void include(Value *root) {
            SmallVector<std::pair<Instruction *, bool>, 32> stack;
            if (auto *i = dyn_cast<Instruction>(root)) stack.push_back({i, false});
            SmallVector<Value *, 4> operands;
            while (!stack.empty()) {
                auto [i, done] = stack.pop_back_val();
                if (done) {
                    index[i] = items.size();
                    items.push_back(i);
                    unsigned width = i->getType()->getIntegerBitWidth();
                    ranges.push_back(ConstantRange::getEmpty(width));
                    changes.push_back(0);
                    readers.emplace_back();
                    continue;
                }
                if (!i->getType()->isIntegerTy() || index.count(i) || !reachable(i)) continue;
                index[i] = ~0u;
                stack.push_back({i, true});
                operands.clear();
                inputs(i, operands);
                for (Value *operand : operands)
                    if (auto *next = dyn_cast<Instruction>(operand)) stack.push_back({next, false});
            }
        }

        ConstantRange get(Value *v) const {
            if (auto *c = dyn_cast<ConstantInt>(v)) return ConstantRange(c->getValue());
            auto at = index.find(v);
            if (at == index.end() || at->second == ~0u)
                return ConstantRange::getFull(v->getType()->getScalarSizeInBits());
            if (reader != ~0u && !is_contained(readers[at->second], reader))
                readers[at->second].push_back(reader);
            return ranges[at->second];
        }

        // `v` narrowed by `condition` being `truth`, where `condition` and `v`
        // are evaluated with the same values of their operands.
        ConstantRange narrow(ConstantRange range, Value *v, Value *condition, bool truth) const {
            using namespace PatternMatch;
            Value *a, *b;
            if (truth ? match(condition, m_LogicalAnd(m_Value(a), m_Value(b)))
                      : match(condition, m_LogicalOr(m_Value(a), m_Value(b))))
                return narrow(narrow(range, v, a, truth), v, b, truth);
            auto *compare = dyn_cast<ICmpInst>(condition);
            if (!compare || !compare->getOperand(0)->getType()->isIntegerTy()) return range;
            CmpInst::Predicate predicate =
                truth ? compare->getPredicate() : compare->getInversePredicate();
            for (unsigned side = 0; side < 2; side++) {
                Value *x = compare->getOperand(side);
                const APInt *k;
                bool direct = x == v;
                // x = v + k, so v is in allowed - k.
                bool above = !direct && (match(x, m_Add(m_Specific(v), m_APInt(k))) || offsetOf(x, v, k));
                // v = x + k, so v is in allowed + k.
                bool below = !direct && !above &&
                             (match(v, m_Add(m_Specific(x), m_APInt(k))) || offsetOf(v, x, k));
                if (!direct && !above && !below) continue;
                ConstantRange other = get(compare->getOperand(1 - side));
                if (other.isEmptySet()) continue;
                ConstantRange allowed = ConstantRange::makeAllowedICmpRegion(
                    side ? CmpInst::getSwappedPredicate(predicate) : predicate, other);
                if (direct) range = range.intersectWith(allowed);
                else if (above) range = range.intersectWith(allowed.sub(ConstantRange(*k)));
                else range = range.intersectWith(allowed.add(ConstantRange(*k)));
            }
            return range;
        }

        // `sum` is the wrapped value of a checked `base + k`.
        static bool offsetOf(Value *sum, Value *base, const APInt *&k) {
            using namespace PatternMatch;
            auto *extract = dyn_cast<ExtractValueInst>(sum);
            if (!extract || extract->getNumIndices() != 1 || extract->getIndices()[0] != 0) return false;
            auto *op = dyn_cast<WithOverflowInst>(extract->getAggregateOperand());
            return op && op->getBinaryOp() == Instruction::Add && op->getLHS() == base &&
                   match(op->getRHS(), m_APInt(k));
        }

        static bool definedIn(Value *v, const SmallPtrSetImpl<BasicBlock *> &blocks) {
            auto *i = dyn_cast<Instruction>(v);
            return i && blocks.contains(i->getParent());
        }

        // Whether narrowing `v` by `condition` can change it.
        static bool tests(Value *v, Value *condition) {
            using namespace PatternMatch;
            Value *a, *b;
            if (match(condition, m_LogicalAnd(m_Value(a), m_Value(b))) ||
                match(condition, m_LogicalOr(m_Value(a), m_Value(b))))
                return tests(v, a) || tests(v, b);
            auto *compare = dyn_cast<ICmpInst>(condition);
            if (!compare) return false;
            for (Value *x : compare->operands()) {
                const APInt *k;
                if (x == v || match(x, m_Add(m_Specific(v), m_APInt(k))) || offsetOf(x, v, k) ||
                    match(v, m_Add(m_Specific(x), m_APInt(k))) || offsetOf(v, x, k))
                    return true;
            }
            return false;
        }

        // The branch conditions, with the outcome taken, that hold on the
        // edge into `phi` from its incoming block `k` and test the value it
        // receives there: those on the chain of single-predecessor blocks
        // above the edge, up to the block that defines the value.
        const SmallVector<std::pair<Value *, bool>, 2> &conditions(PHINode *phi, unsigned k) const {
            auto [at, missing] = edges.try_emplace({phi, k});
            auto &list = at->second;
            Value *v = phi->getIncomingValue(k);
            if (!missing || isa<Constant>(v)) return list;
            auto *home = dyn_cast<Instruction>(v);
            BasicBlock *from = phi->getIncomingBlock(k), *to = phi->getParent();
            SmallPtrSet<BasicBlock *, 8> between;
            for (;;) {
                auto *branch = dyn_cast<CondBrInst>(from->getTerminator());
                if (branch && branch->getSuccessor(0) != branch->getSuccessor(1) &&
                    !definedIn(branch->getCondition(), between)) {
                    auto *compare = dyn_cast<ICmpInst>(branch->getCondition());
                    bool fresh = !compare || (!definedIn(compare->getOperand(0), between) &&
                                              !definedIn(compare->getOperand(1), between));
                    if (fresh && tests(v, branch->getCondition()))
                        list.push_back({branch->getCondition(), branch->getSuccessor(0) == to});
                }
                // Above its own block the value is not the one the branches tested.
                if (home && home->getParent() == from) break;
                BasicBlock *above = from->getSinglePredecessor();
                if (!above || !between.insert(from).second) break;
                to = from;
                from = above;
            }
            return list;
        }

        // The value `phi` receives from its incoming block `k`.
        ConstantRange incoming(PHINode *phi, unsigned k) const {
            Value *v = phi->getIncomingValue(k);
            ConstantRange range = get(v);
            for (auto [condition, truth] : conditions(phi, k)) {
                if (range.isEmptySet()) break;
                range = narrow(range, v, condition, truth);
            }
            return range;
        }

        ConstantRange evaluate(Instruction &i) const {
            unsigned width = i.getType()->getIntegerBitWidth();
            if (auto *phi = dyn_cast<PHINode>(&i)) {
                ConstantRange range = ConstantRange::getEmpty(width);
                for (unsigned k = 0; k < phi->getNumIncomingValues(); k++)
                    if (tree.isReachableFromEntry(phi->getIncomingBlock(k)))
                        range = range.unionWith(incoming(phi, k));
                return range;
            }
            if (auto *op = dyn_cast<BinaryOperator>(&i)) {
                ConstantRange l = get(op->getOperand(0)), r = get(op->getOperand(1));
                if (auto *wrapping = dyn_cast<OverflowingBinaryOperator>(op)) {
                    unsigned kind = 0;
                    if (wrapping->hasNoUnsignedWrap()) kind |= OverflowingBinaryOperator::NoUnsignedWrap;
                    if (wrapping->hasNoSignedWrap()) kind |= OverflowingBinaryOperator::NoSignedWrap;
                    if (kind) return l.overflowingBinaryOp(op->getOpcode(), r, kind);
                }
                return l.binaryOp(op->getOpcode(), r);
            }
            if (auto *cast = dyn_cast<CastInst>(&i)) {
                if (!cast->getSrcTy()->isIntegerTy()) return ConstantRange::getFull(width);
                switch (cast->getOpcode()) {
                case Instruction::ZExt: case Instruction::SExt: case Instruction::Trunc:
                    return get(cast->getOperand(0)).castOp(cast->getOpcode(), width);
                default: return ConstantRange::getFull(width);
                }
            }
            if (auto *select = dyn_cast<SelectInst>(&i)) {
                Value *condition = select->getCondition();
                if (!condition->getType()->isIntegerTy(1)) return ConstantRange::getFull(width);
                return narrow(get(select->getTrueValue()), select->getTrueValue(), condition, true)
                    .unionWith(narrow(get(select->getFalseValue()), select->getFalseValue(),
                                      condition, false));
            }
            if (auto *extract = dyn_cast<ExtractValueInst>(&i)) {
                auto *op = dyn_cast<WithOverflowInst>(extract->getAggregateOperand());
                if (op && extract->getNumIndices() == 1 && extract->getIndices()[0] == 0)
                    return get(op->getLHS()).binaryOp(op->getBinaryOp(), get(op->getRHS()));
                return ConstantRange::getFull(width);
            }
            if (auto *call = dyn_cast<IntrinsicInst>(&i)) {
                if (!ConstantRange::isIntrinsicSupported(call->getIntrinsicID()))
                    return ConstantRange::getFull(width);
                SmallVector<ConstantRange, 2> arguments;
                for (Value *argument : call->args()) {
                    if (!argument->getType()->isIntegerTy()) return ConstantRange::getFull(width);
                    arguments.push_back(get(argument));
                }
                return ConstantRange::intrinsic(call->getIntrinsicID(), arguments);
            }
            return ConstantRange::getFull(width);
        }

        // The smallest range of thresholds that contains `range`, in the
        // unsigned order, else the signed one, else the whole type.
        ConstantRange widen(const ConstantRange &range, const std::vector<APInt> &list) const {
            unsigned width = range.getBitWidth();
            for (bool sign : {false, true}) {
                if (sign ? range.isSignWrappedSet() : range.isWrappedSet()) continue;
                APInt low = sign ? range.getSignedMin() : range.getUnsignedMin();
                APInt high = sign ? range.getSignedMax() : range.getUnsignedMax();
                APInt floor = sign ? APInt::getSignedMinValue(width) : APInt::getMinValue(width);
                APInt ceiling = sign ? APInt::getSignedMaxValue(width) : APInt::getMaxValue(width);
                auto le = [&](const APInt &a, const APInt &b) { return sign ? a.sle(b) : a.ule(b); };
                for (const APInt &t : list) {
                    if (le(t, low) && le(floor, t)) floor = t;
                    if (le(high, t) && le(t, ceiling)) ceiling = t;
                }
                return ConstantRange::getNonEmpty(floor, ceiling + 1);
            }
            return ConstantRange::getFull(width);
        }

        void solve() {
            static const std::vector<APInt> none;
            std::priority_queue<unsigned, std::vector<unsigned>, std::greater<unsigned>> work;
            std::vector<bool> queued(items.size(), true);
            for (unsigned k = 0; k < items.size(); k++) work.push(k);
            while (!work.empty()) {
                unsigned k = work.top();
                work.pop();
                queued[k] = false;
                Instruction &i = *items[k];
                const ConstantRange &old = ranges[k];
                reader = k;
                ConstantRange range = evaluate(i);
                reader = ~0u;
                if (isa<PHINode>(&i)) {
                    range = range.unionWith(old);
                    if (range != old) {
                        auto at = thresholds.find(range.getBitWidth());
                        const std::vector<APInt> &list = at == thresholds.end() ? none : at->second;
                        unsigned count = ++changes[k];
                        if (count > 2 + list.size()) range = ConstantRange::getFull(range.getBitWidth());
                        else if (count > 2) range = widen(range, list).unionWith(old);
                    }
                }
                if (range == old) continue;
                ranges[k] = range;
                for (unsigned next : readers[k])
                    if (!queued[next]) {
                        queued[next] = true;
                        work.push(next);
                    }
            }
        }

        bool neverOverflows(WithOverflowInst &op) const {
            ConstantRange l = get(op.getLHS()), r = get(op.getRHS());
            if (l.isEmptySet() || r.isEmptySet()) return false;
            auto never = ConstantRange::OverflowResult::NeverOverflows;
            switch (op.getIntrinsicID()) {
            case Intrinsic::sadd_with_overflow: return l.signedAddMayOverflow(r) == never;
            case Intrinsic::uadd_with_overflow: return l.unsignedAddMayOverflow(r) == never;
            case Intrinsic::ssub_with_overflow: return l.signedSubMayOverflow(r) == never;
            case Intrinsic::usub_with_overflow: return l.unsignedSubMayOverflow(r) == never;
            case Intrinsic::umul_with_overflow: return l.unsignedMulMayOverflow(r) == never;
            default: return false;
            }
        }

        bool fold(unsigned &removed) {
            bool changed = false;
            for (WithOverflowInst *op : overflows) {
                if (!neverOverflows(*op)) continue;
                for (User *user : make_early_inc_range(op->users())) {
                    auto *flag = dyn_cast<ExtractValueInst>(user);
                    if (!flag || flag->getNumIndices() != 1 || flag->getIndices()[0] != 1) continue;
                    if (!flag->use_empty()) ++removed;
                    flag->replaceAllUsesWith(ConstantInt::getFalse(f.getContext()));
                    flag->eraseFromParent();
                    changed = true;
                }
            }
            for (CondBrInst *guard : guards) {
                auto *compare = cast<ICmpInst>(guard->getCondition());
                ConstantRange l = get(compare->getOperand(0)), r = get(compare->getOperand(1));
                if (l.isEmptySet() || r.isEmptySet()) continue;
                bool fails = reachesRaise(guard->getSuccessor(0));
                CmpInst::Predicate predicate = fails ? compare->getInversePredicate()
                                                     : compare->getPredicate();
                // The branch takes its non-failing edge for every operand value.
                if (!l.icmp(predicate, r)) continue;
                guard->setCondition(ConstantInt::getBool(f.getContext(), !fails));
                if (compare->use_empty()) compare->eraseFromParent();
                ++removed;
                changed = true;
            }
            return changed;
        }
    };
};
} // namespace hirsel_llvm
