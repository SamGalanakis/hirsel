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
// Private register-return adapter. The FORM destination ABI stays exported.
static void register_results(LLVMModuleRef module, LLVMContextRef context) {
    LLVMBuilderRef builder = LLVMCreateBuilderInContext(context);
    LLVMValueRef last = LLVMGetLastFunction(module);
    for (LLVMValueRef f = LLVMGetFirstFunction(module); f;) {
        LLVMValueRef next = f == last ? NULL : LLVMGetNextFunction(f);
        LLVMAttributeRef marker = LLVMGetStringAttributeAtIndex(f, LLVMAttributeFunctionIndex,
            "hirsel-destination-size", 23);
        if (!marker || !LLVMCountBasicBlocks(f)) { f = next; continue; }
        unsigned len;
        const char *value = LLVMGetStringAttributeValue(marker, &len);
        unsigned bytes = 0;
        for (unsigned i = 0; i < len; ++i) bytes = bytes * 10 + (unsigned)(value[i] - '0');
        LLVMTypeRef old_ty = LLVMGlobalGetValueType(f), old_ret = LLVMGetReturnType(old_ty);
        unsigned count = LLVMCountParams(f);
        if (!bytes || bytes > 16 || count < 2 ||
            (LLVMGetTypeKind(old_ret) != LLVMVoidTypeKind && old_ret != LLVMInt8TypeInContext(context))) {
            f = next; continue;
        }
        // Only direct calls change convention. Pointer users keep FORM's ABI.
        unsigned calls = 0;
        for (LLVMUseRef use = LLVMGetFirstUse(f); use; use = LLVMGetNextUse(use)) {
            LLVMValueRef call = LLVMGetUser(use);
            if (LLVMIsACallInst(call) && LLVMGetCalledValue(call) == f &&
                LLVMGetOperandUse(call, LLVMGetNumOperands(call)-1) == use) ++calls;
        }
        if (!calls) { f = next; continue; }
        LLVMValueRef *sites = malloc(calls * sizeof(*sites));
        unsigned at = 0;
        for (LLVMUseRef use = LLVMGetFirstUse(f); use; use = LLVMGetNextUse(use)) {
            LLVMValueRef call = LLVMGetUser(use);
            if (LLVMIsACallInst(call) && LLVMGetCalledValue(call) == f &&
                LLVMGetOperandUse(call, LLVMGetNumOperands(call)-1) == use) sites[at++] = call;
        }
        LLVMTypeRef *params = malloc(count * sizeof(*params));
        LLVMGetParamTypes(old_ty, params);
        for (unsigned i = 1; i + 1 < count; ++i) params[i] = params[i+1];
        unsigned chunks = (bytes+7)/8;
        int status = LLVMGetTypeKind(old_ret) != LLVMVoidTypeKind;
        LLVMTypeRef fields[5];
        for (unsigned i = 0; i < chunks; ++i)
            fields[i] = LLVMIntTypeInContext(context, (bytes - i*8 < 8 ? bytes-i*8 : 8)*8);
        if (status) fields[chunks] = old_ret;
        LLVMTypeRef result = LLVMStructTypeInContext(context, fields, chunks+status, 0);
        LLVMTypeRef new_ty = LLVMFunctionType(result, params, count-1, 0);
        size_t n;
        const char *name = LLVMGetValueName2(f, &n);
        char *label = malloc(n+16); memcpy(label, name, n); strcpy(label+n, ".register");
        LLVMValueRef promoted = LLVMAddFunction(module, label, new_ty); free(label);
        LLVMSetLinkage(promoted, LLVMInternalLinkage); LLVMSetFunctionCallConv(promoted, LLVMFastCallConv);
        LLVMBasicBlockRef entry = LLVMAppendBasicBlockInContext(context, promoted, "entry");
        LLVMPositionBuilderAtEnd(builder, entry);
        LLVMTypeRef storage = LLVMArrayType2(LLVMInt8TypeInContext(context), bytes);
        LLVMValueRef dest = LLVMBuildAlloca(builder, storage, "result"); LLVMSetAlignment(dest, 8);
        // Like an ordinary C aggregate coercion, raw chunks may contain
        // unspecified padding or inactive fields. Do not assert noundef or
        // freeze a whole word: only initialized source fields are observed.
        LLVMValueRef *args = malloc(count * sizeof(*args));
        args[0] = LLVMGetParam(promoted, 0); args[1] = dest;
        for (unsigned i = 2; i < count; ++i) args[i] = LLVMGetParam(promoted, i-1);
        LLVMValueRef call = LLVMBuildCall2(builder, old_ty, f, args, count, status ? "status" : "");
        unsigned alwaysinline = LLVMGetEnumAttributeKindForName("alwaysinline", 12);
        LLVMAddCallSiteAttribute(call, LLVMAttributeFunctionIndex,
            LLVMCreateEnumAttribute(context, alwaysinline, 0));
        // Preserve narrow parameter ABI attributes on the generic call. The
        // private fastcc parameters deliberately need no extension promises.
        for (unsigned i = 0; i < count; ++i) {
            unsigned attr_count = LLVMGetAttributeCountAtIndex(f, i+1);
            LLVMAttributeRef *attrs = malloc(attr_count*sizeof(*attrs));
            LLVMGetAttributesAtIndex(f, i+1, attrs);
            for (unsigned j = 0; j < attr_count; ++j) LLVMAddCallSiteAttribute(call, i+1, attrs[j]);
            free(attrs);
        }
        LLVMValueRef output = LLVMGetPoison(result);
        for (unsigned i = 0; i < chunks; ++i) {
            LLVMValueRef offset = LLVMConstInt(LLVMInt64TypeInContext(context), i*8, 0);
            LLVMValueRef p = LLVMBuildGEP2(builder, LLVMInt8TypeInContext(context), dest, &offset, 1, "at");
            LLVMValueRef word = LLVMBuildLoad2(builder, fields[i], p, "word"); LLVMSetAlignment(word, 1);
            output = LLVMBuildInsertValue(builder, output, word, i, "output");
        }
        if (status) output = LLVMBuildInsertValue(builder, output, call, chunks, "output");
        LLVMBuildRet(builder, output);
        for (unsigned k = 0; k < calls; ++k) {
            LLVMValueRef old_call = sites[k];
            LLVMPositionBuilderBefore(builder, old_call);
            args[0] = LLVMGetArgOperand(old_call, 0);
            dest = LLVMGetArgOperand(old_call, 1);
            for (unsigned i = 2; i < count; ++i) args[i-1] = LLVMGetArgOperand(old_call, i);
            LLVMValueRef r = LLVMBuildCall2(builder, new_ty, promoted, args, count-1, "result");
            LLVMSetInstructionCallConv(r, LLVMFastCallConv);
            for (unsigned i = 0; i < chunks; ++i) {
                LLVMValueRef offset = LLVMConstInt(LLVMInt64TypeInContext(context), i*8, 0);
                LLVMValueRef p = LLVMBuildGEP2(builder, LLVMInt8TypeInContext(context), dest, &offset, 1, "at");
                LLVMValueRef word = LLVMBuildExtractValue(builder, r, i, "word");
                LLVMValueRef store = LLVMBuildStore(builder, word, p); LLVMSetAlignment(store, 1);
            }
            if (status) LLVMReplaceAllUsesWith(old_call, LLVMBuildExtractValue(builder, r, chunks, "status"));
            LLVMInstructionEraseFromParent(old_call);
        }
        free(args); free(params); free(sites); f = next;
    }
    LLVMDisposeBuilder(builder);
}

