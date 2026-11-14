#include "builder.h"
#include <llvm/ADT/DenseMap.h>
#include <llvm/ADT/SmallString.h>
#include <llvm/AsmParser/Parser.h>
#include <llvm/IR/Constants.h>
#include <llvm/IR/DebugInfoMetadata.h>
#include <llvm/IR/Metadata.h>
#include <llvm/IR/IRBuilder.h>
#include <llvm/IR/Module.h>
#include <llvm/IR/NoFolder.h>
#include <llvm/Support/CBindingWrapping.h>
#include <llvm/Support/MemoryBuffer.h>
#include <llvm/Support/SourceMgr.h>
#include <llvm/Support/raw_ostream.h>
#include <cstdlib>
#include <cstring>
#include <memory>
using namespace llvm;

struct HirselLlvmBuilder {
    LLVMContext &context;
    std::unique_ptr<Module> module;
    IRBuilder<NoFolder> builder;
    Function *function = nullptr;
    AllocaInst *last_alloca = nullptr;
    DenseMap<std::pair<unsigned, size_t>, Value *> locals;
    DenseMap<size_t, TrackingMDRef> metadata;
    HirselLlvmBuilder(LLVMContext &c, StringRef name)
        : context(c), module(std::make_unique<Module>(name, c)), builder(c) {}
};
static StringRef local_name(HirselLocalName name, SmallVectorImpl<char> &buffer) {
    const char *prefixes[] = {"t", "v", "a", "s", "p", "context", "dest", "caller_site", "result", "sret", "payload", "input", "worklist"};
    raw_svector_ostream stream(buffer); stream << prefixes[name.kind];
    if (name.kind < 5) stream << name.index;
    return StringRef(buffer.data(), buffer.size());
}
static void name_value(LLVMValueRef value, HirselLocalName name) {
    SmallString<32> text; unwrap(value)->setName(local_name(name, text));
}
static void output(HirselBuilderOutput *out, StringRef text, bool failed = false) {
    out->len = text.size(); out->data = static_cast<uint8_t *>(std::malloc(out->len ? out->len : 1));
    if (!out->data) std::abort();
    std::memcpy(out->data, text.data(), out->len); out->failed = failed;
}
static LLVMContextRef context_new() { return wrap(new LLVMContext); }
static void context_drop(LLVMContextRef c) { delete unwrap(c); }
static HirselLlvmBuilder *unit_new(LLVMContextRef c, const char *name, size_t len) {
    return new HirselLlvmBuilder(*unwrap(c), StringRef(name, len));
}
static void unit_drop(HirselLlvmBuilder *u) { delete u; }
static LLVMModuleRef unit_take(HirselLlvmBuilder *u) {
    auto *module = u->module.release(); delete u; return wrap(module);
}
static void print(HirselLlvmBuilder *u, HirselBuilderOutput *out) {
    std::string text; raw_string_ostream stream(text); u->module->print(stream, nullptr); output(out, text);
}
static void print_text(const char *data, size_t len, HirselBuilderOutput *out) {
    LLVMContext context; SMDiagnostic diagnostic;
    auto buffer = MemoryBuffer::getMemBufferCopy(StringRef(data, len), "hirsel-core");
    auto module = parseAssembly(buffer->getMemBufferRef(), diagnostic, context);
    if (!module) { std::string text; raw_string_ostream stream(text); diagnostic.print("hirsel", stream); output(out, text, true); return; }
    module->setModuleIdentifier("hirsel-core");
    std::string text; raw_string_ostream stream(text); module->print(stream, nullptr); output(out, text);
}
static LLVMTypeRef integer_type(HirselLlvmBuilder *u, unsigned bits) { return wrap(IntegerType::get(u->context, bits)); }
static LLVMTypeRef float_type(HirselLlvmBuilder *u, unsigned bits) { return wrap(bits == 32 ? Type::getFloatTy(u->context) : Type::getDoubleTy(u->context)); }
static LLVMTypeRef pointer_type(HirselLlvmBuilder *u) { return wrap(PointerType::getUnqual(u->context)); }
static LLVMTypeRef void_type(HirselLlvmBuilder *u) { return wrap(Type::getVoidTy(u->context)); }
static LLVMTypeRef array_type(LLVMTypeRef t, uint64_t n) { return wrap(ArrayType::get(unwrap(t), n)); }
static LLVMTypeRef struct_type(HirselLlvmBuilder *u, LLVMTypeRef const *types, size_t n, uint8_t packed) {
    SmallVector<Type *, 8> fields; for (size_t i=0;i<n;++i) fields.push_back(unwrap(types[i]));
    return wrap(StructType::get(u->context, fields, packed));
}
static LLVMValueRef integer(LLVMTypeRef type, uint64_t lo, uint64_t hi) {
    uint64_t words[] = {lo, hi}; return wrap(ConstantInt::get(unwrap(type)->getContext(), APInt(unwrap(type)->getIntegerBitWidth(), words)));
}
static LLVMValueRef poison(LLVMTypeRef type) { return wrap(PoisonValue::get(unwrap(type))); }
static LLVMValueRef zero(LLVMTypeRef type) { return wrap(Constant::getNullValue(unwrap(type))); }
static FunctionType *signature(LLVMTypeRef result, LLVMTypeRef const *types, size_t n) {
    SmallVector<Type *, 8> params; for(size_t i=0;i<n;++i) params.push_back(unwrap(types[i]));
    return FunctionType::get(unwrap(result), params, false);
}
static LLVMValueRef function(HirselLlvmBuilder *u, const char *name, size_t len, LLVMTypeRef result, LLVMTypeRef const *types, size_t n) {
    StringRef symbol(name,len); if (auto *f=u->module->getFunction(symbol)) return wrap(f);
    return wrap(Function::Create(signature(result,types,n), GlobalValue::ExternalLinkage, symbol, *u->module));
}
static void define(HirselLlvmBuilder *u, LLVMValueRef f, unsigned linkage) {
    u->function=cast<Function>(unwrap(f)); u->function->setLinkage(linkage ? GlobalValue::InternalLinkage : GlobalValue::ExternalLinkage);
    u->last_alloca=nullptr; u->locals.clear();
}
static LLVMValueRef parameter(LLVMValueRef f, unsigned index, HirselLocalName name) {
    LLVMValueRef value=wrap(cast<Function>(unwrap(f))->getArg(index)); name_value(value,name); return value;
}
static LLVMBasicBlockRef block(HirselLlvmBuilder *u, const char *name, size_t len) { return wrap(BasicBlock::Create(u->context,StringRef(name,len))); }
static void begin_block(HirselLlvmBuilder *u, LLVMBasicBlockRef ref) {
    auto *b=unwrap(ref); if (!b->getParent()) b->insertInto(u->function); u->builder.SetInsertPoint(b);
}
static LLVMValueRef local(HirselLlvmBuilder *u, LLVMTypeRef type, HirselLocalName name) {
    auto key=std::make_pair(unsigned(name.kind),name.index); auto it=u->locals.find(key);
    if(it!=u->locals.end()) return wrap(it->second);
    auto *value=new Argument(unwrap(type)); u->locals[key]=value; return wrap(value);
}
static void bind(HirselLlvmBuilder *u, HirselLocalName name, LLVMValueRef ref) {
    auto key=std::make_pair(unsigned(name.kind),name.index); auto it=u->locals.find(key); auto *value=unwrap(ref);
    if(it!=u->locals.end() && it->second!=value) { auto *pending=cast<Argument>(it->second); pending->replaceAllUsesWith(value); delete pending; }
    u->locals[key]=value; name_value(ref,name);
}
static LLVMValueRef alloca_value(HirselLlvmBuilder *u, LLVMTypeRef type, unsigned alignment, HirselLocalName name) {
    auto &entry=u->function->getEntryBlock(); auto point=u->builder.saveIP(); auto location=u->builder.getCurrentDebugLocation();
    u->builder.SetInsertPoint(&entry, u->last_alloca ? std::next(u->last_alloca->getIterator()) : entry.begin());
    u->builder.SetCurrentDebugLocation(DebugLoc()); auto *a=u->builder.CreateAlloca(unwrap(type)); if(alignment) a->setAlignment(Align(alignment));
    u->last_alloca=a; u->builder.restoreIP(point); u->builder.SetCurrentDebugLocation(location); auto value=wrap(a); bind(u,name,value); return value;
}
static LLVMValueRef binary(HirselLlvmBuilder *u, unsigned op, LLVMValueRef left, LLVMValueRef right) {
    const Instruction::BinaryOps ops[]={Instruction::Add,Instruction::Sub,Instruction::Mul,Instruction::UDiv,Instruction::SDiv,Instruction::URem,Instruction::SRem,Instruction::And,Instruction::Or,Instruction::Xor,Instruction::Shl,Instruction::LShr,Instruction::AShr,Instruction::FAdd,Instruction::FSub,Instruction::FMul,Instruction::FDiv,Instruction::FRem};
    return wrap(u->builder.CreateBinOp(ops[op],unwrap(left),unwrap(right)));
}
static LLVMValueRef cast_value(HirselLlvmBuilder *u, unsigned op, LLVMValueRef value, LLVMTypeRef type) {
    const Instruction::CastOps ops[]={Instruction::Trunc,Instruction::ZExt,Instruction::SExt,Instruction::FPToUI,Instruction::FPToSI,Instruction::UIToFP,Instruction::SIToFP,Instruction::FPTrunc,Instruction::FPExt,Instruction::PtrToInt,Instruction::IntToPtr,Instruction::BitCast};
    return wrap(u->builder.CreateCast(ops[op],unwrap(value),unwrap(type)));
}
static LLVMValueRef compare(HirselLlvmBuilder *u, unsigned p, LLVMValueRef a, LLVMValueRef b) { return wrap(u->builder.CreateCmp(CmpInst::Predicate(p),unwrap(a),unwrap(b))); }
static LLVMValueRef select(HirselLlvmBuilder *u, LLVMValueRef c, LLVMValueRef a, LLVMValueRef b) { return wrap(u->builder.CreateSelect(unwrap(c),unwrap(a),unwrap(b))); }
static LLVMValueRef load(HirselLlvmBuilder *u, LLVMTypeRef t, LLVMValueRef p, unsigned a) { return wrap(u->builder.CreateAlignedLoad(unwrap(t),unwrap(p),a ? MaybeAlign(a) : MaybeAlign())); }
static LLVMValueRef store(HirselLlvmBuilder *u, LLVMValueRef v, LLVMValueRef p, unsigned a) { return wrap(u->builder.CreateAlignedStore(unwrap(v),unwrap(p),a ? MaybeAlign(a) : MaybeAlign())); }
static LLVMValueRef gep(HirselLlvmBuilder *u, LLVMTypeRef t, LLVMValueRef p, LLVMValueRef const *indices, size_t n) {
    SmallVector<Value *,4> values; for(size_t i=0;i<n;++i) values.push_back(unwrap(indices[i])); return wrap(u->builder.CreateGEP(unwrap(t),unwrap(p),values));
}
static LLVMValueRef extract(HirselLlvmBuilder *u, LLVMValueRef v, unsigned i) { return wrap(u->builder.CreateExtractValue(unwrap(v),i)); }
static LLVMValueRef insert(HirselLlvmBuilder *u, LLVMValueRef a, LLVMValueRef v, unsigned i) { return wrap(u->builder.CreateInsertValue(unwrap(a),unwrap(v),i)); }
static LLVMValueRef phi(HirselLlvmBuilder *u, LLVMTypeRef type, LLVMValueRef const *values, LLVMBasicBlockRef const *blocks, size_t n) {
    auto *p=u->builder.CreatePHI(unwrap(type),n); for(size_t i=0;i<n;++i) p->addIncoming(unwrap(values[i]),unwrap(blocks[i])); return wrap(p);
}
static LLVMValueRef call(HirselLlvmBuilder *u, LLVMTypeRef result, LLVMTypeRef const *types, size_t n, LLVMValueRef callee, LLVMValueRef const *args, size_t argc) {
    SmallVector<Value *,8> values; for(size_t i=0;i<argc;++i) values.push_back(unwrap(args[i])); return wrap(u->builder.CreateCall(signature(result,types,n),unwrap(callee),values));
}
static void branch(HirselLlvmBuilder *u, LLVMBasicBlockRef b) { u->builder.CreateBr(unwrap(b)); }
static LLVMValueRef conditional(HirselLlvmBuilder *u, LLVMValueRef c, LLVMBasicBlockRef a, LLVMBasicBlockRef b) { return wrap(u->builder.CreateCondBr(unwrap(c),unwrap(a),unwrap(b))); }
static void return_value(HirselLlvmBuilder *u, LLVMValueRef v) { if(v) u->builder.CreateRet(unwrap(v)); else u->builder.CreateRetVoid(); }
static void unreachable(HirselLlvmBuilder *u) { u->builder.CreateUnreachable(); }

