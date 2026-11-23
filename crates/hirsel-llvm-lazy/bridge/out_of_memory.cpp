// LLVM's allocation failures end the process through the compiler's memory
// guard (hirsel-guard), naming the limit and the work, as a refused Rust
// allocation does, rather than aborting with "LLVM ERROR: out of memory".
#include <llvm/Support/ErrorHandling.h>
#include <new>

namespace {
using Handler = void (*)(const char *reason);
Handler handler = nullptr;
void bad_alloc(void *, const char *reason, bool) { handler(reason); }
void new_failed() { handler("operator new failed"); }
} // namespace

// `handler` does not return. Installed once, before any LLVM work.
extern "C" __attribute__((visibility("default"))) void hirsel_llvm_out_of_memory_v1(Handler h) {
    handler = h;
    llvm::install_bad_alloc_error_handler(bad_alloc, nullptr);
    std::set_new_handler(new_failed);
}

// The engine's and builder's own buffers fail the same way.
extern "C" [[noreturn]] void hirsel_llvm_allocation_failed(const char *what) { llvm::report_bad_alloc_error(what); }
