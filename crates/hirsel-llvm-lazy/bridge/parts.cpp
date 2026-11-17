// The release module's codegen parts. Whole-program O3 has run; each
// out-of-line definition is then a part keyed by its bitcode, a part whose
// object a caller already holds is not generated again, and the rest are
// generated together in one object the caller cuts per part.
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
#include <llvm/Target/TargetMachine.h>
#include <llvm/TargetParser/Triple.h>
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
    bool metadata_values = false;
    void metadata(const Metadata *md) {
        if (!md || !seen_md.insert(md).second) return;
        if (auto *v = dyn_cast<ValueAsMetadata>(md)) {
            metadata_values |= !isa<ConstantData>(v->getValue());
            value(v->getValue());
        }
        else if (auto *n = dyn_cast<MDNode>(md))
            for (const MDOperand &op : n->operands()) metadata(op.get());
        else if (auto *list = dyn_cast<DIArgList>(md))
            for (ValueAsMetadata *v : list->getArgs()) metadata(v);
    }
};

// Bitcode of `m` alone: no IR symbol table, which only restates the module.
void bitcode(const Module &m, SmallVectorImpl<char> &out) {
    BitcodeWriter writer(out);
    writer.writeModule(m);
    writer.writeStrtab();
}

// The bitcode of `f` alone, with what it references, in a module of its own.
// The part's definition borrows `f`'s body for the write: its blocks move in,
// its operands that name a global name the part's declaration instead, and
// all of it is put back after, so `m` is as it was. A body the move cannot
// put back exactly (a block whose address is taken, metadata that names a
// value, debug records, a subprogram whose units the part must list) is
// cloned instead; both give the same bitcode.
void part_bitcode(Module &m, Function &f, const StringMap<std::string> &sets, SmallVectorImpl<char> &out) {
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
    bool movable = !f.getSubprogram();
    for (BasicBlock &b : f) {
        movable = movable && !b.hasAddressTaken();
        for (Instruction &i : b) {
            for (const Use &op : i.operands()) refs.value(op.get());
            SmallVector<std::pair<unsigned, MDNode *>, 4> attached;
            i.getAllMetadataOtherThanDebugLoc(attached);
            for (auto &[kind, node] : attached) refs.metadata(node);
            movable = movable && !i.hasDbgRecords();
        }
    }
    movable = movable && !refs.metadata_values;
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
    if (!assembly.empty()) p->appendModuleInlineAsm(Module::GlobalAsmFragment(assembly));
    if (!movable) {
        auto to = own->arg_begin();
        for (Argument &a : f.args()) map[&a] = &*to++;
        SmallVector<ReturnInst *, 8> returns;
        CloneFunctionInto(own, &f, map, CloneFunctionChangeType::DifferentModule, returns);
        bitcode(*p, out);
        return;
    }
    SmallVector<std::pair<unsigned, MDNode *>, 4> attached;
    f.getAllMetadata(attached);
    for (auto &[kind, node] : attached) own->addMetadata(kind, *MapMetadata(node, map));
    // As the clone gives it: the part lists the compile units of its body.
    p->getOrInsertNamedMetadata("llvm.dbg.cu");
    own->splice(own->end(), &f);
    for (auto [from, to] = std::pair(f.arg_begin(), own->arg_begin()); from != f.arg_end(); ++from, ++to)
        from->replaceAllUsesWith(&*to);
    std::vector<std::pair<Use *, Value *>> moved;
    for (BasicBlock &b : *own)
        for (Instruction &i : b)
            for (Use &u : i.operands()) {
                Value *v = u.get();
                if (!isa<Constant>(v) || isa<ConstantData>(v)) continue;
                Value *n = MapValue(v, map);
                if (n && n != v) {
                    moved.push_back({&u, v});
                    u.set(n);
                }
            }
    bitcode(*p, out);
    for (auto it = moved.rbegin(); it != moved.rend(); ++it) it->first->set(it->second);
    for (auto [from, to] = std::pair(f.arg_begin(), own->arg_begin()); from != f.arg_end(); ++from, ++to)
        to->replaceAllUsesWith(&*from);
    f.splice(f.end(), own);
}

// What code generation is a function of besides the IR: the machine and the
// backend options a part's object depends on. A part's key carries it.
std::string machine_text(const TargetMachine &tm) {
    std::string s;
    raw_string_ostream os(s);
    const TargetOptions &o = tm.Options;
    os << "triple " << tm.getTargetTriple().str() << "\ncpu " << tm.getTargetCPU() << "\nfeatures "
       << tm.getTargetFeatureString() << "\nreloc " << (int)tm.getRelocationModel() << "\ncode-model "
       << (int)tm.getCodeModel() << "\nopt " << (int)tm.getOptLevel() << "\nfunction-sections "
       << o.FunctionSections << "\ndata-sections " << o.DataSections << "\nunique-section-names "
       << o.UniqueSectionNames << "\nipra " << o.EnableIPRA << "\nmachine-outliner " << o.EnableMachineOutliner
       << "\nglobal-isel " << o.EnableGlobalISel << "\nfast-isel " << o.EnableFastISel << "\n";
    return s;
}

