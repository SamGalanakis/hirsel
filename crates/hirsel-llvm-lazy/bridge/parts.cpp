// The release module's codegen parts. Whole-program O3 has run; code
// generation then runs once per out-of-line definition, and a part whose
// bitcode a caller already holds an object for is not generated again.
//
// A part is one definition with the declarations it references. A local
// constant it references is defined in it `linkonce_odr` (a weak hidden
// symbol: the link resolves every reference to one copy, so a constant has one
// address per program; no comdat, which the object rewrite does not take).
// Local names that composition numbered per program (glue such as
// `core_drop_N.M`, private constants) are renamed from content first: the name is the fixed point of a hash of each
// definition's text with every such reference replaced by its referent's
// name. A part's bitcode therefore depends on its definition, not on the
// program around it, and the same definition in two programs is the same part.
// The sysroot's glibc predates the clock-wait calls this libstdc++ assumes.
#include <bits/c++config.h>
#if defined(__GLIBC__) && !__GLIBC_PREREQ(2, 30)
#undef _GLIBCXX_USE_PTHREAD_COND_CLOCKWAIT
#undef _GLIBCXX_USE_PTHREAD_MUTEX_CLOCKLOCK
#undef _GLIBCXX_USE_PTHREAD_RWLOCK_CLOCKLOCK
#endif
#include <llvm-c/Core.h>
#include <llvm-c/TargetMachine.h>
#include <llvm/ADT/DenseMap.h>
#include <llvm/ADT/DenseSet.h>
#include <llvm/ADT/StringMap.h>
#include <llvm/Bitcode/BitcodeWriter.h>
#include <llvm/IR/Constants.h>
#include <llvm/IR/Instructions.h>
#include <llvm/IR/IntrinsicInst.h>
#include <llvm/IR/Metadata.h>
#include <llvm/IR/Module.h>
#include <llvm/IR/ModuleSlotTracker.h>
#include <llvm/Support/CBindingWrapping.h>
#include <llvm/Support/raw_ostream.h>
#include <llvm/Support/xxhash.h>
#include <llvm/Transforms/Utils/Cloning.h>
#include <llvm/Transforms/Utils/ValueMapper.h>
#include <string>
#include <vector>

using namespace llvm;

