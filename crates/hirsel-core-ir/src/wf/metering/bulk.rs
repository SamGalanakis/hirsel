//! A successful COPY charge is single-use. Follow unique normal predecessors
//! backwards from the operation. A join, failure transfer, or intervening
//! consumer loses the proof. A charge covers an operation when both name the
//! same SSA count: a saturating product covers the wrapping product the
//! native clear uses as its length, and the exact element count a repeat
//! writes; a wrapping product covers neither past a unit stride.
use super::*;
use hirsel_core_ids::work_steps::{Work, table};

#[derive(Clone, Copy, Eq, PartialEq)]
pub(super) enum Bytes {
    Fixed(u64),
    /// `count * stride` wrapped to u64, the bytes a single operation touches.
    Wrap(ValueId, u64),
    /// `count * stride` saturated to u64.
    Sat(ValueId, u64),
    /// The mathematical `count * stride` a counted repeat writes.
    Exact(ValueId, u64),
}

impl Bytes {
    fn count(self) -> Option<ValueId> {
        match self {
            Self::Fixed(_) => None,
            Self::Wrap(v, _) | Self::Sat(v, _) | Self::Exact(v, _) => Some(v),
        }
    }

    /// Whether a charge of `self` pays for `need`. A saturated charge is at
    /// least the exact product and the wrapped one; one stride makes all
    /// three the count itself.
    pub(super) fn covers(self, need: Self) -> bool {
        match (self, need) {
            (Self::Fixed(a), Self::Fixed(b)) => a == b,
            (
                Self::Wrap(v, k) | Self::Sat(v, k) | Self::Exact(v, k),
                Self::Wrap(w, j) | Self::Sat(w, j) | Self::Exact(w, j),
            ) if v == w && k == j => {
                k == 1
                    || self == need
                    || matches!(self, Self::Sat(..)) && !matches!(need, Self::Sat(..))
            }
            _ => false,
        }
    }
}

#[derive(Clone, Copy, Default)]
struct Incoming {
    count: usize,
    predecessor: Option<usize>,
}

impl Incoming {
    fn add(&mut self, from: Option<usize>) {
        self.count = self.count.saturating_add(1);
        self.predecessor = if self.count == 1 { from } else { None };
    }
}

pub(super) struct Facts<'a> {
    definitions: Vec<Option<&'a Inst>>,
    incoming: Vec<Incoming>,
}

impl<'a> Facts<'a> {
    pub(super) fn new(b: &'a Body, budget: &Debit<'_>) -> Result<Self, Error> {
        let mut definitions = Vec::new();
        budget.reserve(&mut definitions, b.values.len())?;
        definitions.resize(b.values.len(), None);
        let mut incoming = Vec::new();
        budget.reserve(&mut incoming, b.blocks.len())?;
        incoming.resize(b.blocks.len(), Incoming::default());
        incoming[0].add(None);
        for (bi, block) in b.blocks.iter().enumerate() {
            budget.check()?;
            for row in block.insts.get(&b.insts).unwrap_or_default() {
                budget.check()?;
                if let Some(f) = row.fail(b) {
                    incoming[f.index()].add(None);
                }
                match *row {
                    Inst::Const { result, .. }
                    | Inst::IntArith { result, .. }
                    | Inst::Read { result, .. }
                    | Inst::LayoutConst { result, .. } => definitions[result.index()] = Some(row),
                    _ => {}
                }
            }
            let mut edge = |edge: EdgeId| {
                incoming[b.edges[edge.index()].target.index()].add(Some(bi));
            };
            match b.terminators[bi] {
                Terminator::Jump(e) => edge(e),
                Terminator::Branch { yes, no, .. } => {
                    edge(yes);
                    edge(no);
                }
                Terminator::Switch {
                    arms, otherwise, ..
                } => {
                    for arm in arms.get(&b.pools.arms).unwrap_or_default() {
                        budget.check()?;
                        edge(arm.edge);
                    }
                    edge(otherwise);
                }
                Terminator::Raise {
                    cleanup: Some(to), ..
                } => incoming[to.index()].add(None),
                _ => {}
            }
        }
        Ok(Self {
            definitions,
            incoming,
        })
    }

    pub(super) fn incoming(&self, block: usize) -> usize {
        self.incoming[block].count
    }

