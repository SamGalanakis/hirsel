use super::*;

/// What an instruction defines and uses, as names: value `v` is `v`, address `a` is
/// `values + a`. An `address` instruction defines its address and uses its base and
/// step operands.
pub fn operands(b: &Body, i: &Inst) -> (Vec<usize>, Vec<usize>) {
    let n = b.values.len();
    let v = |v: ValueId| v.index();
    let a = |a: AddressId| n + a.index();
    let mut uses = vec![];
    let mut defs = vec![];
    match *i {
        Inst::Const { result, .. }
        | Inst::LayoutConst { result, .. }
        | Inst::ReadStatic { result, .. }
        | Inst::FunctionRef { result, .. }
        | Inst::DescriptorRef { result, .. }
        | Inst::RegionOpen { result } => defs.push(v(result)),
        Inst::Simd {
            result,
            first,
            second,
            ..
        } => {
            uses.extend([first].into_iter().chain(second).map(v));
            defs.push(v(result));
        }
        Inst::PutLanes { to, vector, count } => uses.extend([a(to), v(vector), v(count)]),
        Inst::Unary {
            result, operand, ..
        }
        | Inst::CompactWidth {
            result,
            word: operand,
            ..
        }
        | Inst::ScalarUnary {
            result, operand, ..
        } => {
            uses.push(v(operand));
            defs.push(v(result));
        }
        Inst::Binary {
            result,
            left,
            right,
            ..
        }
        | Inst::Compare {
            result,
            left,
            right,
            ..
        } => {
            uses.extend([v(left), v(right)]);
            defs.push(v(result));
        }
        Inst::Convert {
            result,
            flag,
            operand,
            ..
        } => {
            uses.push(v(operand));
            defs.extend([result].into_iter().chain(flag).map(v));
        }
        Inst::IntArith {
            result,
            flag,
            left,
            right,
            ..
        } => {
            uses.extend([v(left), v(right)]);
            defs.extend([result].into_iter().chain(flag).map(v));
        }
        Inst::MulAdd {
            result,
            left,
            right,
            addend,
        } => {
            uses.extend([v(left), v(right), v(addend)]);
            defs.push(v(result));
        }
        Inst::Address { address, .. } => {
            if let Some(info) = b.addresses.get(address.index()) {
                match info.base {
                    AddressBase::Slot(_) => {}
                    AddressBase::Value(x) => uses.push(v(x)),
                    AddressBase::Address(x) => uses.push(a(x)),
                }
                for s in info.steps.get(&b.pools.steps).unwrap_or_default() {
                    if let Step::Index(x) | Step::Element(x) | Step::Byte(x) = *s {
                        uses.push(v(x));
                    }
                }
            }
            defs.push(a(address));
        }
        Inst::Read {
            result, address, ..
        }
        | Inst::Pointer { result, address }
        | Inst::Case { result, address } => {
            uses.push(a(address));
            defs.push(v(result));
        }
        Inst::Store { address, value } => uses.extend([a(address), v(value)]),
        Inst::StorageLive { .. }
        | Inst::StorageDead { .. }
        | Inst::BuiltinUse { .. }
        | Inst::LoopIteration
        | Inst::LimitExit
        | Inst::Step { .. }
        | Inst::StackCheck { .. } => {}
        Inst::UpdateMask { result, mask, .. } => {
            uses.push(v(mask));
            defs.push(v(result));
        }
        Inst::Call { call } => {
            if let Some(site) = b.calls.get(call.index()) {
                if let Callee::Indirect(x) = site.callee {
                    uses.push(v(x));
                }
                for arg in site.args.get(&b.pools.args).unwrap_or_default() {
                    uses.push(match *arg {
                        Arg::Value(x) => v(x),
                        Arg::Address(x) => a(x),
                    });
                }
                defs.extend(
                    site.results
                        .get(&b.pools.values)
                        .unwrap_or_default()
                        .iter()
                        .map(|&x| v(x)),
                );
            }
        }
        Inst::Alloc {
            result,
            available,
            region_entry,
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
                uses.push(v(x));
            }
            uses.extend([charge, size].into_iter().chain(overflow).map(v));
            defs.extend([result, available].into_iter().chain(region_entry).map(v));
        }
        Inst::Free { block, charge } => uses.extend([v(block), v(charge)]),
        Inst::FrameAdmit {
            storage,
            admitted,
            available,
            charge,
            overflow,
            ..
        } => {
            uses.extend([a(storage), v(charge)]);
            uses.extend(overflow.map(v));
            defs.extend([v(admitted), v(available)]);
        }
        Inst::FrameActive { storage, result } => {
            uses.push(a(storage));
            defs.push(v(result));
        }
        Inst::FrameRefund { storage } => uses.push(a(storage)),
        Inst::Prefetch { data, offset } => uses.extend([v(data), v(offset)]),
        Inst::PromoteFrame { carrier } => uses.push(a(carrier)),
        Inst::Copy { to, from, bytes } | Inst::Move { to, from, bytes } => {
            uses.extend([a(to), a(from), v(bytes)])
        }
        Inst::Grow {
            result,
            available,
            carrier,
            charge,
            overflow,
            size,
            capacity,
            moved,
            refund,
            ..
        } => {
            uses.push(a(carrier));
            uses.extend(
                [charge, size, capacity, moved, refund]
                    .into_iter()
                    .chain(overflow)
                    .map(v),
            );
            defs.extend([v(result), v(available)]);
        }
        Inst::SetCase { address, .. } | Inst::Drop { address, .. } => uses.push(a(address)),
        Inst::RegionClose { region } => uses.push(v(region)),
        Inst::Trim {
            block,
            region,
            entry,
            previous,
            charge,
        } => uses.extend([block, region, entry, previous, charge].map(v)),
        Inst::LimitEnter {
            admitted,
            available,
            bytes,
        } => {
            uses.push(v(bytes));
            defs.extend([v(admitted), v(available)]);
        }
        Inst::Share { node, .. } | Inst::Release { node, .. } => uses.push(v(node)),
        Inst::TryUnwrap { result, node, .. } => {
            uses.push(v(node));
            defs.push(v(result));
        }
        Inst::Mark { address, count, .. } | Inst::Unmark { address, count, .. } => {
            uses.extend([a(address), v(count)])
        }
    }
    (uses, defs)
}

