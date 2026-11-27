#include <version>
#ifdef __GLIBC__
#if !__GLIBC_PREREQ(2, 30)
#undef _GLIBCXX_USE_PTHREAD_COND_CLOCKWAIT
#undef _GLIBCXX_USE_PTHREAD_MUTEX_CLOCKLOCK
#undef _GLIBCXX_USE_PTHREAD_RWLOCK_CLOCKLOCK
#endif
#endif

#include "counted_exit.h"
#include "counter_range.h"
#include "guarded_check.h"
#include "implied_check.h"
#include "pass_cost.h"

#include <llvm-c/Core.h>
#include <llvm-c/TargetMachine.h>
#include <llvm/ADT/MapVector.h>
#include <llvm/Analysis/CFG.h>
#include <llvm/Analysis/CGSCCPassManager.h>
#include <llvm/Analysis/LoopInfo.h>
#include <llvm/Analysis/ValueTracking.h>
#include <llvm/IR/Dominators.h>
#include <llvm/IR/InstIterator.h>
#include <llvm/IR/Instructions.h>
#include <llvm/IR/Module.h>
#include <llvm/IR/ValueHandle.h>
#include <llvm/Bitcode/BitcodeReader.h>
#include <llvm/Linker/Linker.h>
#include <llvm/Support/MemoryBuffer.h>
#include <llvm/Target/TargetMachine.h>
#include <llvm/Passes/PassBuilder.h>
#include <llvm/Passes/StandardInstrumentations.h>
#include <llvm/Transforms/IPO/GlobalDCE.h>
#include <llvm/Transforms/IPO/Inliner.h>
#include <llvm/Analysis/AssumptionCache.h>
#include <llvm/Analysis/CaptureTracking.h>
#include <llvm/IR/MDBuilder.h>
#include <llvm/Analysis/OptimizationRemarkEmitter.h>
#include <llvm/Analysis/ScalarEvolution.h>
#include <llvm/Analysis/ScalarEvolutionExpressions.h>
#include <llvm/Analysis/TargetTransformInfo.h>
#include <llvm/Transforms/InstCombine/InstCombine.h>
#include <llvm/Transforms/Scalar/LICM.h>
#include <llvm/Transforms/Scalar/LoopPassManager.h>
#include <llvm/Transforms/Scalar/LoopUnrollPass.h>
#include <llvm/Transforms/Utils/LoopPeel.h>
#include <llvm/Transforms/Utils/LoopSimplify.h>
#include <llvm/Transforms/Utils/LoopUtils.h>
#include <llvm/Transforms/Utils/UnrollLoop.h>
#include <llvm/Transforms/Scalar/SROA.h>
#include <llvm/Transforms/Scalar/SimpleLoopUnswitch.h>
#include <llvm/Transforms/Utils/Cloning.h>
#include <llvm/Transforms/Utils/ExtraPassManager.h>
#include <llvm/Transforms/Vectorize/VectorCombine.h>

using namespace llvm;

extern "C" void hirsel_llvm_prepay_step_trees(void *module);
extern "C" void hirsel_llvm_note_tail_change(LLVMValueRef function);

namespace {

void limit_recursive_entry_inlining(Module &module) {
    for (Function &function : module) {
        if (function.isDeclaration()) continue;
        unsigned self_calls = 0;
        SmallVector<CallBase *, 4> entries;
        for (Use &use : function.uses()) {
            auto *call = dyn_cast<CallBase>(use.getUser());
            if (!call || !call->isCallee(&use)) continue;
            if (call->getCaller() == &function)
                ++self_calls;
            else
                entries.push_back(call);
        }
        // An outside caller would copy the body and all four recursive edges.
        // The recursive edges remain eligible for leaf specialization.
        if (self_calls < 4) continue;
        for (CallBase *call : entries) {
            if (!call->hasFnAttr(Attribute::AlwaysInline)) call->addFnAttr(Attribute::NoInline);
        }
    }
}

// Specialize the home group before inlining. Its group number and candidate
// mask can then simplify independently of the loop's packed bucket recurrence.
class MapHomeGroupPass : public OptionalPassInfoMixin<MapHomeGroupPass> {
public:
    PreservedAnalyses run(Function &f, FunctionAnalysisManager &analyses) {
        if (!f.hasFnAttribute("hirsel-map-probe")) return PreservedAnalyses::all();
        f.removeFnAttr("hirsel-map-probe");
        auto &dt = analyses.getResult<DominatorTreeAnalysis>(f);
        auto &loops = analyses.getResult<LoopAnalysis>(f);
        auto &assumptions = analyses.getResult<AssumptionAnalysis>(f);
        auto &evolution = analyses.getResult<ScalarEvolutionAnalysis>(f);
        if (loops.getTopLevelLoops().size() == 1) {
            Loop *probe = *loops.begin();
            simplifyLoop(probe, &dt, &loops, &evolution, &assumptions, nullptr, false);
            if (canPeel(probe)) {
                formLCSSARecursively(*probe, dt, &loops, &evolution);
                ValueToValueMapTy home;
                peelLoop(probe, 1, false, &loops, &evolution, dt, &assumptions, true, home);
            }
        }
        return PreservedAnalyses::none();
    }
};

// A library's lent bodies (hirsel_llvm_emit_parts) join the module for its
// inliner: each body a definition calls, and those they call, comes in
// `available_externally`. Its own passes do not run over it, the inliner may
// copy it into a caller, and `elim-avail-extern` then drops it, so each call
// left reaches the library's part. Keep them through the late inliners and
// drop them before per-definition cleanup and emission.
class LendPass : public PassInfoMixin<LendPass> {
    StringRef lent;
    std::string *failure;

public:
    LendPass(StringRef lent, std::string *failure) : lent(lent), failure(failure) {}
    PreservedAnalyses run(Module &m, ModuleAnalysisManager &) {
        auto library = getLazyBitcodeModule(MemoryBufferRef(lent, "hirsel-lent"), m.getContext());
        if (!library) {
            *failure = toString(library.takeError());
            return PreservedAnalyses::all();
        }
        if (Linker::linkModules(m, std::move(*library), Linker::LinkOnlyNeeded))
            *failure = "the lent bodies do not link";
        limit_recursive_entry_inlining(m);
        return PreservedAnalyses::none();
    }
    static bool isRequired() { return true; }
};

// Whether a pass is to run over `ir`: never over only lent bodies.
bool unlent(Any ir) {
    if (const auto *f = llvm::any_cast<const Function *>(&ir))
        return !(*f)->hasAvailableExternallyLinkage();
    if (const auto *scc = llvm::any_cast<const LazyCallGraph::SCC *>(&ir)) {
        for (const LazyCallGraph::Node &n : **scc)
            if (!n.getFunction().hasAvailableExternallyLinkage()) return true;
        return false;
    }
    if (const auto *loop = llvm::any_cast<const Loop *>(&ir))
        return !(*loop)->getHeader()->getParent()->hasAvailableExternallyLinkage();
    return true;
}

class ReleasePipeline {
    Module &module;
    PassInstrumentationCallbacks callbacks;
    LoopAnalysisManager loops;
    FunctionAnalysisManager functions;
    CGSCCAnalysisManager calls;
    ModuleAnalysisManager modules;
    StandardInstrumentations instrumentation;

public:
    PassBuilder builder;

