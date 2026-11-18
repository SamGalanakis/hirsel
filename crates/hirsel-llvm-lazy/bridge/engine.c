// The CoreIR object's LLVM phase, with the same target, pipeline and probe
// settings as hirsel-llvm's direct Context/Module/TargetMachine path.
#include <llvm-c/Analysis.h>
#include <llvm-c/Core.h>
#include <llvm-c/DebugInfo.h>
#include <llvm-c/IRReader.h>
#include <llvm-c/Support.h>
#include <llvm-c/Error.h>
#include <llvm-c/Target.h>
#include <llvm-c/TargetMachine.h>
#include <llvm-c/Transforms/PassBuilder.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <pthread.h>
#include "aggregate_phi.h"
#include "failure_edges.h"
extern void hirsel_llvm_promote_step_window(void *module);
extern void hirsel_llvm_promote_development_slots(void *module);
extern void hirsel_llvm_mark_loop_callbacks(void *module);
extern void hirsel_llvm_inline_loop_callbacks(void *module);
extern void hirsel_llvm_inline_recursive_leaves(void *module);
extern void hirsel_llvm_finish_reads(void *module, void *machine);
extern void hirsel_llvm_share_cold_sites(void *module);
extern void hirsel_llvm_share_cold_step_sync(void *module, void *machine);
extern void hirsel_llvm_reuse_step_flags(void *module);
extern void hirsel_llvm_guard_copy_loops(void *module, void *machine);
extern void hirsel_llvm_fixed_width_checks(void *module);

typedef struct { uint8_t *data; size_t len; uint8_t failed; } HirselLlvmOutput;
typedef struct { const uint8_t *data; size_t len; LLVMModuleRef module; } HirselLlvmUnit;
extern void *hirsel_llvm_composer(LLVMModuleRef module);
extern int hirsel_llvm_compose(void *composer, LLVMModuleRef unit);
extern void hirsel_llvm_composer_free(void *composer);
extern void hirsel_llvm_merge_glue(LLVMModuleRef module);
typedef int (*HirselLlvmReuse)(void *, const char *, size_t, const char *, size_t, const uint8_t *, size_t);
typedef void (*HirselLlvmDeliver)(void *, const uint8_t *, size_t);
extern int hirsel_llvm_emit_parts(LLVMModuleRef module, LLVMTargetMachineRef machine, unsigned jobs,
    HirselLlvmReuse reuse, HirselLlvmDeliver deliver, void *context, char **error);