/// The names block `bi`'s terminator uses: its condition or scrutinee, its edges'
/// arguments, its results, a `raise fail` cause's address.
pub fn terminator_uses(b: &Body, bi: usize) -> Vec<usize> {
    let vals = |r: PoolRange<ValueId>| -> Vec<usize> {
        r.get(&b.pools.values)
            .unwrap_or_default()
            .iter()
            .map(|v| v.index())
            .collect()
    };
    let edge_args = |e: &EdgeId| vals(b.edges[e.index()].args);
    match &b.terminators[bi] {
        Terminator::Jump(e) => edge_args(e),
        Terminator::Branch { condition, yes, no } => [condition.index()]
            .into_iter()
            .chain(edge_args(yes))
            .chain(edge_args(no))
            .collect(),
        Terminator::Switch {
            value,
            arms,
            otherwise,
        } => [value.index()]
            .into_iter()
            .chain(
                arms.get(&b.pools.arms)
                    .unwrap_or_default()
                    .iter()
                    .flat_map(|arm| edge_args(&arm.edge)),
            )
            .chain(edge_args(otherwise))
            .collect(),
        Terminator::Raise { cause, .. } => match cause {
            RaiseCause::Fail(a) => vec![b.values.len() + a.index()],
            RaiseCause::Capacity {
                requested,
                available,
            } => vec![requested.index(), available.index()],
            _ => vec![],
        },
        Terminator::Return(r) => vals(*r),
        Terminator::Propagate | Terminator::Unreachable => vec![],
    }
}
