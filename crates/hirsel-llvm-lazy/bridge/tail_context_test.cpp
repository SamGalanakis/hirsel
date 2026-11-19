// Share the private part writer with the native driver test. This executable
// is built only for tests.
#include "parts.cpp"
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
} // namespace

int main(int argc, char **argv) {
    if (argc != 4) return 2;
    LLVMInitializeX86TargetInfo();
    LLVMInitializeX86Target();
    LLVMInitializeX86TargetMC();
    auto key = MemoryBuffer::getFile(argv[1]);
    auto input = MemoryBuffer::getFile(argv[2]);
    auto cached = MemoryBuffer::getFile(argv[3]);
    if (!key || !input || !cached) return 2;
    StringRef facts = (*key)->getBuffer();
    auto machine = test_machine(facts);
    if (!machine) return 2;

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
    hirsel_llvm_tail_run(tail, wrap(own));
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