    ReleasePipeline(LLVMModuleRef raw_module, LLVMTargetMachineRef raw_machine)
        : module(*unwrap(raw_module)), instrumentation(module.getContext(), /*DebugLogging=*/false),
          builder(reinterpret_cast<TargetMachine *>(raw_machine), PipelineTuningOptions(),
                  std::nullopt, &callbacks) {
        builder.registerLoopAnalyses(loops);
        builder.registerFunctionAnalyses(functions);
        builder.registerCGSCCAnalyses(calls);
        builder.registerModuleAnalyses(modules);
        builder.crossRegisterProxies(loops, functions, calls, modules);
        instrumentation.registerCallbacks(callbacks, &modules);
    }

    // Lent bodies keep the code their library's optimizer made.
    void skip_lent() {
        callbacks.registerShouldRunOptionalPassCallback([](StringRef, Any ir) { return unlent(ir); });
    }

    void track_inlining() {
        // InlinerPass erases the call on success. A WeakVH observes deletion
        // even when the returned value replaces the call's uses. A deleted
        // caller can lose its whole body and has no tail to run.
        struct Sites {
            WeakVH caller;
            SmallVector<WeakVH, 4> calls;
        };
        auto sites = std::make_shared<SmallVector<Sites, 4>>();
        callbacks.registerBeforeNonSkippedPassCallback([sites](StringRef pass, Any ir) {
            if (pass != InlinerPass::name()) return;
            for (const auto &node : *any_cast<const LazyCallGraph::SCC *>(ir)) {
                Function &function = node.getFunction();
                Sites &entry = sites->emplace_back(Sites{&function, {}});
                for (Instruction &instruction : instructions(function))
                    if (auto *call = dyn_cast<CallBase>(&instruction))
                        if (auto *callee = call->getCalledFunction(); callee && !callee->isDeclaration())
                            entry.calls.emplace_back(call);
            }
        });
        auto finish = [sites](StringRef pass) {
            if (pass != InlinerPass::name()) return;
            for (const auto &entry : *sites)
                if (entry.caller && any_of(entry.calls, [](const WeakVH &call) { return !call; }))
                    hirsel_llvm_note_tail_change(wrap(static_cast<Value *>(entry.caller)));
            sites->clear();
        };
        callbacks.registerAfterPassCallback(
            [finish](StringRef pass, Any, const PreservedAnalyses &) { finish(pass); });
        callbacks.registerAfterPassInvalidatedCallback(
            [finish](StringRef pass, const PreservedAnalyses &) { finish(pass); });
    }

