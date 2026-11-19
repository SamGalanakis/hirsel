#pragma once

#include <memory>

#include <llvm/ADT/StringRef.h>

namespace llvm {
class Module;
class PassInstrumentationCallbacks;
}

namespace hirsel_llvm {

class PassCost {
public:
    PassCost(llvm::StringRef phase, const llvm::Module &module);
    ~PassCost();
    void registerCallbacks(llvm::PassInstrumentationCallbacks &callbacks);

private:
    struct Trace;
    std::unique_ptr<Trace> trace;
};

} // namespace hirsel_llvm
