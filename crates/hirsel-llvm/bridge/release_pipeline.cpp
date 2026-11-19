#include <bits/c++config.h>
#if defined(__GLIBC__) && !__GLIBC_PREREQ(2, 30)
#undef _GLIBCXX_USE_PTHREAD_COND_CLOCKWAIT
#undef _GLIBCXX_USE_PTHREAD_MUTEX_CLOCKLOCK
#undef _GLIBCXX_USE_PTHREAD_RWLOCK_CLOCKLOCK
#endif

#include "pass_cost.h"

#include <llvm-c/TargetMachine.h>
#include <llvm/Analysis/CGSCCPassManager.h>
#include <llvm/IR/Module.h>
#include <llvm/Passes/PassBuilder.h>
#include <llvm/Passes/StandardInstrumentations.h>
#include <llvm/Transforms/IPO/GlobalDCE.h>
#include <llvm/Transforms/IPO/Inliner.h>
#include <llvm/Transforms/Scalar/LoopUnrollPass.h>
#include <llvm/Transforms/Scalar/SROA.h>
#include <llvm/Transforms/Scalar/SimpleLoopUnswitch.h>
#include <llvm/Transforms/Utils/ExtraPassManager.h>
#include <llvm/Transforms/Vectorize/VectorCombine.h>

using namespace llvm;

extern "C" void hirsel_llvm_prepay_step_trees(void *module);

namespace {

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

class PrepayStepTreesPass : public OptionalPassInfoMixin<PrepayStepTreesPass> {
public:
    PreservedAnalyses run(Module &module, ModuleAnalysisManager &) {
        hirsel_llvm_prepay_step_trees(&module);
        return PreservedAnalyses::none();
    }
};

// These LLVM 23.1 O3 pass settings have no equivalent in the pipeline parser.
void registerReleasePasses(PassBuilder &builder) {
    builder.registerPipelineParsingCallback(
        [](StringRef name, FunctionPassManager &passes, ArrayRef<PassBuilder::PipelineElement>) {
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
    for (Function &f : *unwrap(module)) {
        if (f.isDeclaration()) continue;
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
    LLVMBool prepay_steps) {
    ReleasePipeline release(raw_module, raw_machine);
    PassBuilder &builder = release.builder;
    registerReleasePasses(builder);

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
            "aggressive-instcombine,libcalls-shrinkwrap,tailcallelim,"
            "simplifycfg<switch-range-to-icmp>,reassociate,constraint-elimination,"
            "loop-mssa(loop-instsimplify,loop-simplifycfg,licm<no-allowspeculation>,"
                      "loop-rotate,licm<allowspeculation>,simple-loop-unswitch<nontrivial;trivial>),"
            "simplifycfg<switch-range-to-icmp>,instcombine<no-verify-fixpoint>,"
            "loop(loop-idiom,indvars,simple-loop-unswitch<trivial>,extra-unswitch,loop-deletion,full-unroll),"
            "sroa<modify-cfg>,early-vector-combine,mldst-motion,gvn,sccp,bdce,instcombine<no-verify-fixpoint>,"
            "dfa-jump-threading,jump-threading,correlated-propagation,adce,memcpyopt,dse,"
            "loop-mssa(licm<allowspeculation>),"
            "simplifycfg<switch-range-to-icmp;switch-to-arithmetic;hoist-common-insts;sink-common-insts>,"
            "instcombine<no-verify-fixpoint>),"
        "function-attrs,function(require<should-not-run-function-passes>)"))
        return wrap(std::move(error));
    inliner.addLateModulePass(createModuleToFunctionPassAdaptor(
        InvalidateAnalysisPass<ShouldNotRunFunctionPassesAnalysis>()));
    pipeline.addPass(std::move(inliner));

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
