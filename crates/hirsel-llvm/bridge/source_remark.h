#pragma once

#include <llvm/IR/DebugInfoMetadata.h>
#include <llvm/IR/Instruction.h>

namespace hirsel_llvm {
enum RemarkKind : uint32_t { BoundsCheck, OverflowCheck, StepSync };
using SourceRemark = void (*)(void *, unsigned, const char *, size_t, const char *, size_t,
                              const char *, size_t, unsigned);

inline void remark(SourceRemark report, void *receiver, RemarkKind kind,
                   const llvm::Instruction &instruction) {
    if (!report) return;
    const auto &loc = instruction.getDebugLoc();
    if (!loc || !loc.getLine()) return;
    auto *scope = loc->getScope()->getSubprogram();
    if (!scope || scope->getLinkageName().empty()) return;
    auto name = scope->getName(), linkage = scope->getLinkageName(), file = loc->getFilename();
    if (name.empty() || file.empty()) return;
    report(receiver, kind, name.data(), name.size(), linkage.data(), linkage.size(),
           file.data(), file.size(), loc.getLine() - 1);
}
}