namespace {

// An instance symbol (`h` and 64 lowercase hex digits) with its bridge
// suffixes (`.register`, `.words`, `.infallible`): unique in a program and the
// same in every program.
bool instance_name(StringRef n) {
    if (n.size() < 65 || n[0] != 'h') return false;
    for (size_t i = 1; i < 65; ++i)
        if (!((n[i] >= '0' && n[i] <= '9') || (n[i] >= 'a' && n[i] <= 'f'))) return false;
    for (size_t i = 65; i < n.size(); ++i)
        if (!(n[i] == '.' || (n[i] >= 'a' && n[i] <= 'z'))) return false;
    return true;
}

// A definition composition named for this program: every local one but an
// instance's body.
bool program_named(const GlobalObject &g) {
    if (g.isDeclaration() || !g.hasLocalLinkage()) return false;
    return !(isa<Function>(g) && instance_name(g.getName()));
}

// The printed text of `g` cut at each `@name` that names a program-named
// definition; `@` and `#`/`!` numbers are module-wide, so they are dropped.
struct Text {
    std::vector<std::string> pieces;  // pieces.size() == refs.size() + 1
    std::vector<unsigned> refs;
};

Text cut(const std::string &s, const StringMap<unsigned> &index) {
    Text t;
    std::string piece;
    for (size_t i = 0; i < s.size();) {
        char c = s[i];
        if ((c == '#' || c == '!') && i + 1 < s.size() && isdigit((unsigned char)s[i + 1])) {
            piece += c;
            for (++i; i < s.size() && isdigit((unsigned char)s[i]); ++i) {}
            continue;
        }
        if (c != '@') { piece += c; ++i; continue; }
        size_t start = i++;
        std::string name;
        if (i < s.size() && s[i] == '"') {
            for (++i; i < s.size() && s[i] != '"'; ++i) {
                if (s[i] == '\\' && i + 2 < s.size() && isxdigit((unsigned char)s[i + 1]) &&
                    isxdigit((unsigned char)s[i + 2])) {
                    name += (char)hexDigitValue(s[i + 1]) * 16 + hexDigitValue(s[i + 2]);
                    i += 2;
                } else name += s[i];
            }
            ++i;
        } else {
            for (; i < s.size() && (isalnum((unsigned char)s[i]) || s[i] == '-' || s[i] == '$' ||
                                    s[i] == '.' || s[i] == '_');
                 ++i)
                name += s[i];
        }
        auto found = index.find(name);
        if (found == index.end()) { piece.append(s, start, i - start); continue; }
        t.pieces.push_back(std::move(piece));
        piece.clear();
        t.refs.push_back(found->second);
    }
    t.pieces.push_back(std::move(piece));
    return t;
}

void hex(std::string &out, uint64_t v) {
    static const char digits[] = "0123456789abcdef";
    for (int s = 60; s >= 0; s -= 4) out += digits[(v >> s) & 15];
}

// Rename every program-named definition from its content.
void name_from_content(Module &m) {
    std::vector<GlobalObject *> named;
    StringMap<unsigned> index;
    for (GlobalObject &g : m.global_objects())
        if (program_named(g)) { index[g.getName()] = named.size(); named.push_back(&g); }
    if (named.empty()) return;
    ModuleSlotTracker slots(&m);
    std::vector<Text> texts;
    texts.reserve(named.size());
    for (GlobalObject *g : named) {
        std::string s;
        raw_string_ostream os(s);
        g->print(os, slots);
        texts.push_back(cut(s, index));
    }
    // Refine until the partition stops splitting: each round hashes a
    // definition's text, its referents' previous classes and its own.
    std::vector<XXH128_hash_t> cls(named.size(), XXH128_hash_t{0, 0}), next(named.size());
    size_t classes = 1;
    for (;;) {
        for (size_t k = 0; k < named.size(); ++k) {
            std::string buffer;
            buffer.append((const char *)&cls[k], sizeof(cls[k]));
            const Text &t = texts[k];
            for (size_t p = 0; p < t.pieces.size(); ++p) {
                uint64_t len = t.pieces[p].size();
                buffer.append((const char *)&len, sizeof(len));
                buffer += t.pieces[p];
                if (p < t.refs.size()) buffer.append((const char *)&cls[t.refs[p]], sizeof(cls[0]));
            }
            next[k] = xxh3_128bits((const uint8_t *)buffer.data(), buffer.size());
        }
        cls.swap(next);
        DenseSet<std::pair<uint64_t, uint64_t>> seen;
        for (auto &c : cls) seen.insert({c.low64, c.high64});
        if (seen.size() == classes) break;
        classes = seen.size();
    }
    // Same-class definitions are alike up to attributes and metadata; they
    // keep module order under one name with an ordinal.
    DenseMap<std::pair<uint64_t, uint64_t>, unsigned> taken;
    for (size_t k = 0; k < named.size(); ++k) {
        GlobalObject *g = named[k];
        std::string name = isa<Function>(g) ? "hirsel.g." : "hirsel.d.";
        hex(name, cls[k].high64);
        hex(name, cls[k].low64);
        unsigned ordinal = taken[{cls[k].low64, cls[k].high64}]++;
        if (ordinal) name += "." + std::to_string(ordinal);
        g->setName(name);
    }
}

// The `.set` line module asm holds for each assembler-local site alias.
StringMap<std::string> asm_lines(const Module &m) {
    StringMap<std::string> lines;
    for (const Module::GlobalAsmFragment &f : m.getModuleInlineAsm()) {
        StringRef rest = f.Asm;
        while (!rest.empty()) {
            auto [line, tail] = rest.split('\n');
            rest = tail;
            StringRef body = line.trim();
            if (!body.consume_front(".set ")) continue;
            lines[body.split(',').first.trim()] = line.str();
        }
    }
    return lines;
}

struct References {
    std::vector<GlobalValue *> order;
    DenseSet<const Value *> seen;
    void value(const Value *v) {
        if (!seen.insert(v).second) return;
        if (auto *g = dyn_cast<GlobalValue>(v)) {
            order.push_back(const_cast<GlobalValue *>(g));
            if (auto *var = dyn_cast<GlobalVariable>(g))
                if (var->hasInitializer() && var->hasLocalLinkage()) value(var->getInitializer());
            return;
        }
        if (auto *c = dyn_cast<Constant>(v)) {
            for (const Use &op : c->operands()) value(op.get());
            return;
        }
        if (auto *md = dyn_cast<MetadataAsValue>(v)) metadata(md->getMetadata());
    }
    DenseSet<const Metadata *> seen_md;
    void metadata(const Metadata *md) {
        if (!md || !seen_md.insert(md).second) return;
        if (auto *v = dyn_cast<ValueAsMetadata>(md)) value(v->getValue());
        else if (auto *n = dyn_cast<MDNode>(md))
            for (const MDOperand &op : n->operands()) metadata(op.get());
        else if (auto *list = dyn_cast<DIArgList>(md))
            for (ValueAsMetadata *v : list->getArgs()) value(v->getValue());
    }
};

// `f` alone, with what it references, in a module of its own.
std::unique_ptr<Module> part(const Module &m, Function &f, const StringMap<std::string> &sets) {
    LLVMContext &context = m.getContext();
    auto p = std::make_unique<Module>("hirsel-part", context);
    p->setSourceFileName("hirsel-part");
    p->setTargetTriple(m.getTargetTriple());
    p->setDataLayout(m.getDataLayout());
    // Module flags steer code generation (the DWARF version among them).
    if (NamedMDNode *flags = m.getModuleFlagsMetadata()) {
        NamedMDNode *copy = p->getOrInsertModuleFlagsMetadata();
        for (MDNode *flag : flags->operands()) copy->addOperand(flag);
    }
    References refs;
    refs.seen.insert(&f);
    for (BasicBlock &b : f)
        for (Instruction &i : b) {
            for (const Use &op : i.operands()) refs.value(op.get());
            SmallVector<std::pair<unsigned, MDNode *>, 4> attached;
            i.getAllMetadataOtherThanDebugLoc(attached);
            for (auto &[kind, node] : attached) refs.metadata(node);
        }
    ValueToValueMapTy map;
    std::string assembly;
    std::vector<std::pair<GlobalVariable *, GlobalVariable *>> constants;
    for (GlobalValue *g : refs.order) {
        GlobalValue *copy;
        if (auto *callee = dyn_cast<Function>(g)) {
            Function *d = Function::Create(callee->getFunctionType(), GlobalValue::ExternalLinkage,
                                           callee->getAddressSpace(), callee->getName(), p.get());
            d->copyAttributesFrom(callee);
            copy = d;
        } else {
            auto *var = cast<GlobalVariable>(g);
            auto *d = new GlobalVariable(*p, var->getValueType(), var->isConstant(),
                GlobalValue::ExternalLinkage, nullptr, var->getName(), nullptr,
                var->getThreadLocalMode(), var->getAddressSpace(), var->isExternallyInitialized());
            d->copyAttributesFrom(var);
            d->copyMetadata(var, 0);
            if (var->hasInitializer() && var->hasLocalLinkage()) {
                d->setLinkage(GlobalValue::LinkOnceODRLinkage);
                constants.push_back({var, d});
            }
            auto line = sets.find(var->getName());
            if (line != sets.end()) assembly += line->second + "\n";
            copy = d;
        }
        // A local definition is the program's alone.
        if (g->hasLocalLinkage()) copy->setVisibility(GlobalValue::HiddenVisibility);
        map[g] = copy;
    }
    Function *own = Function::Create(f.getFunctionType(), f.getLinkage(), f.getAddressSpace(),
                                     f.getName(), p.get());
    own->copyAttributesFrom(&f);
    if (f.hasLocalLinkage()) {
        own->setLinkage(GlobalValue::ExternalLinkage);
        own->setVisibility(GlobalValue::HiddenVisibility);
    }
    map[&f] = own;
    // After `f` is mapped: a constant may point back at it.
    for (auto &[from, to] : constants) to->setInitializer(MapValue(from->getInitializer(), map));
    auto to = own->arg_begin();
    for (Argument &a : f.args()) map[&a] = &*to++;
    SmallVector<ReturnInst *, 8> returns;
    CloneFunctionInto(own, &f, map, CloneFunctionChangeType::DifferentModule, returns);
    if (!assembly.empty()) p->appendModuleInlineAsm(Module::GlobalAsmFragment(assembly));
    return p;
}

}  // namespace

