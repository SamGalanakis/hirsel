// The CoreIR object's LLVM phase, with the same target, pipeline and probe
// settings as hirsel-llvm's direct Context/Module/TargetMachine path.
#include <llvm-c/Core.h>
#include <llvm-c/IRReader.h>
#include <llvm-c/Error.h>
#include <llvm-c/Target.h>
#include <llvm-c/TargetMachine.h>
#include <llvm-c/Transforms/PassBuilder.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>
#include <pthread.h>

typedef struct { uint8_t *data; size_t len; uint8_t failed; } HirselLlvmOutput;
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
void hirsel_llvm_compile_v1(const uint8_t *ir, size_t len, uint8_t speed, HirselLlvmOutput *out) {
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
    LLVMModuleRef module = NULL;
    LLVMMemoryBufferRef buffer = LLVMCreateMemoryBufferWithMemoryRangeCopy((const char*)ir, len, "hirsel-core");
    int failed = LLVMParseIRInContext2(context, buffer, &module, &message);
    LLVMDisposeMemoryBuffer(buffer);
    if (failed || !module) { error(out, message); LLVMDisposeMessage(message); goto done; }
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
        LLVMErrorRef e = LLVMRunPasses(module, "default<O3>", machine, options);
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