// Imported view carriers can be promoted only while their bodies are local:
// O3 discards available_externally bodies before our late argument promotion.
// Keep one private implementation for direct calls. Address uses still name
// the cached public entry, whose FORM convention and identity do not change.
static void local_inputs(LLVMModuleRef module) {
    unsigned always = LLVMGetEnumAttributeKindForName("alwaysinline", 12);
    LLVMValueRef last = LLVMGetLastFunction(module);
    for (LLVMValueRef f = LLVMGetFirstFunction(module); f;) {
        LLVMValueRef next = f == last ? NULL : LLVMGetNextFunction(f);
        if (!LLVMCountBasicBlocks(f) || LLVMGetLinkage(f) != LLVMAvailableExternallyLinkage ||
            !LLVMGetStringAttributeAtIndex(f, LLVMAttributeFunctionIndex, "hirsel-view-input", 17)) {
            f = next; continue;
        }
        unsigned calls = 0, ordinary = 0;
        for (LLVMUseRef use = LLVMGetFirstUse(f); use; use = LLVMGetNextUse(use)) {
            LLVMValueRef call = LLVMGetUser(use);
            if (LLVMIsACallInst(call) && LLVMGetCalledValue(call) == f &&
                LLVMGetOperandUse(call, LLVMGetNumOperands(call)-1) == use) {
                ++calls;
                if (!LLVMGetCallSiteEnumAttribute(call, LLVMAttributeFunctionIndex, always)) ++ordinary;
            }
        }
        // A register-result adapter already forces its call to inline. Leave
        // imports used only there to that existing path.
        if (!ordinary) { f = next; continue; }
        LLVMValueRef *sites = malloc(calls * sizeof(*sites));
        unsigned at = 0;
        for (LLVMUseRef use = LLVMGetFirstUse(f); use; use = LLVMGetNextUse(use)) {
            LLVMValueRef call = LLVMGetUser(use);
            if (LLVMIsACallInst(call) && LLVMGetCalledValue(call) == f &&
                LLVMGetOperandUse(call, LLVMGetNumOperands(call)-1) == use) sites[at++] = call;
        }
        size_t len;
        const char *name = LLVMGetValueName2(f, &len);
        char *label = malloc(len + 7);
        memcpy(label, name, len); strcpy(label + len, ".local");
        LLVMSetValueName2(f, label, len + 6);
        label[len] = 0;
        LLVMValueRef external = LLVMAddFunction(module, label, LLVMGlobalGetValueType(f));
        free(label);
        LLVMSetFunctionCallConv(external, LLVMGetFunctionCallConv(f));
        // Preserve ABI and semantic attributes, including sret and narrow
        // extensions. The marker selects candidates even with facts disabled;
        // it does not assert readonly, nonnull or any additional memory fact.
        for (unsigned i = 0, count = LLVMCountParams(f); i <= count + 1; ++i) {
            unsigned index = i == count + 1 ? LLVMAttributeFunctionIndex : i;
            unsigned n = LLVMGetAttributeCountAtIndex(f, index);
            LLVMAttributeRef *attrs = malloc(n * sizeof(*attrs));
            LLVMGetAttributesAtIndex(f, index, attrs);
            for (unsigned j = 0; j < n; ++j) LLVMAddAttributeAtIndex(external, index, attrs[j]);
            free(attrs);
        }
        // RAUW also handles function addresses nested in constant aggregates.
        LLVMReplaceAllUsesWith(f, external);
        for (unsigned i = 0; i < calls; ++i)
            LLVMSetOperand(sites[i], LLVMGetNumOperands(sites[i])-1, f);
        free(sites);
        LLVMSetLinkage(f, LLVMInternalLinkage);
        f = next;
    }
}

