// The release module's codegen parts. Whole-program O3 has run; each
// out-of-line definition is then a part keyed by what its bitcode records (key.h), a part whose
// object a caller already holds is not generated again, and each of the rest
// is generated from its in-memory part on the composing thread.
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
#include "key.h"
#include "pass_cost.h"

#include <llvm-c/Core.h>
#include <llvm-c/TargetMachine.h>
#include <llvm/ADT/DenseMap.h>
#include <llvm/ADT/STLFunctionalExtras.h>
#include <llvm/ADT/MapVector.h>
#include <llvm/ADT/DenseSet.h>
#include <llvm/ADT/ScopeExit.h>
#include <llvm/ADT/StringMap.h>
#include <llvm/Bitcode/BitcodeReader.h>
#include <llvm/Bitcode/BitcodeWriter.h>
#include <llvm/IR/Constants.h>
#include <llvm/IR/DebugInfoMetadata.h>
#include <llvm/IR/DiagnosticInfo.h>
#include <llvm/IR/DiagnosticPrinter.h>
#include <llvm/IR/Instructions.h>
#include <llvm/IR/LegacyPassManager.h>
#include <llvm/IR/IntrinsicInst.h>
#include <llvm/IR/Metadata.h>
#include <llvm/IR/Module.h>
#include <llvm/IR/ModuleSlotTracker.h>
#include <llvm/MC/TargetRegistry.h>
#include <llvm/Support/CBindingWrapping.h>
#include <llvm/Support/MemoryBuffer.h>
#include <llvm/Support/raw_ostream.h>
#include <llvm/Support/xxhash.h>
#include <llvm/Target/TargetMachine.h>
#include <llvm/TargetParser/Triple.h>
#include <llvm/Transforms/Utils/Cloning.h>
#include <llvm/Transforms/Utils/ValueMapper.h>
#include <algorithm>
#include <cstring>
#include <string>
#include <vector>

using namespace llvm;
extern "C" LLVMBool hirsel_llvm_emit_object(LLVMTargetMachineRef machine, LLVMModuleRef module,
                                          char **message, LLVMMemoryBufferRef *buffer);

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
    // Whether a definition with external linkage brings its initializer: a
    // key does, since a load from a constant folds to it.
    bool initializers = false;
    SmallVectorImpl<MDNode *> *debug = nullptr;
    std::vector<GlobalValue *> order;
    DenseSet<const Value *> seen;
    void value(const Value *v) {
        if (!seen.insert(v).second) return;
        if (auto *g = dyn_cast<GlobalValue>(v)) {
            order.push_back(const_cast<GlobalValue *>(g));
            if (auto *var = dyn_cast<GlobalVariable>(g))
                if (var->hasInitializer() && (initializers || var->hasLocalLinkage())) value(var->getInitializer());
            return;
        }
        if (auto *c = dyn_cast<Constant>(v)) {
            for (const Use &op : c->operands()) value(op.get());
            return;
        }
        if (auto *md = dyn_cast<MetadataAsValue>(v)) metadata(md->getMetadata());
    }
    // Whether each metadata seen names a global (an inlined call's history
    // names its callees): the part's copy of it names the part's declarations.
    DenseMap<const Metadata *, bool> names_global;
    DenseSet<const Metadata *> open;
    bool cycle = false;
    bool metadata(const Metadata *md) {
        if (!md) return false;
        if (auto seen = names_global.find(md); seen != names_global.end()) {
            cycle |= open.contains(md);
            return seen->second;
        }
        names_global[md] = false;
        open.insert(md);
        if (debug && isa<DISubprogram, DICompileUnit>(md))
            debug->push_back(const_cast<MDNode *>(cast<MDNode>(md)));
        bool named = false;
        if (auto *v = dyn_cast<ValueAsMetadata>(md)) {
            named = isa<Constant>(v->getValue()) && !isa<ConstantData>(v->getValue());
            value(v->getValue());
        }
        else if (auto *n = dyn_cast<MDNode>(md))
            for (const MDOperand &op : n->operands()) named |= metadata(op.get());
        else if (auto *list = dyn_cast<DIArgList>(md))
            for (ValueAsMetadata *v : list->getArgs()) named |= metadata(v);
        names_global[md] = named;
        open.erase(md);
        return named;
    }
    // A node met again inside its own operands was answered before them: on
    // a cycle, every node naming one that names a global names one too.
    void settle() {
        for (bool changed = cycle; changed;) {
            changed = false;
            for (auto &[md, named] : names_global) {
                if (named) continue;
                if (auto *n = dyn_cast<MDNode>(md))
                    for (const MDOperand &op : n->operands())
                        if (op && names_global.lookup(op.get())) {
                            named = changed = true;
                            break;
                        }
            }
        }
    }
};

