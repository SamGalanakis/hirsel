#include "builder.h"
#include <llvm/ADT/DenseMap.h>
#include <llvm/ADT/StringMap.h>
#include <llvm-c/TargetMachine.h>
#include <llvm/AsmParser/Parser.h>
#include <llvm/BinaryFormat/Dwarf.h>
#include <llvm/IR/Constants.h>
#include <llvm/IR/DebugInfoMetadata.h>
#include <llvm/IR/IRBuilder.h>
#include <llvm/IR/Intrinsics.h>
#include <llvm/IR/Metadata.h>
#include <llvm/IR/Module.h>
#include <llvm/IR/NoFolder.h>
#include <llvm/Support/CBindingWrapping.h>
#include <llvm/Support/MemoryBuffer.h>
#include <llvm/Support/SourceMgr.h>
#include <llvm/Support/raw_ostream.h>
#include <llvm/Transforms/Utils/ModuleUtils.h>
#include <cstdlib>
#include <cstring>
#include <memory>
#include <vector>
using namespace llvm;

extern "C" [[noreturn]] void hirsel_llvm_allocation_failed(const char *);

// Construction mirrors LLParser: a global referenced before its definition is
// an unnamed i8 placeholder that the definition replaces, so the module lists
// globals and functions in definition order exactly as the parsed text does.
struct HirselLlvmBuilder {
    LLVMContext &context;
    std::unique_ptr<Module> module;
    IRBuilder<NoFolder> builder;
    Function *function = nullptr;
    AllocaInst *last_alloca = nullptr;
    StringMap<GlobalVariable *> forward;
    DenseMap<size_t, TrackingMDRef> metadata;
    HirselLlvmBuilder(LLVMContext &c, StringRef name)
        : context(c), module(std::make_unique<Module>(name, c)), builder(c) {}
    ~HirselLlvmBuilder() {
        for (auto &entry : forward) {
            entry.second->replaceAllUsesWith(PoisonValue::get(entry.second->getType()));
            entry.second->eraseFromParent();
        }
    }
};
static void output(HirselBuilderOutput *out, StringRef text, bool failed = false) {
    out->len = text.size(); out->data = static_cast<uint8_t *>(std::malloc(out->len ? out->len : 1));
    if (!out->data) hirsel_llvm_allocation_failed("Builder output allocation failed");
    std::memcpy(out->data, text.data(), out->len); out->failed = failed;
}
static LLVMContextRef context_new(uint8_t anonymous_locals) {
    auto *context = new LLVMContext;
    context->setDiscardValueNames(anonymous_locals != 0);
    return wrap(context);
}
static void context_drop(LLVMContextRef c) { delete unwrap(c); }
static HirselLlvmBuilder *unit_new(LLVMContextRef c, const char *name, size_t len) {
    return new HirselLlvmBuilder(*unwrap(c), StringRef(name, len));
}
static void unit_drop(HirselLlvmBuilder *u) { delete u; }
static LLVMModuleRef unit_take(HirselLlvmBuilder *u, HirselBuilderOutput *out) {
    if (!u->forward.empty()) {
        output(out, ("use of undefined value '@" + u->forward.begin()->first() + "'").str(), true);
        delete u;
        return nullptr;
    }
    auto *module = u->module.release(); delete u; output(out, ""); return wrap(module);
}
static void module_drop(LLVMModuleRef module) { delete unwrap(module); }
// The names the unit uses and has not yet defined or declared, sorted, each
// ended by a NUL: a release partition declares the ones its other partitions
// define (`core::emit_module`).
static void unit_forward(HirselLlvmBuilder *u, HirselBuilderOutput *out) {
    std::vector<StringRef> names;
    for (auto &entry : u->forward) names.push_back(entry.first());
    llvm::sort(names);
    std::string text;
    for (StringRef name : names) { text += name; text += '\0'; }
    output(out, text);
}
static void print(HirselLlvmBuilder *u, HirselBuilderOutput *out) {
    std::string text; raw_string_ostream stream(text); u->module->print(stream, nullptr); output(out, text);
}
static void module_print(LLVMModuleRef module, HirselBuilderOutput *out) {
    std::string text; raw_string_ostream stream(text); unwrap(module)->print(stream, nullptr); output(out, text);
}
static void print_text(const char *data, size_t len, uint8_t anonymous, HirselBuilderOutput *out) {
    LLVMContext context; SMDiagnostic diagnostic;
    auto buffer = MemoryBuffer::getMemBufferCopy(StringRef(data, len), "hirsel-core");
    auto module = parseAssembly(buffer->getMemBufferRef(), diagnostic, context);
    if (!module) { std::string text; raw_string_ostream stream(text); diagnostic.print("hirsel", stream); output(out, text, true); return; }
    module->setModuleIdentifier("hirsel-core");
    if (anonymous) {
        for (Function &function : *module) {
            for (Argument &argument : function.args()) argument.setName("");
            for (BasicBlock &block : function) {
                block.setName("");
                for (Instruction &inst : block) inst.setName("");
            }
        }
    }
    std::string text; raw_string_ostream stream(text); module->print(stream, nullptr); output(out, text);
}
static LLVMTypeRef integer_type(HirselLlvmBuilder *u, unsigned bits) { return wrap(IntegerType::get(u->context, bits)); }
static LLVMTypeRef float_type(HirselLlvmBuilder *u, unsigned bits) { return wrap(bits == 32 ? Type::getFloatTy(u->context) : Type::getDoubleTy(u->context)); }
static LLVMTypeRef pointer_type(HirselLlvmBuilder *u) { return wrap(PointerType::getUnqual(u->context)); }
static LLVMTypeRef void_type(HirselLlvmBuilder *u) { return wrap(Type::getVoidTy(u->context)); }
static LLVMTypeRef metadata_type(HirselLlvmBuilder *u) { return wrap(Type::getMetadataTy(u->context)); }
static LLVMTypeRef array_type(LLVMTypeRef t, uint64_t n) { return wrap(ArrayType::get(unwrap(t), n)); }
static LLVMTypeRef vector_type(LLVMTypeRef element, unsigned count) { return wrap(FixedVectorType::get(unwrap(element), count)); }
static LLVMTypeRef struct_type(HirselLlvmBuilder *u, LLVMTypeRef const *types, size_t n, uint8_t packed) {
    SmallVector<Type *, 8> fields; for (size_t i = 0; i < n; ++i) fields.push_back(unwrap(types[i]));
    return wrap(StructType::get(u->context, fields, packed));
}
static LLVMValueRef integer(LLVMTypeRef type, uint64_t lo, uint64_t hi) {
    unsigned bits = unwrap(type)->getIntegerBitWidth();
    APInt value(128, {lo, hi});
    return wrap(ConstantInt::get(unwrap(type)->getContext(), value.trunc(bits)));
}
static LLVMValueRef poison(LLVMTypeRef type) { return wrap(PoisonValue::get(unwrap(type))); }
static LLVMValueRef zero(LLVMTypeRef type) { return wrap(Constant::getNullValue(unwrap(type))); }
static LLVMValueRef constant_cast(unsigned op, LLVMValueRef v, LLVMTypeRef t) {
    auto *c = cast<Constant>(unwrap(v)); auto *type = unwrap(t);
    switch (op) {
    case 9: return wrap(ConstantExpr::getPtrToInt(c, type));
    case 10: return wrap(ConstantExpr::getIntToPtr(c, type));
    default: return wrap(ConstantExpr::getBitCast(c, type));
    }
}
static LLVMValueRef constant_add(LLVMValueRef a, LLVMValueRef b) { return wrap(ConstantExpr::getAdd(cast<Constant>(unwrap(a)), cast<Constant>(unwrap(b)))); }
static LLVMValueRef constant_sub(LLVMValueRef a, LLVMValueRef b) { return wrap(ConstantExpr::getSub(cast<Constant>(unwrap(a)), cast<Constant>(unwrap(b)))); }
static LLVMValueRef constant_struct(HirselLlvmBuilder *u, LLVMValueRef const *values, size_t n, uint8_t packed) {
    SmallVector<Constant *, 8> fields; for (size_t i = 0; i < n; ++i) fields.push_back(cast<Constant>(unwrap(values[i])));
    return wrap(ConstantStruct::getAnon(u->context, fields, packed));
}
static LLVMValueRef constant_bytes(HirselLlvmBuilder *u, const uint8_t *data, size_t len) {
    return wrap(ConstantDataArray::getString(u->context, StringRef(reinterpret_cast<const char *>(data), len), false));
}
static LLVMValueRef global_ref(HirselLlvmBuilder *u, const char *name, size_t len) {
    StringRef symbol(name, len);
    if (auto *g = u->module->getNamedValue(symbol)) return wrap(g);
    auto &slot = u->forward[symbol];
    if (!slot)
        slot = new GlobalVariable(*u->module, Type::getInt8Ty(u->context), false,
                                  GlobalValue::ExternalWeakLinkage, nullptr, "");
    return wrap(slot);
}
// The last, a release partition's own body or another partition's (`Linkage::Hidden`), is
// external with hidden visibility.
static const GlobalValue::LinkageTypes linkages[] = {GlobalValue::ExternalLinkage, GlobalValue::InternalLinkage, GlobalValue::PrivateLinkage, GlobalValue::WeakODRLinkage, GlobalValue::WeakAnyLinkage, GlobalValue::ExternalLinkage};
static const unsigned HIDDEN = 5;
static void resolve(HirselLlvmBuilder *u, StringRef symbol, GlobalValue *definition) {
    auto it = u->forward.find(symbol);
    if (it == u->forward.end()) return;
    it->second->replaceAllUsesWith(definition);
    it->second->eraseFromParent();
    u->forward.erase(it);
}
static void global(HirselLlvmBuilder *u, const char *name, size_t len, LLVMTypeRef type, LLVMValueRef init,
                   unsigned linkage, uint8_t constant, uint8_t externally_initialized, unsigned alignment, uint8_t absolute,
                   const char *section, size_t section_len) {
    StringRef symbol(name, len);
    auto *g = new GlobalVariable(*u->module, unwrap(type), false, GlobalValue::ExternalLinkage, nullptr, symbol);
    if (init) g->setInitializer(cast<Constant>(unwrap(init)));
    g->setConstant(constant); g->setLinkage(linkages[linkage]); g->setExternallyInitialized(externally_initialized);
    if (linkage == HIDDEN) g->setVisibility(GlobalValue::HiddenVisibility);
    if (alignment) g->setAlignment(Align(alignment));
    if (section_len) g->setSection(StringRef(section, section_len));
    if (absolute) {
        // Site words are below source_positions::SITE_END (2^31), so code
        // materialises them as sign-extended 32-bit immediates.
        auto *i64 = Type::getInt64Ty(u->context);
        g->setMetadata(LLVMContext::MD_absolute_symbol,
                       MDNode::get(u->context, {ConstantAsMetadata::get(ConstantInt::get(i64, 0)),
                                                ConstantAsMetadata::get(ConstantInt::get(i64, uint64_t(1) << 31))}));
    }
    resolve(u, symbol, g);
}
static FunctionType *signature(LLVMTypeRef result, LLVMTypeRef const *types, size_t n) {
    SmallVector<Type *, 8> params; for (size_t i = 0; i < n; ++i) params.push_back(unwrap(types[i]));
    return FunctionType::get(unwrap(result), params, false);
}
static LLVMValueRef function(HirselLlvmBuilder *u, const char *name, size_t len, LLVMTypeRef result,
                             LLVMTypeRef const *types, size_t n, unsigned linkage) {
    StringRef symbol(name, len);
    auto *f = Function::Create(signature(result, types, n), GlobalValue::ExternalLinkage, symbol, *u->module);
    f->setLinkage(linkages[linkage]);
    if (linkage == HIDDEN) f->setVisibility(GlobalValue::HiddenVisibility);
    // The parser assigns the written attributes; intrinsic attributes are
    // reapplied at its end-of-module upgrade (finish_declaration).
    f->setAttributes(AttributeList());
    resolve(u, symbol, f);
    return wrap(f);
}
static void attribute(HirselLlvmBuilder *u, LLVMValueRef value, unsigned index, unsigned kind, LLVMTypeRef type, uint64_t n) {
    Attribute attr;
    switch (kind) {
    case 0: attr = Attribute::get(u->context, Attribute::ZExt); break;
    case 1: attr = Attribute::get(u->context, Attribute::SExt); break;
    case 2: attr = Attribute::get(u->context, Attribute::NoAlias); break;
    case 3: attr = Attribute::get(u->context, Attribute::ReadOnly); break;
    case 4: attr = Attribute::get(u->context, Attribute::StructRet, unwrap(type)); break;
    case 5: attr = Attribute::get(u->context, Attribute::ByVal, unwrap(type)); break;
    case 6: attr = Attribute::get(u->context, Attribute::AlwaysInline); break;
    case 7: attr = Attribute::get(u->context, Attribute::NoInline); break;
    case 8: attr = Attribute::get(u->context, Attribute::Cold); break;
    case 9: attr = Attribute::get(u->context, Attribute::NonNull); break;
    case 10: attr = Attribute::get(u->context, Attribute::NoUnwind); break;
    case 11: attr = Attribute::getWithAlignment(u->context, Align(n)); break;
    case 13: attr = Attribute::getWithDereferenceableBytes(u->context, n); break;
    case 14: attr = Attribute::get(u->context, Attribute::NoFree); break;
    case 15: attr = Attribute::get(u->context, Attribute::NoReturn); break;
    default: attr = Attribute::getWithMemoryEffects(u->context, MemoryEffects::argMemOnly(ModRefInfo::Ref)); break;
    }
    if (auto *f = dyn_cast<Function>(unwrap(value))) f->addAttributeAtIndex(index, attr);
    else cast<CallBase>(unwrap(value))->addAttributeAtIndex(index, attr);
}
static void string_attribute(HirselLlvmBuilder *, LLVMValueRef value, const char *name, size_t len, const char *text, size_t size) {
    auto *target = unwrap(value);
    auto attr = Attribute::get(target->getContext(), StringRef(name, len), StringRef(text, size));
    if (auto *argument = dyn_cast<Argument>(target)) argument->addAttr(attr);
    else if (auto *f = dyn_cast<Function>(target)) f->addFnAttr(attr);
    else cast<CallBase>(target)->addFnAttr(attr);
}
static void finish_declaration(LLVMValueRef value) {
    auto *f = cast<Function>(unwrap(value));
    if (Intrinsic::ID id = f->getIntrinsicID()) {
        SmallVector<Type *> overloads;
        if (Intrinsic::isSignatureValid(id, f->getFunctionType(), overloads))
            f->setAttributes(Intrinsic::getAttributes(f->getContext(), id, f->getFunctionType()));
    }
}
static void subprogram(LLVMValueRef f, LLVMMetadataRef md) { cast<Function>(unwrap(f))->setSubprogram(cast<DISubprogram>(unwrap(md))); }
static void begin_body(HirselLlvmBuilder *u, LLVMValueRef f) {
    u->function = cast<Function>(unwrap(f)); u->last_alloca = nullptr;
    u->builder.SetInsertPoint(BasicBlock::Create(u->context, "entry", u->function));
}
// `@llvm.used` over defined globals, as the parser reads it at the module's end.
static void used(HirselLlvmBuilder *u, LLVMValueRef const *globals, size_t n) {
    SmallVector<GlobalValue *, 8> list;
    for (size_t i = 0; i < n; ++i) list.push_back(cast<GlobalValue>(unwrap(globals[i])));
    appendToUsed(*u->module, list);
}
static void end_body(HirselLlvmBuilder *u) {
    u->builder.ClearInsertionPoint();
    u->function = nullptr;
}
static LLVMValueRef parameter(LLVMValueRef f, unsigned index) { return wrap(cast<Function>(unwrap(f))->getArg(index)); }
static void set_name(LLVMValueRef v, const char *name, size_t len) { unwrap(v)->setName(StringRef(name, len)); }
static LLVMBasicBlockRef block(HirselLlvmBuilder *u, const char *name, size_t len) { return wrap(BasicBlock::Create(u->context, StringRef(name, len))); }
static void begin_block(HirselLlvmBuilder *u, LLVMBasicBlockRef ref) {
    auto *b = unwrap(ref); b->insertInto(u->function); u->builder.SetInsertPoint(b);
}
static uint8_t block_placed(LLVMBasicBlockRef ref) { return unwrap(ref)->getParent() != nullptr; }
static LLVMValueRef placeholder(LLVMTypeRef type) { return wrap(new Argument(unwrap(type))); }
static void replace(LLVMValueRef pending, LLVMValueRef value) {
    auto *argument = cast<Argument>(unwrap(pending)); argument->replaceAllUsesWith(unwrap(value)); delete argument;
}
static LLVMValueRef alloca_value(HirselLlvmBuilder *u, LLVMTypeRef type, unsigned alignment) {
    auto &entry = u->function->getEntryBlock();
    auto *a = new AllocaInst(unwrap(type), 0, nullptr,
                             alignment ? Align(alignment) : u->module->getDataLayout().getPrefTypeAlign(unwrap(type)));
    a->insertInto(&entry, u->last_alloca ? std::next(u->last_alloca->getIterator()) : entry.begin());
    u->last_alloca = a;
    return wrap(a);
}
static LLVMValueRef binary(HirselLlvmBuilder *u, unsigned op, LLVMValueRef left, LLVMValueRef right) {
    const Instruction::BinaryOps ops[] = {Instruction::Add, Instruction::Sub, Instruction::Mul, Instruction::UDiv, Instruction::SDiv, Instruction::URem, Instruction::SRem, Instruction::And, Instruction::Or, Instruction::Xor, Instruction::Shl, Instruction::LShr, Instruction::AShr, Instruction::FAdd, Instruction::FSub, Instruction::FMul, Instruction::FDiv, Instruction::FRem};
    return wrap(u->builder.Insert(BinaryOperator::Create(ops[op], unwrap(left), unwrap(right))));
}
static LLVMValueRef cast_value(HirselLlvmBuilder *u, unsigned op, LLVMValueRef value, LLVMTypeRef type) {
    const Instruction::CastOps ops[] = {Instruction::Trunc, Instruction::ZExt, Instruction::SExt, Instruction::FPToUI, Instruction::FPToSI, Instruction::UIToFP, Instruction::SIToFP, Instruction::FPTrunc, Instruction::FPExt, Instruction::PtrToInt, Instruction::IntToPtr, Instruction::BitCast};
    return wrap(u->builder.Insert(CastInst::Create(ops[op], unwrap(value), unwrap(type))));
}
static LLVMValueRef compare(HirselLlvmBuilder *u, unsigned p, LLVMValueRef a, LLVMValueRef b) {
    auto predicate = CmpInst::Predicate(p);
    if (CmpInst::isFPPredicate(predicate)) return wrap(u->builder.Insert(new FCmpInst(predicate, unwrap(a), unwrap(b))));
    return wrap(u->builder.Insert(new ICmpInst(predicate, unwrap(a), unwrap(b))));
}
static LLVMValueRef select(HirselLlvmBuilder *u, LLVMValueRef c, LLVMValueRef a, LLVMValueRef b) { return wrap(u->builder.Insert(SelectInst::Create(unwrap(c), unwrap(a), unwrap(b)))); }
static LLVMValueRef fneg(HirselLlvmBuilder *u, LLVMValueRef v) { return wrap(u->builder.Insert(UnaryOperator::Create(Instruction::FNeg, unwrap(v)))); }
static Align access_align(HirselLlvmBuilder *u, Type *type, unsigned a) { return a ? Align(a) : u->module->getDataLayout().getABITypeAlign(type); }
static LLVMValueRef load(HirselLlvmBuilder *u, LLVMTypeRef t, LLVMValueRef p, unsigned a, unsigned ordering) {
    auto *load = new LoadInst(unwrap(t), unwrap(p), "", false, access_align(u, unwrap(t), a));
    if (ordering) load->setAtomic(AtomicOrdering(ordering));
    return wrap(u->builder.Insert(load));
}
static LLVMValueRef store(HirselLlvmBuilder *u, LLVMValueRef v, LLVMValueRef p, unsigned a, unsigned ordering) {
    auto *store = new StoreInst(unwrap(v), unwrap(p), false, access_align(u, unwrap(v)->getType(), a));
    if (ordering) store->setAtomic(AtomicOrdering(ordering));
    return wrap(u->builder.Insert(store));
}
static LLVMValueRef gep(HirselLlvmBuilder *u, LLVMTypeRef t, LLVMValueRef p, LLVMValueRef const *indices, size_t n, uint8_t inbounds) {
    SmallVector<Value *, 4> values; for (size_t i = 0; i < n; ++i) values.push_back(unwrap(indices[i]));
    auto *g = GetElementPtrInst::Create(unwrap(t), unwrap(p), values);
    if (inbounds) g->setNoWrapFlags(GEPNoWrapFlags::inBounds());
    return wrap(u->builder.Insert(g));
}
static LLVMValueRef extract(HirselLlvmBuilder *u, LLVMValueRef v, unsigned i) { return wrap(u->builder.Insert(ExtractValueInst::Create(unwrap(v), {i}))); }
static LLVMValueRef insert(HirselLlvmBuilder *u, LLVMValueRef a, LLVMValueRef v, unsigned i) { return wrap(u->builder.Insert(InsertValueInst::Create(unwrap(a), unwrap(v), {i}))); }
static LLVMValueRef extract_element(HirselLlvmBuilder *u, LLVMValueRef vector, unsigned i) {
    return wrap(u->builder.Insert(ExtractElementInst::Create(unwrap(vector), u->builder.getInt32(i))));
}
static LLVMValueRef insert_element(HirselLlvmBuilder *u, LLVMValueRef vector, LLVMValueRef value, unsigned i) {
    return wrap(u->builder.Insert(InsertElementInst::Create(unwrap(vector), unwrap(value), u->builder.getInt32(i))));
}
static LLVMValueRef shuffle(HirselLlvmBuilder *u, LLVMValueRef a, LLVMValueRef b, const int32_t *mask, size_t size) {
    return wrap(u->builder.Insert(new ShuffleVectorInst(unwrap(a), unwrap(b), ArrayRef<int>(mask, size))));
}
static LLVMValueRef phi(HirselLlvmBuilder *u, LLVMTypeRef type, LLVMValueRef const *values, LLVMBasicBlockRef const *blocks, size_t n) {
    auto *p = PHINode::Create(unwrap(type), n); for (size_t i = 0; i < n; ++i) p->addIncoming(unwrap(values[i]), unwrap(blocks[i]));
    return wrap(u->builder.Insert(p));
}
static LLVMValueRef call(HirselLlvmBuilder *u, LLVMTypeRef result, LLVMTypeRef const *types, size_t n,
                         LLVMValueRef callee, LLVMValueRef const *args, size_t argc, uint8_t tail) {
    SmallVector<Value *, 8> values; for (size_t i = 0; i < argc; ++i) values.push_back(unwrap(args[i]));
    auto *c = CallInst::Create(signature(result, types, n), unwrap(callee), values);
    if (tail) c->setTailCallKind(CallInst::TCK_Tail);
    return wrap(u->builder.Insert(c));
}
static LLVMValueRef atomic_rmw(HirselLlvmBuilder *u, unsigned op, LLVMValueRef p, LLVMValueRef v, unsigned ordering) {
    return wrap(u->builder.Insert(new AtomicRMWInst(AtomicRMWInst::BinOp(op), unwrap(p), unwrap(v), Align(8), AtomicOrdering(ordering), SyncScope::System)));
}
static LLVMValueRef fence(HirselLlvmBuilder *u, unsigned ordering) { return wrap(u->builder.Insert(new FenceInst(u->context, AtomicOrdering(ordering)))); }
static LLVMValueRef branch(HirselLlvmBuilder *u, LLVMBasicBlockRef b) { return wrap(u->builder.Insert(BranchInst::Create(unwrap(b)))); }
static LLVMValueRef conditional(HirselLlvmBuilder *u, LLVMValueRef c, LLVMBasicBlockRef a, LLVMBasicBlockRef b) { return wrap(u->builder.Insert(BranchInst::Create(unwrap(a), unwrap(b), unwrap(c)))); }
static LLVMValueRef switch_value(HirselLlvmBuilder *u, LLVMValueRef v, LLVMBasicBlockRef fallback, LLVMValueRef const *values, LLVMBasicBlockRef const *blocks, size_t n) {
    auto *s = SwitchInst::Create(unwrap(v), unwrap(fallback), n);
    for (size_t i = 0; i < n; ++i) s->addCase(cast<ConstantInt>(unwrap(values[i])), unwrap(blocks[i]));
    return wrap(u->builder.Insert(s));
}
static LLVMValueRef return_value(HirselLlvmBuilder *u, LLVMValueRef v) { return wrap(u->builder.Insert(ReturnInst::Create(u->context, v ? unwrap(v) : nullptr))); }
static LLVMValueRef unreachable(HirselLlvmBuilder *u) { return wrap(u->builder.Insert(new UnreachableInst(u->context))); }
static void module_asm(HirselLlvmBuilder *u, const char *text, size_t len) { u->module->appendModuleInlineAsm(StringRef(text, len)); }
static LLVMMetadataRef metadata_ref(HirselLlvmBuilder *u, size_t id) {
    auto &ref = u->metadata[id];
    if (!ref) ref.reset(MDNode::getTemporary(u->context, {}).release());
    return wrap(ref.get());
}
static LLVMMetadataRef metadata_string(HirselLlvmBuilder *u, const char *text, size_t len) { return wrap(MDString::get(u->context, StringRef(text, len))); }
static LLVMMetadataRef metadata_integer(HirselLlvmBuilder *u, unsigned bits, uint64_t value) { return wrap(ConstantAsMetadata::get(ConstantInt::get(IntegerType::get(u->context, bits), value))); }
static LLVMMetadataRef metadata_tuple(HirselLlvmBuilder *u, LLVMMetadataRef const *values, size_t n, uint8_t distinct) {
    SmallVector<Metadata *, 8> fields; for (size_t i = 0; i < n; ++i) fields.push_back(unwrap(values[i]));
    return wrap(distinct ? MDNode::getDistinct(u->context, fields) : MDNode::get(u->context, fields));
}
static void metadata_bind(HirselLlvmBuilder *u, size_t id, LLVMMetadataRef value) {
    auto &ref = u->metadata[id]; auto *node = unwrap(value);
    if (ref && ref.get() != node) { auto *old = cast<MDNode>(ref.get()); old->replaceAllUsesWith(node); MDNode::deleteTemporary(old); }
    ref.reset(node);
}
static void named_metadata(HirselLlvmBuilder *u, const char *name, size_t len, LLVMMetadataRef const *values, size_t n) {
    auto *named = u->module->getOrInsertNamedMetadata(StringRef(name, len));
    for (size_t i = 0; i < n; ++i) named->addOperand(cast<MDNode>(unwrap(values[i])));
}
static void attach(HirselLlvmBuilder *, LLVMValueRef v, unsigned kind, LLVMMetadataRef md) {
    const unsigned kinds[] = {LLVMContext::MD_alias_scope, LLVMContext::MD_noalias, LLVMContext::MD_invariant_load, LLVMContext::MD_prof, LLVMContext::MD_range, LLVMContext::MD_tbaa};
    cast<Instruction>(unwrap(v))->setMetadata(kinds[kind], cast<MDNode>(unwrap(md)));
}
static void location(HirselLlvmBuilder *, LLVMValueRef v, LLVMMetadataRef md) { cast<Instruction>(unwrap(v))->setDebugLoc(DebugLoc(cast<DILocation>(unwrap(md)))); }
static LLVMValueRef metadata_value(HirselLlvmBuilder *u, LLVMMetadataRef md) { return wrap(MetadataAsValue::get(u->context, unwrap(md))); }
// Debug nodes take the parser's field values for the text the Rust producer
// writes: written strings are MDStrings, absent ones are null.
static MDString *const none = nullptr;
static Metadata *const nil = nullptr;
static LLVMMetadataRef di_file(HirselLlvmBuilder *u, const char *name, size_t len) {
    return wrap(DIFile::get(u->context, MDString::get(u->context, StringRef(name, len)), MDString::get(u->context, ""), std::nullopt, none));
}
static LLVMMetadataRef di_compile_unit(HirselLlvmBuilder *u, LLVMMetadataRef file) {
    return wrap(DICompileUnit::getDistinct(
        u->context, DISourceLanguageName(dwarf::DW_LANG_Rust), unwrap(file), MDString::get(u->context, "Hirsel"), false, none, 0,
        none, DICompileUnit::FullDebug, nil, nil, nil, nil, nil, 0, true, false,
        (unsigned)DICompileUnit::DebugNameTableKind::Default, false, none, none));
}
static LLVMMetadataRef di_subroutine_type(HirselLlvmBuilder *u, LLVMMetadataRef types) {
    return wrap(DISubroutineType::get(u->context, DINode::FlagZero, 0, unwrap(types)));
}
static LLVMMetadataRef di_subprogram(HirselLlvmBuilder *u, const char *name, size_t len, const char *linkage, size_t linkage_len,
                                     LLVMMetadataRef file, LLVMMetadataRef type, LLVMMetadataRef unit) {
    return wrap(DISubprogram::getDistinct(
        u->context, unwrap(file), MDString::get(u->context, StringRef(name, len)), MDString::get(u->context, StringRef(linkage, linkage_len)),
        unwrap(file), 1, unwrap(type), 1, nil, 0, 0, DINode::FlagZero, DISubprogram::SPFlagDefinition, unwrap(unit),
        nil, nil, nil, nil, nil, none, false));
}
static LLVMMetadataRef di_location(HirselLlvmBuilder *u, unsigned line, unsigned column, LLVMMetadataRef scope) {
    return wrap(DILocation::get(u->context, line, column, unwrap(scope), nil, false, 0, 0));
}
static LLVMMetadataRef di_lexical_block_file(HirselLlvmBuilder *u, LLVMMetadataRef scope, LLVMMetadataRef file) {
    return wrap(DILexicalBlockFile::get(u->context, unwrap(scope), unwrap(file), 0));
}
extern "C" const HirselLlvmBuilderApi *hirsel_llvm_no_folder_api() {
    static const HirselLlvmBuilderApi api = {
        context_new, context_drop, unit_new, unit_drop, unit_take, module_drop, print, print_text,
        integer_type, float_type, pointer_type, void_type, metadata_type, array_type, vector_type, struct_type,
        integer, poison, zero, constant_cast, constant_add, constant_sub, constant_struct, constant_bytes,
        global_ref, global, function, attribute, string_attribute, finish_declaration, subprogram,
        begin_body, parameter, set_name, block, begin_block, block_placed, placeholder, replace, alloca_value,
        binary, cast_value, compare, select, fneg, load, store, gep, extract, insert, phi, call,
        atomic_rmw, fence, branch, conditional, switch_value, return_value, unreachable,
        module_asm, metadata_ref, metadata_string, metadata_integer, metadata_tuple, metadata_bind, named_metadata,
        attach, location, metadata_value, di_file, di_compile_unit, di_subroutine_type, di_subprogram, di_location,
        di_lexical_block_file, shuffle, extract_element, insert_element, unit_forward, end_body, used, module_print};
    return &api;
}
