// The key of a module: a BLAKE3 hash of every fact its bitcode records, read
// from the module in memory, so a key costs no serialization.
//
// The record code below is ported from LLVM 23.1.5
// llvm/lib/Bitcode/Writer/BitcodeWriter.cpp (Apache-2.0 WITH LLVM-exception):
// each record holds the fields the writer's record holds, in its order. What
// changes is how values are named. The writer numbers every type, value,
// metadata node and attribute set up front and writes tables; this walk names
// each on first reference and writes its definition there, framed as a block
// with its number, so the record stream still determines the module. Opcode
// and orderings keep the writer's encodings; enum fields that the writer
// renumbers (linkage, visibility, attribute kinds) are their libLLVM values,
// which the key's libLLVM identity fixes. Abbreviations, offsets and indexes
// only lay the bits out and are dropped. Each value's use-list order among the
// users the module holds is recorded in full, which is what the writer's
// predicted shuffles encode. The context's tables the writer copies (metadata
// kinds, sync scopes, operand bundle tags) are recorded as it records them.
#include "key.h"

#include <map>

#include <llvm/ADT/DenseMap.h>
#include <llvm/ADT/SmallPtrSet.h>
#include <llvm/ADT/SetVector.h>
#include <llvm/BinaryFormat/Dwarf.h>
#include <llvm/Bitcode/BitcodeCommon.h>
#include <llvm/Bitcode/LLVMBitCodes.h>
#include <llvm/Bitstream/BitCodes.h>
#include <llvm/IR/Constants.h>
#include <llvm/IR/DebugInfoMetadata.h>
#include <llvm/IR/DebugProgramInstruction.h>
#include <llvm/IR/InlineAsm.h>
#include <llvm/IR/Instructions.h>
#include <llvm/IR/Metadata.h>
#include <llvm/IR/Module.h>
#include <llvm/IR/Operator.h>
#include <llvm/IR/ValueSymbolTable.h>
#include <llvm/Support/BLAKE3.h>
#include <llvm/Support/ErrorHandling.h>

using namespace llvm;

namespace hirsel_key {
namespace {

// The writer's abbreviation numbers, kept so the ported records read as the
// writer's; the sink ignores them.
enum {
  CONSTANTS_SETTYPE_ABBREV = bitc::FIRST_APPLICATION_ABBREV,
  CONSTANTS_INTEGER_ABBREV,
  CONSTANTS_BYTE_ABBREV,
  CONSTANTS_CE_CAST_Abbrev,
  CONSTANTS_NULL_Abbrev,
};
enum {
  FUNCTION_INST_LOAD_ABBREV = bitc::FIRST_APPLICATION_ABBREV,
  FUNCTION_INST_STORE_ABBREV,
  FUNCTION_INST_UNOP_ABBREV,
  FUNCTION_INST_UNOP_FLAGS_ABBREV,
  FUNCTION_INST_BINOP_ABBREV,
  FUNCTION_INST_BINOP_FLAGS_ABBREV,
  FUNCTION_INST_CAST_ABBREV,
  FUNCTION_INST_CAST_FLAGS_ABBREV,
  FUNCTION_INST_RET_VOID_ABBREV,
  FUNCTION_INST_RET_VAL_ABBREV,
  FUNCTION_INST_BR_UNCOND_ABBREV,
  FUNCTION_INST_BR_COND_ABBREV,
  FUNCTION_INST_UNREACHABLE_ABBREV,
  FUNCTION_INST_GEP_ABBREV,
  FUNCTION_INST_CMP_ABBREV,
  FUNCTION_INST_CMP_FLAGS_ABBREV,
  FUNCTION_DEBUG_RECORD_VALUE_ABBREV,
  FUNCTION_DEBUG_LOC_ABBREV,
};

// What a definition block defines. Block ids above every bitcode block id.
enum Defines : unsigned {
  DefineType = 64,
  DefineValue,
  DefineMetadata,
  DefineAttributeGroup,
  DefineAttributeList,
  DefineComdat,
  DefineName,
  DefineUses,
};

// The record stream, hashed as it is written: each record is its code, its
// length and its values, each block its id and its end, all LEB128, so the
// bytes hashed determine the records.
class Sink {
  BLAKE3 hasher;
  uint8_t buffer[1 << 16];
  size_t used = 0;

  void flush() {
    hasher.update(ArrayRef<uint8_t>(buffer, used));
    used = 0;
  }

public:
  void u(uint64_t v) {
    if (used + 10 > sizeof buffer) flush();
    do {
      uint8_t byte = v & 0x7f;
      v >>= 7;
      buffer[used++] = byte | (v ? 0x80 : 0);
    } while (v);
  }
  template <typename Values>
  void EmitRecord(unsigned Code, const Values &Vals, unsigned = 0) {
    u(0);
    u(Code);
    u(std::size(Vals));
    for (auto v : Vals) u(uint64_t(v));
  }
  void EnterSubblock(unsigned Block, unsigned = 0) {
    u(1);
    u(Block);
  }
  void ExitBlock() { u(2); }
  Digest digest() {
    flush();
    return hasher.final();
  }
};

static void writeStringRecord(Sink &Stream, unsigned Code, StringRef Str,
                              unsigned = 0) {
  SmallVector<unsigned, 64> Vals(Str.bytes_begin(), Str.bytes_end());
  Stream.EmitRecord(Code, Vals);
}

static unsigned serializeSanitizerMetadata(
    const GlobalValue::SanitizerMetadata &Meta) {
  return Meta.NoAddress | (Meta.NoHWAddress << 1) | (Meta.Memtag << 2) |
         (Meta.IsDynInit << 3);
}

static unsigned getEncodedSyncScopeID(SyncScope::ID SSID) {
  return unsigned(SSID);
}
static unsigned getEncodedAlign(MaybeAlign Alignment) {
  return encode(Alignment);
}

static unsigned getEncodedCastOpcode(unsigned Opcode) {
  switch (Opcode) {
  default: llvm_unreachable("Unknown cast instruction!");
  case Instruction::Trunc   : return bitc::CAST_TRUNC;
  case Instruction::ZExt    : return bitc::CAST_ZEXT;
  case Instruction::SExt    : return bitc::CAST_SEXT;
  case Instruction::FPToUI  : return bitc::CAST_FPTOUI;
  case Instruction::FPToSI  : return bitc::CAST_FPTOSI;
  case Instruction::UIToFP  : return bitc::CAST_UITOFP;
  case Instruction::SIToFP  : return bitc::CAST_SITOFP;
  case Instruction::FPTrunc : return bitc::CAST_FPTRUNC;
  case Instruction::FPExt   : return bitc::CAST_FPEXT;
  case Instruction::PtrToAddr: return bitc::CAST_PTRTOADDR;
  case Instruction::PtrToInt: return bitc::CAST_PTRTOINT;
  case Instruction::IntToPtr: return bitc::CAST_INTTOPTR;
  case Instruction::BitCast : return bitc::CAST_BITCAST;
  case Instruction::AddrSpaceCast: return bitc::CAST_ADDRSPACECAST;
  }
}

static unsigned getEncodedUnaryOpcode(unsigned Opcode) {
  switch (Opcode) {
  default: llvm_unreachable("Unknown binary instruction!");
  case Instruction::FNeg: return bitc::UNOP_FNEG;
  }
}

static unsigned getEncodedBinaryOpcode(unsigned Opcode) {
  switch (Opcode) {
  default: llvm_unreachable("Unknown binary instruction!");
  case Instruction::Add:
  case Instruction::FAdd: return bitc::BINOP_ADD;
  case Instruction::Sub:
  case Instruction::FSub: return bitc::BINOP_SUB;
  case Instruction::Mul:
  case Instruction::FMul: return bitc::BINOP_MUL;
  case Instruction::UDiv: return bitc::BINOP_UDIV;
  case Instruction::FDiv:
  case Instruction::SDiv: return bitc::BINOP_SDIV;
  case Instruction::URem: return bitc::BINOP_UREM;
  case Instruction::FRem:
  case Instruction::SRem: return bitc::BINOP_SREM;
  case Instruction::Shl:  return bitc::BINOP_SHL;
  case Instruction::LShr: return bitc::BINOP_LSHR;
  case Instruction::AShr: return bitc::BINOP_ASHR;
  case Instruction::And:  return bitc::BINOP_AND;
  case Instruction::Or:   return bitc::BINOP_OR;
  case Instruction::Xor:  return bitc::BINOP_XOR;
  }
}

static unsigned getEncodedRMWOperation(const AtomicRMWInst &I) {
  unsigned Encoding = 0;
  switch (I.getOperation()) {
  default: llvm_unreachable("Unknown RMW operation!");
  case AtomicRMWInst::Xchg:
    Encoding = bitc::RMW_XCHG;
    break;
  case AtomicRMWInst::Add:
    Encoding = bitc::RMW_ADD;
    break;
  case AtomicRMWInst::Sub:
    Encoding = bitc::RMW_SUB;
    break;
  case AtomicRMWInst::And:
    Encoding = bitc::RMW_AND;
    break;
  case AtomicRMWInst::Nand:
    Encoding = bitc::RMW_NAND;
    break;
  case AtomicRMWInst::Or:
    Encoding = bitc::RMW_OR;
    break;
  case AtomicRMWInst::Xor:
    Encoding = bitc::RMW_XOR;
    break;
  case AtomicRMWInst::Max:
    Encoding = bitc::RMW_MAX;
    break;
  case AtomicRMWInst::Min:
    Encoding = bitc::RMW_MIN;
    break;
  case AtomicRMWInst::UMax:
    Encoding = bitc::RMW_UMAX;
    break;
  case AtomicRMWInst::UMin:
    Encoding = bitc::RMW_UMIN;
    break;
  case AtomicRMWInst::FAdd:
    Encoding = bitc::RMW_FADD;
    break;
  case AtomicRMWInst::FSub:
    Encoding = bitc::RMW_FSUB;
    break;
  case AtomicRMWInst::FMax:
    Encoding = bitc::RMW_FMAX;
    break;
  case AtomicRMWInst::FMin:
    Encoding = bitc::RMW_FMIN;
    break;
  case AtomicRMWInst::FMaximum:
    Encoding = bitc::RMW_FMAXIMUM;
    break;
  case AtomicRMWInst::FMinimum:
    Encoding = bitc::RMW_FMINIMUM;
    break;
  case AtomicRMWInst::FMaximumNum:
    Encoding = bitc::RMW_FMAXIMUMNUM;
    break;
  case AtomicRMWInst::FMinimumNum:
    Encoding = bitc::RMW_FMINIMUMNUM;
    break;
  case AtomicRMWInst::UIncWrap:
    Encoding = bitc::RMW_UINC_WRAP;
    break;
  case AtomicRMWInst::UDecWrap:
    Encoding = bitc::RMW_UDEC_WRAP;
    break;
  case AtomicRMWInst::USubCond:
    Encoding = bitc::RMW_USUB_COND;
    break;
  case AtomicRMWInst::USubSat:
    Encoding = bitc::RMW_USUB_SAT;
    break;
  }

  if (I.isElementwise())
    Encoding |= bitc::RMW_ELEMENTWISE_FLAG;
  return Encoding;
}

static unsigned getEncodedOrdering(AtomicOrdering Ordering) {
  switch (Ordering) {
  case AtomicOrdering::NotAtomic: return bitc::ORDERING_NOTATOMIC;
  case AtomicOrdering::Unordered: return bitc::ORDERING_UNORDERED;
  case AtomicOrdering::Monotonic: return bitc::ORDERING_MONOTONIC;
  case AtomicOrdering::Acquire: return bitc::ORDERING_ACQUIRE;
  case AtomicOrdering::Release: return bitc::ORDERING_RELEASE;
  case AtomicOrdering::AcquireRelease: return bitc::ORDERING_ACQREL;
  case AtomicOrdering::SequentiallyConsistent: return bitc::ORDERING_SEQCST;
  }
  llvm_unreachable("Invalid ordering");
}

static void emitSignedInt64(SmallVectorImpl<uint64_t> &Vals, uint64_t V) {
  if ((int64_t)V >= 0)
    Vals.push_back(V << 1);
  else
    Vals.push_back((-V << 1) | 1);
}

static void emitWideAPInt(SmallVectorImpl<uint64_t> &Vals, const APInt &A) {
  // We have an arbitrary precision integer value to write whose
  // bit width is > 64. However, in canonical unsigned integer
  // format it is likely that the high bits are going to be zero.
  // So, we only write the number of active words.
  unsigned NumWords = A.getActiveWords();
  const uint64_t *RawData = A.getRawData();
  for (unsigned i = 0; i < NumWords; i++)
    emitSignedInt64(Vals, RawData[i]);
}

static void emitConstantRange(SmallVectorImpl<uint64_t> &Record,
                              const ConstantRange &CR, bool EmitBitWidth) {
  unsigned BitWidth = CR.getBitWidth();
  if (EmitBitWidth)
    Record.push_back(BitWidth);
  if (BitWidth > 64) {
    Record.push_back(CR.getLower().getActiveWords() |
                     (uint64_t(CR.getUpper().getActiveWords()) << 32));
    emitWideAPInt(Record, CR.getLower());
    emitWideAPInt(Record, CR.getUpper());
  } else {
    emitSignedInt64(Record, CR.getLower().getSExtValue());
    emitSignedInt64(Record, CR.getUpper().getSExtValue());
  }
}

static uint64_t getOptimizationFlags(const Value *V) {
  uint64_t Flags = 0;

  if (const auto *OBO = dyn_cast<OverflowingBinaryOperator>(V)) {
    if (OBO->hasNoSignedWrap())
      Flags |= 1 << bitc::OBO_NO_SIGNED_WRAP;
    if (OBO->hasNoUnsignedWrap())
      Flags |= 1 << bitc::OBO_NO_UNSIGNED_WRAP;
  } else if (const auto *PEO = dyn_cast<PossiblyExactOperator>(V)) {
    if (PEO->isExact())
      Flags |= 1 << bitc::PEO_EXACT;
  } else if (const auto *PDI = dyn_cast<PossiblyDisjointInst>(V)) {
    if (PDI->isDisjoint())
      Flags |= 1 << bitc::PDI_DISJOINT;
  } else if (const auto *FPMO = dyn_cast<FPMathOperator>(V)) {
    if (FPMO->hasAllowReassoc())
      Flags |= bitc::AllowReassoc;
    if (FPMO->hasNoNaNs())
      Flags |= bitc::NoNaNs;
    if (FPMO->hasNoInfs())
      Flags |= bitc::NoInfs;
    if (FPMO->hasNoSignedZeros())
      Flags |= bitc::NoSignedZeros;
    if (FPMO->hasAllowReciprocal())
      Flags |= bitc::AllowReciprocal;
    if (FPMO->hasAllowContract())
      Flags |= bitc::AllowContract;
    if (FPMO->hasApproxFunc())
      Flags |= bitc::ApproxFunc;

    // Handle uitofp.
    if (const auto *NNI = dyn_cast<PossiblyNonNegInst>(V)) {
      Flags <<= 1;
      if (NNI->hasNonNeg())
        Flags |= 1 << bitc::PNNI_NON_NEG;
    }
  } else if (const auto *NNI = dyn_cast<PossiblyNonNegInst>(V)) {
    if (NNI->hasNonNeg())
      Flags |= 1 << bitc::PNNI_NON_NEG;
  } else if (const auto *TI = dyn_cast<TruncInst>(V)) {
    if (TI->hasNoSignedWrap())
      Flags |= 1 << bitc::TIO_NO_SIGNED_WRAP;
    if (TI->hasNoUnsignedWrap())
      Flags |= 1 << bitc::TIO_NO_UNSIGNED_WRAP;
  } else if (const auto *GEP = dyn_cast<GEPOperator>(V)) {
    if (GEP->isInBounds())
      Flags |= 1 << bitc::GEP_INBOUNDS;
    if (GEP->hasNoUnsignedSignedWrap())
      Flags |= 1 << bitc::GEP_NUSW;
    if (GEP->hasNoUnsignedWrap())
      Flags |= 1 << bitc::GEP_NUW;
  } else if (const auto *ICmp = dyn_cast<ICmpInst>(V)) {
    if (ICmp->hasSameSign())
      Flags |= 1 << bitc::ICMP_SAME_SIGN;
  }

  return Flags;
}


class KeyWriter {
  const Module &M;
  Sink Stream;
  // The ported records ask the writer's enumerator for numbers; here the
  // walk is its own enumerator.
  KeyWriter &VE = *this;