// Generate `module` as parts, in module order. For each part `reuse` gets its
// definition's name and bitcode; unless it answers 1, `deliver` gets the part's
// object. Returns 0, or 1 with `*error` set to an LLVM message.
extern "C" int hirsel_llvm_emit_parts(LLVMModuleRef module, LLVMTargetMachineRef machine,
    int (*reuse)(void *, const char *, size_t, const uint8_t *, size_t),
    void (*deliver)(void *, const uint8_t *, size_t), void *context, char **error) {
    Module &m = *unwrap(module);
    // Local value names label printed IR only.
    for (Function &f : m) {
        for (Argument &a : f.args()) a.setName("");
        for (BasicBlock &b : f) {
            b.setName("");
            for (Instruction &i : b) i.setName("");
        }
    }
    name_from_content(m);
    StringMap<std::string> sets = asm_lines(m);
    for (Function &f : m) {
        if (f.isDeclaration()) continue;
        std::unique_ptr<Module> p = part(m, f, sets);
        SmallVector<char, 0> bitcode;
        raw_svector_ostream os(bitcode);
        WriteBitcodeToFile(*p, os);
        StringRef name = f.getName();
        if (reuse(context, name.data(), name.size(), (const uint8_t *)bitcode.data(), bitcode.size()))
            continue;
        LLVMMemoryBufferRef object = nullptr;
        if (LLVMTargetMachineEmitToMemoryBuffer(machine, wrap(p.get()), LLVMObjectFile, error, &object))
            return 1;
        deliver(context, (const uint8_t *)LLVMGetBufferStart(object), LLVMGetBufferSize(object));
        LLVMDisposeMemoryBuffer(object);
    }
    return 0;
}