// A tail carries these distinct nodes in key order. Its locations already
// use item-relative byte offsets. Reattaching their scopes to the current
// module lets the final link resolve them against the current source.
constexpr StringLiteral tail_debug_bindings = "hirsel.tail.debug";

// Gives `to` the uses of `from`, in their order: a use set to a value goes to
// the front of its use list, so the last is set first.
void hand_uses(Value &from, Value &to) {
    SmallVector<Use *, 16> uses;
    for (Use &u : from.uses()) uses.push_back(&u);
    for (auto u = uses.rbegin(); u != uses.rend(); ++u) (*u)->set(&to);
    if (from.isUsedByMetadata()) ValueAsMetadata::handleRAUW(&from, &to);
}

// Bitcode of `m` alone: no IR symbol table, which only restates the module.
// It keeps each value's use-list order, which instruction selection and
// register allocation follow: a part generated from its bitcode is then the
// code its definition gets in the module.
void bitcode(const Module &m, SmallVectorImpl<char> &out) {
    BitcodeWriter writer(out);
    writer.writeModule(m, /*ShouldPreserveUseListOrder=*/true);
    writer.writeStrtab();
}

// Lists the uses of each local value of `f`'s clone in the order `f` lists
// those of the value it copies.
void order_uses_like(const Function &f, const ValueToValueMapTy &map) {
    auto order = [&](const Value &from) {
        if (!from.hasNUsesOrMore(2)) return;
        auto copy = map.find(&from);
        if (copy == map.end() || !copy->second) return;
        DenseMap<std::pair<const User *, unsigned>, unsigned> position;
        unsigned n = 0;
        for (const Use &u : from.uses()) {
            auto user = map.find(u.getUser());
            if (user != map.end() && user->second) {
                const Value *copied = user->second;
                position[{cast<User>(copied), u.getOperandNo()}] = n;
            }
            ++n;
        }
        copy->second->sortUseList([&](const Use &l, const Use &r) {
            return position.lookup({l.getUser(), l.getOperandNo()}) < position.lookup({r.getUser(), r.getOperandNo()});
        });
    };
    for (const Argument &a : f.args()) order(a);
    for (const BasicBlock &b : f) {
        order(b);
        for (const Instruction &i : b) order(i);
    }
}

