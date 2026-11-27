use super::*;

/// The uses of `inst` mapped through `f`; results, addresses and blocks kept.
pub(super) fn map_uses(inst: &Inst, f: &mut impl FnMut(ValueId) -> ValueId) -> Inst {
    let mut i = inst.clone();
    match &mut i {
        Inst::Const { .. }
        | Inst::ReadStatic { .. }
        | Inst::FunctionRef { .. }
        | Inst::DescriptorRef { .. }
        | Inst::RegionOpen { .. }
        | Inst::LayoutConst { .. }
        | Inst::Address { .. }
        | Inst::Read { .. }
        | Inst::Pointer { .. }
        | Inst::Case { .. }
        | Inst::SetCase { .. }
        | Inst::Drop { .. }
        | Inst::StorageLive { .. }
        | Inst::StorageDead { .. }
        | Inst::BuiltinUse { .. }
        | Inst::LoopIteration
        | Inst::LimitExit
        | Inst::Step { .. }
        | Inst::StackCheck { .. }
        | Inst::PromoteFrame { .. }
        | Inst::Call { .. } => {}
        Inst::Simd { first, second, .. } => {
            *first = f(*first);
            if let Some(s) = second {
                *s = f(*s);
            }
        }
        Inst::PutLanes { vector, count, .. } => {
            *vector = f(*vector);
            *count = f(*count);
        }
        Inst::Unary { operand, .. }
        | Inst::CompactWidth { word: operand, .. }
        | Inst::ScalarUnary { operand, .. }
        | Inst::Convert { operand, .. } => *operand = f(*operand),
        Inst::Binary { left, right, .. }
        | Inst::Compare { left, right, .. }
        | Inst::IntArith { left, right, .. } => {
            *left = f(*left);
            *right = f(*right);
        }
        Inst::MulAdd {
            left,
            right,
            addend,
            ..
        } => {
            *left = f(*left);
            *right = f(*right);
            *addend = f(*addend);
        }
        Inst::Store { value, .. } => *value = f(*value),
        Inst::UpdateMask { mask, .. } => *mask = f(*mask),
        Inst::Alloc {
            place,
            charge,
            overflow,
            size,
            ..
        } => {
            if let AllocPlace::Like(x)
            | AllocPlace::Region(x)
            | AllocPlace::LentFrame(x)
            | AllocPlace::ByteTail(x)
            | AllocPlace::FilePartner(x) = place
            {
                *x = f(*x);
            }
            *charge = f(*charge);
            if let Some(o) = overflow {
                *o = f(*o);
            }
            *size = f(*size);
        }
        Inst::Free { block, charge } => {
            *block = f(*block);
            *charge = f(*charge);
        }
        Inst::FrameAdmit {
            charge, overflow, ..
        } => {
            *charge = f(*charge);
            if let Some(v) = overflow {
                *v = f(*v);
            }
        }
        Inst::FrameActive { .. } | Inst::FrameRefund { .. } => {}
        Inst::Copy { bytes, .. } | Inst::Move { bytes, .. } => *bytes = f(*bytes),
        Inst::Prefetch { data, offset } => {
            *data = f(*data);
            *offset = f(*offset);
        }
        Inst::Grow {
            charge,
            overflow,
            size,
            capacity,
            moved,
            refund,
            ..
        } => {
            for v in [charge, size, capacity, moved, refund] {
                *v = f(*v);
            }
            if let Some(o) = overflow {
                *o = f(*o);
            }
        }
        Inst::RegionClose { region } => *region = f(*region),
        Inst::Trim {
            block,
            region,
            entry,
            previous,
            charge,
        } => {
            for v in [block, region, entry, previous, charge] {
                *v = f(*v);
            }
        }
        Inst::LimitEnter { bytes, .. } => *bytes = f(*bytes),
        Inst::Share { node, .. } | Inst::Release { node, .. } | Inst::TryUnwrap { node, .. } => {
            *node = f(*node)
        }
        Inst::Mark { count, .. } | Inst::Unmark { count, .. } => *count = f(*count),
    }
    i
}