    void run(ModulePassManager &pipeline, StringRef phase) {
        hirsel_llvm::PassCost cost(phase, module);
        cost.registerCallbacks(callbacks);
        pipeline.run(module, modules);
    }
};

LLVMErrorRef runPasses(LLVMModuleRef module, LLVMTargetMachineRef machine,
                       StringRef phase, StringRef pipeline_text) {
    ReleasePipeline release(module, machine);
    ModulePassManager pipeline;
    if (auto error = release.builder.parsePassPipeline(pipeline, pipeline_text))
        return wrap(std::move(error));
    release.run(pipeline, phase);
    return LLVMErrorSuccess;
}

// HotColdSplit outlines each growth arm (a `cold` call into a body this
// module defines: list/grow and its carrier twin) before the inliner, so every
// inlining decision prices the hot body. Once the last inlining pass is done, an
// arm goes back into its caller when the sequence it fills is held behind a
// pointer the caller does not own, such as an element of a heap array: the loop
// then shares the header's loads with the arm instead of reloading them past an
// outlined call (awfy/Havlak's identify_edges, 2% of its instructions). An arm
// whose sequence is the caller's own stack slot stays outlined: the loop reaches
// that header through memory either way, and the returned arm only adds register
// pressure to the loop (langarena Maze's queue, Compress's output buffers). The
// header is the parameter the outline both loads and stores back. A returned arm
// keeps its 2000:1 edge and its `cold` call for block placement.
class GrowthArmsBackPass : public OptionalPassInfoMixin<GrowthArmsBackPass> {
public:
    PreservedAnalyses run(Module &module, ModuleAnalysisManager &) {
        // Each arm's header parameter, arms in module order.
        MapVector<Function *, unsigned> arms;
        for (Function &f : module) {
            if (f.isDeclaration() || !f.getName().contains(".cold.")) continue;
            bool growth = false;
            SmallPtrSet<Value *, 4> loaded, stored;
            for (BasicBlock &block : f)
                for (Instruction &i : block) {
                    if (auto *call = dyn_cast<CallBase>(&i))
                        if (call->hasFnAttr(Attribute::Cold) && call->getCalledFunction() &&
                            !call->getCalledFunction()->isDeclaration())
                            growth = true;
                    if (auto *load = dyn_cast<LoadInst>(&i)) loaded.insert(load->getPointerOperand());
                    if (auto *store = dyn_cast<StoreInst>(&i)) stored.insert(store->getPointerOperand());
                }
            if (!growth) continue;
            int header = -1;
            for (Argument &arg : f.args())
                if (loaded.contains(&arg) && stored.contains(&arg)) header = arg.getArgNo();
            if (header < 0) continue;
            arms[&f] = header;
        }
        // Each arm's calls in module order, not its use list's: a body restored
        // from the release tail cache lists its uses in another order than one
        // optimized in place, and the order the arms go back in is the order
        // the caller's context lists its new uses, which a part's key records.
        MapVector<Function *, SmallVector<CallBase *, 4>> calls;
        for (auto &[arm, header] : arms) calls[arm];
        for (Function &f : module)
            for (BasicBlock &block : f)
                for (Instruction &i : block)
                    if (auto *call = dyn_cast<CallBase>(&i)) {
                        auto arm = arms.find(call->getCalledFunction());
                        if (arm != arms.end() &&
                            !isa<AllocaInst>(getUnderlyingObject(call->getArgOperand(arm->second))))
                            calls[arm->first].push_back(call);
                    }
        SmallVector<CallBase *> back;
        for (auto &[arm, sites] : calls) back.append(sites.begin(), sites.end());
        for (CallBase *call : back) {
            InlineFunctionInfo info;
            InlineFunction(*call, info, /*MergeAttributes=*/false, /*CalleeAAR=*/nullptr,
                           /*InsertLifetime=*/false);
        }
        return back.empty() ? PreservedAnalyses::all() : PreservedAnalyses::none();
    }
};

class PrepayStepTreesPass : public OptionalPassInfoMixin<PrepayStepTreesPass> {
public:
    PreservedAnalyses run(Module &module, ModuleAnalysisManager &) {
        hirsel_llvm_prepay_step_trees(&module);
        return PreservedAnalyses::none();
    }
};

// Nontrivial unswitching prices a loop-invariant switch at the loop body its
// arms share, times one clone per extra successor. A pure value computed above
// the switch and used only inside some arms counts as shared, so a seven-way
// match over one byte stayed unswitched though each clone keeps one small arm
// (langarena LogParser's `candidate` folds the byte's case before its match on
// the pattern: 2.19G instructions, 1.81G once unswitched). Each such value is
// recomputed at the head of every arm that uses it instead: the shared body is
// then what every arm really runs, and an arm that never uses the value never
// computes it. Values with any other user, memory access or side effect stay.
class InvariantArmSinkPass : public OptionalPassInfoMixin<InvariantArmSinkPass> {
public:
    PreservedAnalyses run(Function &f, FunctionAnalysisManager &analyses) {
        LoopInfo &loops = analyses.getResult<LoopAnalysis>(f);
        DominatorTree &tree = analyses.getResult<DominatorTreeAnalysis>(f);
        bool changed = false;
        for (BasicBlock &block : f) {
            auto *choice = dyn_cast<SwitchInst>(block.getTerminator());
            Loop *loop = loops.getLoopFor(&block);
            if (!choice || !loop || !loop->isLoopInvariant(choice->getCondition())) continue;
            SmallVector<BasicBlock *, 8> arms;
            for (BasicBlock *arm : successors(&block))
                if (arm->getUniquePredecessor() == &block && loop->contains(arm) &&
                    !is_contained(arms, arm))
                    arms.push_back(arm);
            for (Instruction &value : make_early_inc_range(reverse(block))) {
                if (value.isTerminator() || isa<PHINode>(value) || value.mayReadOrWriteMemory() ||
                    value.mayHaveSideEffects() || value.use_empty())
                    continue;
                // The arm each use lies in, or none when a use is shared.
                SmallVector<std::pair<Use *, BasicBlock *>, 8> uses;
                for (Use &use : value.uses()) {
                    auto *user = cast<Instruction>(use.getUser());
                    BasicBlock *at = user->getParent();
                    if (auto *phi = dyn_cast<PHINode>(user)) at = phi->getIncomingBlock(use);
                    BasicBlock *home = nullptr;
                    for (BasicBlock *arm : arms)
                        if (tree.dominates(arm, at)) home = arm;
                    if (!home) break;
                    uses.push_back({&use, home});
                }
                if (uses.size() != value.getNumUses()) continue;
                SmallDenseMap<BasicBlock *, Instruction *, 8> copies;
                for (auto [use, home] : uses) {
                    Instruction *&copy = copies[home];
                    if (!copy) {
                        copy = value.clone();
                        copy->setName(value.getName());
                        copy->insertBefore(home->getFirstInsertionPt());
                    }
                    use->set(copy);
                }
                value.eraseFromParent();
                changed = true;
            }
        }
        if (!changed) return PreservedAnalyses::all();
        PreservedAnalyses kept;
        kept.preserveSet<CFGAnalyses>();
        return kept;
    }
};

// A loop that keeps C running values in a stack array and updates the one its
// counter selects modulo C, C a power of two, runs C copies of its body per
// iteration instead (langarena Hash::SHA256 updates `hashes[at % 8]` per byte:
// 20.9G instructions, as Rust's; Goose strip-mines the loop by 8 in its own
// front end and runs 5.1G). The loop vectorizer cannot widen the original
// loop, since lanes 8 apart write the same cell, and the SLP vectorizer sees
// one cell per iteration. In copy j the index is `(counter + j) & (C - 1)`
// with the counter a multiple of C, so it folds to a constant; the cells then
// stay in registers across iterations, and SLP packs the C independent
// updates into vectors.
//
// The rule: an innermost loop whose only exit is its latch and which makes no
// calls, where every load or store of a static alloca indexes it by `and (P +
// k), C - 1`, C in [2, 16] and the same for each alloca, P a header phi
// counting up by 1 from a constant that is a multiple of C. Its body has at
// most 64 instructions and at most 320 once copied. LLVM's unroller makes the
// copies with a remainder loop after them for the last trip count modulo C
// iterations, which keeps the main loop's counter a multiple of C. Before
// that, when the cells provably cannot alias the loop's other accesses,
// `separateCells` records it in alias scopes so LICM can keep the cells in
// registers. The function then takes one instcombine and LICM pass, and the
// loop is not unrolled again. This adds no semantics: every check that remains stays in
// each copy with its site and cause.
class MaskedCellUnrollPass : public OptionalPassInfoMixin<MaskedCellUnrollPass> {
    static constexpr unsigned MaxBody = 64, MaxCopied = 320;

