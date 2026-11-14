#ifndef HIRSEL_NUMERIC_NAN_H
#define HIRSEL_NUMERIC_NAN_H

#include <llvm-c/Core.h>
#include <math.h>
#include <stdlib.h>
#include <stdint.h>
#include <string.h>

static inline int hirsel_has_fast_math(LLVMValueRef value) {
  return LLVMCanValueUseFastMathFlags(value) && LLVMGetFastMathFlags(value);
}

static inline size_t hirsel_nan_hash(LLVMValueRef value, size_t capacity) {
  uintptr_t hash = (uintptr_t)value >> 4;
  hash ^= hash >> 17;
  hash *= (uintptr_t)0x9e3779b97f4a7c15ULL;
  return (size_t)hash & (capacity - 1);
}

// Return x only for the exact canonical map: isnan(x) ? positive qNaN : x.
static inline LLVMValueRef hirsel_canonical_nan_source(LLVMValueRef select,
                                                      LLVMContextRef context) {
  if (!LLVMIsASelectInst(select) || hirsel_has_fast_math(select))
    return NULL;
  LLVMTypeKind kind = LLVMGetTypeKind(LLVMTypeOf(select));
  if (kind != LLVMFloatTypeKind && kind != LLVMDoubleTypeKind)
    return NULL;
  LLVMValueRef canonical = LLVMGetOperand(select, 1);
  if (!LLVMIsAConstantFP(canonical))
    return NULL;
  LLVMTypeRef integer = kind == LLVMFloatTypeKind
                            ? LLVMInt32TypeInContext(context)
                            : LLVMInt64TypeInContext(context);
  unsigned long long bits =
      LLVMConstIntGetZExtValue(LLVMConstBitCast(canonical, integer));
  if (bits != (kind == LLVMFloatTypeKind ? 0x7fc00000ULL
                                        : 0x7ff8000000000000ULL))
    return NULL;
  LLVMValueRef condition = LLVMGetOperand(select, 0);
  if (!LLVMIsAFCmpInst(condition) || hirsel_has_fast_math(condition) ||
      LLVMGetFCmpPredicate(condition) != LLVMRealUNO)
    return NULL;
  LLVMValueRef source = LLVMGetOperand(select, 2);
  LLVMValueRef left = LLVMGetOperand(condition, 0);
  LLVMValueRef right = LLVMGetOperand(condition, 1);
  LLVMValueRef other = left == source ? right : right == source ? left : NULL;
  if (!other)
    return NULL;
  if (other != source) {
    LLVMBool loses_info;
    if (!LLVMIsAConstantFP(other) ||
        isnan(LLVMConstRealGetDouble(other, &loses_info)))
      return NULL;
  }
  return source;
}

// This is a closed use walk, recomputed for each root. A visited cycle does not
// imply another root is safe: every exit of this root's reachable graph is
// inspected. Stores, bits, packing, calls and returns are observer boundaries.
static inline int hirsel_nan_uses_are_numerical(LLVMValueRef root,
                                               LLVMContextRef context) {
  LLVMValueRef local[32] = {root};
  LLVMValueRef *values = local;
  LLVMValueRef local_seen[64] = {0};
  LLVMValueRef *seen = local_seen;
  seen[hirsel_nan_hash(root, 64)] = root;
  size_t len = 1, capacity = 32;
  int safe = 1;
  for (size_t i = 0; safe && i < len; ++i) {
    for (LLVMUseRef use = LLVMGetFirstUse(values[i]); use;
         use = LLVMGetNextUse(use)) {
      LLVMValueRef user = LLVMGetUser(use);
      if (!LLVMIsAInstruction(user) || hirsel_has_fast_math(user)) {
        safe = 0;
        break;
      }
      if (LLVMIsAFCmpInst(user) ||
          hirsel_canonical_nan_source(user, context) == values[i])
        continue;
      switch (LLVMGetInstructionOpcode(user)) {
      case LLVMFAdd:
      case LLVMFSub:
      case LLVMFMul:
      case LLVMFDiv:
      case LLVMFRem:
      case LLVMFNeg:
      case LLVMFPExt:
      case LLVMFPTrunc:
      case LLVMPHI:
      case LLVMSelect:
        break;
      default:
        safe = 0;
        break;
      }
      LLVMTypeKind kind = LLVMGetTypeKind(LLVMTypeOf(user));
      if (!safe || (kind != LLVMFloatTypeKind && kind != LLVMDoubleTypeKind)) {
        safe = 0;
        break;
      }
      size_t slot = hirsel_nan_hash(user, capacity * 2);
      while (seen[slot] && seen[slot] != user)
        slot = (slot + 1) & (capacity * 2 - 1);
      if (seen[slot])
        continue;
      if (len == capacity) {
        if (capacity > SIZE_MAX / (4 * sizeof(*seen)))
          abort();
        size_t next = capacity * 2;
        LLVMValueRef *grown = (LLVMValueRef *)malloc(next * sizeof(*grown));
        LLVMValueRef *new_seen = (LLVMValueRef *)calloc(next * 2, sizeof(*seen));
        if (!grown || !new_seen)
          abort();
        memcpy(grown, values, len * sizeof(*grown));
        for (size_t j = 0; j < len; ++j) {
          size_t at = hirsel_nan_hash(values[j], next * 2);
          while (new_seen[at])
            at = (at + 1) & (next * 2 - 1);
          new_seen[at] = values[j];
        }
        if (values != local)
          free(values);
        if (seen != local_seen)
          free(seen);
        values = grown;
        seen = new_seen;
        capacity = next;
        slot = hirsel_nan_hash(user, capacity * 2);
        while (seen[slot])
          slot = (slot + 1) & (capacity * 2 - 1);
      }
      seen[slot] = user;
      values[len++] = user;
    }
  }
  if (values != local)
    free(values);
  if (seen != local_seen)
    free(seen);
  return safe;
}

static inline void hirsel_defer_numerical_nan_maps(LLVMModuleRef module,
                                                  LLVMContextRef context) {
  for (LLVMValueRef f = LLVMGetFirstFunction(module); f;
       f = LLVMGetNextFunction(f)) {
    for (LLVMBasicBlockRef block = LLVMGetFirstBasicBlock(f); block;
         block = LLVMGetNextBasicBlock(block)) {
      for (LLVMValueRef inst = LLVMGetFirstInstruction(block); inst;) {
        LLVMValueRef next = LLVMGetNextInstruction(inst);
        LLVMValueRef source = hirsel_canonical_nan_source(inst, context);
        if (source && hirsel_nan_uses_are_numerical(inst, context)) {
          LLVMValueRef condition = LLVMGetOperand(inst, 0);
          LLVMReplaceAllUsesWith(inst, source);
          LLVMInstructionEraseFromParent(inst);
          if (!LLVMGetFirstUse(condition))
            LLVMInstructionEraseFromParent(condition);
        }
        inst = next;
      }
    }
  }
}

#endif
