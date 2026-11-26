// The release module's codegen parts. Whole-program O3 has run; each
// out-of-line definition is then a part keyed by what its bitcode records (key.h), a part whose
// object a caller already holds is not generated again, and each of the rest
// is generated from that bitcode alone, the parts in parallel.
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
#include <version>
#ifdef __GLIBC__
#if !__GLIBC_PREREQ(2, 30)
#undef _GLIBCXX_USE_PTHREAD_COND_CLOCKWAIT
#undef _GLIBCXX_USE_PTHREAD_MUTEX_CLOCKLOCK
#undef _GLIBCXX_USE_PTHREAD_RWLOCK_CLOCKLOCK
#endif
#endif
#include "key.h"
#include "pass_cost.h"

#include <llvm-c/Core.h>
#include <llvm-c/TargetMachine.h>
#include <llvm/ADT/DenseMap.h>
#include <llvm/ADT/STLFunctionalExtras.h>
#include <llvm/ADT/MapVector.h>
#include <llvm/ADT/DenseSet.h>
#include <llvm/ADT/SmallPtrSet.h>
#include <llvm/ADT/ScopeExit.h>
#include <llvm/ADT/StringMap.h>
#include <llvm/Bitcode/BitcodeReader.h>
#include <llvm/Bitcode/BitcodeWriter.h>
#include <llvm/IR/Constants.h>
#include <llvm/IR/DebugInfo.h>
#include <llvm/IR/DebugInfoMetadata.h>
#include <llvm/IR/DiagnosticInfo.h>
#include <llvm/IR/DiagnosticPrinter.h>
#include <llvm/IR/Instructions.h>
#include <llvm/IR/LegacyPassManager.h>
#include <llvm/IR/IntrinsicInst.h>
#include <llvm/IR/Metadata.h>
#include <llvm/IR/Module.h>
#include <llvm/IR/AssemblyAnnotationWriter.h>
#include <llvm/MC/TargetRegistry.h>
#include <llvm/Support/CBindingWrapping.h>
#include <llvm/Support/FormattedStream.h>
#include <llvm/Support/MemoryBuffer.h>
#include <llvm/Support/raw_ostream.h>
#include <llvm/Support/xxhash.h>
#include <llvm/Target/TargetMachine.h>
#include <llvm/TargetParser/Triple.h>
#include <llvm/Transforms/Utils/Cloning.h>
#include <llvm/Transforms/Utils/ValueMapper.h>
#include <algorithm>
#include <cstdio>
#include <cstring>
#include <atomic>
#include <numeric>
#include <optional>
#include <pthread.h>
#include <string>
#include <sys/resource.h>
#include <vector>

using namespace llvm;
extern "C" LLVMBool hirsel_llvm_emit_object(LLVMTargetMachineRef machine, LLVMModuleRef module,
                                          char **message, LLVMMemoryBufferRef *buffer);

