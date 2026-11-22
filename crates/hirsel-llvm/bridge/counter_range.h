// Drops overflow and failure checks on values that depend on a loop counter
// when a range fixed point proves they can never fire, including checks on
// counters that a reset keeps in range.
#pragma once
#include "loop_version.h"
#include "source_remark.h"
#include <llvm/ADT/BitVector.h>
#include <llvm/ADT/DenseMap.h>
#include <llvm/ADT/MapVector.h>
#include <llvm/ADT/PostOrderIterator.h>
#include <llvm/ADT/SetVector.h>
#include <llvm/ADT/SmallPtrSet.h>
#include <llvm/Analysis/LoopInfo.h>
#include <llvm/Analysis/ScalarEvolution.h>
#include <llvm/Analysis/ValueTracking.h>
#include <llvm/IR/CFG.h>
#include <llvm/IR/ConstantRange.h>
#include <llvm/IR/Constants.h>
#include <llvm/IR/DataLayout.h>
#include <llvm/IR/Dominators.h>
#include <llvm/IR/IRBuilder.h>
#include <llvm/IR/Instructions.h>
#include <llvm/IR/IntrinsicInst.h>
#include <llvm/IR/Operator.h>
#include <llvm/IR/PassManager.h>
#include <llvm/IR/PatternMatch.h>
#include <llvm/Transforms/Utils/Local.h>
#include <numeric>
#include <optional>
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
//
// Closed memory. Loads from a closed object (see `close`) are values of the
// slice too: Bounce's balls are an `Inline[Ball, 100]` whose x and vx stay
// in [0, 500] and [-150, 150], which no SSA value carries across iterations.
// Each integer cell of an object has a summary range holding every value any
// element's field has where the object is initialised, except the fields of
// one materialised element, written through one SSA base pointer since the
// last flush, whose current contents are tracked as slots (a range and the
// SSA values equal to it, narrowed on edges like a phi). The slots flush into
// the summaries on a store through another base, at a join that disagrees on
// the base, on an edge leaving the base's dominance, at every retreating edge
// and at every fill, copy or lifetime marker. Where `init` (a must-fact over
// all edges) does not hold, a load has its type's whole range. Rounds
// alternate the SSA fixed point with one reverse-postorder sweep over memory;
// a summary is widened like a phi per round, so the rounds end. The full
// argument is the design note of collection-ranges-1529.
//
// Bounded counters. `count += if alive {1} else {0}` over a list of neighbours
// (LangArena GameOfLife) keeps its overflow check: the count has no range of
// its own, since it can grow by one per element of a list of any length.
// What bounds it is the index: the count grows by at most one for each step
// of an index that stays below the list's length `n`, so it stays at most
// its start plus `n`. The rule: for a check that fails only when `count`
// exceeds `limit`, find an index `i` with `i < n` wherever the check runs, and
// an upper bound `D` on `count - i` over the integers that holds wherever both
// are defined; then `count <= n - 1 + D`, and the check cannot fail when
// `n + D <= limit + 1`. `D` is a fixed point over pairs of values: a count
// plus a constant or a zero-extended bool adds it, an index plus k that
// cannot wrap subtracts k, a select joins its arms, and a pair that has a phi
// joins the pairs on the phi's incoming edges (both phis of one block in
// step, or a phi against a value that dominates its block). A pair with no
// phi is constants or bounds itself as `count - i`; joining two different
// such pairs is unbounded, and so is a variable whose bound grows a fifth
// time. `i < n` is a dominating branch outcome or assumption at the check, or
// for a phi a coinductive fact: every value it receives is below `n` on its
// edge, as zero where `n` is nonzero, as `x + 1` with `x < n` where it is not
// `n`, or as a phi with the fact; an assumption an edge refutes is withdrawn
// until the rest hold. Every path, early exit and nested loop is in the fixed
// point; a count that grows faster than its index has no bound.
// Only loops that call nothing but intrinsics and failures are discharged: a
// check beside a call costs little, and folding it moves the unroller's and
// the specialize inliner's choices (AWFY Queens unrolled its ten solves and
// lost the inlined levels of its recursion, +17% instructions).
// Discharges. When `n + D <= limit + 1` holds for every value (constant
// operands, or the range of `n`), the check is removed as above. Otherwise
// the outermost loop around the check whose preheader sees
// the same `n` and pair is versioned (loop_version.h): the preheader tests the
// condition once, in 128 bits, frozen; the loop runs without the check when
// it holds, and its unchanged checked copy runs when it does not, so a
// reachable overflow fails at the same step with the same effects.
class CounterRangePass : public OptionalPassInfoMixin<CounterRangePass> {
    using Report = void (*)(void *, const char *, size_t, unsigned);
    Report report;
    SourceRemark source;
    void *receiver;
    // After inlining: bounded counters only, and they may version loops.
    bool version;

public:
    CounterRangePass(Report report, SourceRemark source, void *receiver, bool version = false)
        : report(report), source(source), receiver(receiver), version(version) {}