__attribute__((visibility("default")))
void hirsel_llvm_program_v1(const HirselLlvmUnit *units, size_t count,
                          const char *const *exports, size_t export_count,
                          uint8_t speed, const char *triple, const char *cpu, HirselLlvmOutput *out) {
    pthread_once(&initialized, initialize);
    unsigned major, minor, patch;
    LLVMGetVersion(&major, &minor, &patch);
    if (major != 23 || minor != 1 || patch != 5) {
        error(out, "LLVM component requires pinned LLVM 23.1.5"); return;
    }
    LLVMTargetRef target = NULL;
    char *message = NULL;
    if (LLVMGetTargetFromTriple(triple, &target, &message)) {
        error(out, message); LLVMDisposeMessage(message); return;
    }
    LLVMTargetMachineRef machine = LLVMCreateTargetMachine(target, triple, cpu, "",
        speed ? LLVMCodeGenLevelAggressive : LLVMCodeGenLevelNone, LLVMRelocPIC, LLVMCodeModelDefault);
    if (!machine) { error(out, "LLVM could not create the target machine"); return; }
    LLVMContextRef context = LLVMContextCreate();
    // Standalone emission keeps the original module and symbol order exactly.
    // Release composition goes through the linker, which may reorder globals.
    int standalone = count == 1 && !export_count;
    LLVMModuleRef module = standalone ? NULL : LLVMModuleCreateWithNameInContext("hirsel-core", context);
    LLVMMemoryBufferRef buffer = NULL;
    int failed;
    char **imports = NULL;
    size_t imports_len = 0, imports_capacity = 0;
    for (size_t i = 0; i < count; ++i) {
        LLVMModuleRef unit = NULL;
        buffer = LLVMCreateMemoryBufferWithMemoryRangeCopy((const char*)units[i].data, units[i].len, "hirsel-core");
        failed = LLVMParseIRInContext2(context, buffer, &unit, &message);
        LLVMDisposeMemoryBuffer(buffer);
        if (failed || !unit) { error(out, message); LLVMDisposeMessage(message); goto done; }
        if (units[i].imported) {
            for (LLVMValueRef f = LLVMGetFirstFunction(unit); f; f = LLVMGetNextFunction(f)) {
                if (LLVMCountBasicBlocks(f) && LLVMGetLinkage(f) == LLVMExternalLinkage) {
                    size_t len;
                    const char *name = LLVMGetValueName2(f, &len);
                    if (imports_len == imports_capacity) {
                        imports_capacity = imports_capacity ? imports_capacity * 2 : 8;
                        imports = realloc(imports, imports_capacity * sizeof(*imports));
                        if (!imports) abort();
                    }
                    imports[imports_len] = malloc(len + 1);
                    if (!imports[imports_len]) abort();
                    memcpy(imports[imports_len], name, len);
                    imports[imports_len++][len] = 0;
                }
            }
        }
        // The linker resolves each unit's instance-named declarations and
        // renames its private statics/helpers. It consumes the source module.
        if (standalone) module = unit;
        else if (LLVMLinkModules2(module, unit)) { error(out, "LLVM module composition failed"); goto done; }
    }
    // IRLinker may discard an unreferenced available_externally definition
    // before a later unit supplies its callers. Keep definitions until every
    // unit is linked, then restore their non-emitting inline-import linkage.
    for (size_t i = 0; i < imports_len; ++i)
        LLVMSetLinkage(LLVMGetNamedFunction(module, imports[i]), LLVMAvailableExternallyLinkage);
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
    if (speed && export_count) {
        register_results(module, context);
        local_inputs(module);
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
        unsigned hint = LLVMGetEnumAttributeKindForName("inlinehint", 10);
        for (LLVMValueRef f = LLVMGetFirstFunction(module); f; f = LLVMGetNextFunction(f))
            if (LLVMCountBasicBlocks(f))
                LLVMAddAttributeAtIndex(f, LLVMAttributeFunctionIndex,
                    LLVMCreateEnumAttribute(context, hint, 0));
        // The ordinary pipeline infers nonrecursive cleanup attributes after
        // argument promotion. Revisit those arguments, then expose their local
        // carrier values and fold checks implied by the caller's room test.
        // Scalar cleanup also exposes checked end offsets to constraints again.
        // A typed empty-drop guard can disappear in that round. DSE then
        // removes carrier clears/restores that its cleanup used to observe.
        LLVMErrorRef e = LLVMRunPasses(module,
            "default<O3>,cgscc(argpromotion),function(sroa,early-cse,correlated-propagation,instcombine,constraint-elimination,simplifycfg,dse)",
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
    for (size_t i = 0; i < imports_len; ++i) free(imports[i]);
    free(imports);
    if (module) LLVMDisposeModule(module);
    LLVMContextDispose(context); LLVMDisposeTargetMachine(machine);
}
__attribute__((visibility("default")))
void hirsel_llvm_compile_v1(const uint8_t *ir, size_t len, uint8_t speed, const char *triple, const char *cpu, HirselLlvmOutput *out) {
    HirselLlvmUnit unit = { ir, len, 0 };
    hirsel_llvm_program_v1(&unit, 1, NULL, 0, speed, triple, cpu, out);
}