  DenseMap<Type *, unsigned> Types;
  DenseMap<const Value *, unsigned> Values;
  // Every numbered value, in number order, for the use lists.
  std::vector<const Value *> Numbered;
  DenseMap<const BasicBlock *, unsigned> Blocks;
  std::vector<const BasicBlock *> NumberedBlocks;
  DenseMap<const Metadata *, unsigned> MDs;
  DenseMap<AttributeList, unsigned> AttributeLists;
  DenseMap<std::pair<unsigned, AttributeSet>, unsigned> AttributeGroups;
  DenseMap<const Comdat *, unsigned> Comdats;
  unsigned NextType = 0, NextValue = 0, NextMD = 1, NextAttributeList = 1,
           NextAttributeGroup = 1, NextComdat = 1, NextName = 0;

  // A definition: a block holding its number and its records.
  struct Define {
    Sink &S;
    Define(Sink &S, unsigned Kind, unsigned ID) : S(S) {
      S.EnterSubblock(Kind);
      S.u(ID);
    }
    ~Define() { S.ExitBlock(); }
  };

public:
  explicit KeyWriter(const Module &M) : M(M) {}
  Digest write();

private:
  // The enumerator.
  unsigned getTypeID(Type *T);
  unsigned getValueID(const Value *V);
  unsigned getMetadataID(const Metadata *MD);
  unsigned getMetadataOrNullID(const Metadata *MD) {
    return MD ? getMetadataID(MD) : 0;
  }
  unsigned getAttributeListID(AttributeList AL);
  unsigned getAttributeGroupID(std::pair<unsigned, AttributeSet> Group);
  unsigned getComdatID(const Comdat *C);
  unsigned getInstructionID(const Instruction *I) { return Values.lookup(I); }
  void setInstructionID(const Instruction *) {}
  unsigned getGlobalBasicBlockID(const BasicBlock *BB);
  unsigned addToStrtab(StringRef Str);
  void number(const Value *V);

  void writeType(Type *T, unsigned ID);
  void writeConstant(const Value *V, unsigned ID);
  void writeMetadata(const Metadata *MD, unsigned ID);
  void writeContextTables();
  void writeModuleInfo();
  void writeModuleMetadata();
  void pushGlobalMetadataAttachment(SmallVectorImpl<uint64_t> &Record,
                                    const GlobalObject &GO);
  void writeFunctionMetadataAttachment(const Function &F);
  void writeFunctionNames(const Function &F);
  void writeFunction(const Function &F);
  void writeUseLists();

  bool pushValueAndType(const Value *V, unsigned InstID,
                        SmallVectorImpl<unsigned> &Vals);
  bool pushValueOrMetadata(const Value *V, unsigned InstID,
                           SmallVectorImpl<unsigned> &Vals);
  void writeOperandBundles(const CallBase &CB, unsigned InstID);
  void pushValue(const Value *V, unsigned InstID,
                 SmallVectorImpl<unsigned> &Vals);
  void pushValueSigned(const Value *V, unsigned InstID,
                       SmallVectorImpl<uint64_t> &Vals);
  void writeInstruction(const Instruction &I, unsigned InstID,
                        SmallVectorImpl<unsigned> &Vals);

