// LLVM 23.1.4 TargetMachineC.cpp:201-209 constructs default TargetOptions.
// This option has no C-API setter. Keep it per machine, never process-global.
#include <llvm/Target/TargetMachine.h>
#include "aggregate_phi.h"
#include "numeric_nan.h"
extern "C" void hirsel_llvm_scalarize_aggregate_phis(LLVMModuleRef module,
                                                    LLVMContextRef context) {
  hirsel_scalarize_aggregate_phis(module, context);
}
extern "C" void hirsel_llvm_defer_numerical_nan_maps(LLVMModuleRef module,
                                                   LLVMContextRef context) {
  hirsel_defer_numerical_nan_maps(module, context);
}
extern "C" void hirsel_llvm_emit_stack_sizes(void *machine) noexcept {
  auto &options = static_cast<llvm::TargetMachine *>(machine)->Options;
  options.EmitStackSizeSection = true;
  options.EnableMachineOutliner = false;
  options.GuaranteedTailCallOpt = false;
}
