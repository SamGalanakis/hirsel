// C entries for the core pipeline's post-O3 module passes.
#include <cstddef>
#include "aggregate_phi.h"
#include "failure_edges.h"
extern "C" void hirsel_llvm_cold_failure_edges(LLVMModuleRef module,
                                               LLVMContextRef context) {
  hirsel_cold_failure_edges(module, context);
}
extern "C" void hirsel_llvm_scalarize_aggregate_phis(LLVMModuleRef module,
                                                    LLVMContextRef context) {
  hirsel_scalarize_aggregate_phis(module, context);
}
