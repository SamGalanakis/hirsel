#include <bits/c++config.h>
#if defined(__GLIBC__) && !__GLIBC_PREREQ(2, 30)
#undef _GLIBCXX_USE_PTHREAD_COND_CLOCKWAIT
#undef _GLIBCXX_USE_PTHREAD_MUTEX_CLOCKLOCK
#undef _GLIBCXX_USE_PTHREAD_RWLOCK_CLOCKLOCK
#endif

#include "pass_cost.h"

#include <cerrno>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <string>
#include <vector>

#include <linux/perf_event.h>
#include <sys/syscall.h>
#include <unistd.h>

#include <llvm/Analysis/LazyCallGraph.h>
#include <llvm/Analysis/LoopInfo.h>
#include <llvm/IR/Module.h>
#include <llvm/IR/PassInstrumentation.h>
#include <llvm/Support/ErrorHandling.h>
#include <llvm/Support/JSON.h>

using namespace llvm;

namespace {

std::FILE *traceFile() {
    static auto *file = []() -> std::FILE * {
        const char *path = std::getenv("HIRSEL_LLVM_PASS_COST");
        if (!path) return nullptr;
        auto *output = std::fopen(path, "a");
        if (!output)
            report_fatal_error(Twine("open LLVM pass cost output: ") + std::strerror(errno));
        return output;
    }();
    return file;
}

struct Counter {
    uint64_t instructions, enabled, running;
};

int openCounter() {
    perf_event_attr event{};
    event.size = sizeof(event);
    event.type = PERF_TYPE_HARDWARE;
    event.config = PERF_COUNT_HW_INSTRUCTIONS;
    event.exclude_kernel = 1;
    event.exclude_hv = 1;
    event.pinned = 1;
    event.read_format = PERF_FORMAT_TOTAL_TIME_ENABLED | PERF_FORMAT_TOTAL_TIME_RUNNING;
    int fd = syscall(SYS_perf_event_open, &event, 0, -1, -1, PERF_FLAG_FD_CLOEXEC);
    if (fd < 0)
        report_fatal_error(Twine("open LLVM pass instructions:u counter: ") + std::strerror(errno));
    return fd;
}

Counter readCounter(int fd) {
    Counter count;
    ssize_t bytes;
    do {
        bytes = read(fd, &count, sizeof(count));
    } while (bytes < 0 && errno == EINTR);
    if (bytes != sizeof(count))
        report_fatal_error("could not read the pinned LLVM pass instructions:u counter");
    return count;
}

std::string moduleName(const Module &module) {
    return module.getName().empty() ? "[module]" : module.getName().str();
}

std::string irName(const Any &ir) {
    if (const auto *m = any_cast<const Module *>(&ir)) return moduleName(**m);
    if (const auto *f = any_cast<const Function *>(&ir)) return (*f)->getName().str();
    if (const auto *scc = any_cast<const LazyCallGraph::SCC *>(&ir)) return (*scc)->getName();
    if (const auto *l = any_cast<const Loop *>(&ir))
        return ("loop %" + (*l)->getName() + " in " + (*l)->getHeader()->getParent()->getName()).str();
    report_fatal_error("unknown LLVM pass cost IR unit");
}

void writeRecord(std::FILE *file, json::Object fields) {
    std::string line;
    raw_string_ostream stream(line);
    stream << json::Value(std::move(fields)) << '\n';
    if (std::fwrite(line.data(), 1, line.size(), file) != line.size())
        report_fatal_error("could not write LLVM pass cost record");
}

} // namespace

namespace hirsel_llvm {

struct PassCost::Trace {
    struct Frame {
        StringRef pass;
        std::string unit;
        uint64_t start = 0;
        uint64_t instructions = 0;
    };

    std::FILE *file;
    StringRef phase;
    std::string module;
    int fd = openCounter();
    Counter start = readCounter(fd);
    std::vector<Frame> stack;

    Trace(std::FILE *file, StringRef phase, const Module &module)
        : file(file), phase(phase), module(moduleName(module)) {}

    ~Trace() {
        Counter end = readCounter(fd);
        close(fd);
        if (end.enabled - start.enabled != end.running - start.running)
            report_fatal_error("LLVM pass instructions:u counter was multiplexed");
        writeRecord(file, json::Object{{"phase", phase}, {"ir_unit", module},
                           {"pipeline_instructions", end.instructions - start.instructions}});
        if (std::fflush(file) != 0)
            report_fatal_error("could not flush LLVM pass cost records");
    }

    void before(StringRef pass, const Any &ir) {
        if (!stack.empty()) {
            auto &parent = stack.back();
            parent.instructions += readCounter(fd).instructions - parent.start;
        }
        stack.push_back({pass, irName(ir)});
        stack.back().start = readCounter(fd).instructions;
    }

    void after() {
        uint64_t end = readCounter(fd).instructions;
        auto &frame = stack.back();
        writeRecord(file, json::Object{{"phase", phase}, {"pass", frame.pass}, {"ir_unit", frame.unit},
                           {"instructions", frame.instructions + end - frame.start}});
        stack.pop_back();
        if (!stack.empty()) stack.back().start = readCounter(fd).instructions;
    }
};

PassCost::PassCost(StringRef phase, const Module &module) {
    if (auto *file = traceFile()) trace = std::make_unique<Trace>(file, phase, module);
}

PassCost::~PassCost() = default;

void PassCost::registerCallbacks(PassInstrumentationCallbacks &callbacks) {
    if (!trace) return;
    callbacks.registerBeforeNonSkippedPassCallback(
        [this](StringRef pass, const Any &ir) { trace->before(pass, ir); });
    callbacks.registerAfterPassCallback(
        [this](StringRef, const Any &, const PreservedAnalyses &) { trace->after(); }, true);
    callbacks.registerAfterPassInvalidatedCallback(
        [this](StringRef, const PreservedAnalyses &) { trace->after(); }, true);
}

} // namespace hirsel_llvm
