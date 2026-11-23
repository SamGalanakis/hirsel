#include <llvm-c/Core.h>
#include <llvm/Bitcode/BitcodeReader.h>
#include <llvm/Bitcode/BitcodeWriter.h>
#include <llvm/IR/LLVMContext.h>
#include <llvm/IR/Module.h>
#include <llvm/Linker/Linker.h>
#include <llvm/Support/CBindingWrapping.h>
#include <llvm/Support/Compression.h>
#include <llvm/Support/MemoryBuffer.h>
#include <llvm/Support/raw_ostream.h>
#include <llvm/Transforms/IPO/MergeFunctions.h>
#include <cstring>

// One linker composes every unit of a program. LLVMLinkModules2 builds a
// fresh IRMover per call, and each one rescans the whole destination for its
// struct types, so composition grew with the square of the program. A single
// mover keeps those maps across units and yields the same module, as llvm-link
// does when it composes many files.
extern "C" void *hirsel_llvm_composer(LLVMModuleRef module) {
    return new llvm::Linker(*llvm::unwrap(module));
}
extern "C" int hirsel_llvm_compose(void *composer, LLVMModuleRef unit) {
    return static_cast<llvm::Linker *>(composer)->linkInModule(std::unique_ptr<llvm::Module>(llvm::unwrap(unit)));
}
extern "C" void hirsel_llvm_composer_free(void *composer) { delete static_cast<llvm::Linker *>(composer); }

struct HirselLlvmUnit { const uint8_t *data; size_t len; LLVMModuleRef module; };

extern "C" int hirsel_llvm_join_lent(const HirselLlvmUnit *units, size_t count,
    void (*deliver)(void *, const uint8_t *, size_t), void *receiver, char **error) {
    llvm::LLVMContext context;
    llvm::Module module("hirsel-lent", context);
    llvm::Linker linker(module);
    for (size_t i = 0; i < count; ++i) {
        llvm::StringRef bytes(reinterpret_cast<const char *>(units[i].data), units[i].len);
        auto unit = llvm::getLazyBitcodeModule(llvm::MemoryBufferRef(bytes, "hirsel-lent"), context);
        if (!unit) {
            *error = strdup(llvm::toString(unit.takeError()).c_str());
            return 1;
        }
        // These bodies have no callers until a program borrows them. Keep
        // them through this link even though available_externally lets the
        // linker drop unreferenced definitions.
        for (llvm::Function &function : **unit)
            if (function.hasAvailableExternallyLinkage())
                function.setLinkage(llvm::GlobalValue::ExternalLinkage);
        if (linker.linkInModule(std::move(*unit))) {
            *error = strdup("the release std's lent modules do not link");
            return 1;
        }
    }
    for (llvm::Function &function : module)
        if (!function.isDeclaration())
            function.setLinkage(llvm::GlobalValue::AvailableExternallyLinkage);
    llvm::SmallVector<char, 0> bytes;
    llvm::raw_svector_ostream out(bytes);
    llvm::WriteBitcodeToFile(module, out);
    deliver(receiver, reinterpret_cast<const uint8_t *>(bytes.data()), bytes.size());
    return 0;
}

// Every unit of a program emits the drop and compare glue its own types need,
// so composition renames one body per unit (core_drop_N.1, .2, ...). Merge the
// identical ones before optimisation, so the pipeline optimises each body once.
// MergeFunctions also rewrites the callers whose only difference was a merged
// callee, so whole glue trees collapse.
extern "C" void hirsel_llvm_merge_glue(LLVMModuleRef module) {
    std::vector<llvm::Function *> glue;
    for (llvm::Function &f : *llvm::unwrap(module))
        if (!f.isDeclaration() && f.hasLocalLinkage() &&
            (f.getName().starts_with("core_drop_") || f.getName().starts_with("core_compare_")))
            glue.push_back(&f);
    if (glue.size() > 1) llvm::MergeFunctionsPass::runOnFunctions(glue);
}

// Compress the prepared std object and its detached rows as one store record.
extern "C" int hirsel_llvm_std_object_codec(const uint8_t *data, size_t size, size_t decoded_size,
    void (*deliver)(void *, const uint8_t *, size_t), void *receiver, char **error) {
    namespace zlib = llvm::compression::zlib;
    llvm::SmallVector<uint8_t, 0> bytes;
    llvm::ArrayRef<uint8_t> input(data, size);
    if (!decoded_size) {
        zlib::compress(input, bytes);
    } else if (auto failure = zlib::decompress(input, bytes, decoded_size)) {
        *error = strdup(llvm::toString(std::move(failure)).c_str());
        return 1;
    }
    deliver(receiver, bytes.data(), bytes.size());
    return 0;
}
