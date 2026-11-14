// Preserve the raw parser's use-list order as well as its instructions. LLVM's
// default bitcode order may otherwise change an application's optimization.
#include <llvm-c/Core.h>
#include <llvm/ADT/SmallVector.h>
#include <llvm/Bitcode/BitcodeWriter.h>
#include <llvm/IR/Module.h>
#include <llvm/Support/CBindingWrapping.h>
#include <llvm/Support/raw_ostream.h>
extern "C" LLVMMemoryBufferRef hirsel_llvm_raw_bitcode(LLVMModuleRef module) {
    llvm::SmallVector<char, 0> bytes;
    llvm::raw_svector_ostream stream(bytes);
    llvm::WriteBitcodeToFile(*llvm::unwrap(module), stream, true);
    return LLVMCreateMemoryBufferWithMemoryRangeCopy(bytes.data(), bytes.size(), "hirsel-core");
}
