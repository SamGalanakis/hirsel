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
/* Same-build component ABI. Each operation builds what the LLVM assembly
   parser builds for the corresponding text; CoreIR rules stay in Rust. */
typedef struct {
    LLVMContextRef (*context_new)(void);
    void (*context_drop)(LLVMContextRef);
    HirselLlvmBuilder *(*unit_new)(LLVMContextRef, const char *, size_t);
    void (*unit_drop)(HirselLlvmBuilder *);
    LLVMModuleRef (*unit_take)(HirselLlvmBuilder *, HirselBuilderOutput *);
    void (*module_drop)(LLVMModuleRef);
    void (*unit_bitcode)(HirselLlvmBuilder *, HirselBuilderOutput *);
    void (*print)(HirselLlvmBuilder *, HirselBuilderOutput *);
    void (*print_text)(const char *, size_t, HirselBuilderOutput *);
    LLVMTypeRef (*integer_type)(HirselLlvmBuilder *, unsigned);
    LLVMTypeRef (*float_type)(HirselLlvmBuilder *, unsigned);
    LLVMTypeRef (*pointer_type)(HirselLlvmBuilder *);
    LLVMTypeRef (*void_type)(HirselLlvmBuilder *);
    LLVMTypeRef (*metadata_type)(HirselLlvmBuilder *);
    LLVMTypeRef (*array_type)(LLVMTypeRef, uint64_t);
    LLVMTypeRef (*vector_type)(LLVMTypeRef, unsigned);
    LLVMTypeRef (*struct_type)(HirselLlvmBuilder *, LLVMTypeRef const *, size_t, uint8_t);
    LLVMValueRef (*integer)(LLVMTypeRef, uint64_t, uint64_t);
    LLVMValueRef (*poison)(LLVMTypeRef);
    LLVMValueRef (*zero)(LLVMTypeRef);
    LLVMValueRef (*constant_cast)(unsigned, LLVMValueRef, LLVMTypeRef);
    LLVMValueRef (*constant_add)(LLVMValueRef, LLVMValueRef);
    LLVMValueRef (*constant_struct)(HirselLlvmBuilder *, LLVMValueRef const *, size_t, uint8_t);
    LLVMValueRef (*constant_bytes)(HirselLlvmBuilder *, const uint8_t *, size_t);
    LLVMValueRef (*global_ref)(HirselLlvmBuilder *, const char *, size_t);
    void (*global)(HirselLlvmBuilder *, const char *, size_t, LLVMTypeRef, LLVMValueRef,
                   unsigned, uint8_t, uint8_t, unsigned, uint8_t);
    LLVMValueRef (*function)(HirselLlvmBuilder *, const char *, size_t, LLVMTypeRef,
                             LLVMTypeRef const *, size_t, unsigned);
    void (*attribute)(HirselLlvmBuilder *, LLVMValueRef, unsigned, unsigned, LLVMTypeRef, uint64_t);
    void (*string_attribute)(HirselLlvmBuilder *, LLVMValueRef, const char *, size_t, const char *, size_t);
    void (*finish_declaration)(LLVMValueRef);
    void (*subprogram)(LLVMValueRef, LLVMMetadataRef);
    void (*begin_body)(HirselLlvmBuilder *, LLVMValueRef);
    LLVMValueRef (*parameter)(LLVMValueRef, unsigned);
    void (*set_name)(LLVMValueRef, const char *, size_t);
    LLVMBasicBlockRef (*block)(HirselLlvmBuilder *, const char *, size_t);
    void (*begin_block)(HirselLlvmBuilder *, LLVMBasicBlockRef);
    uint8_t (*block_placed)(LLVMBasicBlockRef);
    LLVMValueRef (*placeholder)(LLVMTypeRef);
    void (*replace)(LLVMValueRef, LLVMValueRef);
    LLVMValueRef (*alloca)(HirselLlvmBuilder *, LLVMTypeRef, unsigned);
    LLVMValueRef (*binary)(HirselLlvmBuilder *, unsigned, LLVMValueRef, LLVMValueRef);
    LLVMValueRef (*cast)(HirselLlvmBuilder *, unsigned, LLVMValueRef, LLVMTypeRef);
    LLVMValueRef (*compare)(HirselLlvmBuilder *, unsigned, LLVMValueRef, LLVMValueRef);
    LLVMValueRef (*select)(HirselLlvmBuilder *, LLVMValueRef, LLVMValueRef, LLVMValueRef);
    LLVMValueRef (*fneg)(HirselLlvmBuilder *, LLVMValueRef);
    LLVMValueRef (*load)(HirselLlvmBuilder *, LLVMTypeRef, LLVMValueRef, unsigned, unsigned);
    LLVMValueRef (*store)(HirselLlvmBuilder *, LLVMValueRef, LLVMValueRef, unsigned, unsigned);
    LLVMValueRef (*gep)(HirselLlvmBuilder *, LLVMTypeRef, LLVMValueRef, LLVMValueRef const *, size_t, uint8_t);
    LLVMValueRef (*extract)(HirselLlvmBuilder *, LLVMValueRef, unsigned);
    LLVMValueRef (*insert)(HirselLlvmBuilder *, LLVMValueRef, LLVMValueRef, unsigned);
    LLVMValueRef (*phi)(HirselLlvmBuilder *, LLVMTypeRef, LLVMValueRef const *, LLVMBasicBlockRef const *, size_t);
    LLVMValueRef (*call)(HirselLlvmBuilder *, LLVMTypeRef, LLVMTypeRef const *, size_t,
                         LLVMValueRef, LLVMValueRef const *, size_t, uint8_t);
    LLVMValueRef (*compare_exchange)(HirselLlvmBuilder *, LLVMValueRef, LLVMValueRef, LLVMValueRef);
    LLVMValueRef (*atomic_sub)(HirselLlvmBuilder *, LLVMValueRef, LLVMValueRef);
    LLVMValueRef (*fence)(HirselLlvmBuilder *, unsigned);
    LLVMValueRef (*branch)(HirselLlvmBuilder *, LLVMBasicBlockRef);
    LLVMValueRef (*conditional)(HirselLlvmBuilder *, LLVMValueRef, LLVMBasicBlockRef, LLVMBasicBlockRef);
    LLVMValueRef (*switch_value)(HirselLlvmBuilder *, LLVMValueRef, LLVMBasicBlockRef, LLVMValueRef const *, LLVMBasicBlockRef const *, size_t);
    LLVMValueRef (*return_value)(HirselLlvmBuilder *, LLVMValueRef);
    LLVMValueRef (*unreachable)(HirselLlvmBuilder *);
    void (*module_asm)(HirselLlvmBuilder *, const char *, size_t);
    LLVMMetadataRef (*metadata_ref)(HirselLlvmBuilder *, size_t);
    LLVMMetadataRef (*metadata_string)(HirselLlvmBuilder *, const char *, size_t);
    LLVMMetadataRef (*metadata_integer)(HirselLlvmBuilder *, unsigned, uint64_t);
    LLVMMetadataRef (*metadata_tuple)(HirselLlvmBuilder *, LLVMMetadataRef const *, size_t, uint8_t);
    void (*metadata_bind)(HirselLlvmBuilder *, size_t, LLVMMetadataRef);
    void (*named_metadata)(HirselLlvmBuilder *, const char *, size_t, LLVMMetadataRef const *, size_t);
    void (*attach)(HirselLlvmBuilder *, LLVMValueRef, unsigned, LLVMMetadataRef);
    void (*location)(HirselLlvmBuilder *, LLVMValueRef, LLVMMetadataRef);
    LLVMValueRef (*metadata_value)(HirselLlvmBuilder *, LLVMMetadataRef);
    LLVMMetadataRef (*di_file)(HirselLlvmBuilder *, const char *, size_t);
    LLVMMetadataRef (*di_compile_unit)(HirselLlvmBuilder *, LLVMMetadataRef);
    LLVMMetadataRef (*di_subroutine_type)(HirselLlvmBuilder *, LLVMMetadataRef);
    LLVMMetadataRef (*di_subprogram)(HirselLlvmBuilder *, const char *, size_t, const char *, size_t,
                                     LLVMMetadataRef, LLVMMetadataRef, LLVMMetadataRef);
    LLVMMetadataRef (*di_location)(HirselLlvmBuilder *, unsigned, unsigned, LLVMMetadataRef);
    LLVMMetadataRef (*di_lexical_block_file)(HirselLlvmBuilder *, LLVMMetadataRef, LLVMMetadataRef);
} HirselLlvmBuilderApi;
const HirselLlvmBuilderApi *hirsel_llvm_no_folder_api(void);
#ifdef __cplusplus
}
#endif
#endif
