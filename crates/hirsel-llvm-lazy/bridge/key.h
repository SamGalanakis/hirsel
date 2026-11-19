// The key of a module: see key.cpp.
#pragma once

#include <array>
#include <cstdint>

namespace llvm {
class Module;
}

namespace hirsel_key {
using Digest = std::array<uint8_t, 32>;

// A BLAKE3 digest of every fact the module's bitcode records, computed from
// the module in memory. Two modules have the same key exactly when they write
// the same bitcode, up to its layout.
Digest module_key(const llvm::Module &M);
} // namespace hirsel_key