static LLVMTypeRef vector_type(LLVMTypeRef element, unsigned count) { return wrap(FixedVectorType::get(unwrap(element),count)); }
static LLVMValueRef constant_cast(unsigned op, LLVMValueRef v, LLVMTypeRef t) {
    auto *c=cast<Constant>(unwrap(v)); auto *type=unwrap(t);
    switch(op) {
    case 9: return wrap(ConstantExpr::getPtrToInt(c,type));
    case 10: return wrap(ConstantExpr::getIntToPtr(c,type));
    default: return wrap(ConstantExpr::getBitCast(c,type));
    }
}
static LLVMValueRef constant_add(LLVMValueRef a, LLVMValueRef b) { return wrap(ConstantExpr::getAdd(cast<Constant>(unwrap(a)),cast<Constant>(unwrap(b)))); }
static LLVMValueRef global(HirselLlvmBuilder *u, const char *name, size_t len, LLVMTypeRef type) {
    StringRef symbol(name,len); if(auto *g=u->module->getNamedValue(symbol)) return wrap(g);
    return wrap(new GlobalVariable(*u->module,unwrap(type),false,GlobalValue::ExternalLinkage,nullptr,symbol));
}
static LLVMValueRef constant_struct(HirselLlvmBuilder *u, LLVMValueRef const *values, size_t n, uint8_t packed) {
    SmallVector<Constant *,8> fields; for(size_t i=0;i<n;++i) fields.push_back(cast<Constant>(unwrap(values[i])));
    return wrap(ConstantStruct::getAnon(u->context,fields,packed));
}
static LLVMValueRef constant_array(LLVMTypeRef element, LLVMValueRef const *values, size_t n) {
    SmallVector<Constant *,16> fields; for(size_t i=0;i<n;++i) fields.push_back(cast<Constant>(unwrap(values[i])));
    return wrap(ConstantArray::get(ArrayType::get(unwrap(element),n),fields));
}
static void initialize_global(LLVMValueRef v, LLVMValueRef init, unsigned linkage, uint8_t constant, uint8_t externally_initialized, unsigned alignment) {
    auto *g=cast<GlobalVariable>(unwrap(v));
    if(init) g->setInitializer(cast<Constant>(unwrap(init)));
    const GlobalValue::LinkageTypes links[]={GlobalValue::ExternalLinkage,GlobalValue::InternalLinkage,GlobalValue::PrivateLinkage};
    g->setLinkage(links[linkage]); g->setConstant(constant); g->setExternallyInitialized(externally_initialized);
    if(alignment) g->setAlignment(Align(alignment));
}
static void module_asm(HirselLlvmBuilder *u, const char *text, size_t len) { u->module->appendModuleInlineAsm(StringRef(text,len)); }
static void attribute(HirselLlvmBuilder *u, LLVMValueRef value, unsigned index, unsigned kind, LLVMTypeRef type) {
    const Attribute::AttrKind kinds[]={Attribute::ZExt,Attribute::SExt,Attribute::NoAlias,Attribute::ReadOnly,Attribute::StructRet,Attribute::ByVal,Attribute::AlwaysInline,Attribute::NoInline,Attribute::Cold};
    auto attr=type ? Attribute::get(u->context,kinds[kind],unwrap(type)) : Attribute::get(u->context,kinds[kind]);
    if(auto *f=dyn_cast<Function>(unwrap(value))) f->addAttributeAtIndex(index,attr);
    else cast<CallBase>(unwrap(value))->addAttributeAtIndex(index,attr);
}
static void string_attribute(HirselLlvmBuilder *u, LLVMValueRef value, const char *name, size_t len, const char *text, size_t size) {
    cast<Function>(unwrap(value))->addFnAttr(StringRef(name,len),StringRef(text,size));
}
static void tail(LLVMValueRef v) { cast<CallInst>(unwrap(v))->setTailCallKind(CallInst::TCK_Tail); }
static LLVMValueRef fneg(HirselLlvmBuilder *u, LLVMValueRef v) { return wrap(u->builder.CreateFNeg(unwrap(v))); }
static LLVMValueRef atomic_load(HirselLlvmBuilder *u, LLVMTypeRef t, LLVMValueRef p, unsigned a, unsigned ordering) {
    auto *load=u->builder.CreateAlignedLoad(unwrap(t),unwrap(p),Align(a)); load->setAtomic(AtomicOrdering(ordering)); return wrap(load);
}
static void atomic_store(HirselLlvmBuilder *u, LLVMValueRef v, LLVMValueRef p, unsigned a, unsigned ordering) {
    auto *store=u->builder.CreateAlignedStore(unwrap(v),unwrap(p),Align(a)); store->setAtomic(AtomicOrdering(ordering));
}
static LLVMValueRef atomic_rmw(HirselLlvmBuilder *u, unsigned op, LLVMValueRef p, LLVMValueRef v, unsigned ordering) {
    return wrap(u->builder.CreateAtomicRMW(AtomicRMWInst::BinOp(op),unwrap(p),unwrap(v),MaybeAlign(),AtomicOrdering(ordering)));
}
static LLVMValueRef compare_exchange(HirselLlvmBuilder *u, LLVMValueRef p, LLVMValueRef expected, LLVMValueRef desired, unsigned success, unsigned failure) {
    return wrap(u->builder.CreateAtomicCmpXchg(unwrap(p),unwrap(expected),unwrap(desired),MaybeAlign(),AtomicOrdering(success),AtomicOrdering(failure)));
}
static void fence(HirselLlvmBuilder *u, unsigned ordering) { u->builder.CreateFence(AtomicOrdering(ordering)); }
static void switch_value(HirselLlvmBuilder *u, LLVMValueRef v, LLVMBasicBlockRef fallback, LLVMValueRef const *values, LLVMBasicBlockRef const *blocks, size_t n) {
    auto *s=u->builder.CreateSwitch(unwrap(v),unwrap(fallback),n);
    for(size_t i=0;i<n;++i) s->addCase(cast<ConstantInt>(unwrap(values[i])),unwrap(blocks[i]));
}
static LLVMMetadataRef metadata_ref(HirselLlvmBuilder *u, size_t id) {
    auto &ref=u->metadata[id];
    if(!ref) ref.reset(MDNode::getTemporary(u->context,{}).release());
    return wrap(ref.get());
}
static LLVMMetadataRef metadata_string(HirselLlvmBuilder *u, const char *text, size_t len) { return wrap(MDString::get(u->context,StringRef(text,len))); }
static LLVMMetadataRef metadata_integer(HirselLlvmBuilder *u, unsigned bits, uint64_t value) { return wrap(ConstantAsMetadata::get(ConstantInt::get(IntegerType::get(u->context,bits),value))); }
static LLVMMetadataRef metadata_tuple(HirselLlvmBuilder *u, LLVMMetadataRef const *values, size_t n, uint8_t distinct) {
    SmallVector<Metadata *,8> fields; for(size_t i=0;i<n;++i) fields.push_back(unwrap(values[i]));
    return wrap(distinct ? MDNode::getDistinct(u->context,fields) : MDNode::get(u->context,fields));
}
static void metadata_bind(HirselLlvmBuilder *u, size_t id, LLVMMetadataRef value) {
    auto &ref=u->metadata[id]; auto *node=unwrap(value);
    if(ref && ref.get()!=node) { auto *old=cast<MDNode>(ref.get()); old->replaceAllUsesWith(node); MDNode::deleteTemporary(old); }
    ref.reset(node);
}
static void named_metadata(HirselLlvmBuilder *u, const char *name, size_t len, LLVMMetadataRef const *values, size_t n) {
    auto *named=u->module->getOrInsertNamedMetadata(StringRef(name,len));
    for(size_t i=0;i<n;++i) named->addOperand(cast<MDNode>(unwrap(values[i])));
}
static void attach(HirselLlvmBuilder *u, LLVMValueRef v, const char *name, size_t len, LLVMMetadataRef md) {
    unsigned kind=u->context.getMDKindID(StringRef(name,len));
    if(auto *i=dyn_cast<Instruction>(unwrap(v))) i->setMetadata(kind,cast<MDNode>(unwrap(md)));
    else cast<GlobalObject>(unwrap(v))->setMetadata(kind,cast<MDNode>(unwrap(md)));
}
static void location(HirselLlvmBuilder *u, LLVMMetadataRef md) { u->builder.SetCurrentDebugLocation(md ? DebugLoc(cast<DILocation>(unwrap(md))) : DebugLoc()); }
static LLVMValueRef metadata_value(HirselLlvmBuilder *u, LLVMMetadataRef md) { return wrap(MetadataAsValue::get(u->context,unwrap(md))); }
extern "C" const HirselLlvmBuilderApi *hirsel_llvm_no_folder_api() {
    static const HirselLlvmBuilderApi api={context_new,context_drop,unit_new,unit_drop,unit_take,print,print_text,integer_type,float_type,pointer_type,void_type,array_type,struct_type,integer,poison,zero,function,define,parameter,block,begin_block,name_value,local,bind,alloca_value,binary,cast_value,compare,select,load,store,gep,extract,insert,phi,call,branch,conditional,return_value,unreachable,vector_type,constant_cast,constant_add,global,constant_struct,constant_array,initialize_global,module_asm,attribute,string_attribute,tail,fneg,atomic_load,atomic_store,atomic_rmw,compare_exchange,fence,switch_value,metadata_ref,metadata_string,metadata_integer,metadata_tuple,metadata_bind,named_metadata,attach,location,metadata_value};
    return &api;
}