// Why `m` cannot be split into per-definition objects, or empty. A part's
// object is cut from the program's object by its text section; anything
// whose output the module owns as a whole (constructors, used lists, module
// assembly beyond the site aliases, explicit sections, comdats, aliases,
// writable or thread-local data, unwinding personalities, named metadata
// that reaches the object) is not a part's, so such a module is generated
// and keyed whole.
std::string unsplittable(const Module &m) {
    Triple triple(m.getTargetTriple());
    if (triple.getArch() != Triple::x86_64 || !triple.isOSBinFormatELF())
        return "the object split is x86-64 ELF only";
    if (!m.alias_empty() || !m.ifunc_empty()) return "aliases";
    if (!m.getComdatSymbolTable().empty()) return "comdats";
    for (const GlobalVariable &g : m.globals()) {
        if (g.hasAppendingLinkage()) return "appending global " + g.getName().str();
        if (g.isDeclaration()) continue;
        // A part copies each constant it reaches as its own weak hidden
        // definition, so a constant must be local: nothing outside names it.
        if (!g.isConstant() || !g.hasLocalLinkage() || g.isThreadLocal() || g.hasSection() ||
            g.hasComdat())
            return "data " + g.getName().str();
    }
    for (const Function &f : m) {
        if (f.isDeclaration()) continue;
        if (f.hasSection() || f.hasComdat() || f.hasPersonalityFn() || f.hasPrefixData() ||
            f.hasPrologueData() || f.hasGC())
            return "function " + f.getName().str();
    }
    for (const Module::GlobalAsmFragment &f : m.getModuleInlineAsm()) {
        if (!f.hasSameProperties(Module::GlobalAsmFragment(std::string()))) return "module assembly properties";
        StringRef rest = f.Asm;
        while (!rest.empty()) {
            auto [line, tail] = rest.split('\n');
            rest = tail;
            StringRef body = line.trim();
            if (!body.empty() && !body.starts_with(".set ")) return "module assembly";
        }
    }
    for (const NamedMDNode &n : m.named_metadata())
        if (n.getName() != "llvm.module.flags" && n.getName() != "llvm.dbg.cu")
            return "named metadata " + n.getName().str();
    return "";
}

}  // namespace

// Generate `module` once for every definition a caller does not hold. Each
// out-of-line definition is a part keyed by its own bitcode (`part`); `reuse`
// gets the machine text, the part's definition name and bitcode, and answers 1
// when it holds the part's object, whose body is then dropped. One codegen
// over what is left gives one object with a section per definition, which
// `deliver` gets and the caller cuts per part. A module that cannot be cut
// (`unsplittable`) is one part named "" keyed by the module's bitcode.
// Returns 0, or 1 with `*error` set to an LLVM message.
extern "C" int hirsel_llvm_emit_parts(LLVMModuleRef module, LLVMTargetMachineRef machine,
    int (*reuse)(void *, const char *, size_t, const char *, size_t, const uint8_t *, size_t),
    void (*deliver)(void *, const uint8_t *, size_t), void *context, char **error) {
    Module &m = *unwrap(module);
    TargetMachine &tm = *reinterpret_cast<TargetMachine *>(machine);
    // A section per definition, and no code generation across definitions:
    // a part's code is a function of the part alone.
    tm.Options.FunctionSections = true;
    tm.Options.DataSections = true;
    tm.Options.UniqueSectionNames = true;
    tm.Options.EnableIPRA = false;
    tm.Options.EnableMachineOutliner = false;
    std::string machine_key = machine_text(tm);
    // Local value names label printed IR only.
    for (Function &f : m) {
        for (Argument &a : f.args()) a.setName("");
        for (BasicBlock &b : f) {
            b.setName("");
            for (Instruction &i : b) i.setName("");
        }
    }
    name_from_content(m);
    bool whole = !unsplittable(m).empty();
    auto ask = [&](StringRef name, const SmallVectorImpl<char> &code) {
        return reuse(context, name.data(), name.size(), machine_key.data(), machine_key.size(),
                     (const uint8_t *)code.data(), code.size()) != 0;
    };
    size_t misses = 0;
    if (whole) {
        SmallVector<char, 0> code;
        bitcode(m, code);
        if (ask("", code)) return 0;
        misses = 1;
    } else {
        // Keyed while local definitions are still local: a part's bitcode
        // then carries each constant it reaches with its initializer, which
        // the part's object holds a copy of.
        StringMap<std::string> sets = asm_lines(m);
        std::vector<Function *> held;
        for (Function &f : m) {
            if (f.isDeclaration()) continue;
            SmallVector<char, 0> code;
            part_bitcode(m, f, sets, code);
            if (ask(f.getName(), code)) held.push_back(&f);
            else ++misses;
        }
        // Every local definition is a hidden global: a part's references to
        // another part's definitions resolve at link time.
        for (GlobalObject &g : m.global_objects())
            if (!g.isDeclaration() && g.hasLocalLinkage()) {
                g.setLinkage(GlobalValue::ExternalLinkage);
                g.setVisibility(GlobalValue::HiddenVisibility);
            }
        for (Function *f : held) f->deleteBody();
    }
    if (!misses) return 0;
    LLVMMemoryBufferRef object = nullptr;
    if (LLVMTargetMachineEmitToMemoryBuffer(machine, module, LLVMObjectFile, error, &object)) return 1;
    deliver(context, (const uint8_t *)LLVMGetBufferStart(object), LLVMGetBufferSize(object));
    LLVMDisposeMemoryBuffer(object);
    return 0;
}