    // The C of a GEP whose last index is `and x, C - 1` on a static alloca.
    static unsigned cellWidth(const Value *pointer, Value **counted = nullptr) {
        auto *gep = dyn_cast<GetElementPtrInst>(pointer);
        if (!gep || gep->getNumIndices() == 0) return 0;
        auto *alloca = dyn_cast<AllocaInst>(gep->getPointerOperand()->stripPointerCasts());
        if (!alloca || !alloca->isStaticAlloca()) return 0;
        Value *index = *std::prev(gep->idx_end());
        Value *x;
        const APInt *mask;
        if (!PatternMatch::match(index, PatternMatch::m_And(PatternMatch::m_Value(x),
                                                            PatternMatch::m_APInt(mask))))
            return 0;
        if (!(*mask + 1).isPowerOf2() || mask->ugt(15) || mask->isZero()) return 0;
        if (counted) *counted = x;
        return unsigned(mask->getZExtValue()) + 1;
    }

    // The C to unroll `loop` by, or 0.
    static unsigned width(Loop &loop, ScalarEvolution &scev) {
        BasicBlock *latch = loop.getLoopLatch(), *header = loop.getHeader();
        BasicBlock *preheader = loop.getLoopPreheader();
        if (!latch || !preheader || loop.getExitingBlock() != latch) return 0;
        unsigned body = 0, cells = 0;
        SmallDenseMap<const Value *, unsigned, 4> widths;
        for (BasicBlock *block : loop.blocks())
            for (Instruction &i : *block) {
                if (isa<PHINode>(i) || i.isDebugOrPseudoInst()) continue;
                if (++body > MaxBody || isa<CallBase>(i)) return 0;
                Value *pointer = getLoadStorePointerOperand(&i);
                if (!pointer) {
                    if (i.mayReadOrWriteMemory()) return 0;
                    continue;
                }
                if (auto *load = dyn_cast<LoadInst>(&i); load && !load->isSimple()) return 0;
                if (auto *store = dyn_cast<StoreInst>(&i); store && !store->isSimple()) return 0;
                auto *alloca = dyn_cast<AllocaInst>(getUnderlyingObject(pointer));
                if (!alloca || !alloca->isStaticAlloca()) continue;
                Value *x = nullptr;
                unsigned c = cellWidth(pointer, &x);
                if (!c) return 0;
                // x is P or P + k for a header phi P counting up by 1 from a
                // constant multiple of C.
                Value *counter = x;
                PatternMatch::match(x, PatternMatch::m_Add(PatternMatch::m_Value(counter),
                                                           PatternMatch::m_ConstantInt()));
                auto *phi = dyn_cast<PHINode>(counter);
                if (!phi || phi->getParent() != header) return 0;
                auto *start = dyn_cast<ConstantInt>(phi->getIncomingValueForBlock(preheader));
                if (!start || start->getValue().urem(c) != 0) return 0;
                auto *rec = dyn_cast<SCEVAddRecExpr>(scev.getSCEV(phi));
                if (!rec || rec->getLoop() != &loop || !rec->isAffine() || !rec->getStepRecurrence(scev)->isOne())
                    return 0;
                unsigned &seen = widths[alloca];
                if (seen && seen != c) return 0;
                seen = c;
                cells = std::max(cells, c);
            }
        if (!cells || body * cells > MaxCopied) return 0;
        // All of an alloca's accesses use one C; with two allocas of
        // different C, unrolling by the larger keeps each index constant.
        for (auto &[alloca, c] : widths)
            if (cells % c) return 0;
        return cells;
    }

