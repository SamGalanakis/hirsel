// Share the private part writer with the native driver test. This executable
// is built only for tests.
#include "parts.cpp"
#include <functional>
#include <llvm/Support/TargetSelect.h>

namespace {
std::unique_ptr<TargetMachine> test_machine(StringRef &facts) {
    auto setting = [](StringRef &remaining) {
        auto [line, rest] = remaining.split('\n');
        remaining = rest;
        return line.split(' ').second;
    };
    StringRef settings = facts;
    Triple triple(setting(settings));
    StringRef cpu = setting(settings), features = setting(settings);
    std::string error;
    const Target *target = TargetRegistry::lookupTarget(triple, error);
    if (!target) return nullptr;
    // The x86-64 release machine from the driver. Every property it
    // contributes to the key must agree with the reconstructed machine.
    auto machine = std::unique_ptr<TargetMachine>(target->createTargetMachine(
        triple, cpu, features, TargetOptions(), Reloc::PIC_, std::nullopt,
        CodeGenOptLevel::Aggressive));
    if (!machine || !facts.consume_front(machine_text(*machine))) return nullptr;
    return machine;
}

std::string decoded_part(StringRef code) {
    LLVMContext context;
    auto part = cantFail(parseBitcodeFile(MemoryBufferRef(code, "test-tail"), context));
    std::string text;
    raw_string_ostream out(text);
    // Compare what LLVM decodes, including the recorded use-list order.
    // Reading bitcode can normalize its attribute encoding.
    part->print(out, nullptr, /*ShouldPreserveUseListOrder=*/true);
    return text;
}

// The key law: the part's key read back from its bitcode in another context
// is the key the build computed in memory, and each change to a fact the
// part's code is a function of changes it. Reading bitcode resets each
// intrinsic declaration's attributes to the intrinsic's own
// (`UpgradeIntrinsicFunction`), so a part that declares an intrinsic is not
// the built part once read back; for it, the key read back is the key of the
// part read back again. Returns whether the read-back key was the built key.
enum class Law { Built, Reread, Broken };
Law key_law(StringRef input, StringRef ir) {
    auto read = [](StringRef code, LLVMContext &context) {
        context.setDiscardValueNames(true);
        return cantFail(parseBitcodeFile(MemoryBufferRef(code, "test-tail"), context));
    };
    auto key = [&](function_ref<void(Module &)> change) {
        LLVMContext context;
        auto part = read(input, context);
        change(*part);
        return hirsel_key::module_key(*part);
    };
    hirsel_key::Digest built;
    if (ir.size() != built.size()) return Law::Broken;
    memcpy(built.data(), ir.data(), built.size());
    auto own = [](Module &part) -> Function & {
        for (Function &f : part)
            if (!f.isDeclaration()) return f;
        report_fatal_error("a tail input defines its function");
    };
    hirsel_key::Digest base = key([](Module &) {});
    bool intrinsic = false;
    {
        LLVMContext context;
        auto part = read(input, context);
        for (Function &f : *part) intrinsic |= f.isIntrinsic();
    }
    if (!intrinsic && base != built) {
        errs() << "the key of the part read back differs from the built key\n";
        return Law::Broken;
    }
    if (intrinsic) {
        LLVMContext context;
        auto part = read(input, context);
        SmallVector<char, 0> code;
        bitcode(*part, code);
        LLVMContext again;
        if (hirsel_key::module_key(*read(StringRef(code.data(), code.size()), again)) != base) {
            errs() << "the key of the part read back twice differs from the key read back once\n";
            return Law::Broken;
        }
    }
    std::pair<const char *, std::function<bool(Module &)>> changes[] = {
        {"a function attribute", [&](Module &part) {
             Function &f = own(part);
             if (f.hasFnAttribute(Attribute::Cold)) f.removeFnAttr(Attribute::Cold);
             else f.addFnAttr(Attribute::Cold);
             return true;
         }},
        {"a debug location's line", [&](Module &part) {
             for (BasicBlock &b : own(part))
                 for (Instruction &i : b)
                     if (DILocation *at = i.getDebugLoc()) {
                         i.setDebugLoc(DILocation::get(at->getContext(), at->getLine() + 1, at->getColumn(),
                                                       at->getScope(), at->getInlinedAt()));
                         return true;
                     }
             return false;
         }},
        {"a global initializer", [&](Module &part) {
             for (GlobalVariable &g : part.globals())
                 if (g.hasInitializer()) {
                     Constant *init = g.getInitializer();
                     g.setInitializer(init->isNullValue() ? UndefValue::get(init->getType())
                                                          : Constant::getNullValue(init->getType()));
                     return true;
                 }
             return false;
         }},
        {"the data layout", [&](Module &part) {
             part.setDataLayout(part.getDataLayoutStr() + "-ni:1");
             return true;
         }},
    };
    for (auto &[what, change] : changes) {
        bool changed = false;
        if (key([&](Module &part) { changed = change(part); }) == base) {
            if (!changed) continue;
            errs() << "changing " << what << " keeps the part's key\n";
            return Law::Broken;
        }
        if (!changed) report_fatal_error("an unchanged part changed its key");
    }
    return intrinsic ? Law::Reread : Law::Built;
}
} // namespace