    PreservedAnalyses run(Function &f, FunctionAnalysisManager &analyses) {
        if (f.isDeclaration()) return PreservedAnalyses::all();
        LoopInfo &loops = analyses.getResult<LoopAnalysis>(f);
        if (loops.empty()) return PreservedAnalyses::all();
        DominatorTree &tree = analyses.getResult<DominatorTreeAnalysis>(f);
        unsigned removed = 0;
        bool changed = false;
        if (!version) {
            State state(f, tree);
            state.close();
            if (state.collect(loops)) {
                state.solve();
                changed |= state.fold(removed, source, receiver);
            }
        }
        bool reshaped = bound(f, loops, tree, analyses, removed, changed);
        if (!changed) return PreservedAnalyses::all();
        if (report && removed && f.hasName())
            report(receiver, f.getName().data(), f.getName().size(), removed);
        if (reshaped) return PreservedAnalyses::none();
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

    // Only a known Bounds cause identifies a bounds check. The pass also
    // proves comparisons that lead to other causes.
    static bool reachesBounds(BasicBlock *block) {
        for (int hop = 0; block && hop < 4; hop++) {
            for (Instruction &i : *block) {
                auto *call = dyn_cast<CallInst>(&i);
                if (!call || !call->getCalledFunction()) continue;
                if (call->getCalledFunction()->getName() == "hirsel_core_raise" && call->arg_size() == 2) {
                    auto *cause = call->getArgOperand(1)->stripPointerCasts();
                    return isa<GlobalVariable>(cause) && cause->getName().starts_with("hirsel_cause_1_");
                }
            }
            auto *branch = dyn_cast<UncondBrInst>(block->getTerminator());
            if (!branch) return false;
            block = branch->getSuccessor();
        }
        return false;
    }

    struct State {
        Function &f;
        DominatorTree &tree;
        const DataLayout &layout;
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

        std::priority_queue<unsigned, std::vector<unsigned>, std::greater<unsigned>> work;
        std::vector<bool> queued;

        State(Function &f, DominatorTree &tree) : f(f), tree(tree), layout(f.getDataLayout()) {}

        // Closed memory. An object is a static alloca whose every derived
        // pointer is used only by GEPs, pointer compares, simple loads and
        // stores of scalars, non-volatile memsets with constant bounds,
        // non-volatile whole copies between objects, and lifetime markers.
        // Nothing else can read or write its bytes. Objects joined by whole
        // copies form one group with one partition into cells: residues
        // modulo a power-of-two grain, which every variable stride and the
        // size are multiples of, so a residue survives any wrap of the
        // address arithmetic. Any failure of these rules opens the whole
        // group, and an open object's loads keep their type's whole range.
        struct Cell {
            uint64_t residue;
            unsigned bytes;
            bool integer;
        };
        struct Object {
            AllocaInst *alloca;
            uint64_t size;
            unsigned group;
            bool open = false;
        };
        struct Group {
            uint64_t grain = 0;
            std::vector<Cell> cells;
            bool open = false;
        };
        enum class Kind { Load, Store, Fill, Copy, Reset };
        // An access to a closed object. Loads and stores name an integer cell
        // (or none for a non-integer one) and a base: the pointer with its
        // trailing constant offsets stripped, plus the stripped offset.
        struct Event {
            Kind kind;
            unsigned object;
            int cell = -1;
            Value *base = nullptr;
            int64_t field = 0;
            uint64_t offset = 0, length = 0;
            uint8_t byte = 0;
            unsigned source = 0;
        };
        struct Slot {
            int64_t field;
            unsigned cell;
            ConstantRange range;
            SmallVector<Value *, 2> values;
        };
        // An object's materialised element: the fields of the element at
        // `base` whose current contents are known more precisely than the
        // cell summaries.
        struct Dirty {
            Value *base = nullptr;
            SmallVector<Slot, 4> slots;
        };
        std::vector<Object> objects;
        std::vector<Group> groups;
        DenseMap<Instruction *, Event> events;
        std::vector<std::vector<ConstantRange>> summaries, contributions;
        std::vector<std::vector<unsigned>> summaryChanges;
        std::vector<std::vector<std::vector<APInt>>> summaryThresholds;
        DenseMap<LoadInst *, ConstantRange> loaded;
        std::vector<BasicBlock *> order;
        DenseMap<BasicBlock *, unsigned> position;
        std::vector<BitVector> initialised;
        // Whether a read of the object (a load, or a copy from it) is
        // reachable from the start of each block.
        std::vector<BitVector> live;
        bool gaveUp = false;

        // The address `p` derived from object `object` as `object + Σ k·s + c`,
        // with its base and the offset from the base.
        bool decompose(Value *p, unsigned object, SmallVectorImpl<APInt> &strides, APInt &offset,
                       Value *&base, int64_t &field) const {
            offset = APInt(64, 0);
            APInt stripped(64, 0);
            base = nullptr;
            while (auto *gep = dyn_cast<GEPOperator>(p)) {
                SmallMapVector<Value *, APInt, 4> variables;
                APInt constant(64, 0);
                if (!gep->collectOffset(layout, 64, variables, constant)) return false;
                offset += constant;
                for (auto &entry : variables)
                    if (!entry.second.isZero()) strides.push_back(entry.second);
                if (!base) {
                    if (variables.empty()) stripped += constant;
                    else base = gep;
                }
                p = gep->getPointerOperand();
            }
            if (p != objects[object].alloca) return false;
            if (!base) base = p;
            field = stripped.getSExtValue();
            return true;
        }

        static bool scalar(Type *type) {
            return type->isIntegerTy() || type->isFloatingPointTy() || type->isPointerTy();
        }

        unsigned root(std::vector<unsigned> &parent, unsigned k) const {
            while (parent[k] != k) k = parent[k] = parent[parent[k]];
            return k;
        }

        void close() {
            DenseMap<Value *, unsigned> objectOf;
            for (Instruction &i : f.getEntryBlock()) {
                auto *alloca = dyn_cast<AllocaInst>(&i);
                if (!alloca || !alloca->isStaticAlloca()) continue;
                if (layout.getIndexTypeSizeInBits(alloca->getType()) != 64) continue;
                std::optional<TypeSize> size = alloca->getAllocationSize(layout);
                if (!size || size->isScalable() || size->getFixedValue() == 0) continue;
                objectOf[alloca] = objects.size();
                objects.push_back({alloca, size->getFixedValue(), 0});
            }
            if (objects.empty()) return;
            std::vector<unsigned> parent(objects.size());
            for (unsigned k = 0; k < objects.size(); k++) parent[k] = k;
            // Every instruction that touches each object, and the strides,
            // offsets and widths of its loads and stores.
            struct Raw {
                Instruction *i;
                unsigned object;
                SmallVector<APInt, 2> strides;
                APInt offset;
                Value *base;
                int64_t field;
                unsigned bytes;
                bool integer;
            };
            std::vector<Raw> raws;
            for (unsigned k = 0; k < objects.size(); k++) {
                Object &o = objects[k];
                SmallVector<Value *, 16> stack{o.alloca};
                SmallPtrSet<Value *, 16> seen{o.alloca};
                SetVector<Instruction *> touching;
                while (!stack.empty() && !o.open) {
                    Value *v = stack.pop_back_val();
                    for (Use &use : v->uses()) {
                        User *user = use.getUser();
                        if (isa<GEPOperator>(user) && use.getOperandNo() == 0) {
                            if (seen.insert(user).second) stack.push_back(user);
                        } else if (isa<ICmpInst>(user)) {
                        } else if (auto *load = dyn_cast<LoadInst>(user)) {
                            if (!load->isSimple() || !scalar(load->getType())) o.open = true;
                            else touching.insert(load);
                        } else if (auto *store = dyn_cast<StoreInst>(user)) {
                            if (use.getOperandNo() != 1 || !store->isSimple() ||
                                !scalar(store->getValueOperand()->getType()))
                                o.open = true;
                            else touching.insert(store);
                        } else if (auto *fill = dyn_cast<MemSetInst>(user)) {
                            if (use.getOperandNo() != 0 || fill->isVolatile()) o.open = true;
                            else touching.insert(fill);
                        } else if (auto *copy = dyn_cast<MemTransferInst>(user)) {
                            if (copy->isVolatile()) o.open = true;
                            else touching.insert(copy);
                        } else if (isa<LifetimeIntrinsic>(user)) {
                            touching.insert(cast<Instruction>(user));
                        } else {
                            o.open = true;
                        }
                        if (o.open) break;
                    }
                }
                if (o.open) continue;
                for (Instruction *i : touching) {
                    Raw raw{i, k, {}, APInt(64, 0), nullptr, 0, 0, false};
                    auto fail = [&] { o.open = true; };
                    if (isa<LoadInst>(i) || isa<StoreInst>(i)) {
                        Value *pointer = getLoadStorePointerOperand(i);
                        Type *type = getLoadStoreType(i);
                        if (!decompose(pointer, k, raw.strides, raw.offset, raw.base, raw.field)) { fail(); break; }
                        raw.integer = type->isIntegerTy();
                        if (raw.integer && type->getIntegerBitWidth() % 8) { fail(); break; }
                        raw.bytes = layout.getTypeStoreSize(type).getFixedValue();
                        if (raw.integer && raw.bytes * 8 != type->getIntegerBitWidth()) { fail(); break; }
                        raws.push_back(std::move(raw));
                    } else if (auto *fill = dyn_cast<MemSetInst>(i)) {
                        auto *length = dyn_cast<ConstantInt>(fill->getLength());
                        auto *byte = dyn_cast<ConstantInt>(fill->getValue());
                        if (!length || !byte ||
                            !decompose(fill->getRawDest(), k, raw.strides, raw.offset, raw.base, raw.field) ||
                            !raw.strides.empty()) { fail(); break; }
                        // 0 <= offset <= size and 0 <= length <= size - offset.
                        APInt size(64, o.size), off = raw.offset, len = length->getValue().zextOrTrunc(64);
                        if (length->getValue().getActiveBits() > 64 || off.isNegative() || off.sgt(size) ||
                            len.ugt(size - off)) { fail(); break; }
                        Event event{Kind::Fill, k};
                        event.offset = off.getZExtValue();
                        event.length = len.getZExtValue();
                        event.byte = byte->getZExtValue();
                        events[i] = event;
                    } else if (auto *copy = dyn_cast<MemTransferInst>(i)) {
                        Value *destination = copy->getRawDest();
                        SmallVector<APInt, 2> strides;
                        APInt offset(64, 0);
                        Value *base;
                        int64_t field;
                        // As a source only, the object is read.
                        Value *into = destination;
                        while (auto *gep = dyn_cast<GEPOperator>(into)) into = gep->getPointerOperand();
                        if (into != o.alloca) continue;
                        if (!decompose(destination, k, strides, offset, base, field)) { fail(); break; }
                        auto *length = dyn_cast<ConstantInt>(copy->getLength());
                        if (!strides.empty() || !offset.isZero() || !length ||
                            length->getValue() != o.size) { fail(); break; }
                        Value *from = copy->getRawSource();
                        while (auto *gep = dyn_cast<GEPOperator>(from)) from = gep->getPointerOperand();
                        auto at = objectOf.find(from);
                        if (at == objectOf.end() || objects[at->second].size != o.size) { fail(); break; }
                        unsigned source = at->second;
                        strides.clear();
                        if (!decompose(copy->getRawSource(), source, strides, offset, base, field) ||
                            !strides.empty() || !offset.isZero()) { fail(); break; }
                        parent[root(parent, k)] = root(parent, source);
                        Event event{Kind::Copy, k};
                        event.source = source;
                        events[i] = event;
                    } else {
                        events[i] = Event{Kind::Reset, k};
                    }
                }
            }
            // One partition per group.
            DenseMap<unsigned, unsigned> groupOf;
            for (unsigned k = 0; k < objects.size(); k++) {
                unsigned r = root(parent, k);
                auto [at, fresh] = groupOf.try_emplace(r, groups.size());
                if (fresh) groups.emplace_back();
                objects[k].group = at->second;
                Group &g = groups[at->second];
                g.open |= objects[k].open;
                g.grain = std::gcd(g.grain, objects[k].size);
            }
            for (Raw &raw : raws) {
                Group &g = groups[objects[raw.object].group];
                // abs(-2^63) is 2^63 read unsigned.
                for (APInt &stride : raw.strides) g.grain = std::gcd(g.grain, stride.abs().getZExtValue());
            }
            for (Group &g : groups)
                if (g.grain == 0 || (g.grain & (g.grain - 1))) g.open = true;
            DenseMap<Instruction *, unsigned> cellOf;
            for (Raw &raw : raws) {
                Group &g = groups[objects[raw.object].group];
                if (g.open) continue;
                // The grain is a power of two, so the unsigned remainder of
                // the wrapped offset is its Euclidean residue.
                uint64_t residue = raw.offset.getZExtValue() & (g.grain - 1);
                if (residue + raw.bytes > g.grain) { g.open = true; continue; }
                unsigned found = ~0u;
                for (unsigned c = 0; c < g.cells.size(); c++) {
                    Cell &cell = g.cells[c];
                    if (cell.residue == residue && cell.bytes == raw.bytes && cell.integer == raw.integer)
                        found = c;
                    else if (residue < cell.residue + cell.bytes && cell.residue < residue + raw.bytes)
                        g.open = true;
                }
                if (g.open) continue;
                if (found == ~0u) {
                    found = g.cells.size();
                    g.cells.push_back({residue, raw.bytes, raw.integer});
                }
                cellOf[raw.i] = found;
            }
            // A group is worth solving only when an integer load reads it.
            std::vector<bool> read(groups.size(), false);
            for (Raw &raw : raws)
                if (raw.integer && isa<LoadInst>(raw.i)) read[objects[raw.object].group] = true;
            for (unsigned g = 0; g < groups.size(); g++)
                if (!read[g]) groups[g].open = true;
            for (Object &o : objects) o.open = groups[o.group].open;
            for (Raw &raw : raws) {
                if (objects[raw.object].open || !raw.integer) continue;
                Event event{isa<LoadInst>(raw.i) ? Kind::Load : Kind::Store, raw.object};
                event.cell = cellOf[raw.i];
                event.base = raw.base;
                event.field = raw.field;
                events[raw.i] = event;
            }
            // Fills, copies and lifetime markers were recorded before their
            // group could open.
            events.remove_if([&](auto &entry) { return objects[entry.second.object].open; });
            summaries.resize(objects.size());
            contributions.resize(objects.size());
            summaryChanges.resize(objects.size());
            summaryThresholds.resize(objects.size());
            for (unsigned k = 0; k < objects.size(); k++) {
                if (objects[k].open) continue;
                for (Cell &cell : groups[objects[k].group].cells) {
                    unsigned width = cell.integer ? cell.bytes * 8 : 1;
                    summaries[k].push_back(ConstantRange::getEmpty(width));
                    contributions[k].push_back(ConstantRange::getEmpty(width));
                }
                summaryChanges[k].assign(summaries[k].size(), 0);
                summaryThresholds[k].resize(summaries[k].size());
            }
            // Thresholds: the constants stored into a cell and those its
            // loads are compared with.
            for (auto &[i, event] : events) {
                if (event.cell < 0) continue;
                auto &list = summaryThresholds[event.object][event.cell];
                SmallVector<const APInt *, 4> constants;
                using namespace PatternMatch;
                const APInt *c;
                if (event.kind == Kind::Store) {
                    if (match(cast<StoreInst>(i)->getValueOperand(), m_APInt(c))) constants.push_back(c);
                } else if (event.kind == Kind::Load) {
                    CmpPredicate predicate;
                    for (User *user : i->users())
                        if (match(user, m_ICmp(predicate, m_Value(), m_APInt(c))) ||
                            match(user, m_ICmp(predicate, m_APInt(c), m_Value())))
                            constants.push_back(c);
                }
                for (const APInt *k : constants) {
                    list.push_back(*k - 1);
                    list.push_back(*k);
                    list.push_back(*k + 1);
                }
            }
            for (auto &object : summaryThresholds)
                for (auto &list : object) {
                    llvm::sort(list, [](const APInt &a, const APInt &b) { return a.ult(b); });
                    list.erase(std::unique(list.begin(), list.end()), list.end());
                }
            for (BasicBlock *block : ReversePostOrderTraversal<Function *>(&f)) {
                position[block] = order.size();
                order.push_back(block);
            }
            solveInitialised();
            solveLive();
        }

        // A value flushed where no read of the object is reachable is never
        // observed, so it need not reach the summaries: the raise and
        // step-limit paths that leave mid-update end without reading it.
        void solveLive() {
            live.assign(order.size(), BitVector(objects.size(), false));
            std::vector<BitVector> reads(order.size(), BitVector(objects.size(), false));
            for (unsigned b = 0; b < order.size(); b++)
                for (Instruction &i : *order[b]) {
                    auto at = events.find(&i);
                    if (at == events.end()) continue;
                    if (at->second.kind == Kind::Load) reads[b].set(at->second.object);
                    if (at->second.kind == Kind::Copy) reads[b].set(at->second.source);
                }
            for (bool changed = true; changed;) {
                changed = false;
                for (unsigned b = order.size(); b-- > 0;) {
                    BitVector in = reads[b];
                    for (BasicBlock *next : successors(order[b])) in |= live[position.lookup(next)];
                    if (in != live[b]) {
                        live[b] = in;
                        changed = true;
                    }
                }
            }
        }

        bool anyClosed() const {
            for (const Object &o : objects)
                if (!o.open) return true;
            return false;
        }

        bool closedLoad(Instruction *i) const {
            auto at = events.find(i);
            return at != events.end() && at->second.kind == Kind::Load;
        }

        // `init`: every cell of the object has been written on every path
        // since entry or its last lifetime marker. A must-fact over all
        // edges, solved for every object at once.
        void stepInitialised(Instruction &i, BitVector &bits) const {
            auto at = events.find(&i);
            if (at == events.end()) return;
            const Event &event = at->second;
            switch (event.kind) {
            case Kind::Fill:
                if (event.offset == 0 && event.length == objects[event.object].size) bits.set(event.object);
                break;
            case Kind::Copy: bits[event.object] = bits[event.source]; break;
            case Kind::Reset: bits.reset(event.object); break;
            default: break;
            }
        }

        void solveInitialised() {
            initialised.assign(order.size(), BitVector(objects.size(), true));
            initialised[0] = BitVector(objects.size(), false);
            std::vector<BitVector> after(order.size(), BitVector(objects.size(), true));
            for (bool changed = true; changed;) {
                changed = false;
                for (unsigned b = 0; b < order.size(); b++) {
                    BitVector in(objects.size(), b != 0);
                    if (b != 0)
                        for (BasicBlock *pred : predecessors(order[b])) {
                            auto at = position.find(pred);
                            if (at != position.end()) in &= after[at->second];
                        }
                    initialised[b] = in;
                    for (Instruction &i : *order[b]) stepInitialised(i, in);
                    if (in != after[b]) {
                        after[b] = in;
                        changed = true;
                    }
                }
            }
        }

        void flush(unsigned object, Dirty &dirty, unsigned block) {
            if (live[block][object])
                for (Slot &slot : dirty.slots)
                    contributions[object][slot.cell] = contributions[object][slot.cell].unionWith(slot.range);
            dirty = Dirty();
        }

        // Whether a materialised element at `base` can cross into `block`:
        // its address is the same SSA instance there.
        bool keeps(Value *base, BasicBlock *block) const {
            auto *home = dyn_cast<Instruction>(base);
            if (!home || isa<AllocaInst>(home)) return true;
            return tree.properlyDominates(home->getParent(), block);
        }

        // `range` of a field whose contents equal each of `values`, narrowed
        // by the branch taken from `from` to `to`.
        ConstantRange across(BasicBlock *from, BasicBlock *to, ConstantRange range,
                             ArrayRef<Value *> values) const {
            auto *branch = dyn_cast<CondBrInst>(from->getTerminator());
            if (!branch || branch->getSuccessor(0) == branch->getSuccessor(1)) return range;
            for (Value *v : values) {
                if (range.isEmptySet()) break;
                range = narrow(range, v, branch->getCondition(), branch->getSuccessor(0) == to);
            }
            return range;
        }

        // One pass over the function in reverse postorder. Loads read the
        // summaries of the previous round; every value written this round is
        // collected in `contributions`. Returns whether any summary or load
        // range grew.
        bool sweep() {
            for (unsigned k = 0; k < objects.size(); k++)
                for (unsigned c = 0; c < contributions[k].size(); c++)
                    contributions[k][c] = ConstantRange::getEmpty(contributions[k][c].getBitWidth());
            std::vector<std::vector<Dirty>> after(order.size());
            DenseMap<LoadInst *, ConstantRange> fresh;
            for (unsigned b = 0; b < order.size(); b++) {
                BasicBlock *block = order[b];
                std::vector<Dirty> state(objects.size());
                if (b != 0) {
                    SmallVector<std::pair<BasicBlock *, unsigned>, 4> preds;
                    for (BasicBlock *pred : predecessors(block)) {
                        auto at = position.find(pred);
                        if (at != position.end()) preds.push_back({pred, at->second});
                    }
                    for (unsigned k = 0; k < objects.size(); k++) {
                        if (objects[k].open) continue;
                        SmallVector<std::pair<BasicBlock *, Dirty>, 4> in;
                        for (auto [pred, p] : preds) {
                            // A retreating edge's state was flushed at its
                            // source; it enters clean.
                            if (p >= b) {
                                in.push_back({pred, Dirty()});
                                continue;
                            }
                            Dirty dirty = after[p][k];
                            if (dirty.base && !keeps(dirty.base, block)) flush(k, dirty, b);
                            in.push_back({pred, std::move(dirty)});
                        }
                        bool agree = !in.empty() && in[0].second.base;
                        for (auto &entry : in) agree &= entry.second.base == in[0].second.base;
                        if (!agree) {
                            for (auto &entry : in) flush(k, entry.second, b);
                            continue;
                        }
                        Dirty merged;
                        merged.base = in[0].second.base;
                        for (auto &entry : in)
                            for (Slot &slot : entry.second.slots)
                                if (none_of(merged.slots, [&](Slot &s) { return s.field == slot.field; }))
                                    merged.slots.push_back({slot.field, slot.cell,
                                                            ConstantRange::getEmpty(slot.range.getBitWidth()),
                                                            {}});
                        for (Slot &slot : merged.slots) {
                            bool missing = false, first = true;
                            SmallVector<Value *, 2> values;
                            for (auto &[pred, dirty] : in) {
                                Slot *own = nullptr;
                                for (Slot &s : dirty.slots)
                                    if (s.field == slot.field) own = &s;
                                if (!own) {
                                    // An edge that did not write the field
                                    // brings a summarised value.
                                    missing = true;
                                    slot.range = slot.range.unionWith(summaries[k][slot.cell]);
                                    continue;
                                }
                                slot.range = slot.range.unionWith(across(pred, block, own->range, own->values));
                                if (first) values = own->values;
                                else erase_if(values, [&](Value *v) { return !is_contained(own->values, v); });
                                first = false;
                            }
                            if (missing) continue;
                            slot.values = values;
                            // A phi that receives the field's contents on
                            // every edge equals it here.
                            for (PHINode &phi : block->phis())
                                if (all_of(in, [&](auto &entry) {
                                        for (Slot &s : entry.second.slots)
                                            if (s.field == slot.field)
                                                return is_contained(s.values, phi.getIncomingValueForBlock(entry.first));
                                        return false;
                                    }))
                                    slot.values.push_back(&phi);
                        }
                        state[k] = std::move(merged);
                    }
                }
                BitVector init = initialised[b];
                for (Instruction &i : *block) {
                    auto at = events.find(&i);
                    if (at == events.end()) continue;
                    const Event &event = at->second;
                    unsigned k = event.object;
                    Dirty &dirty = state[k];
                    switch (event.kind) {
                    case Kind::Store: {
                        if (dirty.base != event.base) {
                            flush(k, dirty, b);
                            dirty.base = event.base;
                        }
                        Value *v = cast<StoreInst>(&i)->getValueOperand();
                        SmallVector<Value *, 2> values;
                        if (!isa<Constant>(v)) values.push_back(v);
                        Slot *own = nullptr;
                        for (Slot &s : dirty.slots)
                            if (s.field == event.field) own = &s;
                        if (own) {
                            own->range = get(v);
                            own->values = values;
                        } else {
                            dirty.slots.push_back({event.field, unsigned(event.cell), get(v), values});
                        }
                        break;
                    }
                    case Kind::Load: {
                        auto *load = cast<LoadInst>(&i);
                        unsigned width = load->getType()->getIntegerBitWidth();
                        ConstantRange range = ConstantRange::getFull(width);
                        if (init[k]) {
                            Slot *own = nullptr;
                            if (dirty.base == event.base)
                                for (Slot &s : dirty.slots)
                                    if (s.field == event.field) own = &s;
                            if (own) {
                                range = own->range;
                                own->values.push_back(load);
                            } else {
                                // The load may read the materialised element.
                                range = summaries[k][event.cell];
                                for (Slot &s : dirty.slots)
                                    if (s.cell == unsigned(event.cell)) range = range.unionWith(s.range);
                            }
                        }
                        fresh.insert({load, range});
                        break;
                    }
                    case Kind::Fill: {
                        flush(k, dirty, b);
                        const Group &g = groups[objects[k].group];
                        uint64_t size = objects[k].size, low = event.offset, high = event.offset + event.length;
                        for (unsigned c = 0; c < g.cells.size(); c++) {
                            const Cell &cell = g.cells[c];
                            if (!cell.integer || low == high) continue;
                            bool whole = false, part = false;
                            // The first and last elements the fill meets
                            // may be partly covered; any between are whole.
                            uint64_t first = low / g.grain, last = std::min((high - 1) / g.grain, size / g.grain - 1);
                            for (uint64_t e : {first, last}) {
                                uint64_t a = e * g.grain + cell.residue, z = a + cell.bytes;
                                if (z <= low || a >= high) continue;
                                if (a >= low && z <= high) whole = true;
                                else part = true;
                            }
                            if (last > first + 1) whole = true;
                            ConstantRange &into = contributions[k][c];
                            if (part) into = ConstantRange::getFull(into.getBitWidth());
                            else if (whole)
                                into = into.unionWith(ConstantRange(
                                    APInt::getSplat(cell.bytes * 8, APInt(8, event.byte))));
                        }
                        stepInitialised(i, init);
                        break;
                    }
                    case Kind::Copy:
                        flush(k, dirty, b);
                        flush(event.source, state[event.source], b);
                        for (unsigned c = 0; c < contributions[k].size(); c++)
                            contributions[k][c] = contributions[k][c].unionWith(summaries[event.source][c]);
                        stepInitialised(i, init);
                        break;
                    case Kind::Reset:
                        flush(k, dirty, b);
                        stepInitialised(i, init);
                        break;
                    }
                }
                for (BasicBlock *next : successors(block))
                    if (unsigned to = position.lookup(next); to <= b)
                        for (unsigned k = 0; k < objects.size(); k++) {
                            Dirty copy = state[k];
                            flush(k, copy, to);
                        }
                after[b] = std::move(state);
            }
            bool grew = false;
            for (unsigned k = 0; k < objects.size(); k++)
                for (unsigned c = 0; c < summaries[k].size(); c++) {
                    ConstantRange &old = summaries[k][c];
                    ConstantRange range = old.unionWith(contributions[k][c]);
                    if (range == old) continue;
                    const std::vector<APInt> &list = summaryThresholds[k][c];
                    unsigned count = ++summaryChanges[k][c];
                    if (count > 2 + list.size()) range = ConstantRange::getFull(range.getBitWidth());
                    else if (count > 2) range = widen(range, list).unionWith(old);
                    old = range;
                    grew = true;
                }
            for (auto &[load, range] : fresh) {
                auto [at, added] = loaded.try_emplace(load, range);
                if (!added) {
                    ConstantRange joined = at->second.unionWith(range);
                    if (joined == at->second) continue;
                    at->second = joined;
                } else if (range.isEmptySet()) {
                    continue;
                }
                grew = true;
                auto item = index.find(load);
                if (item == index.end() || item->second == ~0u) continue;
                ranges[item->second] = at->second;
                for (unsigned next : readers[item->second]) enqueue(next);
            }
            return grew;
        }

        bool reachable(Instruction *i) const { return tree.isReachableFromEntry(i->getParent()); }

        // The checks on values that depend on a loop header's integer phi,
        // and the values they read.
        bool collect(LoopInfo &loops) {
            SmallVector<Instruction *, 32> stack;
            SmallPtrSet<Instruction *, 32> seen;
            for (Loop *loop : loops.getLoopsInPreorder())
                for (PHINode &phi : loop->getHeader()->phis())
                    if (phi.getType()->isIntegerTy() && seen.insert(&phi).second) stack.push_back(&phi);
            for (auto &[i, event] : events)
                if (event.kind == Kind::Load && reachable(i) && seen.insert(i).second) stack.push_back(i);
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
            for (auto &[i, event] : events)
                if (event.kind == Kind::Store) include(cast<StoreInst>(i)->getValueOperand());
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
            if (auto *load = dyn_cast<LoadInst>(&i)) {
                if (!closedLoad(load)) return ConstantRange::getFull(width);
                auto at = loaded.find(load);
                return at == loaded.end() ? ConstantRange::getEmpty(width) : at->second;
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

        void enqueue(unsigned k) {
            if (queued[k]) return;
            queued[k] = true;
            work.push(k);
        }

        // The SSA fixed point, alternated with sweeps over closed memory
        // until neither grows.
        void solve() {
            queued.assign(items.size(), true);
            for (unsigned k = 0; k < items.size(); k++) work.push(k);
            settle();
            if (!anyClosed()) return;
            for (unsigned round = 0; sweep(); round++) {
                // Widening bounds the rounds; this only guards compile time.
                if (round == 256) {
                    gaveUp = true;
                    return;
                }
                settle();
            }
        }

        void settle() {
            static const std::vector<APInt> none;
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
                for (unsigned next : readers[k]) enqueue(next);
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

        bool fold(unsigned &removed, SourceRemark source, void *receiver) {
            if (gaveUp) return false;
            bool changed = false;
            for (WithOverflowInst *op : overflows) {
                if (!neverOverflows(*op)) continue;
                for (User *user : make_early_inc_range(op->users())) {
                    auto *flag = dyn_cast<ExtractValueInst>(user);
                    if (!flag || flag->getNumIndices() != 1 || flag->getIndices()[0] != 1) continue;
                    if (!flag->use_empty()) {
                        ++removed;
                        remark(source, receiver, OverflowCheck, *op);
                    }
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
                if (source && reachesBounds(guard->getSuccessor(fails ? 0 : 1)))
                    remark(source, receiver, BoundsCheck, *guard);
                guard->setCondition(ConstantInt::getBool(f.getContext(), !fails));
                if (compare->use_empty()) compare->eraseFromParent();
                ++removed;
                changed = true;
            }
            return changed;
        }
    };

    // Bounded counters (see the rule at the top of the file).
    struct Bounded {
        Function &f;
        DominatorTree &tree;
        LoopInfo &loops;

        // A check that fails exactly when `count` exceeds `limit`: a branch
        // into a failure on `count` compared with a constant, or an add with
        // overflow of a constant.
        struct Check {
            Instruction *site;
            Value *count;
            APInt limit;
        };
        std::vector<Check> checks;

        // An upper bound on count - index over the integers: `offset`, plus
        // `count - index` of one pair of values when they are set. Bottom is
        // no value yet, top is unbounded.
        struct Diff {
            enum Kind : uint8_t { Bottom, Known, Top } kind = Bottom;
            int64_t offset = 0;
            Value *count = nullptr, *index = nullptr;
            static Diff top() {
                Diff d;
                d.kind = Top;
                return d;
            }
            static Diff known(int64_t offset, Value *count = nullptr, Value *index = nullptr) {
                Diff d;
                d.kind = Known;
                d.offset = offset;
                d.count = count;
                d.index = index;
                return d;
            }
            bool operator==(const Diff &o) const {
                return kind == o.kind &&
                       (kind != Known || (offset == o.offset && count == o.count && index == o.index));
            }
            Diff plus(int64_t k) const {
                if (kind != Known) return *this;
                int64_t r;
                if (__builtin_add_overflow(offset, k, &r) || r > (int64_t(1) << 62) || r < -(int64_t(1) << 62))
                    return top();
                return known(r, count, index);
            }
            Diff join(const Diff &o) const {
                if (kind == Bottom) return o;
                if (o.kind == Bottom) return *this;
                if (kind == Top || o.kind == Top || count != o.count || index != o.index) return top();
                return known(std::max(offset, o.offset), count, index);
            }
        };
        // A pair with a phi in it is a variable of the fixed point. Mode 0:
        // both are phis of one block; 1: the count is a phi and the index is
        // available at its block; 2: the other way round.
        struct Var {
            Value *count, *index;
            unsigned mode;
            Diff value;
            unsigned changes = 0;
        };
        std::vector<Var> vars;
        DenseMap<std::pair<Value *, Value *>, unsigned> varIndex;

        using Fact = std::pair<Value *, bool>;
        using Facts = SmallVector<Fact, 8>;
        DenseMap<BasicBlock *, Facts> dominating;
        // Index phis optimistically below a bound, and those refuted.
        DenseMap<std::pair<Value *, Value *>, bool> below;
        std::vector<std::pair<PHINode *, Value *>> assumed;
        // Compile-time budget; once spent nothing more is proved.
        unsigned budget = 200000;

        Bounded(Function &f, DominatorTree &tree, LoopInfo &loops) : f(f), tree(tree), loops(loops) {}

        bool spend() {
            if (!budget) return false;
            --budget;
            return true;
        }

        static bool counter(Value *v) {
            return isa<Instruction>(v) && v->getType()->isIntegerTy() && v->getType()->getIntegerBitWidth() <= 64;
        }

        void collect() {
            using namespace PatternMatch;
            for (BasicBlock &block : f) {
                if (!loops.getLoopFor(&block) || !tree.isReachableFromEntry(&block)) continue;
                for (Instruction &i : block) {
                    auto *op = dyn_cast<WithOverflowInst>(&i);
                    const APInt *k;
                    if (!op || op->getIntrinsicID() != Intrinsic::uadd_with_overflow || !counter(op->getLHS()) ||
                        !match(op->getRHS(), m_APInt(k)))
                        continue;
                    bool used = any_of(op->users(), [](User *user) {
                        auto *flag = dyn_cast<ExtractValueInst>(user);
                        return flag && flag->getNumIndices() == 1 && flag->getIndices()[0] == 1;
                    });
                    if (used && !k->isZero())
                        checks.push_back({op, op->getLHS(), APInt::getMaxValue(k->getBitWidth()) - *k});
                }
                auto *branch = dyn_cast<CondBrInst>(block.getTerminator());
                auto *compare = branch ? dyn_cast<ICmpInst>(branch->getCondition()) : nullptr;
                if (!compare || !(isa<ConstantInt>(compare->getOperand(0)) || isa<ConstantInt>(compare->getOperand(1))))
                    continue;
                bool fails = reachesRaise(branch->getSuccessor(0));
                if (fails == reachesRaise(branch->getSuccessor(1))) continue;
                for (unsigned side = 0; side < 2; side++) {
                    Value *x = compare->getOperand(side);
                    const APInt *k;
                    if (!counter(x) || !match(compare->getOperand(1 - side), m_APInt(k))) continue;
                    CmpInst::Predicate predicate = fails ? compare->getPredicate() : compare->getInversePredicate();
                    if (side) predicate = CmpInst::getSwappedPredicate(predicate);
                    ConstantRange failing = ConstantRange::makeExactICmpRegion(predicate, *k);
                    if (failing.isEmptySet() || failing.getUnsignedMin().isZero()) continue;
                    checks.push_back({branch, x, failing.getUnsignedMin() - 1});
                }
            }
        }

        bool available(Value *v, BasicBlock *block) const {
            auto *i = dyn_cast<Instruction>(v);
            return !i || tree.properlyDominates(i->getParent(), block);
        }

        static void assumes(BasicBlock *block, Instruction *before, Facts &out) {
            for (Instruction &i : *block) {
                if (&i == before) break;
                if (auto *assume = dyn_cast<AssumeInst>(&i)) out.push_back({assume->getArgOperand(0), true});
            }
        }

        // Conditions that hold whenever `block` runs: the outcomes of the
        // branches whose edge dominates it, and the assumptions in the
        // blocks that dominate it.
        const Facts &dominated(BasicBlock *block) {
            auto [at, fresh] = dominating.try_emplace(block);
            if (!fresh) return at->second;
            Facts facts;
            DomTreeNode *node = tree.getNode(block);
            for (unsigned depth = 0; node && node->getIDom() && depth < 32; depth++) {
                BasicBlock *above = node->getIDom()->getBlock();
                assumes(above, nullptr, facts);
                if (auto *branch = dyn_cast<CondBrInst>(above->getTerminator());
                    branch && branch->getSuccessor(0) != branch->getSuccessor(1))
                    for (unsigned k = 0; k < 2; k++)
                        if (tree.dominates(BasicBlockEdge(above, branch->getSuccessor(k)), block))
                            facts.push_back({branch->getCondition(), k == 0});
                node = node->getIDom();
            }
            return dominating[block] = std::move(facts);
        }

        Facts at(Instruction *i) {
            Facts facts = dominated(i->getParent());
            assumes(i->getParent(), i, facts);
            return facts;
        }

        Facts edge(BasicBlock *from, BasicBlock *to) {
            Facts facts = dominated(from);
            assumes(from, nullptr, facts);
            if (auto *branch = dyn_cast<CondBrInst>(from->getTerminator());
                branch && branch->getSuccessor(0) != branch->getSuccessor(1))
                facts.push_back({branch->getCondition(), branch->getSuccessor(0) == to});
            return facts;
        }

        // Whether `condition` being `truth` gives `a predicate b` for one of
        // the unsigned predicates in `accepted`.
        static bool implies(Value *condition, bool truth, Value *a, Value *b,
                            std::initializer_list<CmpInst::Predicate> accepted, unsigned depth = 0) {
            using namespace PatternMatch;
            Value *l, *r;
            if (depth < 4 && (truth ? match(condition, m_LogicalAnd(m_Value(l), m_Value(r)))
                                    : match(condition, m_LogicalOr(m_Value(l), m_Value(r)))))
                return implies(l, truth, a, b, accepted, depth + 1) || implies(r, truth, a, b, accepted, depth + 1);
            auto *compare = dyn_cast<ICmpInst>(condition);
            if (!compare) return false;
            CmpInst::Predicate predicate = truth ? compare->getPredicate() : compare->getInversePredicate();
            Value *x = compare->getOperand(0), *y = compare->getOperand(1);
            if (x == b && y == a) predicate = CmpInst::getSwappedPredicate(predicate);
            else if (x != a || y != b) return false;
            return is_contained(accepted, predicate);
        }

        static bool holds(const Facts &facts, Value *a, Value *b, std::initializer_list<CmpInst::Predicate> accepted) {
            for (auto [condition, truth] : facts)
                if (implies(condition, truth, a, b, accepted)) return true;
            return false;
        }

        static bool nonzero(Value *n, const Facts &facts) {
            if (auto *c = dyn_cast<ConstantInt>(n)) return !c->isZero();
            Value *zero = ConstantInt::get(n->getType(), 0);
            if (holds(facts, n, zero, {CmpInst::ICMP_NE, CmpInst::ICMP_UGT})) return true;
            // x < n for any x.
            for (auto [condition, truth] : facts)
                if (auto *compare = dyn_cast<ICmpInst>(condition))
                    for (unsigned side = 0; side < 2; side++)
                        if (compare->getOperand(1 - side) == n &&
                            implies(condition, truth, compare->getOperand(side), n, {CmpInst::ICMP_ULT}))
                            return true;
            return false;
        }

        // Whether `v < n` (unsigned) wherever `facts` hold.
        bool lt(Value *v, Value *n, const Facts &facts, unsigned depth) {
            if (depth > 6 || !spend()) return false;
            if (auto *c = dyn_cast<ConstantInt>(v)) {
                if (auto *m = dyn_cast<ConstantInt>(n)) return c->getValue().ult(m->getValue());
                if (c->isZero() && nonzero(n, facts)) return true;
            }
            if (holds(facts, v, n, {CmpInst::ICMP_ULT})) return true;
            if (auto *phi = dyn_cast<PHINode>(v); phi && phiBelow(phi, n)) return true;
            // v != n and v <= n.
            if (!holds(facts, v, n, {CmpInst::ICMP_NE})) return false;
            if (holds(facts, v, n, {CmpInst::ICMP_ULE})) return true;
            using namespace PatternMatch;
            Value *x;
            auto *add = dyn_cast<Instruction>(v);
            return add && match(v, m_Add(m_Value(x), m_One())) && lt(x, n, at(add), depth + 1);
        }

        // Every value the phi receives is below `n`: a coinductive
        // assumption, which `verify` withdraws when an edge refutes it.
        bool phiBelow(PHINode *phi, Value *n) {
            if (!available(n, phi->getParent())) return false;
            auto [at, fresh] = below.try_emplace({phi, n}, true);
            if (!fresh) return at->second;
            assumed.push_back({phi, n});
            if (edgesBelow(phi, n)) return true;
            below[{phi, n}] = false;
            return false;
        }

        bool edgesBelow(PHINode *phi, Value *n) {
            for (unsigned k = 0; k < phi->getNumIncomingValues(); k++) {
                BasicBlock *from = phi->getIncomingBlock(k);
                if (tree.isReachableFromEntry(from) &&
                    !lt(phi->getIncomingValue(k), n, edge(from, phi->getParent()), 0))
                    return false;
            }
            return true;
        }

        bool verify() {
            bool withdrawn = false;
            for (bool again = true; again;) {
                again = false;
                for (size_t k = 0; k < assumed.size(); k++) {
                    auto [phi, n] = assumed[k];
                    if (!below.lookup({phi, n}) || edgesBelow(phi, n)) continue;
                    below[{phi, n}] = false;
                    again = withdrawn = true;
                }
            }
            return withdrawn;
        }

        // Whether `index < n` wherever `site` runs.
        bool indexBelow(Value *index, Value *n, Instruction *site) {
            for (unsigned round = 0; round < 16 && budget; round++) {
                size_t before = assumed.size();
                bool result = lt(index, n, at(site), 0);
                if (!verify() && assumed.size() == before) return result;
            }
            return false;
        }

        // The values `index` is compared with, directly, as an incoming
        // value of a phi, or plus one.
        SmallVector<Value *, 4> bounds(Value *index) {
            using namespace PatternMatch;
            SmallVector<Value *, 8> sources{index};
            if (isa<Constant>(index)) return {};
            if (auto *phi = dyn_cast<PHINode>(index))
                for (Value *v : phi->incoming_values()) sources.push_back(v);
            for (User *user : index->users())
                if (match(user, m_Add(m_Specific(index), m_One()))) sources.push_back(user);
            SmallVector<Value *, 4> out;
            for (Value *source : sources) {
                // A constant has no list of users.
                if (isa<Constant>(source)) continue;
                for (User *user : source->users())
                    if (auto *compare = dyn_cast<ICmpInst>(user); compare && out.size() < 6)
                        for (Value *other : compare->operands())
                            if (other != source && other != index && other->getType() == index->getType() &&
                                !is_contained(out, other))
                                out.push_back(other);
            }
            return out;
        }

        // `x < n` at `site` for a bound `n` of x, so `x + 1` cannot wrap.
        bool belowAny(Value *x, Instruction *site) {
            for (Value *n : bounds(x))
                if (indexBelow(x, n, site)) return true;
            return false;
        }

        Diff var(Value *count, Value *index, unsigned mode) {
            auto [at, fresh] = varIndex.try_emplace({count, index}, vars.size());
            if (fresh) vars.push_back({count, index, mode, Diff()});
            return vars[at->second].value;
        }

        Diff diff(Value *count, Value *index, unsigned depth) {
            using namespace PatternMatch;
            if (depth > 32 || !spend()) return Diff::top();
            const uint64_t big = uint64_t(1) << 62;
            auto *c = dyn_cast<ConstantInt>(count), *i = dyn_cast<ConstantInt>(index);
            if (c && i && c->getValue().ule(big) && i->getValue().ule(big))
                return Diff::known(int64_t(c->getZExtValue()) - int64_t(i->getZExtValue()));
            // The count steps up: a wrapped sum is below the unwrapped one.
            Value *x, *b;
            const APInt *k;
            if (match(count, m_Add(m_Value(x), m_APInt(k))) && k->ule(big))
                return diff(x, index, depth + 1).plus(int64_t(k->getZExtValue()));
            if (match(count, m_c_Add(m_Value(x), m_ZExt(m_Value(b)))) && b->getType()->isIntegerTy(1))
                return diff(x, index, depth + 1).plus(1);
            if (auto *extract = dyn_cast<ExtractValueInst>(count);
                extract && extract->getNumIndices() == 1 && extract->getIndices()[0] == 0)
                if (auto *op = dyn_cast<WithOverflowInst>(extract->getAggregateOperand());
                    op && op->getBinaryOp() == Instruction::Add && match(op->getRHS(), m_APInt(k)) &&
                    k->ule(big))
                    return diff(op->getLHS(), index, depth + 1).plus(int64_t(k->getZExtValue()));
            if (auto *select = dyn_cast<SelectInst>(count))
                return diff(select->getTrueValue(), index, depth + 1)
                    .join(diff(select->getFalseValue(), index, depth + 1));
            // The index steps up by exactly k: it must not wrap.
            if (auto *add = dyn_cast<BinaryOperator>(index);
                add && match(add, m_Add(m_Value(x), m_APInt(k))) && !k->isZero() && k->ule(big)) {
                if (add->hasNoUnsignedWrap() || (k->isOne() && belowAny(x, add)))
                    return diff(count, x, depth + 1).plus(-int64_t(k->getZExtValue()));
                return Diff::top();
            }
            auto *pc = dyn_cast<PHINode>(count), *pi = dyn_cast<PHINode>(index);
            if (pc && pi && pc->getParent() == pi->getParent()) return var(count, index, 0);
            if (pc && available(index, pc->getParent())) return var(count, index, 1);
            if (pi && available(count, pi->getParent())) return var(count, index, 2);
            return Diff::known(0, count, index);
        }

        Diff compute(unsigned k) {
            Value *count = vars[k].count, *index = vars[k].index;
            unsigned mode = vars[k].mode;
            auto *phi = cast<PHINode>(mode == 2 ? index : count);
            Diff result;
            for (unsigned j = 0; j < phi->getNumIncomingValues() && result.kind != Diff::Top; j++) {
                BasicBlock *from = phi->getIncomingBlock(j);
                if (!tree.isReachableFromEntry(from)) continue;
                Value *c = mode == 2 ? count : cast<PHINode>(count)->getIncomingValueForBlock(from);
                Value *i = mode == 1 ? index : cast<PHINode>(index)->getIncomingValueForBlock(from);
                Diff d = diff(c, i, 0);
                // A loop's exit edge tests its index against the value the
                // pair holds: either one bounds the difference.
                if (Value *same = equal(from, phi->getParent(), i)) d = tighter(d, diff(c, same, 0));
                result = result.join(d);
            }
            return result;
        }

        // A value equal to `v` on the edge, by the branch taken there.
        static Value *equal(BasicBlock *from, BasicBlock *to, Value *v) {
            auto *branch = dyn_cast<CondBrInst>(from->getTerminator());
            if (!branch || branch->getSuccessor(0) == branch->getSuccessor(1)) return nullptr;
            auto *compare = dyn_cast<ICmpInst>(branch->getCondition());
            if (!compare) return nullptr;
            bool truth = branch->getSuccessor(0) == to;
            CmpInst::Predicate predicate = truth ? compare->getPredicate() : compare->getInversePredicate();
            if (predicate != CmpInst::ICMP_EQ) return nullptr;
            if (compare->getOperand(0) == v) return compare->getOperand(1);
            if (compare->getOperand(1) == v) return compare->getOperand(0);
            return nullptr;
        }

        // Either bound holds; a constant one joins with more of the others.
        static Diff tighter(const Diff &a, const Diff &b) {
            if (a.kind == Diff::Bottom || b.kind == Diff::Bottom) return Diff();
            if (a.kind == Diff::Top) return b;
            if (b.kind == Diff::Top) return a;
            if (a.count == b.count && a.index == b.index) return a.offset <= b.offset ? a : b;
            return a.count ? b : a;
        }

        // The least bound the fixed point gives `count - index`; a variable
        // that grows a fifth time is unbounded.
        Diff query(Value *count, Value *index) {
            for (unsigned round = 0; round < 64; round++) {
                size_t before = vars.size();
                Diff result = diff(count, index, 0);
                bool changed = false;
                for (unsigned k = 0; k < vars.size(); k++) {
                    Diff computed = compute(k);
                    Diff next = vars[k].value.join(computed);
                    if (next == vars[k].value) continue;
                    if (++vars[k].changes > 4) next = Diff::top();
                    vars[k].value = next;
                    changed = true;
                }
                if (!budget) return Diff::top();
                if (!changed && vars.size() == before) return result;
            }
            return Diff::top();
        }

        // `count <= n - 1 + bound` at the check, for an index below `n`.
        struct Plan {
            Check check;
            Value *n;
            Diff bound;
        };

        std::optional<Plan> plan(const Check &check) {
            SmallVector<Value *, 8> candidates;
            auto add = [&](PHINode &phi) {
                if (&phi != check.count && counter(&phi) && !is_contained(candidates, &phi))
                    candidates.push_back(&phi);
            };
            if (auto *phi = dyn_cast<PHINode>(check.count))
                for (PHINode &other : phi->getParent()->phis()) add(other);
            if (Loop *loop = loops.getLoopFor(check.site->getParent()))
                for (PHINode &other : loop->getHeader()->phis()) add(other);
            for (Value *index : candidates)
                for (Value *n : bounds(index)) {
                    if (!budget) return std::nullopt;
                    if (!indexBelow(index, n, check.site)) continue;
                    Diff bound = query(check.count, index);
                    if (bound.kind != Diff::Known) break;
                    return Plan{check, n, bound};
                }
            return std::nullopt;
        }

        // The check passes when n + offset + (count - index) <= limit + 1.
        // Returns the constant threshold for `n` when the bound has no pair.
        static std::optional<__int128> threshold(const Plan &plan) {
            if (plan.bound.count) {
                auto *c = dyn_cast<ConstantInt>(plan.bound.count), *i = dyn_cast<ConstantInt>(plan.bound.index);
                if (!c || !i) return std::nullopt;
                return __int128(plan.check.limit.getZExtValue()) + 1 - plan.bound.offset -
                       __int128(c->getZExtValue()) + __int128(i->getZExtValue());
            }
            return __int128(plan.check.limit.getZExtValue()) + 1 - plan.bound.offset;
        }

        // True when the plan's condition holds for every value of its operands.
        bool always(const Plan &plan) const {
            auto t = threshold(plan);
            if (!t || *t < 0) return false;
            APInt most = computeConstantRange(plan.n, false, SimplifyQuery(f.getDataLayout())).getUnsignedMax();
            return __int128(most.getZExtValue()) <= *t;
        }

        static Value *condition(IRBuilder<> &builder, const Plan &plan) {
            Value *n = plan.n;
            unsigned width = n->getType()->getIntegerBitWidth();
            if (auto t = threshold(plan)) {
                if (*t < 0) return builder.getFalse();
                if (*t >= (__int128(1) << width) - 1) return builder.getTrue();
                return builder.CreateICmpULE(n, ConstantInt::get(n->getType(), uint64_t(*t)));
            }
            Type *wide = builder.getInt128Ty();
            Value *sum = builder.CreateAdd(builder.CreateZExt(n, wide),
                                           ConstantInt::getSigned(wide, plan.bound.offset));
            sum = builder.CreateAdd(sum, builder.CreateZExt(plan.bound.count, wide));
            sum = builder.CreateSub(sum, builder.CreateZExt(plan.bound.index, wide));
            Constant *ceiling = ConstantInt::get(wide, plan.check.limit.zext(128) + 1);
            return builder.CreateICmpSLE(sum, ceiling);
        }

        // The outermost loop around the check that has a preheader and
        // leaves the condition's operands invariant.
        Loop *versioned(const Plan &plan) const {
            Loop *chosen = nullptr;
            for (Loop *loop = loops.getLoopFor(plan.check.site->getParent()); loop; loop = loop->getParentLoop()) {
                auto invariant = [&](Value *v) {
                    auto *i = dyn_cast_or_null<Instruction>(v);
                    return !i || (!loop->contains(i) && tree.dominates(i, loop->getHeader()));
                };
                if (!invariant(plan.n) || !invariant(plan.bound.count) || !invariant(plan.bound.index)) break;
                if (loop->getLoopPreheader()) chosen = loop;
            }
            return chosen;
        }

        // Whether the loop calls nothing but intrinsics and failures.
        static bool callFree(const Loop &loop) {
            for (BasicBlock *block : loop.blocks())
                for (Instruction &i : *block)
                    if (auto *call = dyn_cast<CallBase>(&i);
                        call && !isa<IntrinsicInst>(call) && !call->doesNotReturn())
                        return false;
            return true;
        }

        // A copy of the loop must behave like the loop.
        static bool clonable(Loop &loop) {
            unsigned size = 0;
            for (BasicBlock *block : loop.blocks()) {
                if (block->isEHPad() || !isa<UncondBrInst, CondBrInst, SwitchInst, ReturnInst, UnreachableInst>(
                                            block->getTerminator()))
                    return false;
                for (Instruction &i : *block) {
                    if (++size > 4000 || i.getType()->isTokenTy()) return false;
                    if (auto *call = dyn_cast<CallBase>(&i); call && (call->cannotDuplicate() || call->isConvergent()))
                        return false;
                }
            }
            SmallVector<BasicBlock *, 8> exits;
            loop.getUniqueExitBlocks(exits);
            return none_of(exits, [](BasicBlock *exit) { return exit->isEHPad(); });
        }
    };

    // The check can never fail: an overflow flag becomes false, and a
    // branch into a failure takes its other edge. Returns the checks it
    // removed.
    static unsigned discharge(Function &f, Instruction *site, SourceRemark source, void *receiver) {
        if (auto *op = dyn_cast<WithOverflowInst>(site)) {
            unsigned removed = 0;
            for (User *user : make_early_inc_range(op->users())) {
                auto *flag = dyn_cast<ExtractValueInst>(user);
                if (!flag || flag->getNumIndices() != 1 || flag->getIndices()[0] != 1) continue;
                if (!flag->use_empty()) {
                    ++removed;
                    remark(source, receiver, OverflowCheck, *op);
                }
                flag->replaceAllUsesWith(ConstantInt::getFalse(f.getContext()));
                flag->eraseFromParent();
            }
            return removed;
        }
        auto *guard = cast<CondBrInst>(site);
        auto *compare = dyn_cast<ICmpInst>(guard->getCondition());
        if (!compare) return 0;
        bool fails = reachesRaise(guard->getSuccessor(0));
        if (source && reachesBounds(guard->getSuccessor(fails ? 0 : 1)))
            remark(source, receiver, BoundsCheck, *guard);
        guard->setCondition(ConstantInt::getBool(f.getContext(), !fails));
        if (compare->use_empty()) compare->eraseFromParent();
        return 1;
    }

    // Removes the bounded counters' checks: those whose condition always
    // holds, and, when versioning, those whose condition a loop can test
    // once at its preheader. Returns whether the control flow changed.
    bool bound(Function &f, LoopInfo &loops, DominatorTree &tree, FunctionAnalysisManager &analyses,
               unsigned &removed, bool &changed) const {
        Bounded bounded(f, tree, loops);
        bounded.collect();
        if (bounded.checks.empty()) return false;
        MapVector<Loop *, std::vector<Bounded::Plan>> groups;
        SmallVector<Instruction *, 8> proved;
        for (const Bounded::Check &check : bounded.checks) {
            auto plan = bounded.plan(check);
            if (!plan) continue;
            if (auto t = Bounded::threshold(*plan); t && *t < 0) continue;
            Loop *inner = loops.getLoopFor(check.site->getParent());
            if (!inner || !Bounded::callFree(*inner)) continue;
            if (bounded.always(*plan)) proved.push_back(check.site);
            else if (version)
                if (Loop *loop = bounded.versioned(*plan); loop && Bounded::callFree(*loop))
                    groups[loop].push_back(*plan);
        }
        // The facts the plans read stay intact until every plan is made.
        for (Instruction *site : proved) {
            removed += discharge(f, site, source, receiver);
            changed = true;
        }
        if (groups.empty()) return false;
        // A loop inside another chosen loop is versioned with it or not at all.
        SmallVector<Loop *, 4> chosen;
        for (auto &[loop, plans] : groups) {
            bool nested = false;
            for (auto &[other, _] : groups)
                if (other != loop && other->contains(loop)) nested = true;
            if (!nested && Bounded::clonable(*loop)) chosen.push_back(loop);
        }
        if (chosen.empty()) return false;
        // Every condition is in place before any loop is copied, so a later
        // copy rewrites its uses through its exits like any other.
        SmallVector<std::pair<Loop *, Value *>, 4> safe;
        for (Loop *loop : chosen) {
            IRBuilder<> builder(loop->getLoopPreheader()->getTerminator());
            Value *all = builder.getTrue();
            for (const Bounded::Plan &plan : groups[loop])
                all = builder.CreateAnd(all, Bounded::condition(builder, plan));
            safe.push_back({loop, builder.CreateFreeze(all, "counter.safe")});
        }
        ScalarEvolution &evolution = analyses.getResult<ScalarEvolutionAnalysis>(f);
        for (auto [loop, condition] : safe) {
            versionLoop(*loop, condition, loops, tree, evolution, "counter.fast.ph", ".counter.checked");
            // The loop itself is the fast copy. Its failure edges go, which
            // leaves the loops as they are; the copy moved exits' dominators.
            for (const Bounded::Plan &plan : groups[loop]) {
                BasicBlock *block = plan.check.site->getParent();
                removed += discharge(f, plan.check.site, source, receiver);
                ConstantFoldTerminator(block, true);
            }
            tree.recalculate(f);
        }
        changed = true;
        return true;
    }
};
} // namespace hirsel_llvm
