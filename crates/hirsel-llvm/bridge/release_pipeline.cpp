#include <bits/c++config.h>
#if defined(__GLIBC__) && !__GLIBC_PREREQ(2, 30)
#undef _GLIBCXX_USE_PTHREAD_COND_CLOCKWAIT
#undef _GLIBCXX_USE_PTHREAD_MUTEX_CLOCKLOCK
#undef _GLIBCXX_USE_PTHREAD_RWLOCK_CLOCKLOCK
#endif

#include "counted_exit.h"
#include "counter_range.h"
#include "guarded_check.h"
#include "implied_check.h"
#include "pass_cost.h"

#include <llvm-c/TargetMachine.h>
#include <llvm/ADT/MapVector.h>
#include <llvm/Analysis/CGSCCPassManager.h>
#include <llvm/Analysis/LoopInfo.h>
#include <llvm/Analysis/ValueTracking.h>
#include <llvm/IR/Dominators.h>
#include <llvm/IR/InstIterator.h>
#include <llvm/IR/Instructions.h>
#include <llvm/IR/Module.h>
#include <llvm/Bitcode/BitcodeReader.h>
#include <llvm/Linker/Linker.h>
#include <llvm/Support/MemoryBuffer.h>
#include <llvm/Passes/PassBuilder.h>
#include <llvm/Passes/StandardInstrumentations.h>
#include <llvm/Transforms/IPO/ElimAvailExtern.h>
#include <llvm/Transforms/IPO/GlobalDCE.h>
#include <llvm/Transforms/IPO/Inliner.h>
#include <llvm/Transforms/Scalar/LoopUnrollPass.h>
#include <llvm/Transforms/Scalar/SROA.h>
#include <llvm/Transforms/Scalar/SimpleLoopUnswitch.h>
#include <llvm/Transforms/Utils/Cloning.h>
#include <llvm/Transforms/Utils/ExtraPassManager.h>
#include <llvm/Transforms/Vectorize/VectorCombine.h>

using namespace llvm;

extern "C" void hirsel_llvm_prepay_step_trees(void *module);

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

// A library's lent bodies (hirsel_llvm_emit_parts) join the module for its
// inliner: each body a definition calls, and those they call, comes in
// `available_externally`. Its own passes do not run over it, the inliner may
// copy it into a caller, and `elim-avail-extern` then drops it, so each call
// left reaches the library's part.
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

// These LLVM 23.1 O3 pass settings have no equivalent in the pipeline parser.
using CounterRangeReport = void (*)(void *, const char *, size_t, unsigned);

void registerReleasePasses(PassBuilder &builder, CounterRangeReport report, void *receiver) {
    builder.registerPipelineParsingCallback(
        [report, receiver](StringRef name, FunctionPassManager &passes, ArrayRef<PassBuilder::PipelineElement>) {
            if (name == "implied-check-hoist") {
                passes.addPass(hirsel_llvm::ImpliedCheckHoistPass());
                return true;
            }
            if (name == "counter-range") {
                passes.addPass(hirsel_llvm::CounterRangePass(report, receiver));
                return true;
            }
            if (name == "counted-exit-split") {
                passes.addPass(hirsel_llvm::CountedExitSplitPass());
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
            if (name == "full-unroll") {
                passes.addPass(LoopFullUnrollPass(3));
                return true;
            }
            if (name == "extra-unswitch") {
                ExtraLoopPassManager<ShouldRunExtraSimpleLoopUnswitch> extra;
                extra.addPass(SimpleLoopUnswitchPass(/*NonTrivial=*/true));
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
        f.removeFnAttr(Attribute::UWTable);
        f.setDoesNotThrow();
        if (frame_pointers) f.addFnAttr("frame-pointer", "all");
        // Probe at the smallest supported fiber guard size.
        f.addFnAttr("probe-stack", "inline-asm");
        f.addFnAttr("stack-probe-size", "4096");
    }
}

// Start every warm generated function on a 64 B line, as Go starts every
// function on a 32 B boundary. The decoded-op caches (Zen's op cache, Intel's
// uop cache) hold ops per aligned window, so a hot loop that straddles a
// window costs extra fetches each iteration. At LLVM's 16 B function
// alignment, whether a loop straddled depended on how much unrelated code was
// linked in front of it: AWFY Sieve's cycles moved from -14% to +36% with
// identical instructions. With each start fixed modulo 64, a function has the
// same line layout in every program. The padding lies between functions and
// is never executed. Cold and size-optimized bodies keep the default.
extern "C" void hirsel_llvm_align_release_functions(LLVMModuleRef module) {
    for (Function &f : *unwrap(module)) {
        if (f.isDeclaration() || f.hasFnAttribute(Attribute::Cold) || f.hasOptSize()) continue;
        if (f.getAlign().valueOrOne() < Align(64)) f.setAlignment(Align(64));
    }
}

extern "C" LLVMErrorRef hirsel_llvm_optimize_release(
    LLVMModuleRef raw_module, LLVMTargetMachineRef raw_machine, LLVMBool prune,
    LLVMBool prepay_steps, CounterRangeReport report, void *receiver, const uint8_t *lent,
    size_t lent_len) {
    limit_recursive_entry_inlining(*unwrap(raw_module));
    ReleasePipeline release(raw_module, raw_machine);
    PassBuilder &builder = release.builder;
    registerReleasePasses(builder, report, receiver);
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
        "function<eager-inv>(mem2reg,instcombine<no-verify-fixpoint>,simplifycfg<switch-range-to-icmp>),always-inline"))
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
                      "loop-rotate,licm<allowspeculation>,simple-loop-unswitch<nontrivial;trivial>),"
            "simplifycfg<switch-range-to-icmp>,instcombine<no-verify-fixpoint>,counted-exit-split,"
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
    if (lent_len) pipeline.addPass(EliminateAvailableExternallyPass());

    if (auto error = builder.parsePassPipeline(pipeline,
        "deadargelim,globalopt,globaldce,rpo-function-attrs,recompute-globalsaa,"
        "function<eager-inv>("
            "drop-unnecessary-assumes,float2int,lower-constant-intrinsics,"
            "loop(loop-rotate<check-exit-count>,loop-deletion,loop-interchange),"
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

extern "C" LLVMErrorRef hirsel_llvm_specialize_release(LLVMModuleRef module,
                                                      LLVMTargetMachineRef machine) {
    return runPasses(module, machine, "specialize", "cgscc(argpromotion),ipsccp,partial-inliner,cgscc(inline)");
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
