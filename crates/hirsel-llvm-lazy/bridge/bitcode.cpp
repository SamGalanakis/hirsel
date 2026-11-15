// Preserve the raw parser's use-list order as well as its instructions. LLVM's
// default bitcode order may otherwise change an application's optimization.
#include <llvm-c/Core.h>
#include <llvm/ADT/SmallVector.h>
#include <llvm/Bitcode/BitcodeWriter.h>
#include <llvm/IR/Module.h>
#include <llvm/Linker/Linker.h>
#include <llvm/Support/CBindingWrapping.h>
#include <llvm/Support/raw_ostream.h>
#include <llvm/Transforms/IPO/MergeFunctions.h>
extern "C" LLVMMemoryBufferRef hirsel_llvm_raw_bitcode(LLVMModuleRef module) {
    llvm::SmallVector<char, 0> bytes;
    llvm::raw_svector_ostream stream(bytes);
    llvm::WriteBitcodeToFile(*llvm::unwrap(module), stream, true);
    return LLVMCreateMemoryBufferWithMemoryRangeCopy(bytes.data(), bytes.size(), "hirsel-core");
}

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