    // Puts the loop's cell accesses in a fresh alias scope that its other
    // accesses are noalias to, when that holds: no cell alloca is captured,
    // so a pointer whose underlying objects, traced through every phi, select,
    // GEP and cast, are all other than a cell alloca cannot reach a cell.
    // Without this LICM cannot sink the cell stores past a load whose base is
    // a phi of phis (SHA256 reads its bytes through `phi [null, phi [call,
    // load]]`, which BasicAA gives up on), and the cells stay in memory. The
    // tags state a fact about the pointers, so they stay true wherever later
    // passes move the accesses. Returns whether it tagged anything.
    static bool separateCells(Loop &loop) {
        SmallVector<Instruction *, 32> cells, others;
        SmallPtrSet<const Value *, 4> allocas;
        for (BasicBlock *block : loop.blocks())
            for (Instruction &i : *block)
                if (const Value *pointer = getLoadStorePointerOperand(&i)) {
                    if (cellWidth(pointer)) {
                        cells.push_back(&i);
                        allocas.insert(getUnderlyingObject(pointer));
                    } else {
                        others.push_back(&i);
                    }
                }
        if (cells.empty() || others.empty()) return false;
        for (const Value *alloca : allocas)
            if (PointerMayBeCaptured(alloca, /*ReturnCaptures=*/true)) return false;
        for (Instruction *i : others) {
            SmallVector<const Value *, 4> objects;
            getUnderlyingObjects(getLoadStorePointerOperand(i), objects, nullptr, /*MaxLookup=*/0);
            for (const Value *object : objects)
                if (allocas.contains(object) || getUnderlyingObject(object, 0) != object) return false;
        }
        LLVMContext &context = loop.getHeader()->getContext();
        MDBuilder build(context);
        MDNode *domain = build.createAnonymousAliasScopeDomain("hirsel.cells");
        MDNode *scope = MDNode::get(context, build.createAnonymousAliasScope(domain, "cells"));
        for (Instruction *i : cells)
            i->setMetadata(LLVMContext::MD_alias_scope,
                           MDNode::concatenate(i->getMetadata(LLVMContext::MD_alias_scope), scope));
        for (Instruction *i : others)
            i->setMetadata(LLVMContext::MD_noalias,
                           MDNode::concatenate(i->getMetadata(LLVMContext::MD_noalias), scope));
        return true;
    }

public:
    PreservedAnalyses run(Function &f, FunctionAnalysisManager &analyses) {
        if (none_of(instructions(f), [](Instruction &i) {
                const Value *pointer = getLoadStorePointerOperand(&i);
                return pointer && cellWidth(pointer);
            }))
            return PreservedAnalyses::all();
        LoopInfo &loops = analyses.getResult<LoopAnalysis>(f);
        DominatorTree &tree = analyses.getResult<DominatorTreeAnalysis>(f);
        ScalarEvolution &scev = analyses.getResult<ScalarEvolutionAnalysis>(f);
        AssumptionCache &assumptions = analyses.getResult<AssumptionAnalysis>(f);
        const TargetTransformInfo &target = analyses.getResult<TargetIRAnalysis>(f);
        OptimizationRemarkEmitter &remarks = analyses.getResult<OptimizationRemarkEmitterAnalysis>(f);
        SmallVector<std::pair<Loop *, unsigned>, 2> chosen;
        for (Loop *loop : loops.getLoopsInPreorder())
            if (loop->isInnermost())
                if (unsigned c = width(*loop, scev)) chosen.push_back({loop, c});
        bool changed = false;
        for (auto [loop, c] : chosen) {
            changed |= simplifyLoop(loop, &tree, &loops, &scev, &assumptions, nullptr, true);
            changed |= formLCSSARecursively(*loop, tree, &loops, &scev);
            changed |= separateCells(*loop);
            UnrollLoopOptions options;
            options.Count = c;
            options.Force = false;
            options.Runtime = true;
            options.AllowExpensiveTripCount = false;
            options.UnrollRemainder = false;
            options.ForgetAllSCEV = false;
            options.SCEVExpansionBudget = 4;
            LoopUnrollResult result = UnrollLoop(loop, options, &loops, &scev, &tree, &assumptions,
                                                 &target, &remarks, /*PreserveLCSSA=*/true);
            if (result == LoopUnrollResult::Unmodified) continue;
            if (result == LoopUnrollResult::PartiallyUnrolled) loop->setLoopAlreadyUnrolled();
            changed = true;
        }
        if (!changed) return PreservedAnalyses::all();
        analyses.invalidate(f, PreservedAnalyses::none());
        FunctionPassManager cleanup;
        cleanup.addPass(InstCombinePass());
        cleanup.addPass(createFunctionToLoopPassAdaptor(LICMPass(LICMOptions()), /*UseMemorySSA=*/true));
        cleanup.run(f, analyses);
        return PreservedAnalyses::none();
    }
};

// Keep the price before nontrivial unswitching duplicates an infallible
// helper's loop. The late inliner still checks the full size of each copy.
class PriceUnswitchPass : public OptionalPassInfoMixin<PriceUnswitchPass> {
    SimpleLoopUnswitchPass unswitch{/*NonTrivial=*/true};

public:
    PreservedAnalyses run(Loop &loop, LoopAnalysisManager &analyses,
                          LoopStandardAnalysisResults &results, LPMUpdater &updater) {
        Function &f = *loop.getHeader()->getParent();
        if (!f.getName().ends_with(".infallible") || f.hasFnAttribute("hirsel.pre-unswitch-size"))
            return unswitch.run(loop, analyses, results, updater);
        unsigned before = f.getInstructionCount();
        auto preserved = unswitch.run(loop, analyses, results, updater);
        if (!preserved.areAllPreserved() && f.getInstructionCount() > before) {
            f.addFnAttr("hirsel.pre-unswitch-size", std::to_string(before));
        }
        return preserved;
    }
};

// The unroller prices a memcpy or memset as one call whatever its length,
// but SROA then splits each copy of an alloca it has split into slices
// into one store per slice. A repeat of an aggregate (`[e; 32]`) is a
// counted loop of element copies, so full unrolling it and its nested
// repeats made one store per 8 bytes of the whole value: suite_stack's
// `Inline[Inline[Inline[Inline[u64, 32], 32], 32], 32]` became 98k stores
// in one function, and SROA, LoopAccessAnalysis and counter-range took
// minutes over them. A loop whose iterations together move more bytes than
// the O3 full-unroll threshold (300 instructions) would allow at 8 bytes
// a store is kept a loop.
class BulkCopyUnrollLimitPass : public OptionalPassInfoMixin<BulkCopyUnrollLimitPass> {
    static constexpr uint64_t Limit = 300 * 8;