namespace {

// An instance symbol (`h` and 64 lowercase hex digits) with its bridge
// suffixes (`.register`, `.words`, `.infallible`, `.ok`): unique in a program and the
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

// Where Module::print writes each definition: a function from its
// annotation to the end of its last block, a global variable up to its info
// comment (the end of its line).
struct Spans : AssemblyAnnotationWriter {
    DenseMap<const Value *, std::pair<size_t, size_t>> at;
    void emitFunctionAnnot(const Function *f, formatted_raw_ostream &os) override {
        at[f].first = os.tell();
    }
    void emitBasicBlockEndAnnot(const BasicBlock *b, formatted_raw_ostream &os) override {
        at[b->getParent()].second = os.tell();
    }
    void printInfoComment(const Value &v, formatted_raw_ostream &os) override {
        if (isa<GlobalVariable>(v)) at[&v].second = os.tell();
    }
};

// Rename every program-named definition from its content.
void name_from_content(Module &m) {
    std::vector<GlobalObject *> named;
    StringMap<unsigned> index;
    for (GlobalObject &g : m.global_objects())
        if (program_named(g)) { index[g.getName()] = named.size(); named.push_back(&g); }
    if (named.empty()) return;
    // Print the module once and take each definition's text from it: a
    // GlobalValue::print builds an AssemblyWriter, which walks every global
    // object, so one print per definition is quadratic in the module.
    std::vector<Text> texts;
    texts.reserve(named.size());
    std::string printed;
    {
        raw_string_ostream os(printed);
        Spans spans;
        m.print(os, &spans);
        for (GlobalObject *g : named) {
            auto at = spans.at.find(g);
            if (at == spans.at.end()) report_fatal_error("a definition's printed span is missing");
            auto [start, end] = at->second;
            if (isa<Function>(g)) {
                // The last block ends with the body's closing "}\n".
                if (printed.compare(end, 2, "}\n") != 0) report_fatal_error("a printed body does not close");
                end += 2;
            } else {
                // A global variable prints on one line, ending at its info comment.
                size_t line = printed.rfind('\n', end ? end - 1 : 0);
                start = line == std::string::npos ? 0 : line + 1;
            }
            texts.push_back(cut(printed.substr(start, end - start), index));
        }
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

// A constant a separate library lent with its bodies (`lent_bodies`):
// `linkonce_odr` hidden under the name the library's parts define it by. Like
// a local constant, each part that reaches it carries its own copy, and the
// link keeps one, so its address stays one address.
bool lent_constant(const GlobalVariable &g) {
    return g.hasInitializer() && g.isConstant() && g.hasLinkOnceODRLinkage() && g.hasHiddenVisibility() &&
           !g.isThreadLocal() && !g.hasSection() && !g.hasComdat();
}

// A constant every object that carries it may define, of which the link keeps
// one (`weak`, `weak_odr`, `linkonce`, `linkonce_odr`): a lent constant, a
// descriptor, or a placeholder (`weak`) that another object defines over.
// Like a local constant, each part that reaches it carries its own copy, under
// its own linkage and visibility: a placeholder's contents still never fold,
// and its address stays one address.
bool shared_constant(const GlobalVariable &g) {
    return g.hasInitializer() && g.isConstant() && g.isWeakForLinker() && !g.hasCommonLinkage() &&
           !g.isThreadLocal() && !g.hasSection() && !g.hasComdat();
}

// Whether a part that reaches `g` carries its initializer whatever it is for:
// a local constant, or one the link keeps one copy of.
bool carried(const GlobalVariable &g) { return g.hasLocalLinkage() || shared_constant(g); }

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
                if (var->hasInitializer() && (initializers || carried(*var))) value(var->getInitializer());
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

// Lists in `p`'s "llvm.dbg.cu" each compile unit its debug records name and
// it does not list yet, or the bitcode reader's verifier drops the part's
// debug info: a body moved in without a debug list, or one whose inlined
// records name another unit's subprogram, names a unit the part never saw.
void list_compile_units(Module &p) {
    DebugInfoFinder finder;
    finder.processModule(p);
    NamedMDNode *units = p.getOrInsertNamedMetadata("llvm.dbg.cu");
    SmallPtrSet<const MDNode *, 4> listed;
    for (const MDNode *node : units->operands()) listed.insert(node);
    for (DICompileUnit *unit : finder.compile_units())
        if (listed.insert(unit).second) units->addOperand(unit);
}

// The code descriptors (`$static`, PROG-9) and spawnable markers
// (`$spawnable$`) each defined function owns: a `weak_odr` descriptor whose
// code word is the function, and each marker named after such a descriptor.
// The whole-program optimizer folds a load of a descriptor's code word, so
// no part's code need reach the descriptor any more; its function's part
// defines it, and the markers that join it to a spawn entry, strong, so the
// link indexes them however the program's code was cut. Listed in module
// order. A library that lends its bodies has made its descriptors
// `linkonce_odr` (`lent_bodies`) and owns none.
using Owned = DenseMap<const Function *, SmallVector<GlobalVariable *, 2>>;
Owned owned_descriptors(Module &m) {
    Owned owned;
    StringMap<const Function *> owner;
    for (GlobalVariable &g : m.globals()) {
        if (!g.hasWeakODRLinkage() || !g.isConstant() || !g.getName().ends_with("$static")) continue;
        auto *init = dyn_cast_or_null<ConstantStruct>(g.hasInitializer() ? g.getInitializer() : nullptr);
        if (!init || init->getNumOperands() == 0) continue;
        auto *f = dyn_cast<Function>(init->getOperand(0)->stripPointerCasts());
        if (!f || f->isDeclaration()) continue;
        owner[g.getName()] = f;
        owned[f].push_back(&g);
    }
    for (GlobalVariable &g : m.globals()) {
        if (g.isDeclaration() || !g.isConstant()) continue;
        auto [descriptor, key] = g.getName().split("$spawnable$");
        if (key.empty()) continue;
        if (auto f = owner.find(descriptor); f != owner.end()) owned[f->second].push_back(&g);
    }
    return owned;
}

// Lends `use` the part of `f`: `f` alone, with what it references, in a
// module of its own. The part's definition borrows `f`'s body for the call: its blocks move in,
// its operands and metadata that name a global name the part's declaration
// instead, and all of it is put back after, so `m` is as it was, unless
// `use` takes the part: `f` is then left without a body. The move
// keeps every use-list order the code generator follows. A body the move
// cannot put back exactly (a block whose address is taken or debug records)
// is cloned instead, as is a codegen part with a subprogram. The clone's
// values list their uses in the order `f`'s list theirs.
// With `linkage`, the linkage and visibility the part does not keep of `f`
// and of each global it names, and whether each is defined, are written to it,
// and the part defines every variable it names with its initializer (a tail
// folds a load from a constant): the part is then a release tail's input.
// With `owned`, the part defines strong what `f` owns of it (`owned_descriptors`).
void with_part(Module &m, Function &f, const StringMap<std::string> &sets, std::string *linkage,
               SmallVectorImpl<MDNode *> *debug, function_ref<void(std::unique_ptr<Module> &)> use,
               const Owned *owned = nullptr) {
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
    ArrayRef<GlobalVariable *> own_data;
    if (owned)
        if (auto at = owned->find(&f); at != owned->end()) own_data = at->second;
    for (GlobalVariable *g : own_data) refs.value(g);
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
            if (var->hasInitializer() && (refs.initializers || carried(*var))) {
                // A local constant becomes one weak hidden copy per part; a
                // shared one keeps its own linkage.
                d->setLinkage(refs.initializers || var->hasLocalLinkage() ? GlobalValue::LinkOnceODRLinkage
                                                                          : var->getLinkage());
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
        // A release partition's local definitions are named by their content
        // (`name_from_content`), and another partition may define the same:
        // the link keeps one.
        own->setLinkage(m.getNamedMetadata("hirsel.partition") ? GlobalValue::WeakODRLinkage
                                                                : GlobalValue::ExternalLinkage);
        own->setVisibility(GlobalValue::HiddenVisibility);
    }
    map[&f] = own;
    for (GlobalVariable *g : own_data) cast<GlobalValue>(map[g])->setLinkage(GlobalValue::ExternalLinkage);
    // After `f` is mapped: a constant may point back at it.
    for (auto &[from, to] : constants) to->setInitializer(MapValue(from->getInitializer(), map));
    if (!assembly.empty()) p->appendModuleInlineAsm(Module::GlobalAsmFragment(assembly));
    if (!movable) {
        auto to = own->arg_begin();
        for (Argument &a : f.args()) map[&a] = &*to++;
        SmallVector<ReturnInst *, 8> returns;
        CloneFunctionInto(own, &f, map, CloneFunctionChangeType::DifferentModule, returns);
        order_uses_like(f, map);
        list_compile_units(*p);
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
    list_compile_units(*p);
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
// and keyed whole. With `shared`, a part carries each shared constant it
// reaches (`shared_constant`), as code generation does; without, only a lent
// one: a tail's part defines every constant `linkonce_odr`, and a library's
// lent bodies do too (`lent_bodies`), which would fold a placeholder.
std::string unsplittable_content(const Module &m, bool shared);
std::string unsplittable(const Module &m, bool shared) {
    // The cut reads x86-64 ELF and arm64 Mach-O objects (`split_native_object`).
    Triple triple(m.getTargetTriple());
    bool elf = triple.getArch() == Triple::x86_64 && triple.isOSBinFormatELF();
    bool macho = triple.getArch() == Triple::aarch64 && triple.isOSBinFormatMachO();
    if (!elf && !macho) return "the object split is x86-64 ELF and arm64 Mach-O only";
    return unsplittable_content(m, shared);
}

// Why `m`'s content, whatever its target, cannot be split: `unsplittable`
// less the target.
std::string unsplittable_content(const Module &m, bool shared) {
    if (!m.alias_empty() || !m.ifunc_empty()) return "aliases";
    if (!m.getComdatSymbolTable().empty()) return "comdats";
    for (const GlobalVariable &g : m.globals()) {
        if (g.hasAppendingLinkage()) return "appending global " + g.getName().str();
        if (g.isDeclaration()) continue;
        // A part copies each constant it reaches as its own weak definition,
        // so a constant must be local, or one the link keeps one copy of.
        if (shared ? shared_constant(g) : lent_constant(g)) continue;
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
        if (n.getName() != "llvm.module.flags" && n.getName() != "llvm.dbg.cu" &&
            n.getName() != "hirsel.partition")
            return "named metadata " + n.getName().str();
    return "";
}

// One missed part: its bitcode, or with one job its module in the composing
// context, in; its object or an LLVM message out.
struct Miss {
    SmallVector<char, 0> code;
    std::unique_ptr<Module> part;
    SmallVector<char, 0> object;
    std::string error;
};

// An error diagnostic is the part's error, and other diagnostics are dropped:
// reading a part without debug lines drops its empty compile unit list with a
// warning on every part.
void diagnose(const DiagnosticInfo *d, void *miss) {
    if (d->getSeverity() != DS_Error) return;
    std::string &error = static_cast<Miss *>(miss)->error;
    if (!error.empty()) return;
    raw_string_ostream os(error);
    DiagnosticPrinterRawOStream printer(os);
    d->print(printer);
}

// Whether `c` names globals, and only absolute symbols (`!absolute_symbol`:
// site words), so the link resolves every word of it and a load relocates none.
bool names_only_absolute(const Constant *c, SmallPtrSetImpl<const Constant *> &seen, bool &named) {
    if (!seen.insert(c).second) return true;
    if (auto *g = dyn_cast<GlobalValue>(c)) {
        named = true;
        return g->getAbsoluteSymbolRange().has_value();
    }
    if (isa<BlockAddress>(c) || isa<DSOLocalEquivalent>(c) || isa<NoCFIValue>(c)) return false;
    for (const Use &op : c->operands())
        if (!names_only_absolute(cast<Constant>(op.get()), seen, named)) return false;
    return true;
}

// LLVM files every constant that names a global under .data.rel.ro. A
// constant that names only absolute symbols needs no relocation at load, yet
// there it shares pages with the pointers the loader does relocate, and each
// such page becomes a private copy in every process. Put it in .rodata, which
// processes share. A reference that does need a load-time relocation refuses
// to link from a read-only section, so a wrong call here fails the link.
void absolute_constants_read_only(Module &part, const Triple &target) {
    if (!target.isOSBinFormatELF()) return;
    for (GlobalVariable &g : part.globals()) {
        if (g.isDeclaration() || !g.hasName() || !g.isConstant() || g.isExternallyInitialized() ||
            g.hasSection() || g.isThreadLocal() || g.hasComdat())
            continue;
        SmallPtrSet<const Constant *, 16> seen;
        bool named = false;
        if (names_only_absolute(g.getInitializer(), seen, named) && named)
            g.setSection((".rodata." + g.getName()).str());
    }
}

// The pass manager holds the object stream for its lifetime. LLVM runs
// MachineModuleInfo and AsmPrinter initialization and finalization on each
// module, as in llc's compile-twice path.
class ObjectEmitter {
    SmallVector<char, 0> bytes;
    raw_svector_ostream stream{bytes};
    std::optional<legacy::PassManager> passes;

public:
    // A part held in the composing context is generated as is: its bitcode
    // read back is the same module, use-list orders included.
    void generate_held(TargetMachine &tm, Miss &miss) {
        LLVMContext &context = miss.part->getContext();
        auto previous = context.getDiagnosticHandler();
        context.setDiagnosticHandler(std::make_unique<DiagnosticHandler>());
        context.setDiagnosticHandlerCallBack(diagnose, &miss);
        auto restore = make_scope_exit([&] { context.setDiagnosticHandler(std::move(previous)); });
        run(tm, *miss.part, miss);
        miss.part.reset();
    }

    void generate(TargetMachine &tm, Miss &miss) {
        LLVMContext context;
        context.setDiscardValueNames(true);
        context.setDiagnosticHandlerCallBack(diagnose, &miss);
        auto parsed = parseBitcodeFile(MemoryBufferRef(StringRef(miss.code.data(), miss.code.size()), "hirsel-part"), context);
        if (!parsed) {
            if (miss.error.empty()) miss.error = toString(parsed.takeError());
            else consumeError(parsed.takeError());
            return;
        }
        run(tm, **parsed, miss);
    }

private:
    void run(TargetMachine &tm, Module &part, Miss &miss) {
        part.setDataLayout(tm.createDataLayout());
        absolute_constants_read_only(part, tm.getTargetTriple());
        hirsel_llvm::PassCost cost("object-emission", part);
        if (!passes) {
            passes.emplace();
            if (tm.addPassesToEmitFile(*passes, stream, nullptr, CodeGenFileType::ObjectFile)) {
                passes.reset();
                miss.error = "the target machine cannot emit an object";
                return;
            }
        }
        passes->run(part);
        miss.object = std::move(bytes);
        bytes.clear();
    }
};

constexpr size_t worker_stack = size_t(16) << 20;
// Bank release codegen used about 42 MiB of data per LLVM worker beyond its
// stack. Allow 64 MiB for the target machine, passes and allocator growth.
constexpr size_t worker_budget = worker_stack + (size_t(64) << 20);

size_t worker_width(size_t parts, unsigned jobs) {
    size_t width = std::min<size_t>(std::max(jobs, 1u), parts);
    if (width <= 1) return width;

    struct rlimit limit;
    if (getrlimit(RLIMIT_DATA, &limit) != 0) return 1;
    if (limit.rlim_cur == RLIM_INFINITY) return width;
    FILE *status = std::fopen("/proc/self/status", "r");
    if (!status) return 1;
    auto close = make_scope_exit([&] { std::fclose(status); });
    char line[256];
    while (std::fgets(line, sizeof(line), status)) {
        unsigned long long kib;
        if (std::sscanf(line, "VmData: %llu", &kib) != 1) continue;
        auto used = kib << 10;
        if (used >= limit.rlim_cur) return 1;
        // Count the caller too: it needs LLVM heap space beside the new stacks.
        auto fit = (limit.rlim_cur - used) / worker_budget;
        return std::max<size_t>(1, std::min<unsigned long long>(width, fit));
    }
    return 1;
}

// Runs each thread's `start()` on the indices of `order`, taken in order. Each
// thread, including the caller, has its own target machine, or none when LLVM
// cannot create one. Width limits concurrency without changing the parts.
template <typename Start>
void on_threads(const TargetMachine &tm, const std::vector<size_t> &order, unsigned jobs, Start start) {
    size_t width = worker_width(order.size(), jobs);
    if (width == 0) return;
    struct Shared {
        const TargetMachine &tm;
        const std::vector<size_t> &order;
        Start &start;
        std::atomic<size_t> next{0};
    } shared{tm, order, start};
    void *(*run)(void *) = [](void *arg) -> void * {
        Shared &s = *static_cast<Shared *>(arg);
        std::unique_ptr<TargetMachine> machine(s.tm.getTarget().createTargetMachine(
            s.tm.getTargetTriple(), s.tm.getTargetCPU(), s.tm.getTargetFeatureString(), s.tm.Options,
            s.tm.getRelocationModel(), s.tm.getCodeModel(), s.tm.getOptLevel()));
        auto work = s.start();
        for (size_t k; (k = s.next.fetch_add(1)) < s.order.size();) work(machine.get(), s.order[k]);
        return nullptr;
    };
    std::vector<pthread_t> threads;
    pthread_attr_t attr;
    if (width > 1 && pthread_attr_init(&attr) == 0) {
        if (pthread_attr_setstacksize(&attr, worker_stack) == 0) {
            threads.reserve(width - 1);
            for (size_t i = 1; i < width; ++i) {
                pthread_t t;
                if (pthread_create(&t, &attr, run, &shared) != 0) break;
                threads.push_back(t);
            }
        }
        pthread_attr_destroy(&attr);
    }
    run(&shared);
    for (pthread_t t : threads) pthread_join(t, nullptr);
}

// The indices of `n` items, largest first: the longest jobs start first.
template <typename Size>
std::vector<size_t> largest_first(size_t n, Size size) {
    std::vector<size_t> order(n);
    std::iota(order.begin(), order.end(), 0);
    std::stable_sort(order.begin(), order.end(), [&](size_t a, size_t b) { return size(a) > size(b); });
    return order;
}

// A section per definition, and no code generation across definitions:
// a part's code is a function of the part alone.
void part_options(TargetMachine &tm) {
    tm.Options.FunctionSections = true;
    tm.Options.DataSections = true;
    tm.Options.UniqueSectionNames = true;
    tm.Options.EnableIPRA = false;
    tm.Options.EnableMachineOutliner = false;
}

// A Mach-O object records neither an atom's alignment nor its size: an atom
// runs to the next symbol, and its section takes its widest member's
// alignment. So the cut (`split_native_object`) could give a part neighbour-
// dependent bytes. A split module's definitions all align to 16, so each atom
// runs to its own size rounded up to 16 whatever follows it; this lowers a hot
// loop's 64 (`hirsel_llvm_align_release_functions`). Its switches lower to
// branches: AArch64's printer labels a jump table with an assembler temporary,
// which the object addresses from its section's start with no length. And
// no code is outlined: a part generated on its own machine (`on_threads`),
// which AArch64 builds with the outliner on, would call a local helper the
// cut cannot give it. And no fused compare-select pair is aligned: Apple
// tuning (`align-cmp-csel-pairs`, AArch64CodeLayoutOpt) puts a 32-bit CMP
// and its CSEL at a 64-byte boundary and raises the function to 64 with
// them, past what the cut can place. The parts' keys take all four.
void macho_part_layout(Module &m) {
    for (Function &f : m)
        if (!f.isDeclaration()) {
            f.addFnAttr("no-jump-tables", "true");
            f.addFnAttr(Attribute::NoOutline);
            StringRef features = f.getFnAttribute("target-features").getValueAsString();
            f.addFnAttr("target-features", features.empty() ? std::string("-align-cmp-csel-pairs")
                                                            : (features + ",-align-cmp-csel-pairs").str());
            f.setAlignment(Align(16));
        }
    for (GlobalVariable &g : m.globals())
        if (!g.isDeclaration() && g.getAlign().valueOrOne() < Align(16)) g.setAlignment(Align(16));
}

// Generate every miss held in the composing context, on this thread, as
// `generate_all` would on one.
void generate_here(const TargetMachine &tm, std::vector<Miss> &misses) {
    std::unique_ptr<TargetMachine> machine(tm.getTarget().createTargetMachine(
        tm.getTargetTriple(), tm.getTargetCPU(), tm.getTargetFeatureString(), tm.Options,
        tm.getRelocationModel(), tm.getCodeModel(), tm.getOptLevel()));
    ObjectEmitter emitter;
    for (Miss &miss : misses) {
        if (machine) emitter.generate_held(*machine, miss);
        else miss.error = "LLVM could not create the target machine";
    }
}

// Generate every miss on up to `jobs` threads.
void generate_all(const TargetMachine &tm, std::vector<Miss> &misses, unsigned jobs) {
    auto order = largest_first(misses.size(), [&](size_t k) { return misses[k].code.size(); });
    on_threads(tm, order, jobs, [&] {
        return [&, emitter = std::make_unique<ObjectEmitter>()](TargetMachine *machine, size_t k) {
            if (machine) emitter->generate(*machine, misses[k]);
            else misses[k].error = "LLVM could not create the target machine";
        };
    });
}

// The bodies a separately generated library lends the programs over it, as
// bitcode: every definition `available_externally`, under the name its part's
// object defines (a local definition's own name with `own` after it, as the
// library's link contract renames it), each constant `linkonce_odr` hidden as
// a part defines it. A program's optimizer may inline a body and then drops
// it; every call it keeps reaches the part. A body over `limit` instructions,
// or one naming a symbol module assembly sets, is lent as a declaration.
void lent_bodies(const Module &m, const StringMap<std::string> &sets, size_t limit, StringRef own,
                 SmallVectorImpl<char> &out) {
    // Only the bodies lent are cloned: a whole library's copy would hold every
    // body it keeps as a declaration through the clone.
    DenseSet<const Function *> lent;
    for (const Function &f : m) {
        if (f.isDeclaration()) continue;
        size_t size = 0;
        bool named = false;
        for (const BasicBlock &b : f)
            for (const Instruction &i : b) {
                ++size;
                for (const Use &op : i.operands())
                    if (auto *g = dyn_cast<GlobalVariable>(op.get()); g && sets.count(g->getName())) named = true;
            }
        if (!named && size <= limit) lent.insert(&f);
    }
    ValueToValueMapTy map;
    std::unique_ptr<Module> p = CloneModule(m, map, [&](const GlobalValue *g) {
        auto *f = dyn_cast<Function>(g);
        return !f || lent.count(f);
    });
    p->setModuleInlineAsm(ArrayRef<Module::GlobalAsmFragment>());
    for (GlobalVariable &g : p->globals()) {
        if (g.isDeclaration()) continue;
        g.setLinkage(GlobalValue::LinkOnceODRLinkage);
        g.setVisibility(GlobalValue::HiddenVisibility);
    }
    for (const Function &f : m) {
        if (f.isDeclaration()) continue;
        auto *c = cast<Function>(map[&f]);
        if (f.hasLocalLinkage()) {
            c->setName(f.getName() + own);
            c->setLinkage(GlobalValue::ExternalLinkage);
            c->setVisibility(GlobalValue::HiddenVisibility);
        }
        // A body not cloned is a declaration already; dropping it also clears
        // what the clone copied onto it (its metadata, prefix and prologue).
        if (lent.count(&f)) c->setLinkage(GlobalValue::AvailableExternallyLinkage);
        else c->deleteBody();
    }
    bitcode(*p, out);
}

}  // namespace

// Generate every definition of `module` a caller does not hold. Each
// out-of-line definition is a part keyed by what its bitcode records
// (`hirsel_key::module_key`); `reuse` gets the definition name, the machine
// text and the part's key, and answers 1
// when it holds the part's object. With `jobs` 0 the rest are generated
// together in one object with a section per definition, which `deliver` gets
// and the caller cuts per part. Otherwise each missed part is generated from
// its bitcode alone on up to `jobs` threads, and `deliver` gets its object, in
// part order. A module that cannot be cut (`unsplittable`) is one part named
// "" keyed by the module's key and generated whole. With `lend`, a library's
// bodies (`lent_bodies`, under `lend_limit` and `own`) go to `lend` first.
// With `code`, no part is asked for or generated: each part's name, whether
// its object defines that name strong, and the bitcode its object is
// generated from (`hirsel_llvm_generate_parts`) go to `code`, in part order.
// Returns 0, or 1 with `*error` set to an LLVM message.
extern "C" int hirsel_llvm_emit_parts(LLVMModuleRef module, LLVMTargetMachineRef machine, unsigned jobs,
    int (*reuse)(void *, const char *, size_t, const char *, size_t, const uint8_t *, size_t),
    void (*deliver)(void *, const uint8_t *, size_t), void *context, size_t lend_limit,
    const char *own, void (*lend)(void *, const uint8_t *, size_t),
    void (*code)(void *, const char *, size_t, int, const uint8_t *, size_t), char **error) {
    Module &m = *unwrap(module);
    TargetMachine &tm = *reinterpret_cast<TargetMachine *>(machine);
    part_options(tm);
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
    bool whole = !unsplittable(m, /*shared=*/true).empty();
    if (!whole && tm.getTargetTriple().isOSBinFormatMachO()) macho_part_layout(m);
    if (lend) {
        // A library that lends its bodies defines no placeholder (`weak`):
        // the program's link defines over it, and a lent copy of its contents
        // would fold. Every other constant the link keeps one of is lent.
        for (GlobalVariable &g : m.globals()) {
            if (g.isDeclaration() || !g.isConstant() || g.hasSection() || g.hasComdat()) continue;
            if (g.hasWeakAnyLinkage()) {
                g.setInitializer(nullptr);
                g.setLinkage(GlobalValue::ExternalLinkage);
            } else if (g.hasWeakODRLinkage()) {
                g.setLinkage(GlobalValue::LinkOnceODRLinkage);
                g.setVisibility(GlobalValue::HiddenVisibility);
            }
        }
        // A library whose target alone has no split (AArch64) is lent from
        // and generated whole: its local functions become hidden globals, as
        // its parts' are, so the names the lent bodies call are the names the
        // library's link contract gives its object's definitions.
        if (std::string content = unsplittable_content(m, /*shared=*/false); !content.empty()) {
            *error = strdup(("a library that lends its bodies is cut per definition: " + content).c_str());
            return 1;
        }
        SmallVector<char, 0> lent;
        lent_bodies(m, asm_lines(m), lend_limit, own, lent);
        lend(context, (const uint8_t *)lent.data(), lent.size());
        if (whole)
            for (Function &f : m)
                if (!f.isDeclaration() && f.hasLocalLinkage()) {
                    f.setLinkage(GlobalValue::ExternalLinkage);
                    f.setVisibility(GlobalValue::HiddenVisibility);
                }
    }
    if (code) {
        SmallVector<char, 0> bytes;
        if (whole) {
            bitcode(m, bytes);
            code(context, "", 0, 1, (const uint8_t *)bytes.data(), bytes.size());
            return 0;
        }
        StringMap<std::string> sets = asm_lines(m);
        Owned owned = owned_descriptors(m);
        for (Function &f : m) {
            if (f.isDeclaration()) continue;
            with_part(m, f, sets, nullptr, nullptr, [&](auto &part) {
                Module &p = *part;
                bytes.clear();
                bitcode(p, bytes);
                bool strong = !p.getFunction(f.getName())->isWeakForLinker();
                code(context, f.getName().data(), f.getName().size(), strong,
                     (const uint8_t *)bytes.data(), bytes.size());
            }, &owned);
        }
        return 0;
    }
    auto ask = [&](StringRef name, const hirsel_key::Digest &key) {
        return reuse(context, name.data(), name.size(), machine_key.data(), machine_key.size(),
                     key.data(), key.size()) != 0;
    };
    std::vector<Miss> misses;
    if (whole) {
        if (ask("", hirsel_key::module_key(m))) return 0;
        jobs = 0;
        misses.emplace_back();
    } else {
        // Keyed while local definitions are still local: a part then
        // carries each constant it reaches with its initializer, which the
        // part's object holds a copy of. A missed part's bitcode is the
        // input its worker generates from.
        StringMap<std::string> sets = asm_lines(m);
        Owned owned = owned_descriptors(m);
        std::vector<Function *> held;
        for (Function &f : m) {
            if (f.isDeclaration()) continue;
            with_part(m, f, sets, nullptr, nullptr, [&](auto &p) {
                if (ask(f.getName(), hirsel_key::module_key(*p))) {
                    held.push_back(&f);
                    return;
                }
                Miss &miss = misses.emplace_back();
                // One job generates on this thread, after every part is
                // keyed: the part itself, with no bitcode between.
                if (jobs == 1) miss.part = std::move(p);
                else if (jobs) bitcode(*p, miss.code);
            }, &owned);
        }
        if (jobs) {
            if (misses.empty()) return 0;
            if (jobs == 1) generate_here(tm, misses);
            else generate_all(tm, misses, jobs);
            for (Miss &miss : misses)
                if (!miss.error.empty()) {
                    *error = strdup(miss.error.c_str());
                    return 1;
                }
            for (Miss &miss : misses) deliver(context, (const uint8_t *)miss.object.data(), miss.object.size());
            return 0;
        }
        // Every local definition is a hidden global: a part's references to
        // another part's definitions resolve at link time (weak in a release
        // partition, as `with_part` makes them).
        bool partition = m.getNamedMetadata("hirsel.partition");
        for (GlobalObject &g : m.global_objects())
            if (!g.isDeclaration() && g.hasLocalLinkage()) {
                g.setLinkage(partition ? GlobalValue::WeakODRLinkage : GlobalValue::ExternalLinkage);
                g.setVisibility(GlobalValue::HiddenVisibility);
            }
        // What a generated part owns it defines strong, as its own bitcode
        // does; a held part's object defines its own.
        DenseSet<const Function *> kept(held.begin(), held.end());
        for (auto &[f, data] : owned)
            if (!kept.contains(f))
                for (GlobalVariable *g : data) g->setLinkage(GlobalValue::ExternalLinkage);
        for (Function *f : held) f->deleteBody();
    }
    if (misses.empty()) return 0;
    LLVMMemoryBufferRef object = nullptr;
    if (hirsel_llvm_emit_object(machine, module, error, &object)) return 1;
    deliver(context, (const uint8_t *)LLVMGetBufferStart(object), LLVMGetBufferSize(object));
    LLVMDisposeMemoryBuffer(object);
    return 0;
}

// Generate the `count` parts whose bitcode `codes` holds (`code` of
// `hirsel_llvm_emit_parts`), each alone, on up to `jobs` threads, as
// `hirsel_llvm_emit_parts` generates a missed part: `deliver` gets each
// part's object, in order. Returns 0, or 1 with `*error` set to an LLVM
// message.
extern "C" int hirsel_llvm_generate_parts(LLVMTargetMachineRef machine, const uint8_t *const *codes,
    const size_t *lens, size_t count, unsigned jobs, void (*deliver)(void *, const uint8_t *, size_t),
    void *context, char **error) {
    TargetMachine &tm = *reinterpret_cast<TargetMachine *>(machine);
    part_options(tm);
    std::vector<Miss> misses(count);
    for (size_t i = 0; i < count; ++i) misses[i].code.assign(codes[i], codes[i] + lens[i]);
    generate_all(tm, misses, jobs);
    for (Miss &miss : misses)
        if (!miss.error.empty()) {
            *error = strdup(miss.error.c_str());
            return 1;
        }
    for (Miss &miss : misses) deliver(context, (const uint8_t *)miss.object.data(), miss.object.size());
    return 0;
}

extern "C" void *hirsel_llvm_tail_open(LLVMModuleRef module, LLVMTargetMachineRef machine);
extern "C" LLVMBool hirsel_llvm_take_tail_change(LLVMValueRef function);
extern "C" void hirsel_llvm_tail_run(void *tail, LLVMValueRef function, LLVMBool changed);
extern "C" void hirsel_llvm_tail_close(void *tail);

namespace {
constexpr StringLiteral TailChangedKey = "tail-cleanup post-o3-changed\n";
constexpr StringLiteral TailUnchangedKey = "tail-cleanup post-o3-unchanged\n";

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
    // As in `generate`: reading a part drops its empty compile unit list
    // with a warning, which is not the program's.
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

// What a part declares of a global besides its name and type: a tail that
// changes it for one definition changes the part of every other definition
// that names it.
struct Declared {
    AttributeList attributes;
    unsigned convention = 0;
    MaybeAlign align;
    const Constant *initializer = nullptr;
    bool constant = false;
    GlobalValue::UnnamedAddr unnamed{};
    GlobalValue::VisibilityTypes visibility{};
    std::string section;
    bool operator==(const Declared &o) const {
        return attributes == o.attributes && convention == o.convention && align == o.align &&
               initializer == o.initializer && constant == o.constant && unnamed == o.unnamed &&
               visibility == o.visibility && section == o.section;
    }
    bool operator!=(const Declared &o) const { return !(*this == o); }
};

Declared declared(const GlobalValue &g) {
    Declared d;
    d.unnamed = g.getUnnamedAddr();
    d.visibility = g.getVisibility();
    if (auto *o = dyn_cast<GlobalObject>(&g)) {
        if (auto *f = dyn_cast<Function>(o)) d.align = f->getAlign();
        else d.align = cast<GlobalVariable>(o)->getAlign();
        d.section = o->getSection().str();
    }
    if (auto *f = dyn_cast<Function>(&g)) {
        d.attributes = f->getAttributes();
        d.convention = f->getCallingConv();
    } else if (auto *v = dyn_cast<GlobalVariable>(&g)) {
        d.initializer = v->hasInitializer() ? v->getInitializer() : nullptr;
        d.constant = v->isConstant();
    }
    return d;
}

std::vector<std::pair<const GlobalValue *, Declared>> declared_all(const Module &m) {
    std::vector<std::pair<const GlobalValue *, Declared>> all;
    for (const GlobalValue &g : m.global_values()) all.push_back({&g, declared(g)});
    return all;
}

// The variables `f` reaches: those its instructions name, through constants
// and the initializers of the variables they name (a load from a constant
// folds to its initializer). A function pass sees no other variable, so a
// tail of `f` changes the declaration of no other.
void reached_variables(const Function &f, SmallPtrSetImpl<const GlobalVariable *> &out) {
    SmallPtrSet<const Constant *, 32> seen;
    SmallVector<const Constant *, 64> work;
    auto push = [&](const Value *v) {
        if (auto *c = dyn_cast<Constant>(v); c && !isa<ConstantData>(c) && seen.insert(c).second) work.push_back(c);
    };
    for (const BasicBlock &b : f)
        for (const Instruction &i : b)
            for (const Use &op : i.operands()) push(op.get());
    while (!work.empty()) {
        const Constant *c = work.pop_back_val();
        if (auto *g = dyn_cast<GlobalValue>(c)) {
            if (auto *v = dyn_cast<GlobalVariable>(g)) {
                out.insert(v);
                if (v->hasInitializer()) push(v->getInitializer());
            }
            continue;
        }
        for (const Use &op : c->operands()) push(op.get());
    }
}

// One definition's release tail. Every part is keyed in module order before
// any tail runs, the missed parts' tails run on worker threads, and each
// tail is then applied in module order, so the module goes through exactly
// the states the tails run one after another in place give it.
struct Tail {
    Function *f;
    std::string name;
    bool changed = false;
    // Live: runs in place, unkeyed (a body a part cannot lend back exactly).
    // Held: the caller holds the part after the tail. Missed: a worker runs it.
    enum Kind { Live, Held, Missed } kind = Live;
    std::string key;
    hirsel_key::Digest ir{};
    SmallVector<MDNode *, 8> debug;
    // The part at the cut, which a worker reads and `keep` may get.
    SmallVector<char, 0> input;
    bool witness = false;
    // The part after the tail, as `find` answers and `keep` gets it.
    std::string value;
    // The program's globals the part names.
    std::vector<const GlobalValue *> names;
    // A worker's tail changed what the part declares, or could not run:
    // the tail runs in place, where its changes are the module's.
    bool live = false;
};

// Runs `t`'s tail on its part in a context of its own, as a part read back
// runs it (tail_context_test.cpp): the part's definitions get back the
// linkage the key records, the tail runs, and the part after it is written
// with its use orders.
void tail_part(TargetMachine &tm, Tail &t, size_t machine_text_size) {
    LLVMContext context;
    context.setDiscardValueNames(true);
    struct Quiet : DiagnosticHandler {
        bool failed = false;
        bool handleDiagnostics(const DiagnosticInfo &d) override {
            failed |= d.getSeverity() == DS_Error;
            return true;
        }
    };
    auto quiet = std::make_unique<Quiet>();
    Quiet &heard = *quiet;
    context.setDiagnosticHandler(std::move(quiet));
    auto parsed = parseBitcodeFile(MemoryBufferRef(StringRef(t.input.data(), t.input.size()), "hirsel-tail"), context);
    if (!parsed || heard.failed) {
        if (!parsed) consumeError(parsed.takeError());
        t.live = true;
        return;
    }
    Module &p = **parsed;
    Function *own = p.getFunction(t.name);
    if (!own || own->isDeclaration()) {
        t.live = true;
        return;
    }
    StringRef mode = t.changed ? TailChangedKey : TailUnchangedKey;
    StringRef facts = StringRef(t.key).drop_front(machine_text_size + mode.size());
    while (!facts.empty()) {
        auto [line, rest] = facts.split('\n');
        facts = rest;
        auto [name, attributes] = line.split(' ');
        auto [link, tail] = attributes.split(' ');
        unsigned linkage, visibility;
        GlobalValue *value = p.getNamedValue(name);
        if (!value || link.getAsInteger(10, linkage) || tail.split(' ').first.getAsInteger(10, visibility)) {
            t.live = true;
            return;
        }
        if (!value->isDeclaration()) {
            value->setVisibility(static_cast<GlobalValue::VisibilityTypes>(visibility));
            value->setLinkage(static_cast<GlobalValue::LinkageTypes>(linkage));
        }
    }
    SmallVector<MDNode *, 8> debug;
    if (auto *bindings = p.getNamedMetadata(tail_debug_bindings)) append_range(debug, bindings->operands());
    auto before = declared_all(p);
    size_t variables = p.global_size();
    void *tail = hirsel_llvm_tail_open(wrap(&p), reinterpret_cast<LLVMTargetMachineRef>(&tm));
    hirsel_llvm_tail_run(tail, wrap(own), t.changed);
    hirsel_llvm_tail_close(tail);
    // Only the definition's own body and attributes may change: in place,
    // a change to anything else would be the program's.
    if (p.global_size() != variables || heard.failed) {
        t.live = true;
        return;
    }
    for (auto &[g, was] : before)
        if (g != own && declared(*g) != was) {
            t.live = true;
            return;
        }
    SmallVector<char, 0> after;
    part_bitcode(p, *own, asm_lines(p), after, nullptr, &debug);
    uint64_t size = after.size();
    t.value.assign((const char *)&size, sizeof size);
    t.value.append(after.data(), after.size());
    use_orders(*own, t.value);
}
}  // namespace

// The release tail (`hirsel_llvm_tail_run`) of every definition of `module`,
// in module order. The tail of a definition is a function of its part at the
// cut, so a definition whose body a part can lend and take back exactly is
// keyed by that part's key (`hirsel_key::module_key`), the machine text, and
// whether it changed after O3, and the linkage the part does not keep: `find`
// answers 1 with the part after the
// tail when the caller holds it, the body is restored from it, and otherwise
// the tail runs from the part's bitcode on one of `jobs` threads (one when
// `jobs` is 0 or 1), the body is restored from the part after it in the same
// way, and `keep` gets that part. So a body goes through the same bitcode
// round trip whatever `jobs` is: a body run in place shares its debug
// locations and alias scopes with the module's other bodies, while a
// restored one brings its own, and the lent bodies a library writes from the
// module take that difference. `find`
// answers 2 when the caller does not hold the tail and wants the part's
// bitcode at the cut with it. A module that cannot be split is not keyed: a
// definition the parts do not hold may reach what no part carries.
//
// Each part is keyed before any tail runs, and each tail is applied in module
// order, so the module goes through the states the tails run one after
// another give it: a part that names a global an earlier tail changed the
// declaration of is keyed again when its turn comes, and a worker's tail that
// changes anything besides its definition's body and attributes runs again in
// place.
extern "C" void hirsel_llvm_release_tails(LLVMModuleRef module, LLVMTargetMachineRef machine, unsigned jobs,
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
    bool keyed = find && unsplittable(m, /*shared=*/false).empty();
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
    TargetMachine &tm = *reinterpret_cast<TargetMachine *>(machine);
    std::string machine_key = keyed ? machine_text(tm) : "";
    StringMap<std::string> sets = keyed ? asm_lines(m) : StringMap<std::string>();
    void *tail = hirsel_llvm_tail_open(module, machine);
    // Keys `t`'s part as the module is now and asks the caller for its tail.
    auto ask = [&](Tail &t) {
        t.key = machine_key;
        t.key += t.changed ? TailChangedKey : TailUnchangedKey;
        t.debug.clear();
        t.input.clear();
        t.names.clear();
        int found = 0;
        const uint8_t *held = nullptr;
        size_t held_len = 0;
        with_part(m, *t.f, sets, &t.key, debug_info ? &t.debug : nullptr, [&](auto &part) {
            Module &p = *part;
            t.ir = hirsel_key::module_key(p);
            found = find(context, t.key.data(), t.key.size(), t.ir.data(), t.ir.size(), &held, &held_len);
            if (found != 1) bitcode(p, t.input);
            for (const GlobalValue &g : p.global_values())
                if (g.getName() != t.f->getName()) {
                    // A global the part cannot name back is always stale.
                    const GlobalValue *named = m.getNamedValue(g.getName());
                    t.names.push_back(g.hasName() ? named : nullptr);
                }
        });
        t.witness = found == 2;
        t.kind = found == 1 ? Tail::Held : Tail::Missed;
        t.live = false;
        // The caller's answer is borrowed until its next call.
        if (found == 1) t.value.assign((const char *)held, held_len);
        else t.value.clear();
    };
    std::vector<Tail> tails(bodies.size());
    for (size_t k = 0; k < bodies.size(); ++k) {
        Tail &t = tails[k];
        t.f = bodies[k];
        t.name = t.f->getName().str();
        t.changed = hirsel_llvm_take_tail_change(wrap(t.f));
    }
    for (Tail &t : tails) {
        if (keyed && movable(*t.f)) ask(t);
    }
    {
        std::vector<size_t> missed;
        for (size_t k = 0; k < tails.size(); ++k)
            if (tails[k].kind == Tail::Missed) missed.push_back(k);
        auto order = largest_first(missed.size(), [&](size_t k) { return tails[missed[k]].input.size(); });
        for (size_t &k : order) k = missed[k];
        on_threads(tm, order, jobs, [&] {
            return [&](TargetMachine *worker, size_t k) {
                if (worker) tail_part(*worker, tails[k], machine_key.size());
                else tails[k].live = true;
            };
        });
    }
    // Globals whose declaration an applied tail changed.
    DenseSet<const GlobalValue *> changed;
    auto stale = [&](const Tail &t) {
        return any_of(t.names, [&](const GlobalValue *g) { return !g || changed.contains(g); });
    };
    // What each global the module had when a tail last looked at it
    // declares. A tail may change any function's declaration (a library call
    // it builds takes the existing declaration and its inferred attributes),
    // but only the variables its definition reaches (`reached_variables`).
    DenseMap<const GlobalValue *, Declared> known;
    auto look = [&](const GlobalValue &g) { known.try_emplace(&g, declared(g)); };
    auto recheck = [&](const GlobalValue &g) {
        Declared now = declared(g);
        auto [at, fresh] = known.try_emplace(&g, now);
        if (!fresh && at->second != now) {
            changed.insert(&g);
            at->second = std::move(now);
        }
    };
    // Runs `t`'s tail in place, noting what it changed besides the body.
    auto run_live = [&](Tail &t) {
        if (!keyed) return hirsel_llvm_tail_run(tail, wrap(t.f), t.changed);
        SmallPtrSet<const GlobalVariable *, 32> variables;
        reached_variables(*t.f, variables);
        for (const Function &f : m) look(f);
        for (const GlobalVariable *v : variables) look(*v);
        hirsel_llvm_tail_run(tail, wrap(t.f), t.changed);
        reached_variables(*t.f, variables);
        for (const Function &f : m) recheck(f);
        for (const GlobalVariable *v : variables) recheck(*v);
    };
    // Runs `t`'s tail in place and gives `keep` the part after it.
    auto run_kept = [&](Tail &t) {
        run_live(t);
        SmallVector<char, 0> after;
        part_bitcode(m, *t.f, sets, after, nullptr, debug_info ? &t.debug : nullptr);
        uint64_t size = after.size();
        std::string value((const char *)&size, sizeof size);
        value.append(after.data(), after.size());
        use_orders(*t.f, value);
        keep(context, t.key.data(), t.key.size(), t.ir.data(), t.ir.size(), (const uint8_t *)t.input.data(),
             t.witness ? t.input.size() : 0, (const uint8_t *)value.data(), value.size());
    };
    // Restores `t`'s body from the part after its tail.
    auto restored = [&](Tail &t) {
        AttributeList was = t.f->getAttributes();
        if (!restore(m, *t.f, t.value, t.debug)) return false;
        if (t.f->getAttributes() != was) changed.insert(t.f);
        if (auto at = known.find(t.f); at != known.end()) at->second = declared(*t.f);
        return true;
    };
    for (Tail &t : tails) {
        if (!keyed || !movable(*t.f)) {
            run_live(t);
            continue;
        }
        // What the part names changed since it was keyed: key it again, now.
        if (stale(t)) {
            ask(t);
            if (t.kind == Tail::Missed) t.live = true;
        }
        if (t.kind == Tail::Held) {
            if (!restored(t)) run_kept(t);
            continue;
        }
        if (!t.live && restored(t)) {
            keep(context, t.key.data(), t.key.size(), t.ir.data(), t.ir.size(), (const uint8_t *)t.input.data(),
                 t.witness ? t.input.size() : 0, (const uint8_t *)t.value.data(), t.value.size());
            continue;
        }
        run_kept(t);
    }
    hirsel_llvm_tail_close(tail);
}