int main(int argc, char **argv) {
    if (argc != 5) return 2;
    LLVMInitializeX86TargetInfo();
    LLVMInitializeX86Target();
    LLVMInitializeX86TargetMC();
    auto key = MemoryBuffer::getFile(argv[1]);
    auto input = MemoryBuffer::getFile(argv[2]);
    auto cached = MemoryBuffer::getFile(argv[3]);
    auto ir = MemoryBuffer::getFile(argv[4]);
    if (!key || !input || !cached || !ir) return 2;
    Law law = key_law((*input)->getBuffer(), (*ir)->getBuffer());
    if (law == Law::Broken) return 1;
    // The driver test requires some part to meet the built key exactly.
    if (law == Law::Built) outs() << "built key\n";
    StringRef facts = (*key)->getBuffer();
    auto machine = test_machine(facts);
    if (!machine) return 2;
    bool changed = facts.consume_front(TailChangedKey);
    if (!changed && !facts.consume_front(TailUnchangedKey)) return 2;

    LLVMContext context;
    context.setDiscardValueNames(true);
    auto module = cantFail(parseBitcodeFile((*input)->getMemBufferRef(), context));
    Function *own = nullptr;
    for (Function &f : *module)
        if (!f.isDeclaration()) own = &f;
    if (!own) return 2;
    while (!facts.empty()) {
        auto [line, rest] = facts.split('\n');
        facts = rest;
        auto [name, attributes] = line.split(' ');
        auto [link, tail] = attributes.split(' ');
        unsigned linkage, visibility;
        GlobalValue *value = module->getNamedValue(name);
        if (!value || link.getAsInteger(10, linkage) ||
            tail.split(' ').first.getAsInteger(10, visibility))
            return 2;
        // External declarations in the part stand in for other bodies.
        if (!value->isDeclaration()) {
            value->setVisibility(static_cast<GlobalValue::VisibilityTypes>(visibility));
            value->setLinkage(static_cast<GlobalValue::LinkageTypes>(linkage));
        }
    }
    SmallVector<MDNode *, 8> debug;
    if (auto *bindings = module->getNamedMetadata(tail_debug_bindings))
        append_range(debug, bindings->operands());
    auto *tail = hirsel_llvm_tail_open(wrap(module.get()),
        reinterpret_cast<LLVMTargetMachineRef>(machine.get()));
    hirsel_llvm_tail_run(tail, wrap(own), changed);
    hirsel_llvm_tail_close(tail);

    SmallVector<char, 0> rebuilt;
    part_bitcode(*module, *own, asm_lines(*module), rebuilt, nullptr, &debug);
    std::string orders;
    use_orders(*own, orders);
    StringRef expected = (*cached)->getBuffer();
    uint64_t size;
    if (expected.size() < sizeof size) return 2;
    memcpy(&size, expected.data(), sizeof size);
    expected = expected.drop_front(sizeof size);
    if (size > expected.size()) return 2;
    if (decoded_part(expected.take_front(size)) != decoded_part({rebuilt.data(), rebuilt.size()}) ||
        expected.drop_front(size) != orders) {
        errs() << own->getName() << ": rebuilt and cached tails differ\n";
        return 1;
    }
}