    // The bytes `loop` moves in all its iterations, saturating at Limit + 1,
    // or 0 when it has no constant trip bound and so is never fully unrolled.
    static uint64_t moved(Loop &loop, LoopInfo &loops, ScalarEvolution &evolution) {
        uint64_t each = 0;
        for (BasicBlock *block : loop.blocks()) {
            if (loops.getLoopFor(block) != &loop) continue;
            for (Instruction &i : *block)
                if (auto *bulk = dyn_cast<MemIntrinsic>(&i))
                    if (auto *length = dyn_cast<ConstantInt>(bulk->getLength()))
                        each = std::min(Limit + 1, each + length->getValue().getLimitedValue(Limit + 1));
        }
        for (Loop *inner : loop) {
            uint64_t bytes = moved(*inner, loops, evolution);
            if (!bytes && any_of(inner->blocks(), [](BasicBlock *b) {
                    return any_of(*b, [](Instruction &i) { return isa<MemIntrinsic>(i); });
                }))
                bytes = Limit + 1;
            each = std::min(Limit + 1, each + bytes);
        }
        uint64_t trips = evolution.getSmallConstantMaxTripCount(&loop);
        if (!trips || !each) return 0;
        return each > (Limit + 1) / trips ? Limit + 1 : each * trips;
    }

public:
    PreservedAnalyses run(Function &f, FunctionAnalysisManager &analyses) {
        LoopInfo &loops = analyses.getResult<LoopAnalysis>(f);
        if (loops.empty()) return PreservedAnalyses::all();
        ScalarEvolution &evolution = analyses.getResult<ScalarEvolutionAnalysis>(f);
        for (Loop *loop : loops.getLoopsInPreorder())
            if (loop->getLoopLatch() && !getBooleanLoopAttribute(loop, "llvm.loop.unroll.disable") &&
                moved(*loop, loops, evolution) > Limit)
                addStringMetadataToLoop(loop, "llvm.loop.unroll.disable", 1);
        // Loop metadata is not an analysis input.
        return PreservedAnalyses::all();
    }
};

// These LLVM 23.1 O3 pass settings have no equivalent in the pipeline parser.
using CounterRangeReport = void (*)(void *, const char *, size_t, unsigned);

void registerReleasePasses(PassBuilder &builder, CounterRangeReport report, hirsel_llvm::SourceRemark source, void *receiver) {
    builder.registerPipelineParsingCallback(
        [report, source, receiver](StringRef name, FunctionPassManager &passes, ArrayRef<PassBuilder::PipelineElement>) {
            if (name == "implied-check-hoist") {
                passes.addPass(hirsel_llvm::ImpliedCheckHoistPass());
                return true;
            }
            if (name == "counter-range") {
                passes.addPass(hirsel_llvm::CounterRangePass(report, source, receiver));
                return true;
            }
            if (name == "counter-range<version>") {
                passes.addPass(hirsel_llvm::CounterRangePass(report, source, receiver, /*version=*/true));
                return true;
            }
            if (name == "counted-exit-split") {
                passes.addPass(hirsel_llvm::CountedExitSplitPass());
                return true;
            }
            if (name == "bulk-copy-unroll-limit") {
                passes.addPass(BulkCopyUnrollLimitPass());
                return true;
            }
            if (name == "masked-cell-unroll") {
                passes.addPass(MaskedCellUnrollPass());
                return true;
            }
            if (name == "invariant-arm-sink") {
                passes.addPass(InvariantArmSinkPass());
                return true;
            }
            if (name != "early-vector-combine") return false;
            passes.addPass(VectorCombinePass(/*TryEarlyFoldsOnly=*/true));
            return true;
        });
    builder.registerPipelineParsingCallback(
        [](StringRef name, LoopPassManager &passes, ArrayRef<PassBuilder::PipelineElement>) {
            if (name == "priced-unswitch") {
                passes.addPass(PriceUnswitchPass());
                return true;
            }
            if (name == "full-unroll") {
                passes.addPass(LoopFullUnrollPass(3));
                return true;
            }
            if (name == "extra-unswitch") {
                ExtraLoopPassManager<ShouldRunExtraSimpleLoopUnswitch> extra;
                extra.addPass(PriceUnswitchPass());
                passes.addPass(std::move(extra));
                return true;
            }
            return false;
        });
}

} // namespace

extern "C" void hirsel_llvm_configure_codegen(LLVMModuleRef module, LLVMBool frame_pointers) {
    Module &m = *unwrap(module);
    const bool x86 = m.getTargetTriple().getArch() == Triple::x86_64;
    // Probe a page at a time: the fiber guard is one page of the target,
    // 16 KiB on Darwin and 4 KiB on Linux (hirsel_core_layout::Target::page_log2).
    const char *probe = m.getTargetTriple().isOSDarwin() ? "16384" : "4096";
    for (Function &f : m) {
        if (f.isDeclaration()) continue;
        // The v3 schedule reduces spills in packed floating-point code.
        // Tune after optimization so the instruction features and IR stay fixed.
        if (x86 && any_of(instructions(f), [](const Instruction &i) {
                return isa<BinaryOperator>(i) && i.getType()->isVectorTy() &&
                       i.getType()->getScalarType()->isFloatingPointTy();
            }))
            f.addFnAttr("tune-cpu", "x86-64-v3");
        // Hirsel failures propagate through the call ABI with source sites.
        f.setUWTableKind(UWTableKind::Async);
        f.setDoesNotThrow();
        if (frame_pointers) f.addFnAttr("frame-pointer", "all");
        f.addFnAttr("probe-stack", "inline-asm");
        f.addFnAttr("stack-probe-size", probe);
    }
}