static pthread_once_t initialized = PTHREAD_ONCE_INIT;
static void initialize(void) {
    // Dense control flow leaves many copies of the same rematerializable
    // register. Batch its live-interval updates before the coalescer visits
    // every copy separately.
    const char *options[] = {"hirsel", "-late-remat-update-threshold=16"};
    LLVMParseCommandLineOptions(2, options, NULL);
    LLVMInitializeX86TargetInfo(); LLVMInitializeX86Target();
    LLVMInitializeX86TargetMC(); LLVMInitializeX86AsmPrinter();
    LLVMInitializeX86AsmParser();
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

// Private ABI workers still own source-independent debug scopes. Their callers
// and inlined bodies keep ordinary LLVM locations; generated instructions use 0.
static LLVMMetadataRef worker_scope(LLVMModuleRef module,
                                   LLVMDIBuilderRef *debug, LLVMValueRef function,
                                   LLVMMetadataRef original) {
    if (!original) return NULL;
    LLVMMetadataRef file = LLVMDIScopeGetFile(original);
    if (!*debug) {
        *debug = LLVMCreateDIBuilder(module);
        LLVMDIBuilderCreateCompileUnit(*debug, LLVMDWARFSourceLanguageRust, file,
            "Hirsel", 6, 1, "", 0, 0, "", 0, LLVMDWARFEmissionFull, 0, 1, 0, "", 0, "", 0);
    }
    unsigned name_len;
    size_t linkage_len;
    const char *name = LLVMDIFileGetFilename(file, &name_len);
    const char *linkage = LLVMGetValueName2(function, &linkage_len);
    unsigned line = LLVMDISubprogramGetLine(original);
    LLVMMetadataRef type = LLVMDIBuilderCreateSubroutineType(*debug, file, NULL, 0, LLVMDIFlagZero);
    LLVMMetadataRef scope = LLVMDIBuilderCreateFunction(*debug, file, name, name_len,
        linkage, linkage_len, file, line, type, 1, 1, line, LLVMDIFlagZero, 1);
    LLVMSetSubprogram(function, scope);
    return scope;
}

static LLVMMetadataRef worker_location(LLVMDIBuilderRef debug, LLVMContextRef context,
                                      LLVMMetadataRef location, LLVMMetadataRef scope) {
    if (!location) return NULL;
    LLVMMetadataRef at = LLVMDILocationGetInlinedAt(location);
    LLVMMetadataRef source_scope = LLVMDILocationGetScope(location);
    if (at) at = worker_location(debug, context, at, scope);
    else {
        LLVMMetadataRef file = LLVMDIScopeGetFile(source_scope);
        source_scope = file == LLVMDIScopeGetFile(scope) ? scope
            : LLVMDIBuilderCreateLexicalBlockFile(debug, scope, file, 0);
    }
    return LLVMDIBuilderCreateDebugLocation(context, LLVMDILocationGetLine(location),
        LLVMDILocationGetColumn(location), source_scope, at);
}

static void finish_worker_debug(LLVMDIBuilderRef debug) {
    if (debug) { LLVMDIBuilderFinalize(debug); LLVMDisposeDIBuilder(debug); }
}
// The parameters a function marks "hirsel-carriers" = "I:K[:N],...": LLVM parameter
// I points to a sequence header (data pointer, length, capacity) lent as K, where
// `m` (mut or take) comes back and `v` (view) does not. N, when present, is the
// exclusive bound of its length and capacity words (FORM #113's `!range`).
#define MAX_CARRIERS 8
typedef struct { unsigned index; char kind; uint64_t bound; } Carrier;
static unsigned carriers_of(LLVMValueRef f, Carrier *out) {
    LLVMAttributeRef marker = LLVMGetStringAttributeAtIndex(f, LLVMAttributeFunctionIndex,
        "hirsel-carriers", 15);
    if (!marker) return 0;
    unsigned len, n = 0, field = 0;
    uint64_t number = 0;
    const char *text = LLVMGetStringAttributeValue(marker, &len);
    for (unsigned i = 0; i <= len && n < MAX_CARRIERS; ++i) {
        char c = i < len ? text[i] : ',';
        if (c >= '0' && c <= '9') number = number * 10 + (uint64_t)(c - '0');
        else if (c == 'm' || c == 'v') out[n].kind = c;
        else if (c == ':') {
            if (field == 0) { out[n].index = (unsigned)number; out[n].bound = 0; }
            number = 0; ++field;
        } else if (c == ',') {
            if (field == 2) out[n].bound = number;
            if (field) ++n;
            number = 0; field = 0;
        }
    }
    return n;
}
static void set_carriers(LLVMContextRef context, LLVMValueRef f, const Carrier *cs, unsigned n) {
    char text[MAX_CARRIERS * 40];
    int at = 0;
    for (unsigned i = 0; i < n; ++i) {
        at += snprintf(text + at, sizeof(text) - (size_t)at, "%s%u:%c", i ? "," : "",
                       cs[i].index, cs[i].kind);
        if (cs[i].bound)
            at += snprintf(text + at, sizeof(text) - (size_t)at, ":%llu",
                           (unsigned long long)cs[i].bound);
    }
    LLVMRemoveStringAttributeAtIndex(f, LLVMAttributeFunctionIndex, "hirsel-carriers", 15);
    if (n) LLVMAddAttributeAtIndex(f, LLVMAttributeFunctionIndex,
        LLVMCreateStringAttribute(context, "hirsel-carriers", 15, text, (unsigned)at));
}
static unsigned direct_calls(LLVMValueRef f, LLVMValueRef **out) {
    unsigned calls = 0;
    for (LLVMUseRef use = LLVMGetFirstUse(f); use; use = LLVMGetNextUse(use)) {
        LLVMValueRef call = LLVMGetUser(use);
        if (LLVMIsACallInst(call) && LLVMGetCalledValue(call) == f &&
            LLVMGetOperandUse(call, LLVMGetNumOperands(call)-1) == use) ++calls;
    }
    if (!calls) return 0;
    *out = malloc(calls * sizeof(**out));
    unsigned at = 0;
    for (LLVMUseRef use = LLVMGetFirstUse(f); use; use = LLVMGetNextUse(use)) {
        LLVMValueRef call = LLVMGetUser(use);
        if (LLVMIsACallInst(call) && LLVMGetCalledValue(call) == f &&
            LLVMGetOperandUse(call, LLVMGetNumOperands(call)-1) == use) (*out)[at++] = call;
    }
    return calls;
}
static unsigned return_words(LLVMTypeRef t) {
    switch (LLVMGetTypeKind(t)) {
    case LLVMVoidTypeKind: return 0;
    case LLVMStructTypeKind: {
        unsigned n = 0;
        for (unsigned i = 0, c = LLVMCountStructElementTypes(t); i < c; ++i)
            n += return_words(LLVMStructGetTypeAtIndex(t, i));
        return n;
    }
    case LLVMArrayTypeKind: return 99;
    default: return 1;
    }
}

// The attributes `f` gives its index `from` (a parameter's is its number plus
// one, the result's LLVMAttributeReturnIndex), added at index `to` of `target`:
// a call site's when `site`, else a function's.
static void copy_attributes(LLVMValueRef f, unsigned from, LLVMValueRef target, unsigned to,
                            int site) {
    unsigned na = LLVMGetAttributeCountAtIndex(f, from);
    LLVMAttributeRef *attrs = malloc((na + 1) * sizeof(*attrs));
    LLVMGetAttributesAtIndex(f, from, attrs);
    for (unsigned j = 0; j < na; ++j) {
        if (site) LLVMAddCallSiteAttribute(target, to, attrs[j]);
        else LLVMAddAttributeAtIndex(target, to, attrs[j]);
    }
    free(attrs);
}

// One call of a carrier twin: each carried header's words are loaded from its
// pointer, and a `mut` header's words are stored back before anything reads the
// call's status. Returns the call's original result (or the call when void).
static LLVMValueRef call_twin(LLVMBuilderRef builder, LLVMContextRef context,
                              LLVMValueRef twin, LLVMTypeRef new_ty, LLVMValueRef *args,
                              unsigned count, const int *carried, const uint64_t *bounds,
                              unsigned outs, int has_ret, LLVMValueRef *new_args, unsigned np) {
    LLVMTypeRef i64 = LLVMInt64TypeInContext(context), i8 = LLVMInt8TypeInContext(context);
    LLVMTypeRef word_ty[3] = {LLVMPointerTypeInContext(context, 0), i64, i64};
    LLVMValueRef back[MAX_CARRIERS];
    unsigned nb = 0, a = 0;
    for (unsigned i = 0; i < count; ++i) {
        if (!carried[i]) { new_args[a++] = args[i]; continue; }
        for (unsigned w = 0; w < 3; ++w) {
            LLVMValueRef offset = LLVMConstInt(i64, w * 8, 0);
            LLVMValueRef p = LLVMBuildGEP2(builder, i8, args[i], &offset, 1, "at");
            LLVMValueRef word = LLVMBuildLoad2(builder, word_ty[w], p, "word");
            LLVMSetAlignment(word, 1);
            if (w && bounds[i]) {
                LLVMValueRef ends[2] = {LLVMConstInt(i64, 0, 0), LLVMConstInt(i64, bounds[i], 0)};
                LLVMSetMetadata(word, LLVMGetMDKindIDInContext(context, "range", 5),
                    LLVMMDNodeInContext(context, ends, 2));
            }
            new_args[a++] = word;
        }
        if (carried[i] == 'm') back[nb++] = args[i];
    }
    LLVMValueRef r = LLVMBuildCall2(builder, new_ty, twin, new_args, np, "");
    LLVMSetInstructionCallConv(r, LLVMGetFunctionCallConv(twin));
    // The call states the twin's parameter ABI (extensions, `sret`, word
    // ranges), and its result's while the result is the definition's.
    for (unsigned j = 0; j < np; ++j) copy_attributes(twin, j + 1, r, j + 1, 1);
    if (!outs) copy_attributes(twin, LLVMAttributeReturnIndex, r, LLVMAttributeReturnIndex, 1);
    if (!outs) return r;
    unsigned at = has_ret ? 1 : 0;
    for (unsigned b = 0; b < nb; ++b)
        for (unsigned w = 0; w < 3; ++w) {
            LLVMValueRef offset = LLVMConstInt(i64, w * 8, 0);
            LLVMValueRef p = LLVMBuildGEP2(builder, i8, back[b], &offset, 1, "at");
            LLVMValueRef s = LLVMBuildStore(builder, LLVMBuildExtractValue(builder, r, at++, "word"), p);
            LLVMSetAlignment(s, 1);
        }
    return has_ret ? LLVMBuildExtractValue(builder, r, 0, "inner") : r;
}

// Short carriers as words on cold edges. A call the front marked cold (the
// growth path) passes each marked sequence header as its three words, and a
// `mut` header comes back in the result, so the caller never takes its
// header's address for the call and the header stays in registers on the hot
// path. The cold edge calls a twin that holds the words in its own place for
// the definition, which keeps its body and its pointer ABI for every other
// call. The twin loads the words back after the call whatever its status, and
// the caller stores them before it tests the status: both edges see the
// callee's final header. A loan is exclusive or frozen for the whole call
// (OWN-11), so no other path observes the place while its words travel.
// `convention` and `registers` are the target's twin calling convention and how
// many words a twin returns. On x86-64 and AArch64 twins use preserve_most: the
// cold callee saves the registers the caller keeps live, so the hot path holds
// its values across the growth call and the cost sits in the twin. A twin
// returns up to four words, in registers on AArch64; on x86-64 three come back
// in registers and four come back through a caller slot.
static void register_carriers(LLVMModuleRef module, LLVMContextRef context,
                              LLVMCallConv convention, unsigned registers) {
    LLVMBuilderRef builder = LLVMCreateBuilderInContext(context);
    LLVMDIBuilderRef debug = NULL;
    LLVMTypeRef i64 = LLVMInt64TypeInContext(context), i8 = LLVMInt8TypeInContext(context);
    LLVMTypeRef pointer = LLVMPointerTypeInContext(context, 0);
    LLVMTypeRef word_ty[3] = {pointer, i64, i64};
    unsigned cold = LLVMGetEnumAttributeKindForName("cold", 4);
    unsigned sret = LLVMGetEnumAttributeKindForName("sret", 4);
    LLVMValueRef last = LLVMGetLastFunction(module);
    for (LLVMValueRef f = LLVMGetFirstFunction(module); f;) {
        LLVMValueRef next = f == last ? NULL : LLVMGetNextFunction(f);
        Carrier cs[MAX_CARRIERS];
        unsigned n = carriers_of(f, cs);
        LLVMLinkage linkage = LLVMGetLinkage(f);
        unsigned count = LLVMCountParams(f);
        set_carriers(context, f, cs, 0);
        if (!n || !LLVMCountBasicBlocks(f) || count > 64 ||
            (linkage != LLVMInternalLinkage && linkage != LLVMPrivateLinkage)) { f = next; continue; }
        LLVMTypeRef old_ty = LLVMGlobalGetValueType(f), old_ret = LLVMGetReturnType(old_ty);
        // A `mut` header comes back only within the return registers, and
        // never from a function whose result already travels by `sret`.
        unsigned regs = return_words(old_ret);
        for (unsigned i = 0; i < count; ++i)
            if (LLVMGetEnumAttributeAtIndex(f, i + 1, sret)) regs = 99;
        unsigned kept = 0;
        for (unsigned i = 0; i < n; ++i) {
            if (cs[i].kind == 'm') {
                if (regs + 3 > registers) continue;
                regs += 3;
            }
            if (cs[i].index < count &&
                LLVMGetTypeKind(LLVMTypeOf(LLVMGetParam(f, cs[i].index))) == LLVMPointerTypeKind &&
                LLVMGetEnumAttributeAtIndex(f, cs[i].index + 1, sret) == NULL)
                cs[kept++] = cs[i];
        }
        n = kept;
        LLVMValueRef *sites = NULL;
        unsigned calls = n ? direct_calls(f, &sites) : 0, edges = 0;
        for (unsigned k = 0; k < calls; ++k)
            if (LLVMGetCallSiteEnumAttribute(sites[k], LLVMAttributeFunctionIndex, cold))
                sites[edges++] = sites[k];
        if (!edges) { free(sites); f = next; continue; }
        int carried[64] = {0};
        uint64_t bounds[64] = {0};
        unsigned outs = 0;
        for (unsigned i = 0; i < n; ++i) {
            carried[cs[i].index] = cs[i].kind;
            bounds[cs[i].index] = cs[i].bound;
            outs += cs[i].kind == 'm';
        }
        // Twin parameters: each carried pointer becomes three words.
        LLVMTypeRef *old_params = malloc(count * sizeof(*old_params));
        LLVMGetParamTypes(old_ty, old_params);
        LLVMTypeRef *params = malloc((count + 2 * n) * sizeof(*params));
        unsigned np = 0;
        for (unsigned i = 0; i < count; ++i) {
            if (carried[i]) for (unsigned w = 0; w < 3; ++w) params[np++] = word_ty[w];
            else params[np++] = old_params[i];
        }
        int has_ret = LLVMGetTypeKind(old_ret) != LLVMVoidTypeKind;
        LLVMTypeRef result = old_ret;
        if (outs) {
            LLVMTypeRef fields[1 + 3 * MAX_CARRIERS];
            unsigned nf = 0;
            if (has_ret) fields[nf++] = old_ret;
            for (unsigned k = 0; k < outs; ++k)
                for (unsigned w = 0; w < 3; ++w) fields[nf++] = word_ty[w];
            result = LLVMStructTypeInContext(context, fields, nf, 0);
        }
        LLVMTypeRef new_ty = LLVMFunctionType(result, params, np, 0);
        size_t len;
        const char *name = LLVMGetValueName2(f, &len);
        char *label = malloc(len + 8); memcpy(label, name, len); strcpy(label + len, ".words");
        LLVMValueRef twin = LLVMAddFunction(module, label, new_ty); free(label);
        LLVMSetLinkage(twin, LLVMInternalLinkage);
        LLVMSetFunctionCallConv(twin, convention);
        LLVMAddAttributeAtIndex(twin, LLVMAttributeFunctionIndex,
            LLVMCreateEnumAttribute(context, cold, 0));
        // The twin's words: lengths keep their bound; other parameters keep
        // the definition's attributes.
        unsigned range = LLVMGetEnumAttributeKindForName("range", 5), at = 0;
        for (unsigned i = 0; i < count; ++i) {
            if (!carried[i]) {
                copy_attributes(f, i + 1, twin, ++at, 0);
                continue;
            }
            for (unsigned w = 1; w < 3 && bounds[i]; ++w) {
                uint64_t low = 0, high = bounds[i];
                LLVMAddAttributeAtIndex(twin, at + w + 1,
                    LLVMCreateConstantRangeAttribute(context, range, 64, &low, &high));
            }
            at += 3;
        }
        if (!outs) copy_attributes(f, LLVMAttributeReturnIndex, twin, LLVMAttributeReturnIndex, 0);
        // The twin's body: the words into fresh places, the definition's call,
        // the `mut` words back out.
        LLVMMetadataRef scope = worker_scope(module, &debug, twin, LLVMGetSubprogram(f));
        LLVMPositionBuilderAtEnd(builder, LLVMAppendBasicBlockInContext(context, twin, "entry"));
        LLVMSetCurrentDebugLocation2(builder, scope
            ? LLVMDIBuilderCreateDebugLocation(context, 0, 0, scope, NULL) : NULL);
        LLVMTypeRef header = LLVMArrayType2(i8, 24);
        LLVMValueRef *args = malloc(count * sizeof(*args));
        LLVMValueRef places[MAX_CARRIERS];
        unsigned a = 0, nplace = 0;
        for (unsigned i = 0; i < count; ++i) {
            if (!carried[i]) { args[i] = LLVMGetParam(twin, a++); continue; }
            LLVMValueRef place = LLVMBuildAlloca(builder, header, "carrier");
            LLVMSetAlignment(place, 8);
            for (unsigned w = 0; w < 3; ++w) {
                LLVMValueRef offset = LLVMConstInt(i64, w * 8, 0);
                LLVMValueRef p = LLVMBuildGEP2(builder, i8, place, &offset, 1, "at");
                LLVMValueRef s = LLVMBuildStore(builder, LLVMGetParam(twin, a++), p);
                LLVMSetAlignment(s, 8);
            }
            args[i] = place;
            if (carried[i] == 'm') places[nplace++] = place;
        }
        LLVMValueRef inner = LLVMBuildCall2(builder, old_ty, f, args, count, "");
        LLVMSetInstructionCallConv(inner, LLVMGetFunctionCallConv(f));
        // The call states the definition's parameter and result ABI; a carried
        // parameter is the twin's own place, which meets its pointer facts.
        for (unsigned i = 0; i < count; ++i) copy_attributes(f, i + 1, inner, i + 1, 1);
        copy_attributes(f, LLVMAttributeReturnIndex, inner, LLVMAttributeReturnIndex, 1);
        // A call's tail kind starts as none, and LLVM marks no call `tail` that
        // is passed a caller's alloca, so the copy-back reads the places after f.
        if (!outs) {
            if (has_ret) LLVMBuildRet(builder, inner); else LLVMBuildRetVoid(builder);
        } else {
            LLVMValueRef output = LLVMGetPoison(result);
            unsigned slot = 0;
            if (has_ret) output = LLVMBuildInsertValue(builder, output, inner, slot++, "output");
            for (unsigned k = 0; k < nplace; ++k)
                for (unsigned w = 0; w < 3; ++w) {
                    LLVMValueRef offset = LLVMConstInt(i64, w * 8, 0);
                    LLVMValueRef p = LLVMBuildGEP2(builder, i8, places[k], &offset, 1, "at");
                    LLVMValueRef x = LLVMBuildLoad2(builder, word_ty[w], p, "word");
                    LLVMSetAlignment(x, 8);
                    output = LLVMBuildInsertValue(builder, output, x, slot++, "output");
                }
            LLVMBuildRet(builder, output);
        }
        // Each cold call: the caller's header words in, `mut` words back out.
        LLVMValueRef *new_args = malloc(np * sizeof(*new_args));
        for (unsigned k = 0; k < edges; ++k) {
            LLVMValueRef old_call = sites[k];
            LLVMPositionBuilderBefore(builder, old_call);
            LLVMSetCurrentDebugLocation2(builder, LLVMInstructionGetDebugLoc(old_call));
            for (unsigned i = 0; i < count; ++i) args[i] = LLVMGetArgOperand(old_call, i);
            LLVMValueRef r = call_twin(builder, context, twin, new_ty, args, count, carried,
                bounds, outs, has_ret, new_args, np);
            // The call's own hints (cold, noinline) stay on it.
            unsigned na = LLVMGetCallSiteAttributeCount(old_call, LLVMAttributeFunctionIndex);
            LLVMAttributeRef *attrs = malloc((na + 1) * sizeof(*attrs));
            LLVMGetCallSiteAttributes(old_call, LLVMAttributeFunctionIndex, attrs);
            for (unsigned j = 0; j < na; ++j) LLVMAddCallSiteAttribute(LLVMIsACallInst(r)
                ? r : LLVMGetOperand(r, 0), LLVMAttributeFunctionIndex, attrs[j]);
            free(attrs);
            if (has_ret) LLVMReplaceAllUsesWith(old_call, r);
            LLVMInstructionEraseFromParent(old_call);
        }
        free(new_args); free(args); free(params); free(old_params); free(sites);
        f = next;
    }
    finish_worker_debug(debug);
    LLVMDisposeBuilder(builder);
}

// Private register-return adapter. The FORM destination ABI stays exported.
static void register_results(LLVMModuleRef module, LLVMContextRef context) {
    LLVMBuilderRef builder = LLVMCreateBuilderInContext(context);
    LLVMDIBuilderRef debug = NULL;
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
        {
            // An always-inlined body stays so through its adapter.
            unsigned always = LLVMGetEnumAttributeKindForName("alwaysinline", 12);
            if (LLVMGetEnumAttributeAtIndex(f, LLVMAttributeFunctionIndex, always))
                LLVMAddAttributeAtIndex(promoted, LLVMAttributeFunctionIndex,
                    LLVMCreateEnumAttribute(context, always, 0));
        }
        {
            // Its carriers follow, one parameter earlier without the destination.
            Carrier cs[MAX_CARRIERS];
            unsigned nc = carriers_of(f, cs), kept = 0;
            for (unsigned i = 0; i < nc; ++i)
                if (cs[i].index >= 2) { cs[kept] = cs[i]; cs[kept++].index -= 1; }
            set_carriers(context, promoted, cs, kept);
        }
        LLVMMetadataRef scope = worker_scope(module, &debug, promoted, LLVMGetSubprogram(f));
        LLVMSetCurrentDebugLocation2(builder, scope
            ? LLVMDIBuilderCreateDebugLocation(context, 0, 0, scope, NULL) : NULL);
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
            LLVMSetCurrentDebugLocation2(builder, LLVMInstructionGetDebugLoc(old_call));
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
    finish_worker_debug(debug);
    LLVMDisposeBuilder(builder);
}

// A unit's public bodies: its root and the bodies it carries, which every unit
// that carries one defines weak by its code identity.
static int public_body(LLVMValueRef f) {
    LLVMLinkage linkage = LLVMGetLinkage(f);
    return LLVMCountBasicBlocks(f) && (linkage == LLVMExternalLinkage || linkage == LLVMWeakODRLinkage);
}

static void emit_program(const HirselLlvmUnit *units, size_t count,
                          const char *const *exports, size_t export_count,
                          uint8_t speed, const char *triple, const char *cpu, unsigned jobs,
                          HirselLlvmReuse reuse, HirselLlvmDeliver deliver, void *receiver,
                          HirselLlvmOutput *out) {
    unsigned major, minor, patch;
    LLVMGetVersion(&major, &minor, &patch);
    if (major != 23 || minor != 1 || patch != 5) {
        error(out, "LLVM component requires pinned LLVM 23.1.5"); return;
    }
    pthread_once(&initialized, initialize);
    LLVMTargetRef target = NULL;
    char *message = NULL;
    if (LLVMGetTargetFromTriple(triple, &target, &message)) {
        error(out, message); LLVMDisposeMessage(message); return;
    }
    LLVMTargetMachineRef machine = LLVMCreateTargetMachine(target, triple, cpu, "",
        speed ? LLVMCodeGenLevelAggressive : LLVMCodeGenLevelNone, LLVMRelocPIC, LLVMCodeModelDefault);
    if (!machine) { error(out, "LLVM could not create the target machine"); return; }
    LLVMContextRef context = NULL;
    int own_context = 1, discard_names = 0;
    for (size_t i = 0; i < count; ++i) if (units[i].module) { context = LLVMGetModuleContext(units[i].module); own_context = 0; break; }
    if (!context) context = LLVMContextCreate();
    // Standalone emission keeps the original module and symbol order exactly.
    // Release composition goes through the linker, which may reorder globals.
    int standalone = count == 1 && !export_count;
    LLVMModuleRef module = standalone ? NULL : LLVMModuleCreateWithNameInContext("hirsel-core", context);
    void *composer = standalone ? NULL : hirsel_llvm_composer(module);
    LLVMMemoryBufferRef buffer = NULL;
    int failed;
    for (size_t i = 0; i < count; ++i) {
        LLVMModuleRef unit = units[i].module;
        if (unit) {
            if (LLVMGetModuleContext(unit) != context) {
                error(out, "LLVM modules use different contexts");
                goto done;
            }
            failed = 0;
        } else {
        buffer = LLVMCreateMemoryBufferWithMemoryRangeCopy((const char*)units[i].data, units[i].len, "hirsel-core");
        failed = LLVMParseIRInContext2(context, buffer, &unit, &message);
        LLVMDisposeMemoryBuffer(buffer);
        }
        if (failed || !unit) { error(out, message); LLVMDisposeMessage(message); goto done; }
        // The linker resolves each unit's instance-named declarations and
        // renames its private statics/helpers. It consumes the source module.
        if (standalone) module = unit;
        else if (hirsel_llvm_compose(composer, unit)) { error(out, "LLVM module composition failed"); goto done; }
    }
    hirsel_llvm_composer_free(composer);
    composer = NULL;
    // Local value names only label printed IR. Every unit is parsed, so the
    // optimizer and code generator stop naming the values they create.
    discard_names = !standalone && !LLVMContextShouldDiscardValueNames(context);
    if (discard_names) LLVMContextSetDiscardValueNames(context, 1);
    if (export_count) {
        for (LLVMValueRef f = LLVMGetFirstFunction(module); f; f = LLVMGetNextFunction(f)) {
            if (!public_body(f)) continue;
            size_t len;
            const char *name = LLVMGetValueName2(f, &len);
            int exported = 0;
            for (size_t i = 0; i < export_count; ++i)
                if (strlen(exports[i]) == len && !memcmp(name, exports[i], len)) exported = 1;
            if (!exported) LLVMSetLinkage(f, LLVMInternalLinkage);
        }
    }
    if (speed && export_count) {
        hirsel_llvm_merge_glue(module);
        register_results(module, context);
        int x86 = !strncmp(triple, "x86_64", 6), arm = !strncmp(triple, "aarch64", 7) ||
            !strncmp(triple, "arm64", 5);
        register_carriers(module, context, x86 || arm ? LLVMPreserveMostCallConv : LLVMFastCallConv,
            x86 || arm ? 4 : 2);
        // These passes rewrite signatures and calls by hand: a malformed rewrite
        // stops the build here instead of reaching code generation.
        char *message = NULL;
        int broken = LLVMVerifyModule(module, LLVMReturnStatusAction, &message);
        if (broken) error(out, message ? message : "bridge passes produced invalid IR");
        LLVMDisposeMessage(message);
        if (broken) goto done;
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
        // Composition leaves local bodies that no export reaches. Remove
        // them before SROA and instruction combining visit the whole module.
        const char *pipeline = export_count
            ? "globaldce,function(sroa),hotcoldsplit,default<O3>"
            : "function(sroa),hotcoldsplit,default<O3>";
        hirsel_llvm_fixed_width_checks(module);
        LLVMErrorRef e = LLVMRunPasses(module, pipeline, machine, options);
        if (!e) {
            hirsel_llvm_mark_loop_callbacks(module);
            e = LLVMRunPasses(module,
                "cgscc(argpromotion),ipsccp,partial-inliner,cgscc(inline)", machine, options);
        }
        if (!e) {
            hirsel_llvm_inline_loop_callbacks(module);
            hirsel_llvm_inline_recursive_leaves(module);
            hirsel_llvm_guard_copy_loops(module, machine);
            hirsel_llvm_promote_step_window(module);
            hirsel_scalarize_aggregate_phis(module, context);
            e = LLVMRunPasses(module,
                "cgscc(argpromotion),function(sroa,early-cse,correlated-propagation,instcombine<no-verify-fixpoint>,jump-threading,constraint-elimination,gvn,simplifycfg,dse,loop-simplify,lcssa,instcombine<no-verify-fixpoint>,simplifycfg,dse)",
                machine, options);
        }
        if (!e) {
            hirsel_llvm_finish_reads(module, machine);
            hirsel_llvm_reuse_step_flags(module);
            hirsel_llvm_share_cold_sites(module);
            hirsel_cold_failure_edges(module, context);
            hirsel_llvm_share_cold_step_sync(module, machine);
        }
        LLVMDisposePassBuilderOptions(options);
        if (e) {
            char *text = LLVMGetErrorMessage(e); error(out, text);
            LLVMDisposeErrorMessage(text); goto done;
        }
    }
    if (!speed) hirsel_llvm_promote_development_slots(module);
    LLVMAttributeRef probe = LLVMCreateStringAttribute(context, "probe-stack", 11, "inline-asm", 10);
    LLVMAttributeRef interval = LLVMCreateStringAttribute(context, "stack-probe-size", 16, "4096", 4);
#ifdef HIRSEL_PROFILE_FRAME_POINTERS
    LLVMAttributeRef frame_pointer = LLVMCreateStringAttribute(context, "frame-pointer", 13, "all", 3);
#endif
    for (LLVMValueRef f = LLVMGetFirstFunction(module); f; f = LLVMGetNextFunction(f)) {
        if (LLVMCountBasicBlocks(f)) {
#ifdef HIRSEL_PROFILE_FRAME_POINTERS
            LLVMAddAttributeAtIndex(f, LLVMAttributeFunctionIndex, frame_pointer);
#endif
            LLVMAddAttributeAtIndex(f, LLVMAttributeFunctionIndex, probe);
            LLVMAddAttributeAtIndex(f, LLVMAttributeFunctionIndex, interval);
        }
    }
    buffer = NULL; message = NULL;
    // A program is keyed in parts, one per definition, and what no caller
    // holds is generated per part on `jobs` threads, or with `jobs` 0 as one
    // object to cut per part; a standalone module, which keeps its order, as
    // one object.
    if (export_count) failed = hirsel_llvm_emit_parts(module, machine, jobs, reuse, deliver, receiver, &message);
    else {
        failed = LLVMTargetMachineEmitToMemoryBuffer(machine, module, LLVMObjectFile, &message, &buffer);
        if (!failed && buffer) {
            if (deliver) deliver(receiver, (const uint8_t *)LLVMGetBufferStart(buffer), LLVMGetBufferSize(buffer));
            else copy(out, LLVMGetBufferStart(buffer), LLVMGetBufferSize(buffer), 0);
            LLVMDisposeMemoryBuffer(buffer);
        }
    }
    if (failed) error(out, message ? message : "LLVM object emission failed");
    else if (deliver) copy(out, "", 0, 0);
    if (message) LLVMDisposeMessage(message);
done:
    if (composer) hirsel_llvm_composer_free(composer);
    if (module) LLVMDisposeModule(module);
    // A caller's context parses text units again in its next program.
    if (discard_names && !own_context) LLVMContextSetDiscardValueNames(context, 0);
    if (own_context) LLVMContextDispose(context); LLVMDisposeTargetMachine(machine);
}
// Emit units as parts (exports given) or as one standalone object: `reuse` is
// asked first for each part, `deliver` gets each missed part's object on
// `jobs` threads, or with `jobs` 0 the one object generated, if any, and `out`
// is empty or holds the error.
__attribute__((visibility("default")))
void hirsel_llvm_program_v6(const HirselLlvmUnit *units, size_t count,
                          const char *const *exports, size_t export_count,
                          uint8_t speed, const char *triple, const char *cpu, unsigned jobs,
                          HirselLlvmReuse reuse, HirselLlvmDeliver deliver, void *receiver,
                          HirselLlvmOutput *out) {
    emit_program(units, count, exports, export_count, speed, triple, cpu, jobs, reuse, deliver, receiver, out);
}
__attribute__((visibility("default")))
void hirsel_llvm_compile_v1(const uint8_t *ir, size_t len, uint8_t speed, const char *triple, const char *cpu, HirselLlvmOutput *out) {
    HirselLlvmUnit unit = { ir, len, NULL };
    emit_program(&unit, 1, NULL, 0, speed, triple, cpu, 0, NULL, NULL, NULL, out);
}

extern const void *hirsel_llvm_no_folder_api(void);
__attribute__((visibility("default")))
const void *hirsel_llvm_builder_v1(void) { return hirsel_llvm_no_folder_api(); }
