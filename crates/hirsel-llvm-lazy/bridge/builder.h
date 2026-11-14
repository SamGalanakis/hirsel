#ifndef HIRSEL_LLVM_BUILDER_H
#define HIRSEL_LLVM_BUILDER_H
#include <stddef.h>
#include <stdint.h>
#include <llvm-c/Core.h>
#ifdef __cplusplus
extern "C" {
#endif

typedef struct HirselLlvmBuilder HirselLlvmBuilder;
typedef struct { uint8_t *data; size_t len; uint8_t failed; } HirselBuilderOutput;
typedef struct { uint8_t kind; size_t index; } HirselLocalName;
/* Same-build component ABI. Primitive operations never implement CoreIR rules. */
typedef struct {
    LLVMContextRef (*context_new)(void);
    void (*context_drop)(LLVMContextRef);
    HirselLlvmBuilder *(*unit_new)(LLVMContextRef, const char *, size_t);
    void (*unit_drop)(HirselLlvmBuilder *);
    LLVMModuleRef (*unit_take)(HirselLlvmBuilder *);
    void (*unit_bitcode)(HirselLlvmBuilder *, HirselBuilderOutput *);
    void (*print)(HirselLlvmBuilder *, HirselBuilderOutput *);
    void (*print_text)(const char *, size_t, HirselBuilderOutput *);
    LLVMTypeRef (*integer_type)(HirselLlvmBuilder *, unsigned);
    LLVMTypeRef (*float_type)(HirselLlvmBuilder *, unsigned);
    LLVMTypeRef (*pointer_type)(HirselLlvmBuilder *);
    LLVMTypeRef (*void_type)(HirselLlvmBuilder *);
    LLVMTypeRef (*array_type)(LLVMTypeRef, uint64_t);
    LLVMTypeRef (*struct_type)(HirselLlvmBuilder *, LLVMTypeRef const *, size_t, uint8_t);
    LLVMValueRef (*integer)(LLVMTypeRef, uint64_t, uint64_t);
    LLVMValueRef (*poison)(LLVMTypeRef);
    LLVMValueRef (*zero)(LLVMTypeRef);
    LLVMValueRef (*function)(HirselLlvmBuilder *, const char *, size_t,
                             LLVMTypeRef, LLVMTypeRef const *, size_t);
    void (*define)(HirselLlvmBuilder *, LLVMValueRef, unsigned);
    LLVMValueRef (*parameter)(LLVMValueRef, unsigned, HirselLocalName);
    LLVMBasicBlockRef (*block)(HirselLlvmBuilder *, const char *, size_t);
    void (*begin_block)(HirselLlvmBuilder *, LLVMBasicBlockRef);
    void (*name)(LLVMValueRef, HirselLocalName);
    LLVMValueRef (*local)(HirselLlvmBuilder *, LLVMTypeRef, HirselLocalName);
    void (*bind)(HirselLlvmBuilder *, HirselLocalName, LLVMValueRef);
    LLVMValueRef (*alloca)(HirselLlvmBuilder *, LLVMTypeRef, unsigned, HirselLocalName);
    LLVMValueRef (*binary)(HirselLlvmBuilder *, unsigned, LLVMValueRef, LLVMValueRef);
    LLVMValueRef (*cast)(HirselLlvmBuilder *, unsigned, LLVMValueRef, LLVMTypeRef);
    LLVMValueRef (*compare)(HirselLlvmBuilder *, unsigned, LLVMValueRef, LLVMValueRef);
    LLVMValueRef (*select)(HirselLlvmBuilder *, LLVMValueRef, LLVMValueRef, LLVMValueRef);
    LLVMValueRef (*load)(HirselLlvmBuilder *, LLVMTypeRef, LLVMValueRef, unsigned);
    LLVMValueRef (*store)(HirselLlvmBuilder *, LLVMValueRef, LLVMValueRef, unsigned);
    LLVMValueRef (*gep)(HirselLlvmBuilder *, LLVMTypeRef, LLVMValueRef, LLVMValueRef const *, size_t);
    LLVMValueRef (*extract)(HirselLlvmBuilder *, LLVMValueRef, unsigned);
    LLVMValueRef (*insert)(HirselLlvmBuilder *, LLVMValueRef, LLVMValueRef, unsigned);
    LLVMValueRef (*phi)(HirselLlvmBuilder *, LLVMTypeRef, LLVMValueRef const *, LLVMBasicBlockRef const *, size_t);
    LLVMValueRef (*call)(HirselLlvmBuilder *, LLVMTypeRef, LLVMTypeRef const *, size_t,
                         LLVMValueRef, LLVMValueRef const *, size_t);
    void (*branch)(HirselLlvmBuilder *, LLVMBasicBlockRef);
    LLVMValueRef (*conditional)(HirselLlvmBuilder *, LLVMValueRef, LLVMBasicBlockRef, LLVMBasicBlockRef);
    void (*return_value)(HirselLlvmBuilder *, LLVMValueRef);
    void (*unreachable)(HirselLlvmBuilder *);
    LLVMTypeRef (*vector_type)(LLVMTypeRef, unsigned);
    LLVMValueRef (*constant_cast)(unsigned, LLVMValueRef, LLVMTypeRef);
    LLVMValueRef (*constant_add)(LLVMValueRef, LLVMValueRef);
    LLVMValueRef (*global)(HirselLlvmBuilder *, const char *, size_t, LLVMTypeRef);
    LLVMValueRef (*constant_struct)(HirselLlvmBuilder *, LLVMValueRef const *, size_t, uint8_t);
    LLVMValueRef (*constant_array)(LLVMTypeRef, LLVMValueRef const *, size_t);
    void (*initialize_global)(LLVMValueRef, LLVMValueRef, unsigned, uint8_t, uint8_t, unsigned);
    void (*module_asm)(HirselLlvmBuilder *, const char *, size_t);
    void (*attribute)(HirselLlvmBuilder *, LLVMValueRef, unsigned, unsigned, LLVMTypeRef);
    void (*string_attribute)(HirselLlvmBuilder *, LLVMValueRef, const char *, size_t, const char *, size_t);
    void (*tail)(LLVMValueRef);
    LLVMValueRef (*fneg)(HirselLlvmBuilder *, LLVMValueRef);
    LLVMValueRef (*atomic_load)(HirselLlvmBuilder *, LLVMTypeRef, LLVMValueRef, unsigned, unsigned);
    void (*atomic_store)(HirselLlvmBuilder *, LLVMValueRef, LLVMValueRef, unsigned, unsigned);
    LLVMValueRef (*atomic_rmw)(HirselLlvmBuilder *, unsigned, LLVMValueRef, LLVMValueRef, unsigned);
    LLVMValueRef (*compare_exchange)(HirselLlvmBuilder *, LLVMValueRef, LLVMValueRef, LLVMValueRef, unsigned, unsigned);
    void (*fence)(HirselLlvmBuilder *, unsigned);
    void (*switch_value)(HirselLlvmBuilder *, LLVMValueRef, LLVMBasicBlockRef, LLVMValueRef const *, LLVMBasicBlockRef const *, size_t);
    LLVMMetadataRef (*metadata_ref)(HirselLlvmBuilder *, size_t);
    LLVMMetadataRef (*metadata_string)(HirselLlvmBuilder *, const char *, size_t);
    LLVMMetadataRef (*metadata_integer)(HirselLlvmBuilder *, unsigned, uint64_t);
    LLVMMetadataRef (*metadata_tuple)(HirselLlvmBuilder *, LLVMMetadataRef const *, size_t, uint8_t);
    void (*metadata_bind)(HirselLlvmBuilder *, size_t, LLVMMetadataRef);
    void (*named_metadata)(HirselLlvmBuilder *, const char *, size_t, LLVMMetadataRef const *, size_t);
    void (*attach)(HirselLlvmBuilder *, LLVMValueRef, const char *, size_t, LLVMMetadataRef);
    void (*location)(HirselLlvmBuilder *, LLVMMetadataRef);
    LLVMValueRef (*metadata_value)(HirselLlvmBuilder *, LLVMMetadataRef);

} HirselLlvmBuilderApi;
const HirselLlvmBuilderApi *hirsel_llvm_no_folder_api(void);
#ifdef __cplusplus
}
#endif
#endif