// Fix a hot loop's layout modulo 64 B so its alignment in the uop cache
// does not depend on preceding functions. In AWFY Sieve, the moving layout
// changed cycles from -14% to +36% at the same instruction count.
extern "C" void hirsel_llvm_align_release_functions(LLVMModuleRef module) {
    SmallVector<std::pair<const BasicBlock *, const BasicBlock *>, 8> backedges;
    for (Function &f : *unwrap(module)) {
        if (f.isDeclaration()) continue;
        if (f.hasFnAttribute(Attribute::Cold)) {
            // Let the emitter give each definition its own cold section.
            // An explicit section would make a lending library unsplittable.
            if (!f.hasSection()) f.setSectionPrefix("unlikely");
            continue;
        }
        if (f.hasOptSize() || f.getAlign().valueOrOne() >= Align(64)) continue;
        backedges.clear();
        FindFunctionBackedges(f, backedges);
        if (!backedges.empty() || f.hasFnAttribute(Attribute::Hot)) f.setAlignment(Align(64));
    }
}

namespace {

// A partition of a release unit too large to optimise whole
// (`core::emit_release_unit`): its bodies take the large pipeline, and the
// bodies it shares with the unit's other partitions stay hidden globals.
constexpr StringLiteral PartitionMark = "hirsel.partition";
// Preserve call boundaries. Even always-inline helpers can expand this much
// IR past the memory budget. Std calls use the side's objects.
constexpr StringLiteral LargeFunctionPasses =
    "sroa<modify-cfg>,early-cse,instcombine<no-verify-fixpoint>,simplifycfg";

} // namespace

// Whether `module` is a release partition. The mark stays for its parts
// (`with_part`); it names no node, so no object holds it.
extern "C" LLVMBool hirsel_llvm_release_partition(LLVMModuleRef module) {
    return unwrap(module)->getNamedMetadata(PartitionMark) != nullptr;
}

extern "C" LLVMErrorRef hirsel_llvm_optimize_release_partition(LLVMModuleRef module, LLVMTargetMachineRef machine) {
    return runPasses(module, machine, "release",
        ("globaldce,function<eager-inv>(" + LargeFunctionPasses + "),globaldce").str());
}

extern "C" LLVMErrorRef hirsel_llvm_optimize_release(
    LLVMModuleRef raw_module, LLVMTargetMachineRef raw_machine, LLVMBool prune,
    LLVMBool prepay_steps, CounterRangeReport report, hirsel_llvm::SourceRemark source, void *receiver, const uint8_t *lent,
    size_t lent_len) {
    limit_recursive_entry_inlining(*unwrap(raw_module));
    ReleasePipeline release(raw_module, raw_machine);
    PassBuilder &builder = release.builder;
    registerReleasePasses(builder, report, source, receiver);
    std::string lend_failure;
    if (lent_len) release.skip_lent();

    ModulePassManager pipeline;
    // Promote slots before outlining the cold metering path so the inliner
    // prices the hot body. A composed program can also discard unreachable bodies.
    if (prune) pipeline.addPass(GlobalDCEPass());
    pipeline.addPass(createModuleToFunctionPassAdaptor(SROAPass(SROAOptions::ModifyCFG)));
    if (prepay_steps) pipeline.addPass(PrepayStepTreesPass());
    if (auto error = builder.parsePassPipeline(pipeline,
        "hotcoldsplit,inferattrs,function<eager-inv>(simplifycfg,sroa<modify-cfg>,early-cse),"
        "ipsccp,globalopt,"
        "function<eager-inv>(mem2reg,instcombine<no-verify-fixpoint>,simplifycfg<switch-range-to-icmp>)"))
        return wrap(std::move(error));
    pipeline.addPass(createModuleToFunctionPassAdaptor(MapHomeGroupPass()));
    if (auto error = builder.parsePassPipeline(pipeline, "always-inline"))
        return wrap(std::move(error));

    // Keep LLVM's O3 inline advisor and devirtualization iterations. A parsed
    // cgscc(inline) would use the default inline threshold instead.
    ModuleInlinerWrapperPass inliner(getInlineParamsFromOptLevel(3), /*MandatoryFirst=*/false,
        InlineContext{ThinOrFullLTOPhase::None, InlinePass::CGSCCInliner},
        InliningAdvisorMode::Default, /*MaxDevirtIterations=*/4);
    ModulePassManager inline_analyses;
    if (auto error = builder.parsePassPipeline(inline_analyses,
        "require<globals-aa>,function(invalidate<aa>),require<profile-summary>"))
        return wrap(std::move(error));
    inliner.addModulePass(std::move(inline_analyses));
    if (auto error = builder.parsePassPipeline(inliner.getPM(),
        "function-attrs<skip-non-recursive-function-attrs>,argpromotion,"
        "function<eager-inv;no-rerun>("
            "sroa<modify-cfg>,early-cse<memssa>,jump-threading,correlated-propagation,"
            "jump-table-to-switch,simplifycfg<switch-range-to-icmp>,instcombine<no-verify-fixpoint>,"
            "aggressive-instcombine,libcalls-shrinkwrap,tailcallelim,implied-check-hoist,"
            "simplifycfg<switch-range-to-icmp>,reassociate,constraint-elimination,invariant-arm-sink,"
            "loop-mssa(loop-instsimplify,loop-simplifycfg,licm<no-allowspeculation>,"
                      "loop-rotate,licm<allowspeculation>,priced-unswitch),"
            "simplifycfg<switch-range-to-icmp>,instcombine<no-verify-fixpoint>,counted-exit-split,bulk-copy-unroll-limit,"
            "loop(loop-idiom,indvars,simple-loop-unswitch<trivial>,extra-unswitch,loop-deletion,full-unroll),counter-range,"
            "sroa<modify-cfg>,early-vector-combine,mldst-motion,gvn,sccp,bdce,instcombine<no-verify-fixpoint>,"
            "dfa-jump-threading,jump-threading,correlated-propagation,adce,memcpyopt,dse,"
            "loop-mssa(licm<allowspeculation>),"
            "simplifycfg<switch-range-to-icmp;switch-to-arithmetic;hoist-common-insts;sink-common-insts>,"
            "instcombine<no-verify-fixpoint>),"
        "function-attrs,function(require<should-not-run-function-passes>)"))
        return wrap(std::move(error));
    inliner.addLateModulePass(createModuleToFunctionPassAdaptor(
        InvalidateAnalysisPass<ShouldNotRunFunctionPassesAnalysis>()));
    if (lent_len) pipeline.addPass(LendPass(StringRef((const char *)lent, lent_len), &lend_failure));
    pipeline.addPass(std::move(inliner));

    if (auto error = builder.parsePassPipeline(pipeline,
        "deadargelim,globalopt,globaldce,rpo-function-attrs,recompute-globalsaa,"
        "function<eager-inv>("
            "drop-unnecessary-assumes,float2int,lower-constant-intrinsics,"
            "loop(loop-rotate<check-exit-count>,loop-deletion,loop-interchange),masked-cell-unroll,counter-range<version>,"
            "inject-tli-mappings,loop-vectorize,drop-unnecessary-assumes<drop-deref>,"
            "infer-alignment,loop-load-elim,instcombine<no-verify-fixpoint>,"
            "simplifycfg<forward-switch-cond;switch-range-to-icmp;switch-to-arithmetic;"
                        "switch-to-lookup;no-keep-loops;hoist-common-insts;sink-common-insts>,"
            "slp-vectorizer,vector-combine,instcombine<no-verify-fixpoint>,loop-unroll<O3>,"
            "sroa<preserve-cfg;aggregate-to-vector>,instcombine<no-verify-fixpoint>,"
            "loop-mssa(licm<allowspeculation>),alignment-from-assumptions,"
            "loop-sink,instsimplify,div-rem-pairs,mergeicmps,expand-memcmp,tailcallelim,"
            "simplifycfg<switch-range-to-icmp;switch-to-arithmetic;"
                        "hoist-loads-stores-with-cond-faulting;speculate-unpredictables>),"
        "globaldce,constmerge,mergefunc,rel-lookup-table-converter"))
        return wrap(std::move(error));

    release.run(pipeline, "release");
    if (!lend_failure.empty())
        return wrap(make_error<StringError>(lend_failure, inconvertibleErrorCode()));
    return LLVMErrorSuccess;
}