  // The writer's metadata records.
  void writeValueAsMetadata(const ValueAsMetadata *MD,
                            SmallVectorImpl<uint64_t> &Record);
  void writeMDTuple(const MDTuple *N, SmallVectorImpl<uint64_t> &Record,
                    unsigned Abbrev);
  void writeDILocation(const DILocation *N, SmallVectorImpl<uint64_t> &Record,
                       unsigned &Abbrev);
  void writeGenericDINode(const GenericDINode *N,
                          SmallVectorImpl<uint64_t> &Record, unsigned &Abbrev);
  void writeDISubrange(const DISubrange *N, SmallVectorImpl<uint64_t> &Record,
                       unsigned Abbrev);
  void writeDIGenericSubrange(const DIGenericSubrange *N,
                              SmallVectorImpl<uint64_t> &Record,
                              unsigned Abbrev);
  void writeDIEnumerator(const DIEnumerator *N,
                         SmallVectorImpl<uint64_t> &Record, unsigned Abbrev);
  void writeDIBasicType(const DIBasicType *N, SmallVectorImpl<uint64_t> &Record,
                        unsigned Abbrev);
  void writeDIFixedPointType(const DIFixedPointType *N,
                             SmallVectorImpl<uint64_t> &Record,
                             unsigned Abbrev);
  void writeDIStringType(const DIStringType *N,
                         SmallVectorImpl<uint64_t> &Record, unsigned Abbrev);
  void writeDIDerivedType(const DIDerivedType *N,
                          SmallVectorImpl<uint64_t> &Record, unsigned Abbrev);
  void writeDISubrangeType(const DISubrangeType *N,
                           SmallVectorImpl<uint64_t> &Record, unsigned Abbrev);
  void writeDICompositeType(const DICompositeType *N,
                            SmallVectorImpl<uint64_t> &Record, unsigned Abbrev);
  void writeDISubroutineType(const DISubroutineType *N,
                             SmallVectorImpl<uint64_t> &Record,
                             unsigned Abbrev);
  void writeDIFile(const DIFile *N, SmallVectorImpl<uint64_t> &Record,
                   unsigned Abbrev);
  void writeDICompileUnit(const DICompileUnit *N,
                          SmallVectorImpl<uint64_t> &Record, unsigned Abbrev);
  void writeDISubprogram(const DISubprogram *N,
                         SmallVectorImpl<uint64_t> &Record, unsigned Abbrev);
  void writeDILexicalBlock(const DILexicalBlock *N,
                           SmallVectorImpl<uint64_t> &Record, unsigned Abbrev);
  void writeDILexicalBlockFile(const DILexicalBlockFile *N,
                               SmallVectorImpl<uint64_t> &Record,
                               unsigned Abbrev);
  void writeDICommonBlock(const DICommonBlock *N,
                          SmallVectorImpl<uint64_t> &Record, unsigned Abbrev);
  void writeDINamespace(const DINamespace *N, SmallVectorImpl<uint64_t> &Record,
                        unsigned Abbrev);
  void writeDIMacro(const DIMacro *N, SmallVectorImpl<uint64_t> &Record,
                    unsigned Abbrev);
  void writeDIMacroFile(const DIMacroFile *N, SmallVectorImpl<uint64_t> &Record,
                        unsigned Abbrev);
  void writeDIArgList(const DIArgList *N, SmallVectorImpl<uint64_t> &Record);
  void writeDIModule(const DIModule *N, SmallVectorImpl<uint64_t> &Record,
                     unsigned Abbrev);
  void writeDIAssignID(const DIAssignID *N, SmallVectorImpl<uint64_t> &Record,
                       unsigned Abbrev);
  void writeDITemplateTypeParameter(const DITemplateTypeParameter *N,
                                    SmallVectorImpl<uint64_t> &Record,
                                    unsigned Abbrev);
  void writeDITemplateValueParameter(const DITemplateValueParameter *N,
                                     SmallVectorImpl<uint64_t> &Record,
                                     unsigned Abbrev);
  void writeDIGlobalVariable(const DIGlobalVariable *N,
                             SmallVectorImpl<uint64_t> &Record,
                             unsigned Abbrev);
  void writeDILocalVariable(const DILocalVariable *N,
                            SmallVectorImpl<uint64_t> &Record, unsigned Abbrev);
  void writeDILabel(const DILabel *N,
                    SmallVectorImpl<uint64_t> &Record, unsigned Abbrev);
  void writeDIExpression(const DIExpression *N,
                         SmallVectorImpl<uint64_t> &Record, unsigned Abbrev);
  void writeDIGlobalVariableExpression(const DIGlobalVariableExpression *N,
                                       SmallVectorImpl<uint64_t> &Record,
                                       unsigned Abbrev);
  void writeDIObjCProperty(const DIObjCProperty *N,
                           SmallVectorImpl<uint64_t> &Record, unsigned Abbrev);
  void writeDIImportedEntity(const DIImportedEntity *N,
                             SmallVectorImpl<uint64_t> &Record,
                             unsigned Abbrev);
};

void KeyWriter::writeValueAsMetadata(
    const ValueAsMetadata *MD, SmallVectorImpl<uint64_t> &Record) {
  // Mimic an MDNode with a value as one operand.
  Value *V = MD->getValue();
  Record.push_back(VE.getTypeID(V->getType()));
  Record.push_back(VE.getValueID(V));
  Stream.EmitRecord(bitc::METADATA_VALUE, Record, 0);
  Record.clear();
}

void KeyWriter::writeMDTuple(const MDTuple *N,
                                       SmallVectorImpl<uint64_t> &Record,
                                       unsigned Abbrev) {
  for (const MDOperand &MDO : N->operands()) {
    Metadata *MD = MDO;
    assert(!(MD && isa<LocalAsMetadata>(MD)) &&
           "Unexpected function-local metadata");
    Record.push_back(VE.getMetadataOrNullID(MD));
  }
  Stream.EmitRecord(N->isDistinct() ? bitc::METADATA_DISTINCT_NODE
                                    : bitc::METADATA_NODE,
                    Record, Abbrev);
  Record.clear();
}

void KeyWriter::writeDILocation(const DILocation *N,
                                          SmallVectorImpl<uint64_t> &Record,
                                          unsigned &Abbrev) {
  Record.push_back(N->isDistinct());
  Record.push_back(N->getLine());
  Record.push_back(N->getColumn());
  Record.push_back(VE.getMetadataID(N->getScope()));
  Record.push_back(VE.getMetadataOrNullID(N->getInlinedAt()));
  Record.push_back(N->isImplicitCode());
  Record.push_back(N->getAtomGroup());
  Record.push_back(N->getAtomRank());
  Stream.EmitRecord(bitc::METADATA_LOCATION, Record, Abbrev);
  Record.clear();
}

void KeyWriter::writeGenericDINode(const GenericDINode *N,
                                             SmallVectorImpl<uint64_t> &Record,
                                             unsigned &Abbrev) {
  Record.push_back(N->isDistinct());
  Record.push_back(N->getTag());
  Record.push_back(0); // Per-tag version field; unused for now.

  for (auto &I : N->operands())
    Record.push_back(VE.getMetadataOrNullID(I));

  Stream.EmitRecord(bitc::METADATA_GENERIC_DEBUG, Record, Abbrev);
  Record.clear();
}

void KeyWriter::writeDISubrange(const DISubrange *N,
                                          SmallVectorImpl<uint64_t> &Record,
                                          unsigned Abbrev) {
  const uint64_t Version = 2 << 1;
  Record.push_back((uint64_t)N->isDistinct() | Version);
  Record.push_back(VE.getMetadataOrNullID(N->getRawCountNode()));
  Record.push_back(VE.getMetadataOrNullID(N->getRawLowerBound()));
  Record.push_back(VE.getMetadataOrNullID(N->getRawUpperBound()));
  Record.push_back(VE.getMetadataOrNullID(N->getRawStride()));

  Stream.EmitRecord(bitc::METADATA_SUBRANGE, Record, Abbrev);
  Record.clear();
}

void KeyWriter::writeDIGenericSubrange(
    const DIGenericSubrange *N, SmallVectorImpl<uint64_t> &Record,
    unsigned Abbrev) {
  Record.push_back((uint64_t)N->isDistinct());
  Record.push_back(VE.getMetadataOrNullID(N->getRawCountNode()));
  Record.push_back(VE.getMetadataOrNullID(N->getRawLowerBound()));
  Record.push_back(VE.getMetadataOrNullID(N->getRawUpperBound()));
  Record.push_back(VE.getMetadataOrNullID(N->getRawStride()));

  Stream.EmitRecord(bitc::METADATA_GENERIC_SUBRANGE, Record, Abbrev);
  Record.clear();
}

void KeyWriter::writeDIEnumerator(const DIEnumerator *N,
                                            SmallVectorImpl<uint64_t> &Record,
                                            unsigned Abbrev) {
  const uint64_t IsBigInt = 1 << 2;
  Record.push_back(IsBigInt | (N->isUnsigned() << 1) | N->isDistinct());
  Record.push_back(N->getValue().getBitWidth());
  Record.push_back(VE.getMetadataOrNullID(N->getRawName()));
  emitWideAPInt(Record, N->getValue());

  Stream.EmitRecord(bitc::METADATA_ENUMERATOR, Record, Abbrev);
  Record.clear();
}

void KeyWriter::writeDIBasicType(const DIBasicType *N,
                                           SmallVectorImpl<uint64_t> &Record,
                                           unsigned Abbrev) {
  const unsigned SizeIsMetadata = 0x2;
  Record.push_back(SizeIsMetadata | (unsigned)N->isDistinct());
  Record.push_back(N->getTag());
  Record.push_back(VE.getMetadataOrNullID(N->getRawName()));
  Record.push_back(VE.getMetadataOrNullID(N->getRawSizeInBits()));
  Record.push_back(N->getAlignInBits());
  Record.push_back(N->getEncoding());
  Record.push_back(N->getFlags());
  Record.push_back(N->getNumExtraInhabitants());
  Record.push_back(N->getDataSizeInBits());
  Record.push_back(VE.getMetadataOrNullID(N->getFile()));
  Record.push_back(N->getLine());
  Record.push_back(VE.getMetadataOrNullID(N->getScope()));

  Stream.EmitRecord(bitc::METADATA_BASIC_TYPE, Record, Abbrev);
  Record.clear();
}

void KeyWriter::writeDIFixedPointType(
    const DIFixedPointType *N, SmallVectorImpl<uint64_t> &Record,
    unsigned Abbrev) {
  const unsigned SizeIsMetadata = 0x2;
  Record.push_back(SizeIsMetadata | (unsigned)N->isDistinct());
  Record.push_back(N->getTag());
  Record.push_back(VE.getMetadataOrNullID(N->getRawName()));
  Record.push_back(VE.getMetadataOrNullID(N->getRawSizeInBits()));
  Record.push_back(N->getAlignInBits());
  Record.push_back(N->getEncoding());
  Record.push_back(N->getFlags());
  Record.push_back(N->getKind());
  Record.push_back(N->getFactorRaw());

  auto WriteWideInt = [&](const APInt &Value) {
    // Write an encoded word that holds the number of active words and
    // the number of bits.
    uint64_t NumWords = Value.getActiveWords();
    uint64_t Encoded = (NumWords << 32) | Value.getBitWidth();
    Record.push_back(Encoded);
    emitWideAPInt(Record, Value);
  };

  WriteWideInt(N->getNumeratorRaw());
  WriteWideInt(N->getDenominatorRaw());

  Record.push_back(VE.getMetadataOrNullID(N->getFile()));
  Record.push_back(N->getLine());
  Record.push_back(VE.getMetadataOrNullID(N->getScope()));

  Stream.EmitRecord(bitc::METADATA_FIXED_POINT_TYPE, Record, Abbrev);
  Record.clear();
}

void KeyWriter::writeDIStringType(const DIStringType *N,
                                            SmallVectorImpl<uint64_t> &Record,
                                            unsigned Abbrev) {
  const unsigned SizeIsMetadata = 0x2;
  Record.push_back(SizeIsMetadata | (unsigned)N->isDistinct());
  Record.push_back(N->getTag());
  Record.push_back(VE.getMetadataOrNullID(N->getRawName()));
  Record.push_back(VE.getMetadataOrNullID(N->getStringLength()));
  Record.push_back(VE.getMetadataOrNullID(N->getStringLengthExp()));
  Record.push_back(VE.getMetadataOrNullID(N->getStringLocationExp()));
  Record.push_back(VE.getMetadataOrNullID(N->getRawSizeInBits()));
  Record.push_back(N->getAlignInBits());
  Record.push_back(N->getEncoding());

  Stream.EmitRecord(bitc::METADATA_STRING_TYPE, Record, Abbrev);
  Record.clear();
}

void KeyWriter::writeDIDerivedType(const DIDerivedType *N,
                                             SmallVectorImpl<uint64_t> &Record,
                                             unsigned Abbrev) {
  const unsigned SizeIsMetadata = 0x2;
  Record.push_back(SizeIsMetadata | (unsigned)N->isDistinct());
  Record.push_back(N->getTag());
  Record.push_back(VE.getMetadataOrNullID(N->getRawName()));
  Record.push_back(VE.getMetadataOrNullID(N->getFile()));
  Record.push_back(N->getLine());
  Record.push_back(VE.getMetadataOrNullID(N->getScope()));
  Record.push_back(VE.getMetadataOrNullID(N->getBaseType()));
  Record.push_back(VE.getMetadataOrNullID(N->getRawSizeInBits()));
  Record.push_back(N->getAlignInBits());
  Record.push_back(VE.getMetadataOrNullID(N->getRawOffsetInBits()));
  Record.push_back(N->getFlags());
  Record.push_back(VE.getMetadataOrNullID(N->getExtraData()));

  // DWARF address space is encoded as N->getDWARFAddressSpace() + 1. 0 means
  // that there is no DWARF address space associated with DIDerivedType.
  if (const auto &DWARFAddressSpace = N->getDWARFAddressSpace())
    Record.push_back(*DWARFAddressSpace + 1);
  else
    Record.push_back(0);

  Record.push_back(VE.getMetadataOrNullID(N->getAnnotations().get()));

  if (auto PtrAuthData = N->getPtrAuthData())
    Record.push_back(PtrAuthData->RawData);
  else
    Record.push_back(0);

  Stream.EmitRecord(bitc::METADATA_DERIVED_TYPE, Record, Abbrev);
  Record.clear();
}

void KeyWriter::writeDISubrangeType(const DISubrangeType *N,
                                              SmallVectorImpl<uint64_t> &Record,
                                              unsigned Abbrev) {
  const unsigned SizeIsMetadata = 0x2;
  Record.push_back(SizeIsMetadata | (unsigned)N->isDistinct());
  Record.push_back(VE.getMetadataOrNullID(N->getRawName()));
  Record.push_back(VE.getMetadataOrNullID(N->getFile()));
  Record.push_back(N->getLine());
  Record.push_back(VE.getMetadataOrNullID(N->getScope()));
  Record.push_back(VE.getMetadataOrNullID(N->getRawSizeInBits()));
  Record.push_back(N->getAlignInBits());
  Record.push_back(N->getFlags());
  Record.push_back(VE.getMetadataOrNullID(N->getBaseType()));
  Record.push_back(VE.getMetadataOrNullID(N->getRawLowerBound()));
  Record.push_back(VE.getMetadataOrNullID(N->getRawUpperBound()));
  Record.push_back(VE.getMetadataOrNullID(N->getRawStride()));
  Record.push_back(VE.getMetadataOrNullID(N->getRawBias()));

  Stream.EmitRecord(bitc::METADATA_SUBRANGE_TYPE, Record, Abbrev);
  Record.clear();
}

void KeyWriter::writeDICompositeType(
    const DICompositeType *N, SmallVectorImpl<uint64_t> &Record,
    unsigned Abbrev) {
  const unsigned IsNotUsedInOldTypeRef = 0x2;
  const unsigned SizeIsMetadata = 0x4;
  Record.push_back(SizeIsMetadata | IsNotUsedInOldTypeRef |
                   (unsigned)N->isDistinct());
  Record.push_back(N->getTag());
  Record.push_back(VE.getMetadataOrNullID(N->getRawName()));
  Record.push_back(VE.getMetadataOrNullID(N->getFile()));
  Record.push_back(N->getLine());
  Record.push_back(VE.getMetadataOrNullID(N->getScope()));
  Record.push_back(VE.getMetadataOrNullID(N->getBaseType()));
  Record.push_back(VE.getMetadataOrNullID(N->getRawSizeInBits()));
  Record.push_back(N->getAlignInBits());
  Record.push_back(VE.getMetadataOrNullID(N->getRawOffsetInBits()));
  Record.push_back(N->getFlags());
  Record.push_back(VE.getMetadataOrNullID(N->getElements().get()));
  Record.push_back(N->getRuntimeLang());
  Record.push_back(VE.getMetadataOrNullID(N->getVTableHolder()));
  Record.push_back(VE.getMetadataOrNullID(N->getTemplateParams().get()));
  Record.push_back(VE.getMetadataOrNullID(N->getRawIdentifier()));
  Record.push_back(VE.getMetadataOrNullID(N->getDiscriminator()));
  Record.push_back(VE.getMetadataOrNullID(N->getRawDataLocation()));
  Record.push_back(VE.getMetadataOrNullID(N->getRawAssociated()));
  Record.push_back(VE.getMetadataOrNullID(N->getRawAllocated()));
  Record.push_back(VE.getMetadataOrNullID(N->getRawRank()));
  Record.push_back(VE.getMetadataOrNullID(N->getAnnotations().get()));
  Record.push_back(N->getNumExtraInhabitants());
  Record.push_back(VE.getMetadataOrNullID(N->getRawSpecification()));
  Record.push_back(
      N->getEnumKind().value_or(dwarf::DW_APPLE_ENUM_KIND_invalid));
  Record.push_back(VE.getMetadataOrNullID(N->getRawBitStride()));

  Stream.EmitRecord(bitc::METADATA_COMPOSITE_TYPE, Record, Abbrev);
  Record.clear();
}

void KeyWriter::writeDISubroutineType(
    const DISubroutineType *N, SmallVectorImpl<uint64_t> &Record,
    unsigned Abbrev) {
  const unsigned HasNoOldTypeRefs = 0x2;
  Record.push_back(HasNoOldTypeRefs | (unsigned)N->isDistinct());
  Record.push_back(N->getFlags());
  Record.push_back(VE.getMetadataOrNullID(N->getTypeArray().get()));
  Record.push_back(N->getCC());

  Stream.EmitRecord(bitc::METADATA_SUBROUTINE_TYPE, Record, Abbrev);
  Record.clear();
}

void KeyWriter::writeDIFile(const DIFile *N,
                                      SmallVectorImpl<uint64_t> &Record,
                                      unsigned Abbrev) {
  Record.push_back(N->isDistinct());
  Record.push_back(VE.getMetadataOrNullID(N->getRawFilename()));
  Record.push_back(VE.getMetadataOrNullID(N->getRawDirectory()));
  if (N->getRawChecksum()) {
    Record.push_back(N->getRawChecksum()->Kind);
    Record.push_back(VE.getMetadataOrNullID(N->getRawChecksum()->Value));
  } else {
    // Maintain backwards compatibility with the old internal representation of
    // CSK_None in ChecksumKind by writing nulls here when Checksum is None.
    Record.push_back(0);
    Record.push_back(VE.getMetadataOrNullID(nullptr));
  }
  auto Source = N->getRawSource();
  if (Source)
    Record.push_back(VE.getMetadataOrNullID(Source));

  Stream.EmitRecord(bitc::METADATA_FILE, Record, Abbrev);
  Record.clear();
}

void KeyWriter::writeDICompileUnit(const DICompileUnit *N,
                                             SmallVectorImpl<uint64_t> &Record,
                                             unsigned Abbrev) {
  assert(N->isDistinct() && "Expected distinct compile units");
  Record.push_back(/* IsDistinct */ true);

  auto Lang = N->getSourceLanguage();
  Record.push_back(Lang.getName());
  // Set bit so the MetadataLoader can distniguish between versioned and
  // unversioned names.
  if (Lang.hasVersionedName())
    Record.back() ^= (uint64_t(1) << 63);

  Record.push_back(VE.getMetadataOrNullID(N->getFile()));
  Record.push_back(VE.getMetadataOrNullID(N->getRawProducer()));
  Record.push_back(N->isOptimized());
  Record.push_back(VE.getMetadataOrNullID(N->getRawFlags()));
  Record.push_back(N->getRuntimeVersion());
  Record.push_back(VE.getMetadataOrNullID(N->getRawSplitDebugFilename()));
  Record.push_back(N->getEmissionKind());
  Record.push_back(VE.getMetadataOrNullID(N->getEnumTypes().get()));
  Record.push_back(VE.getMetadataOrNullID(N->getRetainedTypes().get()));
  Record.push_back(/* subprograms */ 0);
  Record.push_back(VE.getMetadataOrNullID(N->getGlobalVariables().get()));
  Record.push_back(VE.getMetadataOrNullID(N->getImportedEntities().get()));
  Record.push_back(N->getDWOId());
  Record.push_back(VE.getMetadataOrNullID(N->getMacros().get()));
  Record.push_back(N->getSplitDebugInlining());
  Record.push_back(N->getDebugInfoForProfiling());
  Record.push_back((unsigned)N->getNameTableKind());
  Record.push_back(N->getRangesBaseAddress());
  Record.push_back(VE.getMetadataOrNullID(N->getRawSysRoot()));
  Record.push_back(VE.getMetadataOrNullID(N->getRawSDK()));
  Record.push_back(Lang.hasVersionedName() ? Lang.getVersion() : 0);
  Record.push_back(Lang.getDialect());

  Stream.EmitRecord(bitc::METADATA_COMPILE_UNIT, Record, Abbrev);
  Record.clear();
}

void KeyWriter::writeDISubprogram(const DISubprogram *N,
                                            SmallVectorImpl<uint64_t> &Record,
                                            unsigned Abbrev) {
  const uint64_t HasUnitFlag = 1 << 1;
  const uint64_t HasSPFlagsFlag = 1 << 2;
  Record.push_back(uint64_t(N->isDistinct()) | HasUnitFlag | HasSPFlagsFlag);
  Record.push_back(VE.getMetadataOrNullID(N->getScope()));
  Record.push_back(VE.getMetadataOrNullID(N->getRawName()));
  Record.push_back(VE.getMetadataOrNullID(N->getRawLinkageName()));
  Record.push_back(VE.getMetadataOrNullID(N->getFile()));
  Record.push_back(N->getLine());
  Record.push_back(VE.getMetadataOrNullID(N->getType()));
  Record.push_back(N->getScopeLine());
  Record.push_back(VE.getMetadataOrNullID(N->getContainingType()));
  Record.push_back(N->getSPFlags());
  Record.push_back(N->getVirtualIndex());
  Record.push_back(N->getFlags());
  Record.push_back(VE.getMetadataOrNullID(N->getRawUnit()));
  Record.push_back(VE.getMetadataOrNullID(N->getTemplateParams().get()));
  Record.push_back(VE.getMetadataOrNullID(N->getDeclaration()));
  Record.push_back(VE.getMetadataOrNullID(N->getRetainedNodes().get()));
  Record.push_back(N->getThisAdjustment());
  Record.push_back(VE.getMetadataOrNullID(N->getThrownTypes().get()));
  Record.push_back(VE.getMetadataOrNullID(N->getAnnotations().get()));
  Record.push_back(VE.getMetadataOrNullID(N->getRawTargetFuncName()));
  Record.push_back(N->getKeyInstructionsEnabled());

  Stream.EmitRecord(bitc::METADATA_SUBPROGRAM, Record, Abbrev);
  Record.clear();
}

void KeyWriter::writeDILexicalBlock(const DILexicalBlock *N,
                                              SmallVectorImpl<uint64_t> &Record,
                                              unsigned Abbrev) {
  Record.push_back(N->isDistinct());
  Record.push_back(VE.getMetadataOrNullID(N->getScope()));
  Record.push_back(VE.getMetadataOrNullID(N->getFile()));
  Record.push_back(N->getLine());
  Record.push_back(N->getColumn());

  Stream.EmitRecord(bitc::METADATA_LEXICAL_BLOCK, Record, Abbrev);
  Record.clear();
}

void KeyWriter::writeDILexicalBlockFile(
    const DILexicalBlockFile *N, SmallVectorImpl<uint64_t> &Record,
    unsigned Abbrev) {
  Record.push_back(N->isDistinct());
  Record.push_back(VE.getMetadataOrNullID(N->getScope()));
  Record.push_back(VE.getMetadataOrNullID(N->getFile()));
  Record.push_back(N->getDiscriminator());

  Stream.EmitRecord(bitc::METADATA_LEXICAL_BLOCK_FILE, Record, Abbrev);
  Record.clear();
}

void KeyWriter::writeDICommonBlock(const DICommonBlock *N,
                                             SmallVectorImpl<uint64_t> &Record,
                                             unsigned Abbrev) {
  Record.push_back(N->isDistinct());
  Record.push_back(VE.getMetadataOrNullID(N->getScope()));
  Record.push_back(VE.getMetadataOrNullID(N->getDecl()));
  Record.push_back(VE.getMetadataOrNullID(N->getRawName()));
  Record.push_back(VE.getMetadataOrNullID(N->getFile()));
  Record.push_back(N->getLineNo());

  Stream.EmitRecord(bitc::METADATA_COMMON_BLOCK, Record, Abbrev);
  Record.clear();
}

void KeyWriter::writeDINamespace(const DINamespace *N,
                                           SmallVectorImpl<uint64_t> &Record,
                                           unsigned Abbrev) {
  Record.push_back(N->isDistinct() | N->getExportSymbols() << 1);
  Record.push_back(VE.getMetadataOrNullID(N->getScope()));
  Record.push_back(VE.getMetadataOrNullID(N->getRawName()));

  Stream.EmitRecord(bitc::METADATA_NAMESPACE, Record, Abbrev);
  Record.clear();
}

void KeyWriter::writeDIMacro(const DIMacro *N,
                                       SmallVectorImpl<uint64_t> &Record,
                                       unsigned Abbrev) {
  Record.push_back(N->isDistinct());
  Record.push_back(N->getMacinfoType());
  Record.push_back(N->getLine());
  Record.push_back(VE.getMetadataOrNullID(N->getRawName()));
  Record.push_back(VE.getMetadataOrNullID(N->getRawValue()));

  Stream.EmitRecord(bitc::METADATA_MACRO, Record, Abbrev);
  Record.clear();
}

void KeyWriter::writeDIMacroFile(const DIMacroFile *N,
                                           SmallVectorImpl<uint64_t> &Record,
                                           unsigned Abbrev) {
  Record.push_back(N->isDistinct());
  Record.push_back(N->getMacinfoType());
  Record.push_back(N->getLine());
  Record.push_back(VE.getMetadataOrNullID(N->getFile()));
  Record.push_back(VE.getMetadataOrNullID(N->getElements().get()));

  Stream.EmitRecord(bitc::METADATA_MACRO_FILE, Record, Abbrev);
  Record.clear();
}

void KeyWriter::writeDIArgList(const DIArgList *N,
                                         SmallVectorImpl<uint64_t> &Record) {
  Record.reserve(N->getArgs().size());
  for (ValueAsMetadata *MD : N->getArgs())
    Record.push_back(VE.getMetadataID(MD));

  Stream.EmitRecord(bitc::METADATA_ARG_LIST, Record);
  Record.clear();
}

void KeyWriter::writeDIModule(const DIModule *N,
                                        SmallVectorImpl<uint64_t> &Record,
                                        unsigned Abbrev) {
  Record.push_back(N->isDistinct());
  for (auto &I : N->operands())
    Record.push_back(VE.getMetadataOrNullID(I));
  Record.push_back(N->getLineNo());
  Record.push_back(N->getIsDecl());

  Stream.EmitRecord(bitc::METADATA_MODULE, Record, Abbrev);
  Record.clear();
}

void KeyWriter::writeDIAssignID(const DIAssignID *N,
                                          SmallVectorImpl<uint64_t> &Record,
                                          unsigned Abbrev) {
  // There are no arguments for this metadata type.
  Record.push_back(N->isDistinct());
  Stream.EmitRecord(bitc::METADATA_ASSIGN_ID, Record, Abbrev);
  Record.clear();
}

void KeyWriter::writeDITemplateTypeParameter(
    const DITemplateTypeParameter *N, SmallVectorImpl<uint64_t> &Record,
    unsigned Abbrev) {
  Record.push_back(N->isDistinct());
  Record.push_back(VE.getMetadataOrNullID(N->getRawName()));
  Record.push_back(VE.getMetadataOrNullID(N->getType()));
  Record.push_back(N->isDefault());

  Stream.EmitRecord(bitc::METADATA_TEMPLATE_TYPE, Record, Abbrev);
  Record.clear();
}

void KeyWriter::writeDITemplateValueParameter(
    const DITemplateValueParameter *N, SmallVectorImpl<uint64_t> &Record,
    unsigned Abbrev) {
  Record.push_back(N->isDistinct());
  Record.push_back(N->getTag());
  Record.push_back(VE.getMetadataOrNullID(N->getRawName()));
  Record.push_back(VE.getMetadataOrNullID(N->getType()));
  Record.push_back(N->isDefault());
  Record.push_back(VE.getMetadataOrNullID(N->getValue()));

  Stream.EmitRecord(bitc::METADATA_TEMPLATE_VALUE, Record, Abbrev);
  Record.clear();
}

void KeyWriter::writeDIGlobalVariable(
    const DIGlobalVariable *N, SmallVectorImpl<uint64_t> &Record,
    unsigned Abbrev) {
  const uint64_t Version = 2 << 1;
  Record.push_back((uint64_t)N->isDistinct() | Version);
  Record.push_back(VE.getMetadataOrNullID(N->getScope()));
  Record.push_back(VE.getMetadataOrNullID(N->getRawName()));
  Record.push_back(VE.getMetadataOrNullID(N->getRawLinkageName()));
  Record.push_back(VE.getMetadataOrNullID(N->getFile()));
  Record.push_back(N->getLine());
  Record.push_back(VE.getMetadataOrNullID(N->getType()));
  Record.push_back(N->isLocalToUnit());
  Record.push_back(N->isDefinition());
  Record.push_back(VE.getMetadataOrNullID(N->getStaticDataMemberDeclaration()));
  Record.push_back(VE.getMetadataOrNullID(N->getTemplateParams()));
  Record.push_back(N->getAlignInBits());
  Record.push_back(VE.getMetadataOrNullID(N->getAnnotations().get()));

  Stream.EmitRecord(bitc::METADATA_GLOBAL_VAR, Record, Abbrev);
  Record.clear();
}

void KeyWriter::writeDILocalVariable(
    const DILocalVariable *N, SmallVectorImpl<uint64_t> &Record,
    unsigned Abbrev) {
  // In order to support all possible bitcode formats in BitcodeReader we need
  // to distinguish the following cases:
  // 1) Record has no artificial tag (Record[1]),
  //   has no obsolete inlinedAt field (Record[9]).
  //   In this case Record size will be 8, HasAlignment flag is false.
  // 2) Record has artificial tag (Record[1]),
  //   has no obsolete inlignedAt field (Record[9]).
  //   In this case Record size will be 9, HasAlignment flag is false.
  // 3) Record has both artificial tag (Record[1]) and
  //   obsolete inlignedAt field (Record[9]).
  //   In this case Record size will be 10, HasAlignment flag is false.
  // 4) Record has neither artificial tag, nor inlignedAt field, but
  //   HasAlignment flag is true and Record[8] contains alignment value.
  const uint64_t HasAlignmentFlag = 1 << 1;
  Record.push_back((uint64_t)N->isDistinct() | HasAlignmentFlag);
  Record.push_back(VE.getMetadataOrNullID(N->getScope()));
  Record.push_back(VE.getMetadataOrNullID(N->getRawName()));
  Record.push_back(VE.getMetadataOrNullID(N->getFile()));
  Record.push_back(N->getLine());
  Record.push_back(VE.getMetadataOrNullID(N->getType()));
  Record.push_back(N->getArg());
  Record.push_back(N->getFlags());
  Record.push_back(N->getAlignInBits());
  Record.push_back(VE.getMetadataOrNullID(N->getAnnotations().get()));

  Stream.EmitRecord(bitc::METADATA_LOCAL_VAR, Record, Abbrev);
  Record.clear();
}

void KeyWriter::writeDILabel(
    const DILabel *N, SmallVectorImpl<uint64_t> &Record,
    unsigned Abbrev) {
  uint64_t IsArtificialFlag = uint64_t(N->isArtificial()) << 1;
  Record.push_back((uint64_t)N->isDistinct() | IsArtificialFlag);
  Record.push_back(VE.getMetadataOrNullID(N->getScope()));
  Record.push_back(VE.getMetadataOrNullID(N->getRawName()));
  Record.push_back(VE.getMetadataOrNullID(N->getFile()));
  Record.push_back(N->getLine());
  Record.push_back(N->getColumn());
  Record.push_back(N->getCoroSuspendIdx().has_value()
                       ? (uint64_t)N->getCoroSuspendIdx().value()
                       : std::numeric_limits<uint64_t>::max());

  Stream.EmitRecord(bitc::METADATA_LABEL, Record, Abbrev);
  Record.clear();
}

void KeyWriter::writeDIExpression(const DIExpression *N,
                                            SmallVectorImpl<uint64_t> &Record,
                                            unsigned Abbrev) {
  Record.reserve(N->getElements().size() + 1);
  const uint64_t Version = 3 << 1;
  Record.push_back((uint64_t)N->isDistinct() | Version);
  Record.append(N->elements_begin(), N->elements_end());

  Stream.EmitRecord(bitc::METADATA_EXPRESSION, Record, Abbrev);
  Record.clear();
}

void KeyWriter::writeDIGlobalVariableExpression(
    const DIGlobalVariableExpression *N, SmallVectorImpl<uint64_t> &Record,
    unsigned Abbrev) {
  Record.push_back(N->isDistinct());
  Record.push_back(VE.getMetadataOrNullID(N->getVariable()));
  Record.push_back(VE.getMetadataOrNullID(N->getExpression()));

  Stream.EmitRecord(bitc::METADATA_GLOBAL_VAR_EXPR, Record, Abbrev);
  Record.clear();
}

void KeyWriter::writeDIObjCProperty(const DIObjCProperty *N,
                                              SmallVectorImpl<uint64_t> &Record,
                                              unsigned Abbrev) {
  Record.push_back(N->isDistinct());
  Record.push_back(VE.getMetadataOrNullID(N->getRawName()));
  Record.push_back(VE.getMetadataOrNullID(N->getFile()));
  Record.push_back(N->getLine());
  Record.push_back(VE.getMetadataOrNullID(N->getRawSetterName()));
  Record.push_back(VE.getMetadataOrNullID(N->getRawGetterName()));
  Record.push_back(N->getAttributes());
  Record.push_back(VE.getMetadataOrNullID(N->getType()));

  Stream.EmitRecord(bitc::METADATA_OBJC_PROPERTY, Record, Abbrev);
  Record.clear();
}

void KeyWriter::writeDIImportedEntity(
    const DIImportedEntity *N, SmallVectorImpl<uint64_t> &Record,
    unsigned Abbrev) {
  Record.push_back(N->isDistinct());
  Record.push_back(N->getTag());
  Record.push_back(VE.getMetadataOrNullID(N->getScope()));
  Record.push_back(VE.getMetadataOrNullID(N->getEntity()));
  Record.push_back(N->getLine());
  Record.push_back(VE.getMetadataOrNullID(N->getRawName()));
  Record.push_back(VE.getMetadataOrNullID(N->getRawFile()));
  Record.push_back(VE.getMetadataOrNullID(N->getElements().get()));

  Stream.EmitRecord(bitc::METADATA_IMPORTED_ENTITY, Record, Abbrev);
  Record.clear();
}


bool KeyWriter::pushValueOrMetadata(const Value *V, unsigned InstID,
                                              SmallVectorImpl<unsigned> &Vals) {
  bool IsMetadata = V->getType()->isMetadataTy();
  if (IsMetadata) {
    Vals.push_back(bitc::OB_METADATA);
    Metadata *MD = cast<MetadataAsValue>(V)->getMetadata();
    unsigned ValID = VE.getMetadataID(MD);
    Vals.push_back(InstID - ValID);
    return false;
  }
  return pushValueAndType(V, InstID, Vals);
}

void KeyWriter::writeOperandBundles(const CallBase &CS,
                                              unsigned InstID) {
  SmallVector<unsigned, 64> Record;
  LLVMContext &C = CS.getContext();

  for (unsigned i = 0, e = CS.getNumOperandBundles(); i != e; ++i) {
    const auto &Bundle = CS.getOperandBundleAt(i);
    Record.push_back(C.getOperandBundleTagID(Bundle.getTagName()));

    for (auto &Input : Bundle.Inputs)
      pushValueOrMetadata(Input, InstID, Record);

    Stream.EmitRecord(bitc::FUNC_CODE_OPERAND_BUNDLE, Record);
    Record.clear();
  }
}

/// pushValue - Like pushValueAndType, but where the type of the value is
/// omitted (perhaps it was already encoded in an earlier operand).
void KeyWriter::pushValue(const Value *V, unsigned InstID,
                                    SmallVectorImpl<unsigned> &Vals) {
  unsigned ValID = VE.getValueID(V);
  Vals.push_back(InstID - ValID);
}

void KeyWriter::pushValueSigned(const Value *V, unsigned InstID,
                                          SmallVectorImpl<uint64_t> &Vals) {
  unsigned ValID = VE.getValueID(V);
  int64_t diff = ((int32_t)InstID - (int32_t)ValID);
  emitSignedInt64(Vals, diff);
}

/// WriteInstruction - Emit an instruction to the specified stream.
void KeyWriter::writeInstruction(const Instruction &I,
                                           unsigned InstID,
                                           SmallVectorImpl<unsigned> &Vals) {
  unsigned Code = 0;
  unsigned AbbrevToUse = 0;
  VE.setInstructionID(&I);
  switch (I.getOpcode()) {
  default:
    if (Instruction::isCast(I.getOpcode())) {
      Code = bitc::FUNC_CODE_INST_CAST;
      if (!pushValueAndType(I.getOperand(0), InstID, Vals))
        AbbrevToUse = FUNCTION_INST_CAST_ABBREV;
      Vals.push_back(VE.getTypeID(I.getType()));
      Vals.push_back(getEncodedCastOpcode(I.getOpcode()));
      uint64_t Flags = getOptimizationFlags(&I);
      if (Flags != 0) {
        if (AbbrevToUse == FUNCTION_INST_CAST_ABBREV)
          AbbrevToUse = FUNCTION_INST_CAST_FLAGS_ABBREV;
        Vals.push_back(Flags);
      }
    } else {
      assert(isa<BinaryOperator>(I) && "Unknown instruction!");
      Code = bitc::FUNC_CODE_INST_BINOP;
      if (!pushValueAndType(I.getOperand(0), InstID, Vals))
        AbbrevToUse = FUNCTION_INST_BINOP_ABBREV;
      pushValue(I.getOperand(1), InstID, Vals);
      Vals.push_back(getEncodedBinaryOpcode(I.getOpcode()));
      uint64_t Flags = getOptimizationFlags(&I);
      if (Flags != 0) {
        if (AbbrevToUse == FUNCTION_INST_BINOP_ABBREV)
          AbbrevToUse = FUNCTION_INST_BINOP_FLAGS_ABBREV;
        Vals.push_back(Flags);
      }
    }
    break;
  case Instruction::FNeg: {
    Code = bitc::FUNC_CODE_INST_UNOP;
    if (!pushValueAndType(I.getOperand(0), InstID, Vals))
      AbbrevToUse = FUNCTION_INST_UNOP_ABBREV;
    Vals.push_back(getEncodedUnaryOpcode(I.getOpcode()));
    uint64_t Flags = getOptimizationFlags(&I);
    if (Flags != 0) {
      if (AbbrevToUse == FUNCTION_INST_UNOP_ABBREV)
        AbbrevToUse = FUNCTION_INST_UNOP_FLAGS_ABBREV;
      Vals.push_back(Flags);
    }
    break;
  }
  case Instruction::GetElementPtr: {
    Code = bitc::FUNC_CODE_INST_GEP;
    AbbrevToUse = FUNCTION_INST_GEP_ABBREV;
    auto &GEPInst = cast<GetElementPtrInst>(I);
    Vals.push_back(getOptimizationFlags(&I));
    Vals.push_back(VE.getTypeID(GEPInst.getSourceElementType()));
    for (const Value *Op : I.operands())
      pushValueAndType(Op, InstID, Vals);
    break;
  }
  case Instruction::ExtractValue: {
    Code = bitc::FUNC_CODE_INST_EXTRACTVAL;
    pushValueAndType(I.getOperand(0), InstID, Vals);
    const ExtractValueInst *EVI = cast<ExtractValueInst>(&I);
    Vals.append(EVI->idx_begin(), EVI->idx_end());
    break;
  }
  case Instruction::InsertValue: {
    Code = bitc::FUNC_CODE_INST_INSERTVAL;
    pushValueAndType(I.getOperand(0), InstID, Vals);
    pushValueAndType(I.getOperand(1), InstID, Vals);
    const InsertValueInst *IVI = cast<InsertValueInst>(&I);
    Vals.append(IVI->idx_begin(), IVI->idx_end());
    break;
  }
  case Instruction::Select: {
    Code = bitc::FUNC_CODE_INST_VSELECT;
    pushValueAndType(I.getOperand(1), InstID, Vals);
    pushValue(I.getOperand(2), InstID, Vals);
    pushValueAndType(I.getOperand(0), InstID, Vals);
    uint64_t Flags = getOptimizationFlags(&I);
    if (Flags != 0)
      Vals.push_back(Flags);
    break;
  }
  case Instruction::ExtractElement:
    Code = bitc::FUNC_CODE_INST_EXTRACTELT;
    pushValueAndType(I.getOperand(0), InstID, Vals);
    pushValueAndType(I.getOperand(1), InstID, Vals);
    break;
  case Instruction::InsertElement:
    Code = bitc::FUNC_CODE_INST_INSERTELT;
    pushValueAndType(I.getOperand(0), InstID, Vals);
    pushValue(I.getOperand(1), InstID, Vals);
    pushValueAndType(I.getOperand(2), InstID, Vals);
    break;
  case Instruction::ShuffleVector:
    Code = bitc::FUNC_CODE_INST_SHUFFLEVEC;
    pushValueAndType(I.getOperand(0), InstID, Vals);
    pushValue(I.getOperand(1), InstID, Vals);
    pushValue(cast<ShuffleVectorInst>(I).getShuffleMaskForBitcode(), InstID,
              Vals);
    break;
  case Instruction::ICmp:
  case Instruction::FCmp: {
    // compare returning Int1Ty or vector of Int1Ty
    Code = bitc::FUNC_CODE_INST_CMP2;
    AbbrevToUse = FUNCTION_INST_CMP_ABBREV;
    if (pushValueAndType(I.getOperand(0), InstID, Vals))
      AbbrevToUse = 0;
    pushValue(I.getOperand(1), InstID, Vals);
    Vals.push_back(cast<CmpInst>(I).getPredicate());
    uint64_t Flags = getOptimizationFlags(&I);
    if (Flags != 0) {
      Vals.push_back(Flags);
      if (AbbrevToUse)
        AbbrevToUse = FUNCTION_INST_CMP_FLAGS_ABBREV;
    }
    break;
  }

  case Instruction::Ret:
    {
      Code = bitc::FUNC_CODE_INST_RET;
      unsigned NumOperands = I.getNumOperands();
      if (NumOperands == 0)
        AbbrevToUse = FUNCTION_INST_RET_VOID_ABBREV;
      else if (NumOperands == 1) {
        if (!pushValueAndType(I.getOperand(0), InstID, Vals))
          AbbrevToUse = FUNCTION_INST_RET_VAL_ABBREV;
      } else {
        for (const Value *Op : I.operands())
          pushValueAndType(Op, InstID, Vals);
      }
    }
    break;
    case Instruction::UncondBr: {
      Code = bitc::FUNC_CODE_INST_BR;
      AbbrevToUse = FUNCTION_INST_BR_UNCOND_ABBREV;
      const UncondBrInst &II = cast<UncondBrInst>(I);
      Vals.push_back(VE.getValueID(II.getSuccessor(0)));
    } break;
    case Instruction::CondBr: {
      Code = bitc::FUNC_CODE_INST_BR;
      AbbrevToUse = FUNCTION_INST_BR_COND_ABBREV;
      const CondBrInst &II = cast<CondBrInst>(I);
      Vals.push_back(VE.getValueID(II.getSuccessor(0)));
      Vals.push_back(VE.getValueID(II.getSuccessor(1)));
      pushValue(II.getCondition(), InstID, Vals);
    } break;
  case Instruction::Switch:
    {
      Code = bitc::FUNC_CODE_INST_SWITCH;
      const SwitchInst &SI = cast<SwitchInst>(I);
      Vals.push_back(VE.getTypeID(SI.getCondition()->getType()));
      pushValue(SI.getCondition(), InstID, Vals);
      Vals.push_back(VE.getValueID(SI.getDefaultDest()));
      for (auto Case : SI.cases()) {
        Vals.push_back(VE.getValueID(Case.getCaseValue()));
        Vals.push_back(VE.getValueID(Case.getCaseSuccessor()));
      }
    }
    break;
  case Instruction::IndirectBr:
    Code = bitc::FUNC_CODE_INST_INDIRECTBR;
    Vals.push_back(VE.getTypeID(I.getOperand(0)->getType()));
    // Encode the address operand as relative, but not the basic blocks.
    pushValue(I.getOperand(0), InstID, Vals);
    for (const Value *Op : drop_begin(I.operands()))
      Vals.push_back(VE.getValueID(Op));
    break;

  case Instruction::Invoke: {
    const InvokeInst *II = cast<InvokeInst>(&I);
    const Value *Callee = II->getCalledOperand();
    FunctionType *FTy = II->getFunctionType();

    if (II->hasOperandBundles())
      writeOperandBundles(*II, InstID);

    Code = bitc::FUNC_CODE_INST_INVOKE;

    Vals.push_back(VE.getAttributeListID(II->getAttributes()));
    Vals.push_back(II->getCallingConv() | 1 << 13);
    Vals.push_back(VE.getValueID(II->getNormalDest()));
    Vals.push_back(VE.getValueID(II->getUnwindDest()));
    Vals.push_back(VE.getTypeID(FTy));
    pushValueAndType(Callee, InstID, Vals);

    // Emit value #'s for the fixed parameters.
    for (unsigned i = 0, e = FTy->getNumParams(); i != e; ++i)
      pushValue(I.getOperand(i), InstID, Vals); // fixed param.

    // Emit type/value pairs for varargs params.
    if (FTy->isVarArg()) {
      for (unsigned i = FTy->getNumParams(), e = II->arg_size(); i != e; ++i)
        pushValueAndType(I.getOperand(i), InstID, Vals); // vararg
    }
    break;
  }
  case Instruction::Resume:
    Code = bitc::FUNC_CODE_INST_RESUME;
    pushValueAndType(I.getOperand(0), InstID, Vals);
    break;
  case Instruction::CleanupRet: {
    Code = bitc::FUNC_CODE_INST_CLEANUPRET;
    const auto &CRI = cast<CleanupReturnInst>(I);
    pushValue(CRI.getCleanupPad(), InstID, Vals);
    if (CRI.hasUnwindDest())
      Vals.push_back(VE.getValueID(CRI.getUnwindDest()));
    break;
  }
  case Instruction::CatchRet: {
    Code = bitc::FUNC_CODE_INST_CATCHRET;
    const auto &CRI = cast<CatchReturnInst>(I);
    pushValue(CRI.getCatchPad(), InstID, Vals);
    Vals.push_back(VE.getValueID(CRI.getSuccessor()));
    break;
  }
  case Instruction::CleanupPad:
  case Instruction::CatchPad: {
    const auto &FuncletPad = cast<FuncletPadInst>(I);
    Code = isa<CatchPadInst>(FuncletPad) ? bitc::FUNC_CODE_INST_CATCHPAD
                                         : bitc::FUNC_CODE_INST_CLEANUPPAD;
    pushValue(FuncletPad.getParentPad(), InstID, Vals);

    unsigned NumArgOperands = FuncletPad.arg_size();
    Vals.push_back(NumArgOperands);
    for (unsigned Op = 0; Op != NumArgOperands; ++Op)
      pushValueAndType(FuncletPad.getArgOperand(Op), InstID, Vals);
    break;
  }
  case Instruction::CatchSwitch: {
    Code = bitc::FUNC_CODE_INST_CATCHSWITCH;
    const auto &CatchSwitch = cast<CatchSwitchInst>(I);

    pushValue(CatchSwitch.getParentPad(), InstID, Vals);

    unsigned NumHandlers = CatchSwitch.getNumHandlers();
    Vals.push_back(NumHandlers);
    for (const BasicBlock *CatchPadBB : CatchSwitch.handlers())
      Vals.push_back(VE.getValueID(CatchPadBB));

    if (CatchSwitch.hasUnwindDest())
      Vals.push_back(VE.getValueID(CatchSwitch.getUnwindDest()));
    break;
  }
  case Instruction::CallBr: {
    const CallBrInst *CBI = cast<CallBrInst>(&I);
    const Value *Callee = CBI->getCalledOperand();
    FunctionType *FTy = CBI->getFunctionType();

    if (CBI->hasOperandBundles())
      writeOperandBundles(*CBI, InstID);

    Code = bitc::FUNC_CODE_INST_CALLBR;

    Vals.push_back(VE.getAttributeListID(CBI->getAttributes()));

    Vals.push_back(CBI->getCallingConv() << bitc::CALL_CCONV |
                   1 << bitc::CALL_EXPLICIT_TYPE);

    Vals.push_back(VE.getValueID(CBI->getDefaultDest()));
    Vals.push_back(CBI->getNumIndirectDests());
    for (unsigned i = 0, e = CBI->getNumIndirectDests(); i != e; ++i)
      Vals.push_back(VE.getValueID(CBI->getIndirectDest(i)));

    Vals.push_back(VE.getTypeID(FTy));
    pushValueAndType(Callee, InstID, Vals);

    // Emit value #'s for the fixed parameters.
    for (unsigned i = 0, e = FTy->getNumParams(); i != e; ++i)
      pushValue(I.getOperand(i), InstID, Vals); // fixed param.

    // Emit type/value pairs for varargs params.
    if (FTy->isVarArg()) {
      for (unsigned i = FTy->getNumParams(), e = CBI->arg_size(); i != e; ++i)
        pushValueAndType(I.getOperand(i), InstID, Vals); // vararg
    }
    break;
  }
  case Instruction::Unreachable:
    Code = bitc::FUNC_CODE_INST_UNREACHABLE;
    AbbrevToUse = FUNCTION_INST_UNREACHABLE_ABBREV;
    break;

  case Instruction::PHI: {
    const PHINode &PN = cast<PHINode>(I);
    Code = bitc::FUNC_CODE_INST_PHI;
    // With the newer instruction encoding, forward references could give
    // negative valued IDs.  This is most common for PHIs, so we use
    // signed VBRs.
    SmallVector<uint64_t, 128> Vals64;
    Vals64.push_back(VE.getTypeID(PN.getType()));
    for (unsigned i = 0, e = PN.getNumIncomingValues(); i != e; ++i) {
      pushValueSigned(PN.getIncomingValue(i), InstID, Vals64);
      Vals64.push_back(VE.getValueID(PN.getIncomingBlock(i)));
    }

    uint64_t Flags = getOptimizationFlags(&I);
    if (Flags != 0)
      Vals64.push_back(Flags);

    // Emit a Vals64 vector and exit.
    Stream.EmitRecord(Code, Vals64, AbbrevToUse);
    Vals64.clear();
    return;
  }

  case Instruction::LandingPad: {
    const LandingPadInst &LP = cast<LandingPadInst>(I);
    Code = bitc::FUNC_CODE_INST_LANDINGPAD;
    Vals.push_back(VE.getTypeID(LP.getType()));
    Vals.push_back(LP.isCleanup());
    Vals.push_back(LP.getNumClauses());
    for (unsigned I = 0, E = LP.getNumClauses(); I != E; ++I) {
      if (LP.isCatch(I))
        Vals.push_back(LandingPadInst::Catch);
      else
        Vals.push_back(LandingPadInst::Filter);
      pushValueAndType(LP.getClause(I), InstID, Vals);
    }
    break;
  }

  case Instruction::Alloca: {
    Code = bitc::FUNC_CODE_INST_ALLOCA;
    const AllocaInst &AI = cast<AllocaInst>(I);
    Vals.push_back(VE.getTypeID(AI.getAllocatedType()));
    Vals.push_back(VE.getTypeID(I.getOperand(0)->getType()));
    Vals.push_back(VE.getValueID(I.getOperand(0))); // size.
    using APV = AllocaPackedValues;
    unsigned Record = 0;
    unsigned EncodedAlign = getEncodedAlign(AI.getAlign());
    Bitfield::set<APV::AlignLower>(
        Record, EncodedAlign & ((1 << APV::AlignLower::Bits) - 1));
    Bitfield::set<APV::AlignUpper>(Record,
                                   EncodedAlign >> APV::AlignLower::Bits);
    Bitfield::set<APV::UsedWithInAlloca>(Record, AI.isUsedWithInAlloca());
    Bitfield::set<APV::ExplicitType>(Record, true);
    Bitfield::set<APV::SwiftError>(Record, AI.isSwiftError());
    Vals.push_back(Record);

    unsigned AS = AI.getAddressSpace();
    if (AS != M.getDataLayout().getAllocaAddrSpace())
      Vals.push_back(AS);
    break;
  }

  case Instruction::Load:
    if (cast<LoadInst>(I).isAtomic()) {
      Code = bitc::FUNC_CODE_INST_LOADATOMIC;
      pushValueAndType(I.getOperand(0), InstID, Vals);
    } else {
      Code = bitc::FUNC_CODE_INST_LOAD;
      if (!pushValueAndType(I.getOperand(0), InstID, Vals)) // ptr
        AbbrevToUse = FUNCTION_INST_LOAD_ABBREV;
    }
    Vals.push_back(VE.getTypeID(I.getType()));
    Vals.push_back(getEncodedAlign(cast<LoadInst>(I).getAlign()));
    Vals.push_back(cast<LoadInst>(I).isVolatile());
    if (cast<LoadInst>(I).isAtomic()) {
      Vals.push_back(getEncodedOrdering(cast<LoadInst>(I).getOrdering()));
      Vals.push_back(getEncodedSyncScopeID(cast<LoadInst>(I).getSyncScopeID()));
    }
    break;
  case Instruction::Store:
    if (cast<StoreInst>(I).isAtomic()) {
      Code = bitc::FUNC_CODE_INST_STOREATOMIC;
    } else {
      Code = bitc::FUNC_CODE_INST_STORE;
      AbbrevToUse = FUNCTION_INST_STORE_ABBREV;
    }
    if (pushValueAndType(I.getOperand(1), InstID, Vals)) // ptrty + ptr
      AbbrevToUse = 0;
    if (pushValueAndType(I.getOperand(0), InstID, Vals)) // valty + val
      AbbrevToUse = 0;
    Vals.push_back(getEncodedAlign(cast<StoreInst>(I).getAlign()));
    Vals.push_back(cast<StoreInst>(I).isVolatile());
    if (cast<StoreInst>(I).isAtomic()) {
      Vals.push_back(getEncodedOrdering(cast<StoreInst>(I).getOrdering()));
      Vals.push_back(
          getEncodedSyncScopeID(cast<StoreInst>(I).getSyncScopeID()));
    }
    break;
  case Instruction::AtomicCmpXchg:
    Code = bitc::FUNC_CODE_INST_CMPXCHG;
    pushValueAndType(I.getOperand(0), InstID, Vals); // ptrty + ptr
    pushValueAndType(I.getOperand(1), InstID, Vals); // cmp.
    pushValue(I.getOperand(2), InstID, Vals);        // newval.
    Vals.push_back(cast<AtomicCmpXchgInst>(I).isVolatile());
    Vals.push_back(
        getEncodedOrdering(cast<AtomicCmpXchgInst>(I).getSuccessOrdering()));
    Vals.push_back(
        getEncodedSyncScopeID(cast<AtomicCmpXchgInst>(I).getSyncScopeID()));
    Vals.push_back(
        getEncodedOrdering(cast<AtomicCmpXchgInst>(I).getFailureOrdering()));
    Vals.push_back(cast<AtomicCmpXchgInst>(I).isWeak());
    Vals.push_back(getEncodedAlign(cast<AtomicCmpXchgInst>(I).getAlign()));
    break;
  case Instruction::AtomicRMW:
    Code = bitc::FUNC_CODE_INST_ATOMICRMW;
    pushValueAndType(I.getOperand(0), InstID, Vals); // ptrty + ptr
    pushValueAndType(I.getOperand(1), InstID, Vals); // valty + val
    Vals.push_back(getEncodedRMWOperation(cast<AtomicRMWInst>(I)));
    Vals.push_back(cast<AtomicRMWInst>(I).isVolatile());
    Vals.push_back(getEncodedOrdering(cast<AtomicRMWInst>(I).getOrdering()));
    Vals.push_back(
        getEncodedSyncScopeID(cast<AtomicRMWInst>(I).getSyncScopeID()));
    Vals.push_back(getEncodedAlign(cast<AtomicRMWInst>(I).getAlign()));
    break;
  case Instruction::Fence:
    Code = bitc::FUNC_CODE_INST_FENCE;
    Vals.push_back(getEncodedOrdering(cast<FenceInst>(I).getOrdering()));
    Vals.push_back(getEncodedSyncScopeID(cast<FenceInst>(I).getSyncScopeID()));
    break;
  case Instruction::Call: {
    const CallInst &CI = cast<CallInst>(I);
    FunctionType *FTy = CI.getFunctionType();

    if (CI.hasOperandBundles())
      writeOperandBundles(CI, InstID);

    Code = bitc::FUNC_CODE_INST_CALL;

    Vals.push_back(VE.getAttributeListID(CI.getAttributes()));

    unsigned Flags = getOptimizationFlags(&I);
    Vals.push_back(CI.getCallingConv() << bitc::CALL_CCONV |
                   unsigned(CI.isTailCall()) << bitc::CALL_TAIL |
                   unsigned(CI.isMustTailCall()) << bitc::CALL_MUSTTAIL |
                   1 << bitc::CALL_EXPLICIT_TYPE |
                   unsigned(CI.isNoTailCall()) << bitc::CALL_NOTAIL |
                   unsigned(Flags != 0) << bitc::CALL_FMF);
    if (Flags != 0)
      Vals.push_back(Flags);

    Vals.push_back(VE.getTypeID(FTy));
    pushValueAndType(CI.getCalledOperand(), InstID, Vals); // Callee

    // Emit value #'s for the fixed parameters.
    for (unsigned i = 0, e = FTy->getNumParams(); i != e; ++i)
      pushValue(CI.getArgOperand(i), InstID, Vals); // fixed param.

    // Emit type/value pairs for varargs params.
    if (FTy->isVarArg()) {
      for (unsigned i = FTy->getNumParams(), e = CI.arg_size(); i != e; ++i)
        pushValueAndType(CI.getArgOperand(i), InstID, Vals); // varargs
    }
    break;
  }
  case Instruction::VAArg:
    Code = bitc::FUNC_CODE_INST_VAARG;
    Vals.push_back(VE.getTypeID(I.getOperand(0)->getType()));   // valistty
    pushValue(I.getOperand(0), InstID, Vals);                   // valist.
    Vals.push_back(VE.getTypeID(I.getType())); // restype.
    break;
  case Instruction::Freeze:
    Code = bitc::FUNC_CODE_INST_FREEZE;
    pushValueAndType(I.getOperand(0), InstID, Vals);
    break;
  }

  Stream.EmitRecord(Code, Vals, AbbrevToUse);
  Vals.clear();
}

/// Write a GlobalValue VST to the module. The purpose of this data structure is
/// to allow clients to efficiently find the function body.

bool KeyWriter::pushValueAndType(const Value *V, unsigned InstID,
                                 SmallVectorImpl<unsigned> &Vals) {
  unsigned ValID = VE.getValueID(V);
  // Make encoding relative to the InstID.
  Vals.push_back(InstID - ValID);
  if (ValID >= InstID) {
    Vals.push_back(VE.getTypeID(V->getType()));
    return true;
  }
  return false;
}

// The enumerator: a type, constant, metadata node, attribute set, comdat or
// name is numbered where it is first referenced, and its definition is
// written there. Global values, arguments, blocks and instructions are
// numbered before any record refers to them.

void KeyWriter::number(const Value *V) {
  Values[V] = NextValue++;
  Numbered.push_back(V);
}

unsigned KeyWriter::getTypeID(Type *T) {
  auto [It, Fresh] = Types.try_emplace(T, NextType);
  if (!Fresh)
    return It->second;
  unsigned ID = NextType++;
  writeType(T, ID);
  return ID;
}

unsigned KeyWriter::getValueID(const Value *V) {
  if (const auto *MD = dyn_cast<MetadataAsValue>(V))
    return getMetadataID(MD->getMetadata());
  if (const auto *BB = dyn_cast<BasicBlock>(V))
    return getGlobalBasicBlockID(BB);
  auto [It, Fresh] = Values.try_emplace(V, NextValue);
  if (!Fresh)
    return It->second;
  unsigned ID = NextValue++;
  Numbered.push_back(V);
  writeConstant(V, ID);
  return ID;
}

unsigned KeyWriter::getGlobalBasicBlockID(const BasicBlock *BB) {
  auto It = Blocks.find(BB);
  if (It != Blocks.end())
    return It->second;
  unsigned Index = 0;
  for (const BasicBlock &B : *BB->getParent()) {
    Blocks[&B] = Index++;
    NumberedBlocks.push_back(&B);
  }
  return Blocks.lookup(BB);
}

unsigned KeyWriter::getMetadataID(const Metadata *MD) {
  auto [It, Fresh] = MDs.try_emplace(MD, NextMD);
  if (!Fresh)
    return It->second;
  unsigned ID = NextMD++;
  writeMetadata(MD, ID);
  return ID;
}

unsigned KeyWriter::getComdatID(const Comdat *C) {
  auto [It, Fresh] = Comdats.try_emplace(C, NextComdat);
  if (!Fresh)
    return It->second;
  unsigned ID = NextComdat++;
  Define D(Stream, DefineComdat, ID);
  SmallVector<uint64_t, 64> Vals(C->getName().bytes_begin(),
                                 C->getName().bytes_end());
  Vals.push_back(C->getSelectionKind());
  Stream.EmitRecord(bitc::MODULE_CODE_COMDAT, Vals);
  return ID;
}

unsigned KeyWriter::addToStrtab(StringRef Str) {
  unsigned ID = NextName++;
  Define D(Stream, DefineName, ID);
  writeStringRecord(Stream, bitc::STRTAB_BLOB, Str);
  return ID;
}

unsigned KeyWriter::getAttributeListID(AttributeList AL) {
  if (AL.isEmpty())
    return 0;
  auto [It, Fresh] = AttributeLists.try_emplace(AL, NextAttributeList);
  if (!Fresh)
    return It->second;
  unsigned ID = NextAttributeList++;
  SmallVector<uint64_t, 64> Record;
  for (unsigned i : AL.indexes()) {
    AttributeSet AS = AL.getAttributes(i);
    if (AS.hasAttributes())
      Record.push_back(VE.getAttributeGroupID({i, AS}));
  }
  Define D(Stream, DefineAttributeList, ID);
  Stream.EmitRecord(bitc::PARAMATTR_CODE_ENTRY, Record);
  return ID;
}

unsigned KeyWriter::getAttributeGroupID(
    std::pair<unsigned, AttributeSet> Group) {
  auto [It, Fresh] = AttributeGroups.try_emplace(Group, NextAttributeGroup);
  if (!Fresh)
    return It->second;
  unsigned ID = NextAttributeGroup++;
  unsigned AttrListIndex = Group.first;
  AttributeSet AS = Group.second;
  SmallVector<uint64_t, 64> Record;
  Record.push_back(AttrListIndex);

  for (Attribute Attr : AS) {
    if (Attr.isEnumAttribute()) {
      Record.push_back(0);
      Record.push_back(Attr.getKindAsEnum());
    } else if (Attr.isIntAttribute()) {
      Record.push_back(1);
      Record.push_back(Attr.getKindAsEnum());
      Record.push_back(Attr.getValueAsInt());
    } else if (Attr.isStringAttribute()) {
      StringRef Kind = Attr.getKindAsString();
      StringRef Val = Attr.getValueAsString();

      Record.push_back(Val.empty() ? 3 : 4);
      Record.append(Kind.begin(), Kind.end());
      Record.push_back(0);
      if (!Val.empty()) {
        Record.append(Val.begin(), Val.end());
        Record.push_back(0);
      }
    } else if (Attr.isTypeAttribute()) {
      Type *Ty = Attr.getValueAsType();
      Record.push_back(Ty ? 6 : 5);
      Record.push_back(Attr.getKindAsEnum());
      if (Ty)
        Record.push_back(VE.getTypeID(Attr.getValueAsType()));
    } else if (Attr.isConstantRangeAttribute()) {
      Record.push_back(7);
      Record.push_back(Attr.getKindAsEnum());
      emitConstantRange(Record, Attr.getValueAsConstantRange(),
                        /*EmitBitWidth=*/true);
    } else {
      assert(Attr.isConstantRangeListAttribute());
      Record.push_back(8);
      Record.push_back(Attr.getKindAsEnum());
      ArrayRef<ConstantRange> Val = Attr.getValueAsConstantRangeList();
      Record.push_back(Val.size());
      Record.push_back(Val[0].getBitWidth());
      for (auto &CR : Val)
        emitConstantRange(Record, CR, /*EmitBitWidth=*/false);
    }
  }

  Define D(Stream, DefineAttributeGroup, ID);
  Stream.EmitRecord(bitc::PARAMATTR_GRP_CODE_ENTRY, Record);
  return ID;
}

void KeyWriter::writeType(Type *T, unsigned ID) {
  Define D(Stream, DefineType, ID);
  SmallVector<uint64_t, 64> TypeVals;
  unsigned Code = 0;

  switch (T->getTypeID()) {
  case Type::VoidTyID:      Code = bitc::TYPE_CODE_VOID;      break;
  case Type::HalfTyID:      Code = bitc::TYPE_CODE_HALF;      break;
  case Type::BFloatTyID:    Code = bitc::TYPE_CODE_BFLOAT;    break;
  case Type::FloatTyID:     Code = bitc::TYPE_CODE_FLOAT;     break;
  case Type::DoubleTyID:    Code = bitc::TYPE_CODE_DOUBLE;    break;
  case Type::X86_FP80TyID:  Code = bitc::TYPE_CODE_X86_FP80;  break;
  case Type::FP128TyID:     Code = bitc::TYPE_CODE_FP128;     break;
  case Type::PPC_FP128TyID: Code = bitc::TYPE_CODE_PPC_FP128; break;
  case Type::LabelTyID:     Code = bitc::TYPE_CODE_LABEL;     break;
  case Type::MetadataTyID:
    Code = bitc::TYPE_CODE_METADATA;
    break;
  case Type::X86_AMXTyID:   Code = bitc::TYPE_CODE_X86_AMX;   break;
  case Type::TokenTyID:     Code = bitc::TYPE_CODE_TOKEN;     break;
  case Type::ByteTyID:
    // BYTE: [width]
    Code = bitc::TYPE_CODE_BYTE;
    TypeVals.push_back(T->getByteBitWidth());
    break;
  case Type::IntegerTyID:
    // INTEGER: [width]
    Code = bitc::TYPE_CODE_INTEGER;
    TypeVals.push_back(cast<IntegerType>(T)->getBitWidth());
    break;
  case Type::PointerTyID: {
    PointerType *PTy = cast<PointerType>(T);
    // OPAQUE_POINTER: [address space]
    Code = bitc::TYPE_CODE_OPAQUE_POINTER;
    TypeVals.push_back(PTy->getAddressSpace());
    break;
  }
  case Type::FunctionTyID: {
    FunctionType *FT = cast<FunctionType>(T);
    // FUNCTION: [isvararg, retty, paramty x N]
    Code = bitc::TYPE_CODE_FUNCTION;
    TypeVals.push_back(FT->isVarArg());
    TypeVals.push_back(VE.getTypeID(FT->getReturnType()));
    for (unsigned i = 0, e = FT->getNumParams(); i != e; ++i)
      TypeVals.push_back(VE.getTypeID(FT->getParamType(i)));
    break;
  }
  case Type::StructTyID: {
    StructType *ST = cast<StructType>(T);
    // STRUCT: [ispacked, eltty x N]
    TypeVals.push_back(ST->isPacked());
    // Output all of the element types.
    for (Type *ET : ST->elements())
      TypeVals.push_back(VE.getTypeID(ET));

    if (ST->isLiteral()) {
      Code = bitc::TYPE_CODE_STRUCT_ANON;
    } else {
      if (ST->isOpaque()) {
        Code = bitc::TYPE_CODE_OPAQUE;
      } else {
        Code = bitc::TYPE_CODE_STRUCT_NAMED;
      }

      // Emit the name if it is present.
      if (!ST->getName().empty())
        writeStringRecord(Stream, bitc::TYPE_CODE_STRUCT_NAME, ST->getName());
    }
    break;
  }
  case Type::ArrayTyID: {
    ArrayType *AT = cast<ArrayType>(T);
    // ARRAY: [numelts, eltty]
    Code = bitc::TYPE_CODE_ARRAY;
    TypeVals.push_back(AT->getNumElements());
    TypeVals.push_back(VE.getTypeID(AT->getElementType()));
    break;
  }
  case Type::FixedVectorTyID:
  case Type::ScalableVectorTyID: {
    VectorType *VT = cast<VectorType>(T);
    // VECTOR [numelts, eltty] or
    //        [numelts, eltty, scalable]
    Code = bitc::TYPE_CODE_VECTOR;
    TypeVals.push_back(VT->getElementCount().getKnownMinValue());
    TypeVals.push_back(VE.getTypeID(VT->getElementType()));
    if (isa<ScalableVectorType>(VT))
      TypeVals.push_back(true);
    break;
  }
  case Type::TargetExtTyID: {
    TargetExtType *TET = cast<TargetExtType>(T);
    Code = bitc::TYPE_CODE_TARGET_TYPE;
    writeStringRecord(Stream, bitc::TYPE_CODE_STRUCT_NAME, TET->getName());
    TypeVals.push_back(TET->getNumTypeParameters());
    for (Type *InnerTy : TET->type_params())
      TypeVals.push_back(VE.getTypeID(InnerTy));
    llvm::append_range(TypeVals, TET->int_params());
    break;
  }
  case Type::TypedPointerTyID:
    llvm_unreachable("Typed pointers cannot be added to IR modules");
  }

  // Emit the finished record.
  Stream.EmitRecord(Code, TypeVals);
}

void KeyWriter::writeConstant(const Value *V, unsigned ID) {
  Define D(Stream, DefineValue, ID);
  SmallVector<uint64_t, 64> Record;
  Record.push_back(VE.getTypeID(V->getType()));
  Stream.EmitRecord(bitc::CST_CODE_SETTYPE, Record, CONSTANTS_SETTYPE_ABBREV);
  Record.clear();

  // A value the module does not hold (verified IR has none): its kind and
  // name.
  if (!isa<Constant>(V) && !isa<InlineAsm>(V)) {
    Record.push_back(V->getValueID());
    Stream.EmitRecord(bitc::CST_CODE_NULL, Record);
    return;
  }
  if (const auto *GV = dyn_cast<GlobalValue>(V)) {
    writeStringRecord(Stream, bitc::MODULE_CODE_HASH, GV->getName());
    return;
  }

  unsigned AggregateAbbrev = 0;
  unsigned String8Abbrev = 0;
  unsigned CString7Abbrev = 0;
  unsigned CString6Abbrev = 0;

  if (const InlineAsm *IA = dyn_cast<InlineAsm>(V)) {
    Record.push_back(VE.getTypeID(IA->getFunctionType()));
    Record.push_back(
        unsigned(IA->hasSideEffects()) | unsigned(IA->isAlignStack()) << 1 |
        unsigned(IA->getDialect() & 1) << 2 | unsigned(IA->canThrow()) << 3);

    // Add the asm string.
    StringRef AsmStr = IA->getAsmString();
    Record.push_back(AsmStr.size());
    Record.append(AsmStr.begin(), AsmStr.end());

    // Add the constraint string.
    StringRef ConstraintStr = IA->getConstraintString();
    Record.push_back(ConstraintStr.size());
    Record.append(ConstraintStr.begin(), ConstraintStr.end());
    Stream.EmitRecord(bitc::CST_CODE_INLINEASM, Record);
    return;
  }
  const Constant *C = cast<Constant>(V);
  unsigned Code = -1U;
  unsigned AbbrevToUse = 0;
  if (C->isNullValue()) {
    Code = bitc::CST_CODE_NULL;
  } else if (isa<PoisonValue>(C)) {
    Code = bitc::CST_CODE_POISON;
  } else if (isa<UndefValue>(C)) {
    Code = bitc::CST_CODE_UNDEF;
  } else if (const ConstantInt *IV = dyn_cast<ConstantInt>(C)) {
    if (IV->getBitWidth() <= 64) {
      uint64_t V = IV->getSExtValue();
      emitSignedInt64(Record, V);
      Code = bitc::CST_CODE_INTEGER;
      AbbrevToUse = CONSTANTS_INTEGER_ABBREV;
    } else {                             // Wide integers, > 64 bits in size.
      emitWideAPInt(Record, IV->getValue());
      Code = bitc::CST_CODE_WIDE_INTEGER;
    }
  } else if (const ConstantByte *BV = dyn_cast<ConstantByte>(C)) {
    if (BV->getBitWidth() <= 64) {
      uint64_t V = BV->getSExtValue();
      emitSignedInt64(Record, V);
      Code = bitc::CST_CODE_BYTE;
      AbbrevToUse = CONSTANTS_BYTE_ABBREV;
    } else { // Wide bytes, > 64 bits in size.
      emitWideAPInt(Record, BV->getValue());
      Code = bitc::CST_CODE_WIDE_BYTE;
    }
  } else if (const ConstantFP *CFP = dyn_cast<ConstantFP>(C)) {
    Code = bitc::CST_CODE_FLOAT;
    Type *Ty = CFP->getType()->getScalarType();
    if (Ty->isHalfTy() || Ty->isBFloatTy() || Ty->isFloatTy() ||
        Ty->isDoubleTy()) {
      Record.push_back(CFP->getValueAPF().bitcastToAPInt().getZExtValue());
    } else if (Ty->isX86_FP80Ty()) {
      // api needed to prevent premature destruction
      // bits are not in the same order as a normal i80 APInt, compensate.
      APInt api = CFP->getValueAPF().bitcastToAPInt();
      const uint64_t *p = api.getRawData();
      Record.push_back((p[1] << 48) | (p[0] >> 16));
      Record.push_back(p[0] & 0xffffLL);
    } else if (Ty->isFP128Ty() || Ty->isPPC_FP128Ty()) {
      APInt api = CFP->getValueAPF().bitcastToAPInt();
      const uint64_t *p = api.getRawData();
      Record.push_back(p[0]);
      Record.push_back(p[1]);
    } else {
      assert(0 && "Unknown FP type!");
    }
  } else if (isa<ConstantDataSequential>(C) &&
             cast<ConstantDataSequential>(C)->isString()) {
    const ConstantDataSequential *Str = cast<ConstantDataSequential>(C);
    // Emit constant strings specially.
    uint64_t NumElts = Str->getNumElements();
    // If this is a null-terminated string, use the denser CSTRING encoding.
    if (Str->isCString()) {
      Code = bitc::CST_CODE_CSTRING;
      --NumElts;  // Don't encode the null, which isn't allowed by char6.
    } else {
      Code = bitc::CST_CODE_STRING;
      AbbrevToUse = String8Abbrev;
    }
    bool isCStr7 = Code == bitc::CST_CODE_CSTRING;
    bool isCStrChar6 = Code == bitc::CST_CODE_CSTRING;
    for (uint64_t i = 0; i != NumElts; ++i) {
      unsigned char V = Str->getElementAsInteger(i);
      Record.push_back(V);
      isCStr7 &= (V & 128) == 0;
      if (isCStrChar6)
        isCStrChar6 = BitCodeAbbrevOp::isChar6(V);
    }

    if (isCStrChar6)
      AbbrevToUse = CString6Abbrev;
    else if (isCStr7)
      AbbrevToUse = CString7Abbrev;
  } else if (const ConstantDataSequential *CDS =
                 dyn_cast<ConstantDataSequential>(C)) {
    Code = bitc::CST_CODE_DATA;
    Type *EltTy = CDS->getElementType();
    if (isa<IntegerType>(EltTy) || isa<ByteType>(EltTy)) {
      for (uint64_t i = 0, e = CDS->getNumElements(); i != e; ++i)
        Record.push_back(CDS->getElementAsInteger(i));
    } else {
      for (uint64_t i = 0, e = CDS->getNumElements(); i != e; ++i)
        Record.push_back(
            CDS->getElementAsAPFloat(i).bitcastToAPInt().getLimitedValue());
    }
  } else if (isa<ConstantAggregate>(C)) {
    Code = bitc::CST_CODE_AGGREGATE;
    for (const Value *Op : C->operands())
      Record.push_back(VE.getValueID(Op));
    AbbrevToUse = AggregateAbbrev;
  } else if (const ConstantExpr *CE = dyn_cast<ConstantExpr>(C)) {
    switch (CE->getOpcode()) {
    default:
      if (Instruction::isCast(CE->getOpcode())) {
        Code = bitc::CST_CODE_CE_CAST;
        Record.push_back(getEncodedCastOpcode(CE->getOpcode()));
        Record.push_back(VE.getTypeID(C->getOperand(0)->getType()));
        Record.push_back(VE.getValueID(C->getOperand(0)));
        AbbrevToUse = CONSTANTS_CE_CAST_Abbrev;
      } else {
        assert(CE->getNumOperands() == 2 && "Unknown constant expr!");
        Code = bitc::CST_CODE_CE_BINOP;
        Record.push_back(getEncodedBinaryOpcode(CE->getOpcode()));
        Record.push_back(VE.getValueID(C->getOperand(0)));
        Record.push_back(VE.getValueID(C->getOperand(1)));
        uint64_t Flags = getOptimizationFlags(CE);
        if (Flags != 0)
          Record.push_back(Flags);
      }
      break;
    case Instruction::FNeg: {
      assert(CE->getNumOperands() == 1 && "Unknown constant expr!");
      Code = bitc::CST_CODE_CE_UNOP;
      Record.push_back(getEncodedUnaryOpcode(CE->getOpcode()));
      Record.push_back(VE.getValueID(C->getOperand(0)));
      uint64_t Flags = getOptimizationFlags(CE);
      if (Flags != 0)
        Record.push_back(Flags);
      break;
    }
    case Instruction::GetElementPtr: {
      Code = bitc::CST_CODE_CE_GEP;
      const auto *GO = cast<GEPOperator>(C);
      Record.push_back(VE.getTypeID(GO->getSourceElementType()));
      Record.push_back(getOptimizationFlags(GO));
      if (std::optional<ConstantRange> Range = GO->getInRange()) {
        Code = bitc::CST_CODE_CE_GEP_WITH_INRANGE;
        emitConstantRange(Record, *Range, /*EmitBitWidth=*/true);
      }
      for (const Value *Op : CE->operands()) {
        Record.push_back(VE.getTypeID(Op->getType()));
        Record.push_back(VE.getValueID(Op));
      }
      break;
    }
    case Instruction::ExtractElement:
      Code = bitc::CST_CODE_CE_EXTRACTELT;
      Record.push_back(VE.getTypeID(C->getOperand(0)->getType()));
      Record.push_back(VE.getValueID(C->getOperand(0)));
      Record.push_back(VE.getTypeID(C->getOperand(1)->getType()));
      Record.push_back(VE.getValueID(C->getOperand(1)));
      break;
    case Instruction::InsertElement:
      Code = bitc::CST_CODE_CE_INSERTELT;
      Record.push_back(VE.getValueID(C->getOperand(0)));
      Record.push_back(VE.getValueID(C->getOperand(1)));
      Record.push_back(VE.getTypeID(C->getOperand(2)->getType()));
      Record.push_back(VE.getValueID(C->getOperand(2)));
      break;
    case Instruction::ShuffleVector:
      // If the return type and argument types are the same, this is a
      // standard shufflevector instruction.  If the types are different,
      // then the shuffle is widening or truncating the input vectors, and
      // the argument type must also be encoded.
      if (C->getType() == C->getOperand(0)->getType()) {
        Code = bitc::CST_CODE_CE_SHUFFLEVEC;
      } else {
        Code = bitc::CST_CODE_CE_SHUFVEC_EX;
        Record.push_back(VE.getTypeID(C->getOperand(0)->getType()));
      }
      Record.push_back(VE.getValueID(C->getOperand(0)));
      Record.push_back(VE.getValueID(C->getOperand(1)));
      Record.push_back(VE.getValueID(CE->getShuffleMaskForBitcode()));
      break;
    }
  } else if (const BlockAddress *BA = dyn_cast<BlockAddress>(C)) {
    Code = bitc::CST_CODE_BLOCKADDRESS;
    Record.push_back(VE.getTypeID(BA->getFunction()->getType()));
    Record.push_back(VE.getValueID(BA->getFunction()));
    Record.push_back(VE.getGlobalBasicBlockID(BA->getBasicBlock()));
  } else if (const auto *Equiv = dyn_cast<DSOLocalEquivalent>(C)) {
    Code = bitc::CST_CODE_DSO_LOCAL_EQUIVALENT;
    Record.push_back(VE.getTypeID(Equiv->getGlobalValue()->getType()));
    Record.push_back(VE.getValueID(Equiv->getGlobalValue()));
  } else if (const auto *NC = dyn_cast<NoCFIValue>(C)) {
    Code = bitc::CST_CODE_NO_CFI_VALUE;
    Record.push_back(VE.getTypeID(NC->getGlobalValue()->getType()));
    Record.push_back(VE.getValueID(NC->getGlobalValue()));
  } else if (const auto *CPA = dyn_cast<ConstantPtrAuth>(C)) {
    Code = bitc::CST_CODE_PTRAUTH2;
    Record.push_back(VE.getValueID(CPA->getPointer()));
    Record.push_back(VE.getValueID(CPA->getKey()));
    Record.push_back(VE.getValueID(CPA->getDiscriminator()));
    Record.push_back(VE.getValueID(CPA->getAddrDiscriminator()));
    Record.push_back(VE.getValueID(CPA->getDeactivationSymbol()));
  } else {
    llvm_unreachable("Unknown constant!");
  }
  Stream.EmitRecord(Code, Record, AbbrevToUse);
}

void KeyWriter::writeMetadata(const Metadata *MD, unsigned ID) {
  Define D(Stream, DefineMetadata, ID);
  SmallVector<uint64_t, 64> Record;
  if (const auto *S = dyn_cast<MDString>(MD)) {
    writeStringRecord(Stream, bitc::METADATA_STRING_OLD, S->getString());
    return;
  }
  if (const MDNode *N = dyn_cast<MDNode>(MD)) {
    unsigned Abbrev = 0;
    switch (N->getMetadataID()) {
    default:
      llvm_unreachable("Invalid MDNode subclass");
#define HANDLE_MDNODE_LEAF(CLASS)                                              \
  case Metadata::CLASS##Kind:                                                  \
    write##CLASS(cast<CLASS>(N), Record, Abbrev);                              \
    return;
#include "llvm/IR/Metadata.def"
    }
  }
  if (auto *AL = dyn_cast<DIArgList>(MD)) {
    writeDIArgList(AL, Record);
    return;
  }
  writeValueAsMetadata(cast<ValueAsMetadata>(MD), Record);
}

// The context's tables the writer copies whole.
void KeyWriter::writeContextTables() {
  SmallVector<uint64_t, 64> Record;

  // Write metadata kinds
  // METADATA_KIND - [n x [id, name]]
  SmallVector<StringRef, 8> Names;
  M.getMDKindNames(Names);
  Stream.EnterSubblock(bitc::METADATA_KIND_BLOCK_ID, 3);
  for (unsigned MDKindID = 0, e = Names.size(); MDKindID != e; ++MDKindID) {
    Record.push_back(MDKindID);
    StringRef KName = Names[MDKindID];
    Record.append(KName.begin(), KName.end());

    Stream.EmitRecord(bitc::METADATA_KIND, Record, 0);
    Record.clear();
  }
  Stream.ExitBlock();

  SmallVector<StringRef, 8> Tags;
  M.getOperandBundleTags(Tags);
  Stream.EnterSubblock(bitc::OPERAND_BUNDLE_TAGS_BLOCK_ID, 3);
  for (auto Tag : Tags) {
    Record.append(Tag.begin(), Tag.end());

    Stream.EmitRecord(bitc::OPERAND_BUNDLE_TAG, Record, 0);
    Record.clear();
  }
  Stream.ExitBlock();

  SmallVector<StringRef, 8> SSNs;
  M.getContext().getSyncScopeNames(SSNs);
  Stream.EnterSubblock(bitc::SYNC_SCOPE_NAMES_BLOCK_ID, 2);
  for (auto SSN : SSNs) {
    Record.append(SSN.begin(), SSN.end());
    Stream.EmitRecord(bitc::SYNC_SCOPE_NAME, Record, 0);
    Record.clear();
  }
  Stream.ExitBlock();
}

void KeyWriter::writeModuleInfo() {
  // Emit various pieces of data attached to a module.
  if (!M.getTargetTriple().empty())
    writeStringRecord(Stream, bitc::MODULE_CODE_TRIPLE,
                      M.getTargetTriple().str());
  const std::string &DL = M.getDataLayoutStr();
  if (!DL.empty())
    writeStringRecord(Stream, bitc::MODULE_CODE_DATALAYOUT, DL);

  for (const Module::GlobalAsmFragment &Frag : M.getModuleInlineAsm()) {
    SmallVector<std::pair<StringRef, StringRef>> Props =
        Frag.Props.getAsStrings();
    for (auto [Key, Value] : Props) {
      SmallVector<unsigned, 64> Record;
      Record.append(Key.begin(), Key.end());
      Record.push_back(0);
      Record.append(Value.begin(), Value.end());
      Stream.EmitRecord(bitc::MODULE_CODE_ASM_PROPERTY, Record);
    }
    writeStringRecord(Stream, bitc::MODULE_CODE_ASM, Frag.Asm);
  }

  // Emit information about sections and GC, computing how many there are.
  std::map<std::string, unsigned> SectionMap;
  std::map<std::string, unsigned> GCMap;
  for (const GlobalVariable &GV : M.globals()) {
    if (GV.hasSection()) {
      // Give section names unique ID's.
      unsigned &Entry = SectionMap[std::string(GV.getSection())];
      if (!Entry) {
        writeStringRecord(Stream, bitc::MODULE_CODE_SECTIONNAME,
                          GV.getSection());
        Entry = SectionMap.size();
      }
    }
  }
  for (const Function &F : M) {
    if (F.hasSection()) {
      // Give section names unique ID's.
      unsigned &Entry = SectionMap[std::string(F.getSection())];
      if (!Entry) {
        writeStringRecord(Stream, bitc::MODULE_CODE_SECTIONNAME,
                          F.getSection());
        Entry = SectionMap.size();
      }
    }
    if (F.hasGC()) {
      // Same for GC names.
      unsigned &Entry = GCMap[F.getGC()];
      if (!Entry) {
        writeStringRecord(Stream, bitc::MODULE_CODE_GCNAME, F.getGC());
        Entry = GCMap.size();
      }
    }
  }

  // Emit the module's source file name.
  writeStringRecord(Stream, bitc::MODULE_CODE_SOURCE_FILENAME,
                    M.getSourceFileName());

  SmallVector<uint64_t, 64> Vals;
  for (const GlobalValue &GV : M.global_values()) {
    auto MaybeGUID = GV.getGUIDIfAssigned();
    if (!MaybeGUID)
      continue;
    Vals.push_back(VE.getValueID(&GV));
    Vals.push_back(*MaybeGUID);
  }
  Stream.EmitRecord(bitc::MODULE_CODE_GUIDLIST, Vals);
  Vals.clear();

  // Emit the global variable information.
  for (const GlobalVariable &GV : M.globals()) {
    // GLOBALVAR: [strtab offset, strtab size, type, isconst, initid,
    //             linkage, alignment, section, visibility, threadlocal,
    //             unnamed_addr, externally_initialized, dllstorageclass,
    //             comdat, attributes, DSO_Local, GlobalSanitizer, code_model]
    Vals.push_back(addToStrtab(GV.getName()));
    Vals.push_back(GV.getName().size());
    Vals.push_back(VE.getTypeID(GV.getValueType()));
    Vals.push_back(GV.getType()->getAddressSpace() << 2 | 2 | GV.isConstant());
    Vals.push_back(GV.isDeclaration() ? 0 :
                   (VE.getValueID(GV.getInitializer()) + 1));
    Vals.push_back(GV.getLinkage());
    Vals.push_back(getEncodedAlign(GV.getAlign()));
    Vals.push_back(GV.hasSection() ? SectionMap[std::string(GV.getSection())]
                                   : 0);
    Vals.push_back(GV.getVisibility());
    Vals.push_back(GV.getThreadLocalMode());
    Vals.push_back(unsigned(GV.getUnnamedAddr()));
    Vals.push_back(GV.isExternallyInitialized());
    Vals.push_back(GV.getDLLStorageClass());
    Vals.push_back(GV.hasComdat() ? VE.getComdatID(GV.getComdat()) : 0);

    auto AL = GV.getAttributesAsList(AttributeList::FunctionIndex);
    Vals.push_back(VE.getAttributeListID(AL));

    Vals.push_back(GV.isDSOLocal());
    Vals.push_back(addToStrtab(GV.getPartition()));
    Vals.push_back(GV.getPartition().size());

    Vals.push_back((GV.hasSanitizerMetadata() ? serializeSanitizerMetadata(
                                                    GV.getSanitizerMetadata())
                                              : 0));
    Vals.push_back(GV.getCodeModelRaw());

    Stream.EmitRecord(bitc::MODULE_CODE_GLOBALVAR, Vals);
    Vals.clear();
  }

  // Emit the function proto information.
  for (const Function &F : M) {
    // FUNCTION:  [strtab offset, strtab size, type, callingconv, isproto,
    //             linkage, paramattrs, alignment, section, visibility, gc,
    //             unnamed_addr, prologuedata, dllstorageclass, comdat,
    //             prefixdata, personalityfn, DSO_Local, addrspace,
    //             partition_strtab, partition_size, prefalign]
    Vals.push_back(addToStrtab(F.getName()));
    Vals.push_back(F.getName().size());
    Vals.push_back(VE.getTypeID(F.getFunctionType()));
    Vals.push_back(F.getCallingConv());
    Vals.push_back(F.isDeclaration());
    Vals.push_back(F.getLinkage());
    Vals.push_back(VE.getAttributeListID(F.getAttributes()));
    Vals.push_back(getEncodedAlign(F.getAlign()));
    Vals.push_back(F.hasSection() ? SectionMap[std::string(F.getSection())]
                                  : 0);
    Vals.push_back(F.getVisibility());
    Vals.push_back(F.hasGC() ? GCMap[F.getGC()] : 0);
    Vals.push_back(unsigned(F.getUnnamedAddr()));
    Vals.push_back(F.hasPrologueData() ? (VE.getValueID(F.getPrologueData()) + 1)
                                       : 0);
    Vals.push_back(F.getDLLStorageClass());
    Vals.push_back(F.hasComdat() ? VE.getComdatID(F.getComdat()) : 0);
    Vals.push_back(F.hasPrefixData() ? (VE.getValueID(F.getPrefixData()) + 1)
                                     : 0);
    Vals.push_back(
        F.hasPersonalityFn() ? (VE.getValueID(F.getPersonalityFn()) + 1) : 0);

    Vals.push_back(F.isDSOLocal());
    Vals.push_back(F.getAddressSpace());
    Vals.push_back(addToStrtab(F.getPartition()));
    Vals.push_back(F.getPartition().size());
    Vals.push_back(getEncodedAlign(F.getPreferredAlignment()));

    Stream.EmitRecord(bitc::MODULE_CODE_FUNCTION, Vals);
    Vals.clear();
  }

  // Emit the alias information.
  for (const GlobalAlias &A : M.aliases()) {
    // ALIAS: [strtab offset, strtab size, alias type, aliasee val#, linkage,
    //         visibility, dllstorageclass, threadlocal, unnamed_addr,
    //         DSO_Local]
    Vals.push_back(addToStrtab(A.getName()));
    Vals.push_back(A.getName().size());
    Vals.push_back(VE.getTypeID(A.getValueType()));
    Vals.push_back(A.getType()->getAddressSpace());
    Vals.push_back(VE.getValueID(A.getAliasee()));
    Vals.push_back(A.getLinkage());
    Vals.push_back(A.getVisibility());
    Vals.push_back(A.getDLLStorageClass());
    Vals.push_back(A.getThreadLocalMode());
    Vals.push_back(unsigned(A.getUnnamedAddr()));
    Vals.push_back(A.isDSOLocal());
    Vals.push_back(addToStrtab(A.getPartition()));
    Vals.push_back(A.getPartition().size());

    Stream.EmitRecord(bitc::MODULE_CODE_ALIAS, Vals);
    Vals.clear();
  }

  // Emit the ifunc information.
  for (const GlobalIFunc &I : M.ifuncs()) {
    // IFUNC: [strtab offset, strtab size, ifunc type, address space, resolver
    //         val#, linkage, visibility, DSO_Local]
    Vals.push_back(addToStrtab(I.getName()));
    Vals.push_back(I.getName().size());
    Vals.push_back(VE.getTypeID(I.getValueType()));
    Vals.push_back(I.getType()->getAddressSpace());
    Vals.push_back(VE.getValueID(I.getResolver()));
    Vals.push_back(I.getLinkage());
    Vals.push_back(I.getVisibility());
    Vals.push_back(I.isDSOLocal());
    Vals.push_back(addToStrtab(I.getPartition()));
    Vals.push_back(I.getPartition().size());
    Stream.EmitRecord(bitc::MODULE_CODE_IFUNC, Vals);
    Vals.clear();
  }
}

void KeyWriter::writeModuleMetadata() {
  Stream.EnterSubblock(bitc::METADATA_BLOCK_ID, 4);
  SmallVector<uint64_t, 64> Record;

  for (const NamedMDNode &NMD : M.named_metadata()) {
    // Write name.
    StringRef Str = NMD.getName();
    Record.append(Str.bytes_begin(), Str.bytes_end());
    Stream.EmitRecord(bitc::METADATA_NAME, Record);
    Record.clear();

    // Write named metadata operands.
    for (const MDNode *N : NMD.operands())
      Record.push_back(VE.getMetadataID(N));
    Stream.EmitRecord(bitc::METADATA_NAMED_NODE, Record, 0);
    Record.clear();
  }

  auto AddDeclAttachedMetadata = [&](const GlobalObject &GO) {
    SmallVector<uint64_t, 4> Record;
    Record.push_back(VE.getValueID(&GO));
    pushGlobalMetadataAttachment(Record, GO);
    Stream.EmitRecord(bitc::METADATA_GLOBAL_DECL_ATTACHMENT, Record);
  };
  for (const Function &F : M)
    if (F.isDeclaration() && F.hasMetadata())
      AddDeclAttachedMetadata(F);
  for (const GlobalIFunc &GI : M.ifuncs())
    if (GI.hasMetadata())
      AddDeclAttachedMetadata(GI);
  for (const GlobalVariable &GV : M.globals())
    if (GV.hasMetadata())
      AddDeclAttachedMetadata(GV);

  Stream.ExitBlock();
}

void KeyWriter::pushGlobalMetadataAttachment(
    SmallVectorImpl<uint64_t> &Record, const GlobalObject &GO) {
  // [n x [id, mdnode]]
  SmallVector<std::pair<unsigned, MDNode *>, 4> MDs;
  GO.getAllMetadata(MDs);
  for (const auto &I : MDs) {
    Record.push_back(I.first);
    Record.push_back(VE.getMetadataID(I.second));
  }
}

void KeyWriter::writeFunctionMetadataAttachment(const Function &F) {
  Stream.EnterSubblock(bitc::METADATA_ATTACHMENT_ID, 3);

  SmallVector<uint64_t, 64> Record;

  if (F.hasMetadata()) {
    pushGlobalMetadataAttachment(Record, F);
    Stream.EmitRecord(bitc::METADATA_ATTACHMENT, Record, 0);
    Record.clear();
  }

  // Write metadata attachments
  // METADATA_ATTACHMENT - [m x [value, [n x [id, mdnode]]]
  SmallVector<std::pair<unsigned, MDNode *>, 4> MDs;
  for (const BasicBlock &BB : F)
    for (const Instruction &I : BB) {
      MDs.clear();
      I.getAllMetadataOtherThanDebugLoc(MDs);

      // If no metadata, ignore instruction.
      if (MDs.empty()) continue;

      Record.push_back(VE.getInstructionID(&I));

      for (const auto &[ID, MD] : MDs) {
        Record.push_back(ID);
        Record.push_back(VE.getMetadataID(MD));
      }
      Stream.EmitRecord(bitc::METADATA_ATTACHMENT, Record, 0);
      Record.clear();
    }

  Stream.ExitBlock();
}

// The function's value symbol table, in value order.
void KeyWriter::writeFunctionNames(const Function &F) {
  Stream.EnterSubblock(bitc::VALUE_SYMTAB_BLOCK_ID, 4);
  SmallVector<uint64_t, 64> NameVals;
  auto Name = [&](unsigned Code, unsigned ID, const Value &V) {
    if (!V.hasName())
      return;
    NameVals.push_back(ID);
    for (const auto P : V.getName())
      NameVals.push_back((unsigned char)P);
    Stream.EmitRecord(Code, NameVals);
    NameVals.clear();
  };
  for (const Argument &A : F.args())
    Name(bitc::VST_CODE_ENTRY, Values.lookup(&A), A);
  for (const BasicBlock &BB : F) {
    Name(bitc::VST_CODE_BBENTRY, Blocks.lookup(&BB), BB);
    for (const Instruction &I : BB)
      Name(bitc::VST_CODE_ENTRY, Values.lookup(&I), I);
  }
  Stream.ExitBlock();
}

void KeyWriter::writeFunction(const Function &F) {
  Stream.EnterSubblock(bitc::FUNCTION_BLOCK_ID, 5);

  SmallVector<unsigned, 64> Vals;

  // The function, and the number of basic blocks.
  Vals.push_back(VE.getValueID(&F));
  Vals.push_back(F.size());
  Stream.EmitRecord(bitc::FUNC_CODE_DECLAREBLOCKS, Vals);
  Vals.clear();

  getGlobalBasicBlockID(&F.front());
  for (const Argument &A : F.args())
    number(&A);
  for (const BasicBlock &BB : F)
    for (const Instruction &I : BB)
      number(&I);

  bool NeedsMetadataAttachment = F.hasMetadata();

  DILocation *LastDL = nullptr;
  SmallSetVector<Function *, 4> BlockAddressUsers;

  // Finally, emit all the instructions, in order.
  for (const BasicBlock &BB : F) {
    for (const Instruction &I : BB) {
      unsigned InstID = Values.lookup(&I);
      writeInstruction(I, InstID, Vals);

      // If the instruction has metadata, write a metadata attachment later.
      NeedsMetadataAttachment |= I.hasMetadataOtherThanDebugLoc();

      // If the instruction has a debug location, emit it.
      if (DILocation *DL = I.getDebugLoc()) {
        if (DL == LastDL) {
          // Just repeat the same debug loc as last time.
          Stream.EmitRecord(bitc::FUNC_CODE_DEBUG_LOC_AGAIN, Vals);
        } else {
          Vals.push_back(DL->getLine());
          Vals.push_back(DL->getColumn());
          Vals.push_back(VE.getMetadataOrNullID(DL->getScope()));
          Vals.push_back(VE.getMetadataOrNullID(DL->getInlinedAt()));
          Vals.push_back(DL->isImplicitCode());
          Vals.push_back(DL->getAtomGroup());
          Vals.push_back(DL->getAtomRank());
          Stream.EmitRecord(bitc::FUNC_CODE_DEBUG_LOC, Vals,
                            FUNCTION_DEBUG_LOC_ABBREV);
          Vals.clear();
          LastDL = DL;
        }
      }

      // If the instruction has DbgRecords attached to it, emit them. Note that
      // they come after the instruction so that it's easy to attach them again
      // when reading the bitcode, even though conceptually the debug locations
      // start "before" the instruction.
      if (I.hasDbgRecords()) {
        /// Try to push the value only (unwrapped), otherwise push the
        /// metadata wrapped value. Returns true if the value was pushed
        /// without the ValueAsMetadata wrapper.
        auto PushValueOrMetadata = [&Vals, InstID,
                                    this](Metadata *RawLocation) {
          assert(RawLocation &&
                 "RawLocation unexpectedly null in DbgVariableRecord");
          if (ValueAsMetadata *VAM = dyn_cast<ValueAsMetadata>(RawLocation)) {
            SmallVector<unsigned, 2> ValAndType;
            // If the value is a fwd-ref the type is also pushed. We don't
            // want the type, so fwd-refs are kept wrapped (pushValueAndType
            // returns false if the value is pushed without type).
            if (!pushValueAndType(VAM->getValue(), InstID, ValAndType)) {
              Vals.push_back(ValAndType[0]);
              return true;
            }
          }
          // The metadata is a DIArgList, or ValueAsMetadata wrapping a
          // fwd-ref. Push the metadata ID.
          Vals.push_back(VE.getMetadataID(RawLocation));
          return false;
        };

        // Write out non-instruction debug information attached to this
        // instruction. Write it after the instruction so that it's easy to
        // re-attach to the instruction reading the records in.
        for (DbgRecord &DR : I.DebugMarker->getDbgRecordRange()) {
          if (DbgLabelRecord *DLR = dyn_cast<DbgLabelRecord>(&DR)) {
            Vals.push_back(VE.getMetadataID(&*DLR->getDebugLoc()));
            Vals.push_back(VE.getMetadataID(DLR->getLabel()));
            Stream.EmitRecord(bitc::FUNC_CODE_DEBUG_RECORD_LABEL, Vals);
            Vals.clear();
            continue;
          }

          // First 3 fields are common to all kinds:
          //   DILocation, DILocalVariable, DIExpression
          // dbg_value (FUNC_CODE_DEBUG_RECORD_VALUE)
          //   ..., LocationMetadata
          // dbg_value (FUNC_CODE_DEBUG_RECORD_VALUE_SIMPLE - abbrev'd)
          //   ..., Value
          // dbg_declare (FUNC_CODE_DEBUG_RECORD_DECLARE)
          //   ..., LocationMetadata
          // dbg_assign (FUNC_CODE_DEBUG_RECORD_ASSIGN)
          //   ..., LocationMetadata, DIAssignID, DIExpression, LocationMetadata
          DbgVariableRecord &DVR = cast<DbgVariableRecord>(DR);
          Vals.push_back(VE.getMetadataID(&*DVR.getDebugLoc()));
          Vals.push_back(VE.getMetadataID(DVR.getVariable()));
          Vals.push_back(VE.getMetadataID(DVR.getExpression()));
          if (DVR.isDbgValue()) {
            if (PushValueOrMetadata(DVR.getRawLocation()))
              Stream.EmitRecord(bitc::FUNC_CODE_DEBUG_RECORD_VALUE_SIMPLE, Vals,
                                FUNCTION_DEBUG_RECORD_VALUE_ABBREV);
            else
              Stream.EmitRecord(bitc::FUNC_CODE_DEBUG_RECORD_VALUE, Vals);
          } else if (DVR.isDbgDeclare()) {
            Vals.push_back(VE.getMetadataID(DVR.getRawLocation()));
            Stream.EmitRecord(bitc::FUNC_CODE_DEBUG_RECORD_DECLARE, Vals);
          } else if (DVR.isDbgDeclareValue()) {
            Vals.push_back(VE.getMetadataID(DVR.getRawLocation()));
            Stream.EmitRecord(bitc::FUNC_CODE_DEBUG_RECORD_DECLARE_VALUE, Vals);
          } else {
            assert(DVR.isDbgAssign() && "Unexpected DbgRecord kind");
            Vals.push_back(VE.getMetadataID(DVR.getRawLocation()));
            Vals.push_back(VE.getMetadataID(DVR.getAssignID()));
            Vals.push_back(VE.getMetadataID(DVR.getAddressExpression()));
            Vals.push_back(VE.getMetadataID(DVR.getRawAddress()));
            Stream.EmitRecord(bitc::FUNC_CODE_DEBUG_RECORD_ASSIGN, Vals);
          }
          Vals.clear();
        }
      }
    }

    if (BlockAddress *BA = BlockAddress::lookup(&BB)) {
      SmallVector<Value *> Worklist{BA};
      SmallPtrSet<Value *, 8> Visited{BA};
      while (!Worklist.empty()) {
        Value *V = Worklist.pop_back_val();
        for (User *U : V->users()) {
          if (auto *I = dyn_cast<Instruction>(U)) {
            Function *P = I->getFunction();
            if (P != &F)
              BlockAddressUsers.insert(P);
          } else if (isa<Constant>(U) && !isa<GlobalValue>(U) &&
                     Visited.insert(U).second)
            Worklist.push_back(U);
        }
      }
    }
  }

  if (!BlockAddressUsers.empty()) {
    Vals.resize(BlockAddressUsers.size());
    for (auto I : llvm::enumerate(BlockAddressUsers))
      Vals[I.index()] = VE.getValueID(I.value());
    Stream.EmitRecord(bitc::FUNC_CODE_BLOCKADDR_USERS, Vals);
    Vals.clear();
  }

  // Emit names for all the instructions etc.
  writeFunctionNames(F);

  if (NeedsMetadataAttachment)
    writeFunctionMetadataAttachment(F);
  Stream.ExitBlock();
}

// Each value's uses, in use-list order, among the users the module holds:
// the order the writer's shuffles restore. A value with fewer than two such
// uses has no order.
void KeyWriter::writeUseLists() {
  Stream.EnterSubblock(bitc::USELIST_BLOCK_ID, 3);
  SmallVector<uint64_t, 64> Record;
  // A value with fewer than two uses has no order to record.
  auto List = [&](const Value &V, unsigned Code) {
    if (!V.hasUseList() || !V.hasNUsesOrMore(2)) {
      Record.clear();
      return;
    }
    size_t Named = Record.size();
    for (const Use &U : V.uses()) {
      auto It = Values.find(U.getUser());
      if (It == Values.end())
        continue;
      Record.push_back(It->second);
      Record.push_back(U.getOperandNo());
    }
    if (Record.size() - Named >= 4)
      Stream.EmitRecord(Code, Record);
    Record.clear();
  };
  // `Numbered[ID]` is the value numbered `ID`.
  for (unsigned ID = 0, E = Numbered.size(); ID != E; ++ID) {
    Record.push_back(ID);
    List(*Numbered[ID], bitc::USELIST_CODE_DEFAULT);
  }
  for (const BasicBlock *BB : NumberedBlocks) {
    Record.push_back(Values.lookup(BB->getParent()));
    Record.push_back(Blocks.lookup(BB));
    List(*BB, bitc::USELIST_CODE_BB);
  }
  Stream.ExitBlock();
}

Digest KeyWriter::write() {
  writeContextTables();
  Stream.EnterSubblock(bitc::MODULE_BLOCK_ID, 3);
  for (const GlobalVariable &G : M.globals())
    number(&G);
  for (const Function &F : M)
    number(&F);
  for (const GlobalAlias &A : M.aliases())
    number(&A);
  for (const GlobalIFunc &I : M.ifuncs())
    number(&I);
  writeModuleInfo();
  writeModuleMetadata();
  for (const Function &F : M)
    if (!F.isDeclaration())
      writeFunction(F);
  writeUseLists();
  Stream.ExitBlock();
  return Stream.digest();
}

} // namespace

Digest module_key(const Module &M) {
  return std::make_unique<KeyWriter>(M)->write();
}

} // namespace hirsel_key
