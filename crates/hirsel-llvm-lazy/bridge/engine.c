// The CoreIR object's LLVM phase, with the same target, pipeline and probe
// settings as hirsel-llvm's direct Context/Module/TargetMachine path.
#include <llvm-c/Core.h>
#include <llvm-c/IRReader.h>
#include <llvm-c/Linker.h>
#include <llvm-c/Error.h>
#include <llvm-c/Target.h>
#include <llvm-c/TargetMachine.h>
#include <llvm-c/Transforms/PassBuilder.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>
#include <pthread.h>

typedef struct { uint8_t *data; size_t len; uint8_t failed; } HirselLlvmOutput;
typedef struct { const uint8_t *data; size_t len; uint8_t imported; } HirselLlvmUnit;
static pthread_once_t initialized = PTHREAD_ONCE_INIT;
static void initialize(void) {
    LLVMInitializeX86TargetInfo(); LLVMInitializeX86Target();
    LLVMInitializeX86TargetMC(); LLVMInitializeX86AsmPrinter();
    LLVMInitializeAArch64TargetInfo(); LLVMInitializeAArch64Target();
    LLVMInitializeAArch64TargetMC(); LLVMInitializeAArch64AsmPrinter();
}
static void copy(HirselLlvmOutput *out, const void *data, size_t len, uint8_t failed) {
    out->data = malloc(len ? len : 1);
    if (!out->data) abort();
    memcpy(out->data, data, len); out->len = len; out->failed = failed;
}
static void error(HirselLlvmOutput *out, const char *message) {
    copy(out, message, strlen(message), 1);
}
__attribute__((visibility("default")))
void hirsel_llvm_output_free_v1(uint8_t *data) { free(data); }
__attribute__((visibility("default")))
void hirsel_llvm_program_v1(const HirselLlvmUnit *units, size_t count,
                          const char *const *exports, size_t export_count,
                          uint8_t speed, HirselLlvmOutput *out) {
    pthread_once(&initialized, initialize);
    unsigned major, minor, patch;
    LLVMGetVersion(&major, &minor, &patch);
    if (major != 23 || minor != 1 || patch != 5) {
        error(out, "LLVM component requires pinned LLVM 23.1.5"); return;
    }
    const char *triple = "x86_64-unknown-linux-gnu";
    LLVMTargetRef target = NULL;
    char *message = NULL;
    if (LLVMGetTargetFromTriple(triple, &target, &message)) {
        error(out, message); LLVMDisposeMessage(message); return;
    }
    LLVMTargetMachineRef machine = LLVMCreateTargetMachine(target, triple, "x86-64", "",
        speed ? LLVMCodeGenLevelAggressive : LLVMCodeGenLevelNone, LLVMRelocPIC, LLVMCodeModelDefault);
    if (!machine) { error(out, "LLVM could not create the target machine"); return; }
    LLVMContextRef context = LLVMContextCreate();
    // Standalone emission keeps the original module and symbol order exactly.
    // Release composition goes through the linker, which may reorder globals.
    int standalone = count == 1 && !export_count;
    LLVMModuleRef module = standalone ? NULL : LLVMModuleCreateWithNameInContext("hirsel-core", context);
    LLVMMemoryBufferRef buffer = NULL;
    int failed;
    for (size_t i = 0; i < count; ++i) {
        LLVMModuleRef unit = NULL;
        buffer = LLVMCreateMemoryBufferWithMemoryRangeCopy((const char*)units[i].data, units[i].len, "hirsel-core");
        failed = LLVMParseIRInContext2(context, buffer, &unit, &message);
        LLVMDisposeMemoryBuffer(buffer);
        if (failed || !unit) { error(out, message); LLVMDisposeMessage(message); goto done; }
        if (units[i].imported) {
            for (LLVMValueRef f = LLVMGetFirstFunction(unit); f; f = LLVMGetNextFunction(f)) {
                if (LLVMCountBasicBlocks(f) && LLVMGetLinkage(f) == LLVMExternalLinkage)
                    LLVMSetLinkage(f, LLVMAvailableExternallyLinkage);
            }
        }
        // The linker resolves each unit's instance-named declarations and
        // renames its private statics/helpers. It consumes the source module.
        if (standalone) module = unit;
        else if (LLVMLinkModules2(module, unit)) { error(out, "LLVM module composition failed"); goto done; }
    }
    if (export_count) {
        for (LLVMValueRef f = LLVMGetFirstFunction(module); f; f = LLVMGetNextFunction(f)) {
            if (!LLVMCountBasicBlocks(f) || LLVMGetLinkage(f) != LLVMExternalLinkage) continue;
            size_t len;
            const char *name = LLVMGetValueName2(f, &len);
            int exported = 0;
            for (size_t i = 0; i < export_count; ++i)
                if (strlen(exports[i]) == len && !memcmp(name, exports[i], len)) exported = 1;
            if (!exported) LLVMSetLinkage(f, LLVMInternalLinkage);
        }
    }
    LLVMSetTarget(module, triple);
    LLVMTargetDataRef layout = LLVMCreateTargetDataLayout(machine);
    LLVMSetModuleDataLayout(module, layout); LLVMDisposeTargetData(layout);
    if (speed) {
        LLVMPassBuilderOptionsRef options = LLVMCreatePassBuilderOptions();
        LLVMPassBuilderOptionsSetLoopInterleaving(options, 1);
        LLVMPassBuilderOptionsSetLoopUnrolling(options, 1);
        LLVMPassBuilderOptionsSetLoopVectorization(options, 1);
        LLVMPassBuilderOptionsSetSLPVectorization(options, 1);
        LLVMPassBuilderOptionsSetMergeFunctions(options, 1);
        // The ordinary pipeline infers nonrecursive cleanup attributes after
        // argument promotion. Revisit those arguments, then expose their local
        // carrier values and fold checks implied by the caller's room test.
        LLVMErrorRef e = LLVMRunPasses(module,
            "default<O3>,cgscc(argpromotion),function(sroa,early-cse,correlated-propagation,instcombine,simplifycfg)",
            machine, options);
        LLVMDisposePassBuilderOptions(options);
        if (e) {
            char *text = LLVMGetErrorMessage(e); error(out, text);
            LLVMDisposeErrorMessage(text); goto done;
        }
    }
    LLVMAttributeRef probe = LLVMCreateStringAttribute(context, "probe-stack", 11, "inline-asm", 10);
    LLVMAttributeRef interval = LLVMCreateStringAttribute(context, "stack-probe-size", 16, "4096", 4);
    for (LLVMValueRef f = LLVMGetFirstFunction(module); f; f = LLVMGetNextFunction(f)) {
        if (LLVMCountBasicBlocks(f)) {
            LLVMAddAttributeAtIndex(f, LLVMAttributeFunctionIndex, probe);
            LLVMAddAttributeAtIndex(f, LLVMAttributeFunctionIndex, interval);
        }
    }
    buffer = NULL; message = NULL;
    failed = LLVMTargetMachineEmitToMemoryBuffer(machine, module, LLVMObjectFile, &message, &buffer);
    if (failed || !buffer) { error(out, message ? message : "LLVM object emission failed"); }
    else { copy(out, LLVMGetBufferStart(buffer), LLVMGetBufferSize(buffer), 0); LLVMDisposeMemoryBuffer(buffer); }
    if (message) LLVMDisposeMessage(message);
done:
    if (module) LLVMDisposeModule(module);
    LLVMContextDispose(context); LLVMDisposeTargetMachine(machine);
}
__attribute__((visibility("default")))
void hirsel_llvm_compile_v1(const uint8_t *ir, size_t len, uint8_t speed, HirselLlvmOutput *out) {
    HirselLlvmUnit unit = { ir, len, 0 };
    hirsel_llvm_program_v1(&unit, 1, NULL, 0, speed, out);
}