    pub(super) fn definition(&self, v: ValueId) -> Option<&'a Inst> {
        self.definitions[v.index()]
    }

    pub(super) fn constant(&self, v: ValueId) -> Option<u64> {
        match self.definitions[v.index()] {
            Some(Inst::Const { bits, .. }) => Some(*bits),
            _ => None,
        }
    }

    pub(super) fn bytes(&self, v: ValueId) -> Bytes {
        if let Some(n) = self.constant(v) {
            return Bytes::Fixed(n);
        }
        if let Some(Inst::IntArith {
            op: ArithOp::Mul,
            mode: mode @ (ArithMode::Wrapping | ArithMode::Saturating),
            left,
            right,
            ..
        }) = self.definitions[v.index()]
            && let Some(stride) = self.constant(*right)
        {
            let saturating = *mode == ArithMode::Saturating;
            return match (self.constant(*left), saturating) {
                (Some(n), false) => Bytes::Fixed(n.wrapping_mul(stride)),
                (Some(n), true) => Bytes::Fixed(n.saturating_mul(stride)),
                (None, false) => Bytes::Wrap(*left, stride),
                (None, true) => Bytes::Sat(*left, stride),
            };
        }
        Bytes::Wrap(v, 1)
    }

    pub(super) fn positive_work(&self, b: &Body, call: &CallSite) -> bool {
        call.callee == Callee::Runtime(RtOp::WorkSteps)
            && matches!(call.args.get(&b.pools.args), Some([Arg::Value(n), _, Arg::Value(weight)]) if self.constant(*n).is_some_and(|n| n>0) || self.constant(*weight).is_some_and(|w| w>0))
    }

    pub(super) fn prepaid(
        &self,
        b: &Body,
        mut bi: usize,
        mut before: usize,
        needed: Bytes,
        after: Option<ValueId>,
        work: Work,
        budget: &Debit<'_>,
    ) -> Result<bool, Error> {
        // `after`, when given, must be defined between the charge and the use.
        let mut defined = after.is_none();
        // The length's defining activation must survive from the charge to use.
        // A repeated block on a unique-predecessor chain has no other entry.
        for _ in 0..=b.blocks.len() {
            budget.check()?;
            let block = &b.blocks[bi];
            let rows = block.insts.get(&b.insts).unwrap_or_default();
            for row in rows[..before].iter().rev() {
                budget.check()?;
                if let Inst::Call { call } = row {
                    let call = &b.calls[call.index()];
                    let Some([Arg::Value(units), Arg::Value(shift), Arg::Value(weight)]) =
                        call.args.get(&b.pools.args)
                    else {
                        return Ok(false);
                    };
                    return Ok(call.callee == Callee::Runtime(RtOp::WorkSteps)
                        && defined
                        && self.bytes(*units).covers(needed)
                        && self.constant(*shift) == Some(u64::from(work.shift))
                        && self.constant(*weight) == Some(work.weight));
                }
                if !matches!(
                    row,
                    Inst::Const { .. }
                        | Inst::Binary { .. }
                        | Inst::IntArith { .. }
                        | Inst::Compare { .. }
                        | Inst::Pointer { .. }
                        | Inst::Address { .. }
                        | Inst::Read { .. }
                        | Inst::LayoutConst { .. }
                ) {
                    return Ok(false);
                }
                defined |= matches!(row, Inst::IntArith { result, .. } if Some(*result) == after);
                if let Some(n) = needed.count() {
                    if matches!(row,
                        Inst::Const{result,..} | Inst::Binary{result,..} | Inst::IntArith{result,..}
                        | Inst::Compare{result,..} | Inst::Read{result,..} | Inst::Pointer{result,..}
                        | Inst::LayoutConst{result,..} if *result == n)
                        || matches!(row, Inst::IntArith{flag:Some(v),..} if *v==n)
                    {
                        return Ok(false);
                    }
                }
            }
            if let Some(n) = needed.count() {
                for param in block.params.get(&b.pools.values).unwrap_or_default() {
                    budget.check()?;
                    if *param == n {
                        return Ok(false);
                    }
                }
            }
            let Some(pred) = self.incoming[bi].predecessor else {
                return Ok(false);
            };
            bi = pred;
            before = b.blocks[pred].insts.len as usize;
        }
        Ok(false)
    }

    pub(super) fn check_operations(
        &self,
        b: &Body,
        checks: Checks,
        paid: &[bool],
        budget: &Debit<'_>,
    ) -> Result<(), Error> {
        for (bi, block) in b.blocks.iter().enumerate() {
            budget.check()?;
            for (k, row) in block
                .insts
                .get(&b.insts)
                .unwrap_or_default()
                .iter()
                .enumerate()
            {
                budget.check()?;
                let (needed, work) = match *row {
                    Inst::Call { call } => {
                        let Some(requirement) =
                            self.runtime_charge(b, &b.calls[call.index()], checks)?
                        else {
                            continue;
                        };
                        requirement
                    }
                    // A paid repeat's charge covers the element its latch copies.
                    Inst::Copy { .. } if paid[bi] => continue,
                    Inst::Copy { bytes, .. } | Inst::Move { bytes, .. } if checks.copies => {
                        (self.bytes(bytes), table::COPY)
                    }
                    Inst::Alloc { size, .. } | Inst::Grow { size, .. } if checks.allocations => {
                        (self.bytes(size), table::COPY)
                    }
                    _ => continue,
                };
                if let Inst::Grow { moved, .. } = row
                    && self.constant(*moved) != Some(0)
                {
                    return Err(Error::Invalid(format!(
                        "b{bi}: growth copy has no separate metering proof"
                    )));
                }
                if needed == Bytes::Fixed(0) {
                    continue;
                }
                if !self.prepaid(b, bi, k, needed, None, work, budget)? {
                    return Err(Error::Invalid(format!(
                        "b{bi}: bulk operation lacks matching MEM-30 precharge"
                    )));
                }
            }
        }
        Ok(())
    }

    fn runtime_charge(
        &self,
        body: &Body,
        call: &CallSite,
        checks: Checks,
    ) -> Result<Option<(Bytes, Work)>, Error> {
        let Callee::Runtime(op) = call.callee else {
            return Ok(None);
        };
        let enabled = match op {
            RtOp::ZeroBytes => checks.zero_bytes,
            RtOp::CompareBytes => checks.comparisons,
            RtOp::Xxh3Short => checks.short_hashes,
            _ => checks.kernels,
        };
        if !enabled {
            return Ok(None);
        }
        let args = call.args.get(&body.pools.args).unwrap_or_default();
        if op == RtOp::CompareBytes {
            let [_, Arg::Value(left), _, Arg::Value(right)] = args else {
                return Err(Error::Invalid("invalid compare_bytes lengths".into()));
            };
            let needed = match (self.constant(*left), self.constant(*right)) {
                (Some(0), _) => Some(self.bytes(*right)),
                (_, Some(0)) => Some(self.bytes(*left)),
                (Some(a), Some(b)) => a.checked_add(b).map(Bytes::Fixed),
                _ => None,
            }
            .ok_or_else(|| Error::Invalid("unproved MEM-30 comparison length".into()))?;
            return Ok(Some((needed, table::COPY)));
        }
        let (work, stride) = match op {
            RtOp::ZeroBytes => (table::COPY, 1),
            RtOp::Crc32c => (table::CRC32C, 1),
            RtOp::Crc64Nvme => (table::CRC64, 1),
            RtOp::Sha1Blocks => (table::SHA1, 1),
            RtOp::Sha1Compress => (table::SHA1, 64),
            RtOp::Sha256Blocks => (table::SHA256, 64),
            RtOp::Sha512Blocks => (table::SHA512, 128),
            RtOp::Xxh3Stripes => (table::XXH3, 64),
            RtOp::Xxh3Short => {
                if matches!(args.last(), Some(Arg::Value(len)) if self.constant(*len).is_some_and(|n| n <= 240))
                {
                    return Ok(None);
                }
                return Err(Error::Invalid("unproved short hash length".into()));
            }
            _ => return Ok(None),
        };
        let Some(Arg::Value(n)) = args.last() else {
            return Err(Error::Invalid("missing bulk operation length".into()));
        };
        let needed = if stride == 1 {
            self.bytes(*n)
        } else {
            // A wrapping byte product cannot cover a mathematical block count.
            let bytes = self
                .constant(*n)
                .and_then(|n| n.checked_mul(stride))
                .ok_or_else(|| Error::Invalid("unproved block kernel byte count".into()))?;
            Bytes::Fixed(bytes)
        };
        Ok(Some((needed, work)))
    }
}