// See guarded_check.h. Runs on the final IR, before failure edges are weighted.
extern "C" void hirsel_llvm_if_convert_guarded_checks(LLVMModuleRef module) {
    for (Function &function : *unwrap(module))
        if (!function.isDeclaration()) hirsel_llvm::ifConvertGuardedChecks(function);
}

extern "C" LLVMErrorRef hirsel_llvm_growth_arms_back(LLVMModuleRef module,
                                                     LLVMTargetMachineRef machine) {
    ReleasePipeline release(module, machine);
    ModulePassManager pipeline;
    pipeline.addPass(GrowthArmsBackPass());
    pipeline.addPass(GlobalDCEPass());
    release.run(pipeline, "growth-arms-back");
    return LLVMErrorSuccess;
}

// Keep the late inliner over the composed program or its still-lent library.
extern "C" LLVMErrorRef hirsel_llvm_specialize_release(LLVMModuleRef module,
                                                      LLVMTargetMachineRef machine,
                                                      LLVMBool has_lent) {
    ReleasePipeline release(module, machine);
    ModulePassManager pipeline;
    if (has_lent) release.skip_lent();
    release.track_inlining();
    if (auto error = release.builder.parsePassPipeline(pipeline,
        "cgscc(argpromotion),ipsccp,cgscc(inline)"))
        return wrap(std::move(error));
    release.run(pipeline, "specialize");
    return LLVMErrorSuccess;
}

extern "C" LLVMErrorRef hirsel_llvm_drop_lent(LLVMModuleRef module, LLVMTargetMachineRef machine) {
    return runPasses(module, machine, "specialize", "elim-avail-extern");
}

extern "C" LLVMErrorRef hirsel_llvm_cleanup_release(LLVMModuleRef module,
                                                   LLVMTargetMachineRef machine) {
    // Specialize inlining leaves internal functions with no callers; dropping
    // them here spares each its release tail and its own part's code
    // generation. The tail (`hirsel_llvm_release_tail`) then runs per function.
    return runPasses(module, machine, "cleanup", "cgscc(argpromotion),globaldce");
}

extern "C" LLVMErrorRef hirsel_llvm_sweep_release(LLVMModuleRef module,
                                                 LLVMTargetMachineRef machine) {
    // The tail leaves more internal functions with no callers.
    return runPasses(module, machine, "cleanup", "globaldce");
}

extern "C" LLVMBool hirsel_llvm_emit_object(LLVMTargetMachineRef machine, LLVMModuleRef module,
                                          char **message, LLVMMemoryBufferRef *buffer) {
    // LLVM's legacy codegen pass manager exposes no before/after callbacks.
    hirsel_llvm::PassCost cost("object-emission", *unwrap(module));
    return LLVMTargetMachineEmitToMemoryBuffer(machine, module, LLVMObjectFile, message, buffer);
}