// Lends `use` the part of `f`: `f` alone, with what it references, in a
// module of its own. Its blocks move in and its operands and metadata that
// name a global name the part's declaration instead. Unless `use` takes the
// module for code generation, all of it is put back after the call. The move
// keeps every use-list order the code generator follows. A body the move
// cannot put back exactly (a block whose address is taken or debug records)
// is cloned instead, as is a codegen part with a subprogram. The clone's
// values list their uses in the order `f`'s list theirs.
// With `linkage`, the linkage and visibility the part does not keep of `f`
// and of each global it names, and whether each is defined, are written to it,
// and the part defines every variable it names with its initializer (a tail
// folds a load from a constant): the part is then a release tail's input.
void with_part(Module &m, Function &f, const StringMap<std::string> &sets, std::string *linkage,
               SmallVectorImpl<MDNode *> *debug, function_ref<void(std::unique_ptr<Module> &)> use) {
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
    refs.initializers = linkage != nullptr;
    refs.debug = linkage ? debug : nullptr;
    refs.seen.insert(&f);
    SmallVector<std::pair<unsigned, MDNode *>, 4> attached;
    f.getAllMetadata(attached);
    if (refs.debug)
        for (auto &[kind, node] : attached) refs.metadata(node);
    bool movable = debug || !f.getSubprogram();
    for (BasicBlock &b : f) {
        movable = movable && !b.hasAddressTaken();
        for (Instruction &i : b) {
            for (const Use &op : i.operands()) refs.value(op.get());
            if (refs.debug && i.getDebugLoc()) refs.metadata(i.getDebugLoc().get());
            SmallVector<std::pair<unsigned, MDNode *>, 4> tags;
            i.getAllMetadataOtherThanDebugLoc(tags);
            for (auto &[kind, node] : tags) refs.metadata(node);
            movable = movable && !i.hasDbgRecords();
        }
    }
    refs.settle();
    if (linkage) {
        raw_string_ostream os(*linkage);
        auto line = [&](const GlobalValue &g) {
            os << g.getName() << ' ' << (int)g.getLinkage() << ' ' << (int)g.getVisibility() << ' '
               << g.isDeclaration() << '\n';
        };
        line(f);
        for (GlobalValue *g : refs.order) line(*g);
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
            if (var->hasInitializer() && (refs.initializers || var->hasLocalLinkage())) {
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
        order_uses_like(f, map);
        use(p);
        return;
    }
    if (debug && !debug->empty()) {
        NamedMDNode *bindings = p->getOrInsertNamedMetadata(tail_debug_bindings);
        NamedMDNode *units = p->getOrInsertNamedMetadata("llvm.dbg.cu");
        for (MDNode *node : *debug) {
            bindings->addOperand(node);
            map.MD()[node].reset(node);
            if (isa<DICompileUnit>(node)) units->addOperand(node);
        }
    } else {
        // Match the clone of a body without debug info.
        p->getOrInsertNamedMetadata("llvm.dbg.cu");
    }
    for (auto &[kind, node] : attached) own->addMetadata(kind, *MapMetadata(node, map));
    own->splice(own->end(), &f);
    for (auto [from, to] = std::pair(f.arg_begin(), own->arg_begin()); from != f.arg_end(); ++from, ++to)
        hand_uses(*from, *to);
    std::vector<std::pair<Use *, Value *>> moved;
    std::vector<std::tuple<Instruction *, unsigned, MDNode *>> retagged;
    for (BasicBlock &b : *own)
        for (Instruction &i : b) {
            for (Use &u : i.operands()) {
                Value *v = u.get();
                Value *n = nullptr;
                if (auto *md = dyn_cast<MetadataAsValue>(v)) {
                    if (refs.names_global.lookup(md->getMetadata()))
                        n = MetadataAsValue::get(context, MapMetadata(md->getMetadata(), map, RF_IgnoreMissingLocals));
                } else if (isa<Constant>(v) && !isa<ConstantData>(v))
                    n = MapValue(v, map);
                if (n && n != v) {
                    moved.push_back({&u, v});
                    u.set(n);
                }
            }
            SmallVector<std::pair<unsigned, MDNode *>, 4> tags;
            i.getAllMetadataOtherThanDebugLoc(tags);
            for (auto &[kind, node] : tags)
                if (refs.names_global.lookup(node)) {
                    retagged.push_back({&i, kind, node});
                    i.setMetadata(kind, MapMetadata(node, map));
                }
        }
    use(p);
    if (!p) return;
    for (auto &[i, kind, node] : retagged) i->setMetadata(kind, node);
    for (auto it = moved.rbegin(); it != moved.rend(); ++it) it->first->set(it->second);
    for (auto [from, to] = std::pair(f.arg_begin(), own->arg_begin()); from != f.arg_end(); ++from, ++to)
        hand_uses(*to, *from);
    f.splice(f.end(), own);
}

// The bitcode of `f`'s part (`with_part`).
void part_bitcode(Module &m, Function &f, const StringMap<std::string> &sets,
                  SmallVectorImpl<char> &out, std::string *linkage = nullptr,
                  SmallVectorImpl<MDNode *> *debug = nullptr) {
    with_part(m, f, sets, linkage, debug, [&](auto &p) { bitcode(*p, out); });
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

// One missed part: its module in, its object or an LLVM message out.
struct Miss {
    std::unique_ptr<Module> part;
    SmallVector<char, 0> object;
    std::string error;
};

// An error diagnostic is the part's error. Other diagnostics are dropped.
void diagnose(const DiagnosticInfo *d, void *miss) {
    if (d->getSeverity() != DS_Error) return;
    std::string &error = static_cast<Miss *>(miss)->error;
    if (!error.empty()) return;
    raw_string_ostream os(error);
    DiagnosticPrinterRawOStream printer(os);
    d->print(printer);
}

// Generate one part with `tm`, on the thread that owns the composing module's
// context. The part belongs to that same context and dies on that thread.
void generate(TargetMachine &tm, Miss &miss) {
    auto part = std::move(miss.part);
    Module &p = *part;
    LLVMContext &context = p.getContext();
    auto previous = context.getDiagnosticHandler();
    context.setDiagnosticHandler(std::make_unique<DiagnosticHandler>());
    context.setDiagnosticHandlerCallBack(diagnose, &miss);
    auto restore = make_scope_exit([&] { context.setDiagnosticHandler(std::move(previous)); });
    p.setDataLayout(tm.createDataLayout());
    raw_svector_ostream os(miss.object);
    hirsel_llvm::PassCost cost("object-emission", p);
    legacy::PassManager passes;
    if (tm.addPassesToEmitFile(passes, os, nullptr, CodeGenFileType::ObjectFile)) {
        miss.error = "the target machine cannot emit an object";
        return;
    }
    passes.run(p);
}

}  // namespace

// Generate every definition of `module` a caller does not hold. Each
// out-of-line definition is a part keyed by what its bitcode records
// (`hirsel_key::module_key`); `reuse` gets the definition name, the machine
// text and the part's key, and answers 1
// when it holds the part's object. With `split_objects`, each missed part is
// generated in the composing context and `deliver` gets its object in part
// order. Otherwise the misses are generated together in one object with a
// section per definition, which the caller cuts per part.
// A module that cannot be cut (`unsplittable`) is one part named
// "" keyed by the module's key and generated whole.
// Returns 0, or 1 with `*error` set to an LLVM message.
extern "C" int hirsel_llvm_emit_parts(LLVMModuleRef module, LLVMTargetMachineRef machine, uint8_t split_objects,
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
    auto ask = [&](StringRef name, const hirsel_key::Digest &key) {
        return reuse(context, name.data(), name.size(), machine_key.data(), machine_key.size(),
                     key.data(), key.size()) != 0;
    };
    std::vector<Miss> misses;
    if (whole) {
        if (ask("", hirsel_key::module_key(m))) return 0;
        misses.emplace_back();
    } else {
        // Keyed while local definitions are still local: a part then
        // carries each constant it reaches with its initializer, which the
        // part's object holds a copy of. Compute all keys before any code
        // generation can change the composing context's registries.
        StringMap<std::string> sets = asm_lines(m);
        std::vector<Function *> held;
        for (Function &f : m) {
            if (f.isDeclaration()) continue;
            with_part(m, f, sets, nullptr, nullptr, [&](auto &p) {
                if (ask(f.getName(), hirsel_key::module_key(*p))) {
                    held.push_back(&f);
                    return;
                }
                Miss &miss = misses.emplace_back();
                if (split_objects) miss.part = std::move(p);
            });
        }
        if (split_objects) {
            if (misses.empty()) return 0;
            std::unique_ptr<TargetMachine> codegen(tm.getTarget().createTargetMachine(
                tm.getTargetTriple(), tm.getTargetCPU(), tm.getTargetFeatureString(), tm.Options,
                tm.getRelocationModel(), tm.getCodeModel(), tm.getOptLevel()));
            if (!codegen) {
                *error = strdup("LLVM could not create the target machine");
                return 1;
            }
            for (Miss &miss : misses) {
                generate(*codegen, miss);
                if (!miss.error.empty()) {
                    *error = strdup(miss.error.c_str());
                    return 1;
                }
            }
            for (Miss &miss : misses) deliver(context, (const uint8_t *)miss.object.data(), miss.object.size());
            return 0;
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
    if (misses.empty()) return 0;
    LLVMMemoryBufferRef object = nullptr;
    if (hirsel_llvm_emit_object(machine, module, error, &object)) return 1;
    deliver(context, (const uint8_t *)LLVMGetBufferStart(object), LLVMGetBufferSize(object));
    LLVMDisposeMemoryBuffer(object);
    return 0;
}

extern "C" void *hirsel_llvm_tail_open(LLVMModuleRef module, LLVMTargetMachineRef machine);
extern "C" void hirsel_llvm_tail_run(void *tail, LLVMValueRef function);
extern "C" void hirsel_llvm_tail_close(void *tail);

namespace {
// Whether `f`'s body can be lent to a part and restored from one exactly: as
// `part_bitcode` moves it rather than cloning it.
bool movable(const Function &f) {
    for (const BasicBlock &b : f) {
        if (b.hasAddressTaken()) return false;
        for (const Instruction &i : b)
            if (i.hasDbgRecords()) return false;
    }
    return true;
}

// The constants `f`'s instructions use two or more times that the context
// shares between modules (not data, which lists no uses, nor globals, which a
// part declares), in order of first use, each with its uses in `f` in order.
MapVector<Constant *, SmallVector<Use *, 4>> shared_uses(Function &f) {
    MapVector<Constant *, SmallVector<Use *, 4>> uses;
    for (BasicBlock &b : f)
        for (Instruction &i : b)
            for (Use &u : i.operands())
                if (auto *c = dyn_cast<Constant>(u.get()); c && !isa<ConstantData>(c) && !isa<GlobalValue>(c))
                    uses[c].push_back(&u);
    uses.remove_if([](auto &entry) { return entry.second.size() < 2; });
    return uses;
}

// The order each shared constant lists its uses in `f`. A part's bitcode
// states it, but reading the part into a context whose program uses the same
// constant cannot apply it: the constant's list holds the program's uses too.
void use_orders(Function &f, std::string &out) {
    auto word = [&](uint32_t v) { out.append((const char *)&v, sizeof v); };
    for (auto &[c, uses] : shared_uses(f)) {
        DenseMap<const Use *, uint32_t> at;
        for (uint32_t k = 0; k < uses.size(); ++k) at[uses[k]] = k;
        word(uses.size());
        for (const Use &u : c->uses())
            if (auto k = at.find(&u); k != at.end()) word(k->second);
    }
}

using SharedUses = MapVector<Constant *, SmallVector<Use *, 4>>;

// The order `use_orders` wrote for each of `shared`'s constants, or false when
// `orders` is not exactly one permutation of each constant's uses, in order.
bool read_orders(const SharedUses &shared, StringRef orders, std::vector<uint32_t> &out) {
    if (orders.size() % 4) return false;
    size_t i = 0, n = orders.size() / 4;
    auto word = [&](size_t at) {
        uint32_t v;
        memcpy(&v, orders.data() + 4 * at, sizeof v);
        return v;
    };
    for (auto &[c, uses] : shared) {
        if (i >= n || word(i) != uses.size() || n - i - 1 < uses.size()) return false;
        std::vector<bool> seen(uses.size());
        for (size_t k = 0; k < uses.size(); ++k) {
            uint32_t at = word(i + 1 + k);
            if (at >= uses.size() || seen[at]) return false;
            seen[at] = true;
            out.push_back(at);
        }
        i += 1 + uses.size();
    }
    return i == n;
}

// Lists each shared constant's uses in the order `read_orders` read, after the
// program's other uses, whose order stays. `shared` holds the uses themselves,
// which now name the constant they were mapped to.
void order_uses(const SharedUses &shared, ArrayRef<uint32_t> order) {
    for (auto &[c, uses] : shared) {
        DenseMap<const Use *, uint32_t> rank;
        for (uint32_t k = 0; k < uses.size(); ++k) rank[uses[order[k]]] = k;
        order = order.drop_front(uses.size());
        auto of = [&](const Use &u) { return rank.lookup_or(&u, UINT32_MAX); };
        cast<Constant>(uses.front()->get())->sortUseList([&](const Use &l, const Use &r) { return of(l) < of(r); });
    }
}

// Puts the body of the part `code` holds in place of `f`'s, naming `m`'s
// globals where the part names its declarations. False, with `m` unchanged,
// when the part's definition, a global it names, its debug bindings or its use
// orders do not match `m`'s: all are checked before `m` changes.
bool restore(Module &m, Function &f, StringRef held, ArrayRef<MDNode *> debug) {
    // The part, then the use orders its bitcode cannot carry here.
    uint64_t size;
    if (held.size() < sizeof size) return false;
    memcpy(&size, held.data(), sizeof size);
    if (held.size() - sizeof size < size) return false;
    StringRef code = held.substr(sizeof size, size), orders = held.substr(sizeof size + size);
    LLVMContext &context = m.getContext();
    // Reading a part drops its empty compile unit list with a warning,
    // which is not the program's.
    struct Quiet : DiagnosticHandler {
        bool failed = false;
        bool handleDiagnostics(const DiagnosticInfo &d) override {
            failed |= d.getSeverity() == DS_Error;
            return true;
        }
    };
    std::unique_ptr<DiagnosticHandler> handler = context.getDiagnosticHandler();
    auto quiet = std::make_unique<Quiet>();
    Quiet &heard = *quiet;
    context.setDiagnosticHandler(std::move(quiet));
    auto parsed = parseBitcodeFile(MemoryBufferRef(code, "hirsel-tail"), context);
    bool failed = heard.failed;
    context.setDiagnosticHandler(std::move(handler));
    if (!parsed || failed) {
        if (!parsed) consumeError(parsed.takeError());
        return false;
    }
    Module &q = **parsed;
    // The part's constants outlive it in the context: none may name its
    // globals once they are gone.
    struct Unname {
        Module &q;
        ~Unname() {
            q.dropAllReferences();
            for (GlobalValue &g : q.global_values()) g.removeDeadConstantUsers();
        }
    } unname{q};
    Function *own = q.getFunction(f.getName());
    if (!own || own->isDeclaration() || own->getFunctionType() != f.getFunctionType() ||
        own->getCallingConv() != f.getCallingConv())
        return false;
    ValueToValueMapTy map;
    if (auto *bindings = q.getNamedMetadata(tail_debug_bindings)) {
        if (bindings->getNumOperands() != debug.size()) return false;
        for (auto [from, to] : zip_equal(bindings->operands(), debug)) {
            if (from->getMetadataID() != to->getMetadataID()) return false;
            map.MD()[from].reset(to);
        }
    } else if (!debug.empty()) {
        return false;
    }
    // A body may name itself.
    map[own] = &f;
    std::vector<Function *> declare;
    for (GlobalValue &g : q.global_values()) {
        if (&g == own) continue;
        GlobalValue *to = m.getNamedValue(g.getName());
        if (!to) {
            // A declaration the tail added (an intrinsic, a library call).
            auto *d = dyn_cast<Function>(&g);
            if (!d || !d->isDeclaration()) return false;
            declare.push_back(d);
            continue;
        }
        if (isa<Function>(to) != isa<Function>(&g) || to->getValueType() != g.getValueType()) return false;
        map[&g] = to;
    }
    // The part's uses move with its instructions, so `shared` keeps naming
    // them in `f` once they name `m`'s constants.
    SharedUses shared = shared_uses(*own);
    std::vector<uint32_t> order;
    if (!read_orders(shared, orders, order)) return false;
    for (Function *d : declare) {
        Function *n = Function::Create(d->getFunctionType(), GlobalValue::ExternalLinkage,
                                       d->getAddressSpace(), d->getName(), &m);
        n->copyAttributesFrom(d);
        map[d] = n;
    }
    GlobalValue::LinkageTypes linkage = f.getLinkage();
    GlobalValue::VisibilityTypes visibility = f.getVisibility();
    f.deleteBody();
    f.setLinkage(linkage);
    f.setVisibility(visibility);
    f.setAttributes(own->getAttributes());
    const RemapFlags flags = RF_IgnoreMissingLocals | RF_ReuseAndMutateDistinctMDs;
    SmallVector<std::pair<unsigned, MDNode *>, 4> attached;
    own->getAllMetadata(attached);
    for (auto &[kind, node] : attached) f.addMetadata(kind, *MapMetadata(node, map, flags));
    f.splice(f.end(), own);
    for (auto [from, to] = std::pair(own->arg_begin(), f.arg_begin()); from != own->arg_end(); ++from, ++to)
        hand_uses(*from, *to);
    for (BasicBlock &b : f)
        for (Instruction &i : b) {
            for (Use &u : i.operands()) {
                Value *v = u.get();
                Value *n = nullptr;
                if (auto *md = dyn_cast<MetadataAsValue>(v))
                    n = MetadataAsValue::get(context, MapMetadata(md->getMetadata(), map, flags));
                else if (isa<Constant>(v) && !isa<ConstantData>(v))
                    n = MapValue(v, map, flags);
                if (n && n != v) u.set(n);
            }
            SmallVector<std::pair<unsigned, MDNode *>, 4> tags;
            i.getAllMetadataOtherThanDebugLoc(tags);
            for (auto &[kind, node] : tags)
                if (MDNode *n = MapMetadata(node, map, flags); n != node) i.setMetadata(kind, n);
            if (DebugLoc loc = i.getDebugLoc())
                i.setDebugLoc(DebugLoc(cast<DILocation>(MapMetadata(loc.get(), map, flags))));
        }
    order_uses(shared, order);
    return true;
}
}  // namespace

// The release tail (`hirsel_llvm_tail_run`) of every definition of `module`,
// in module order. The tail of a definition is a function of its part at the
// cut, so a definition whose body a part can lend and take back exactly is
// keyed by that part's key (`hirsel_key::module_key`), the machine text, and
// the linkage the part does not keep: `find` answers 1 with the part after the
// tail when the caller holds it, the body is restored from it, and otherwise
// the tail runs in place and `keep` gets the part after it. `find` answers 2
// when the caller does not hold the tail and wants the part's bitcode at the
// cut with it. A module that cannot be split is not
// keyed: a definition the parts do not hold may reach what no part carries.
extern "C" void hirsel_llvm_release_tails(LLVMModuleRef module, LLVMTargetMachineRef machine,
    int (*find)(void *, const char *, size_t, const uint8_t *, size_t, const uint8_t **, size_t *),
    void (*keep)(void *, const char *, size_t, const uint8_t *, size_t, const uint8_t *, size_t,
                 const uint8_t *, size_t),
    void *context) {
    Module &m = *unwrap(module);
    // Local value names label printed IR only; a key does not take them.
    for (Function &f : m) {
        for (Argument &a : f.args()) a.setName("");
        for (BasicBlock &b : f) {
            b.setName("");
            for (Instruction &i : b) i.setName("");
        }
    }
    std::vector<Function *> bodies;
    for (Function &f : m)
        if (!f.isDeclaration()) bodies.push_back(&f);
    bool keyed = find && unsplittable(m).empty();
    const bool debug_info = keyed && m.getModuleFlag("Debug Info Version");
    // The pinned reader's UpgradeDebugInfo verifier looks up both names
    // even on x86. A key records the context's kind registry, as the
    // bitcode writer does, so a cache hit must not change later parts' keys.
    if (debug_info) {
        m.getContext().getMDKindID("reqd_work_group_size");
        m.getContext().getMDKindID("amdgpu.expected.active.lanes");
    }
    // A held part whose debug info the reader strips (always without line
    // tables, where it reads as version 0) has "heapallocsite" cleared by
    // name on every instruction that carries metadata, registering it.
    if (keyed) m.getContext().getMDKindID("heapallocsite");
    std::string machine_key = keyed ? machine_text(*reinterpret_cast<TargetMachine *>(machine)) : "";
    StringMap<std::string> sets = keyed ? asm_lines(m) : StringMap<std::string>();
    void *tail = hirsel_llvm_tail_open(module, machine);
    for (Function *f : bodies) {
        if (!keyed || !movable(*f)) {
            hirsel_llvm_tail_run(tail, wrap(f));
            continue;
        }
        std::string key = machine_key;
        hirsel_key::Digest ir;
        SmallVector<char, 0> input;
        SmallVector<MDNode *, 8> debug;
        auto *bindings = debug_info ? &debug : nullptr;
        const uint8_t *held = nullptr;
        size_t held_len = 0;
        int found = 0;
        with_part(m, *f, sets, &key, bindings, [&](auto &p) {
            ir = hirsel_key::module_key(*p);
            found = find(context, key.data(), key.size(), ir.data(), ir.size(), &held, &held_len);
            if (found == 2) bitcode(*p, input);
        });
        if (found == 1 && restore(m, *f, StringRef((const char *)held, held_len), debug))
            continue;
        hirsel_llvm_tail_run(tail, wrap(f));
        SmallVector<char, 0> after;
        part_bitcode(m, *f, sets, after, nullptr, bindings);
        uint64_t size = after.size();
        std::string value((const char *)&size, sizeof size);
        value.append(after.data(), after.size());
        use_orders(*f, value);
        keep(context, key.data(), key.size(), ir.data(), ir.size(), (const uint8_t *)input.data(),
             input.size(), (const uint8_t *)value.data(), value.size());
    }
    hirsel_llvm_tail_close(tail);
}
