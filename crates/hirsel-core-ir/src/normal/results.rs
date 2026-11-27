use super::*;

pub(super) fn results<'a>(inst: &'a Inst, body: &'a Body) -> impl Iterator<Item = ValueId> + 'a {
    let pair = |first, second| ([Some(first), second, None], &[][..]);
    let (fixed, variable): (_, &[ValueId]) = match *inst {
        Inst::Const { result, .. }
        | Inst::ReadStatic { result, .. }
        | Inst::FunctionRef { result, .. }
        | Inst::DescriptorRef { result, .. }
        | Inst::Unary { result, .. }
        | Inst::Binary { result, .. }
        | Inst::Compare { result, .. }
        | Inst::MulAdd { result, .. }
        | Inst::ScalarUnary { result, .. }
        | Inst::Read { result, .. }
        | Inst::UpdateMask { result, .. }
        | Inst::Pointer { result, .. }
        | Inst::Case { result, .. }
        | Inst::RegionOpen { result }
        | Inst::TryUnwrap { result, .. }
        | Inst::LayoutConst { result, .. }
        | Inst::CompactWidth { result, .. }
        | Inst::FrameActive { result, .. }
        | Inst::Simd { result, .. } => pair(result, None),
        Inst::IntArith { result, flag, .. } | Inst::Convert { result, flag, .. } => {
            pair(result, flag)
        }
        Inst::Grow {
            result, available, ..
        } => pair(result, Some(available)),
        Inst::FrameAdmit {
            admitted,
            available,
            ..
        }
        | Inst::LimitEnter {
            admitted,
            available,
            ..
        } => pair(admitted, Some(available)),
        Inst::Alloc {
            result,
            available,
            region_entry,
            ..
        } => ([Some(result), Some(available), region_entry], &[]),
        Inst::Call { call } => (
            [None; 3],
            body.calls[call.index()]
                .results
                .get(&body.pools.values)
                .unwrap(),
        ),
        Inst::Address { .. }
        | Inst::Store { .. }
        | Inst::StorageLive { .. }
        | Inst::StorageDead { .. }
        | Inst::Free { .. }
        | Inst::FrameRefund { .. }
        | Inst::Copy { .. }
        | Inst::Move { .. }
        | Inst::SetCase { .. }
        | Inst::RegionClose { .. }
        | Inst::Trim { .. }
        | Inst::BuiltinUse { .. }
        | Inst::LoopIteration
        | Inst::LimitExit
        | Inst::Drop { .. }
        | Inst::Share { .. }
        | Inst::Release { .. }
        | Inst::Mark { .. }
        | Inst::Unmark { .. }
        | Inst::Step { .. }
        | Inst::StackCheck { .. }
        | Inst::PromoteFrame { .. }
        | Inst::PutLanes { .. }
        | Inst::Prefetch { .. } => ([None; 3], &[]),
    };
    fixed.into_iter().flatten().chain(variable.iter().copied())
}

/// Fingerprint a closed ABI signature, independent of type row numbering.
pub fn abi_signature(module: &Module, signature: &Signature) -> ContentHash {
    let mut order = ModuleOrder::new(module);
    let mut out = Write::module(&mut order);
    out.abi = true;
    signature.write(&mut out);
    ContentHash::of_parts(SIGNATURE_DOMAIN, [&out.bytes[..]])
}
