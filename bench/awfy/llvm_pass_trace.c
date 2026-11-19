#include <dlfcn.h>
#include <stdio.h>
#include <stdlib.h>

#include <llvm-c/Transforms/PassBuilder.h>

__attribute__((constructor)) static void disable_child_preload(void) {
    unsetenv("LD_PRELOAD");
}

LLVMErrorRef LLVMRunPasses(LLVMModuleRef module, const char *pipeline,
                           LLVMTargetMachineRef target, LLVMPassBuilderOptionsRef options) {
    // The backend opens its pinned LLVM with RTLD_LOCAL.
    void *llvm = dlopen(HIRSEL_LLVM_SONAME, RTLD_LAZY | RTLD_NOLOAD);
    if (!llvm) {
        fprintf(stderr, "LLVM pass trace: %s\n", dlerror());
        abort();
    }
    LLVMErrorRef (*run)(LLVMModuleRef, const char *, LLVMTargetMachineRef, LLVMPassBuilderOptionsRef) =
        dlsym(llvm, "LLVMRunPasses");
    void (*debug)(LLVMPassBuilderOptionsRef, LLVMBool) =
        dlsym(llvm, "LLVMPassBuilderOptionsSetDebugLogging");
    if (!run || !debug) {
        fprintf(stderr, "LLVM pass trace: pinned LLVM has no pass logging API\n");
        abort();
    }
    debug(options, 1);
    LLVMErrorRef error = run(module, pipeline, target, options);
    dlclose(llvm);
    return error;
}
