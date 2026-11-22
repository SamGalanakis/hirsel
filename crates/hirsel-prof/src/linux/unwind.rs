use std::sync::Arc;

use gimli::{
    BaseAddresses, CfaRule, CieOrFde, DebugFrame, EhFrame, EndianArcSlice, FrameDescriptionEntry,
    LittleEndian, Register, RegisterRule, UnwindContext, UnwindSection,
};

type Reader = EndianArcSlice<LittleEndian>;

#[derive(Default)]
pub(crate) struct UnwindInfo {
    eh: Option<Indexed<EhFrame<Reader>>>,
    debug: Option<Indexed<DebugFrame<Reader>>>,
    malformed: bool,
}

struct Indexed<S> {
    section: S,
    bases: BaseAddresses,
    fdes: Vec<FrameDescriptionEntry<Reader>>,
}

impl<S: UnwindSection<Reader>> Indexed<S> {
    fn new(section: S, bases: BaseAddresses) -> gimli::Result<Self> {
        let mut entries = section.entries(&bases);
        let mut fdes = Vec::new();
        while let Some(entry) = entries.next()? {
            if let CieOrFde::Fde(partial) = entry {
                let fde = partial.parse(S::cie_from_offset)?;
                if fde.len() != 0 {
                    fdes.push(fde);
                }
            }
        }
        fdes.sort_by_key(FrameDescriptionEntry::initial_address);
        Ok(Self {
            section,
            bases,
            fdes,
        })
    }

    fn step(
        &self,
        pc: u64,
        context: &mut UnwindContext<usize>,
        registers: &Registers,
        stack: &Stack<'_>,
    ) -> Option<Step> {
        let count = self
            .fdes
            .partition_point(|entry| entry.initial_address() <= pc);
        let entry = self.fdes[..count]
            .iter()
            .rev()
            .find(|fde| fde.contains(pc))?;
        let row = match entry.unwind_info_for_address(&self.section, &self.bases, context, pc) {
            Ok(row) => row,
            Err(_) => return Some(Step::EvalError),
        };
        if row.registers().any(|(_, rule)| {
            matches!(
                rule,
                RegisterRule::Expression(_) | RegisterRule::ValExpression(_)
            )
        }) {
            return Some(Step::EvalError);
        }
        let cfa = match *row.cfa() {
            CfaRule::RegisterAndOffset { register, offset } => registers
                .get(register)
                .and_then(|value| value.checked_add_signed(offset)),
            _ => None,
        };
        let Some(cfa) = cfa else {
            return Some(Step::EvalError);
        };

        let ra_register = entry.cie().return_address_register();
        let ra_rule = row.register(ra_register).unwrap_or(DEFAULT_RA);
        let bp_rule = row.register(FP).unwrap_or(RegisterRule::SameValue);
        if matches!(ra_rule, RegisterRule::Undefined) {
            return Some(Step::Root);
        }
        let ip = evaluate(ra_register, ra_rule, registers, stack, cfa);
        let bp = evaluate(FP, bp_rule, registers, stack, cfa);
        Some(match (ip, bp) {
            (Ok(Some(pc)), Ok(fp)) if pc != 0 => {
                let next = Registers {
                    pc,
                    sp: cfa,
                    fp,
                    link: Some(pc),
                };
                if cfa < registers.sp || (cfa == registers.sp && pc == registers.pc) {
                    Step::EvalError
                } else {
                    Step::Caller(next)
                }
            }
            (Ok(_), _) => Step::EvalError,
            (Err(Step::Truncated), _) | (_, Err(Step::Truncated)) => Step::Truncated,
            _ => Step::EvalError,
        })
    }
}

impl UnwindInfo {
    pub(crate) fn new(eh: Option<(u64, &[u8])>, debug: Option<&[u8]>, text: u64, got: u64) -> Self {
        let bases = BaseAddresses::default().set_text(text).set_got(got);
        let reader = |bytes: &[u8]| EndianArcSlice::new(Arc::from(bytes), LittleEndian);
        let eh = eh.map(|(address, bytes)| {
            Indexed::new(
                EhFrame::from(reader(bytes)),
                bases.clone().set_eh_frame(address),
            )
        });
        let debug = debug.map(|bytes| Indexed::new(DebugFrame::from(reader(bytes)), bases));
        let malformed =
            eh.as_ref().is_some_and(Result::is_err) || debug.as_ref().is_some_and(Result::is_err);
        Self {
            eh: eh.and_then(Result::ok),
            debug: debug.and_then(Result::ok),
            malformed,
        }
    }

    pub(crate) fn step(
        &self,
        pc: u64,
        context: &mut UnwindContext<usize>,
        registers: &Registers,
        stack: &Stack<'_>,
    ) -> Step {
        let step = self
            .eh
            .as_ref()
            .and_then(|eh| eh.step(pc, context, registers, stack))
            .or_else(|| {
                self.debug
                    .as_ref()
                    .and_then(|debug| debug.step(pc, context, registers, stack))
            });
        match step {
            Some(step) => step,
            None if self.malformed => Step::EvalError,
            None => {
                frame_pointer(registers, stack).map_or(Step::MissingCfi, Step::FramePointerFallback)
            }
        }
    }
}

#[derive(Clone, Copy)]
pub(crate) struct Registers {
    pub(crate) pc: u64,
    pub(crate) sp: u64,
    fp: Option<u64>,
    link: Option<u64>,
}

#[cfg(target_arch = "x86_64")]
const FP: Register = gimli::X86_64::RBP;
#[cfg(target_arch = "x86_64")]
const SP: Register = gimli::X86_64::RSP;
#[cfg(target_arch = "x86_64")]
const RA: Register = gimli::X86_64::RA;
#[cfg(target_arch = "x86_64")]
const DEFAULT_RA: RegisterRule<usize> = RegisterRule::Undefined;

#[cfg(target_arch = "aarch64")]
const FP: Register = gimli::AArch64::X29;
#[cfg(target_arch = "aarch64")]
const SP: Register = gimli::AArch64::SP;
#[cfg(target_arch = "aarch64")]
const RA: Register = gimli::AArch64::X30;
#[cfg(target_arch = "aarch64")]
const DEFAULT_RA: RegisterRule<usize> = RegisterRule::SameValue;

impl Registers {
    pub(crate) fn new(pc: u64, sp: u64, fp: u64, link: u64) -> Self {
        Self {
            pc,
            sp,
            fp: Some(fp),
            link: Some(link),
        }
    }

    fn get(&self, register: Register) -> Option<u64> {
        match register {
            SP => Some(self.sp),
            FP => self.fp,
            RA => self.link,
            _ => None,
        }
    }
}

pub(crate) struct Stack<'a> {
    pub(crate) base: u64,
    pub(crate) bytes: &'a [u8],
}

impl Stack<'_> {
    fn word(&self, addr: u64) -> Option<u64> {
        let offset = usize::try_from(addr.checked_sub(self.base)?).ok()?;
        let (word, _) = self.bytes.get(offset..)?.split_first_chunk::<8>()?;
        Some(u64::from_le_bytes(*word))
    }
}

pub(crate) enum Step {
    Caller(Registers),
    Root,
    Truncated,
    MissingCfi,
    FramePointerFallback(Registers),
    EvalError,
}

fn evaluate(
    target: Register,
    rule: RegisterRule<usize>,
    registers: &Registers,
    stack: &Stack<'_>,
    cfa: u64,
) -> Result<Option<u64>, Step> {
    let value = match rule {
        RegisterRule::Undefined => return Ok(None),
        RegisterRule::SameValue => return Ok(registers.get(target)),
        RegisterRule::Register(source) => registers.get(source),
        RegisterRule::ValOffset(offset) => cfa.checked_add_signed(offset),
        RegisterRule::Offset(offset) => {
            let address = cfa.checked_add_signed(offset).ok_or(Step::EvalError)?;
            return stack.word(address).map(Some).ok_or(Step::Truncated);
        }
        _ => return Err(Step::EvalError),
    };
    value.map(Some).ok_or(Step::EvalError)
}

fn frame_pointer(registers: &Registers, stack: &Stack<'_>) -> Option<Registers> {
    let fp = registers.fp?;
    if fp < registers.sp || fp == 0 || !fp.is_multiple_of(8) {
        return None;
    }
    let previous = stack.word(fp)?;
    let pc = stack.word(fp.checked_add(8)?)?;
    if (previous != 0 && previous <= fp) || pc == 0 {
        return None;
    }
    Some(Registers {
        pc,
        sp: fp.checked_add(16)?,
        fp: Some(previous),
        link: Some(pc),
    })
}

#[cfg(test)]
mod tests {
    use gimli::write::{
        Address, CallFrameInstruction, CommonInformationEntry, EhFrame as OutputFrame, EndianVec,
        Expression, FrameDescriptionEntry, FrameTable,
    };

    use super::*;

    #[test]
    fn cfi_failures_cannot_become_successful_frame_pointer_unwinds() {
        let mut code = Expression::new();
        code.op_constu(0x3000);
        let mut table = FrameTable::default();
        let mut cie = CommonInformationEntry::new(
            gimli::Encoding {
                format: gimli::Format::Dwarf32,
                version: 1,
                address_size: 8,
            },
            1,
            -8,
            RA,
        );
        cie.add_instruction(CallFrameInstruction::CfaExpression(code));
        cie.add_instruction(CallFrameInstruction::Offset(RA, -8));
        let cie = table.add_cie(cie);
        let fde = FrameDescriptionEntry::new(Address::Constant(0x1000), 16);
        table.add_fde(cie, fde);
        #[cfg(target_arch = "x86_64")]
        let (scratch, first, last, offset) = (gimli::X86_64::R11, 0xa, 0x25, 61_448);
        #[cfg(target_arch = "aarch64")]
        let (scratch, first, last, offset) = (gimli::AArch64::X9, 0x8, 0x18, 65_552);
        let mut probe = FrameDescriptionEntry::new(Address::Constant(0x3000), 64);
        probe.add_instruction(0, CallFrameInstruction::Cfa(SP, 8));
        probe.add_instruction(first, CallFrameInstruction::Cfa(scratch, offset));
        probe.add_instruction(last, CallFrameInstruction::Cfa(SP, 8));
        table.add_fde(cie, probe);
        let mut root = FrameDescriptionEntry::new(Address::Constant(0x4000), 8);
        root.add_instruction(0, CallFrameInstruction::Cfa(SP, 0));
        root.add_instruction(0, CallFrameInstruction::Undefined(RA));
        table.add_fde(cie, root);
        let mut section = OutputFrame::from(EndianVec::new(LittleEndian));
        table.write_eh_frame(&mut section).unwrap();
        let info = UnwindInfo::new(Some((0, section.0.slice())), None, 0, 0);
        let mut context = UnwindContext::new();
        let rs = Registers::new(0x1000, 0x2000, 0x2000, 0x1000);
        let stack_bytes = [0u64.to_le_bytes(), 0x1010u64.to_le_bytes()].concat();
        let stack = Stack {
            base: 0x2000,
            bytes: &stack_bytes,
        };
        assert!(matches!(
            info.step(0x1000, &mut context, &rs, &stack),
            Step::EvalError
        ));
        let Step::FramePointerFallback(caller) = info.step(0x1010, &mut context, &rs, &stack)
        else {
            panic!("frame pointer recovery must retain its own outcome");
        };
        assert_eq!((caller.pc, caller.sp), (0x1010, 0x2010));
        for pc in [0x3000 + first as u64, 0x3000 + last as u64 - 1] {
            assert!(matches!(
                info.step(pc, &mut context, &rs, &stack),
                Step::EvalError
            ));
        }
        assert!(matches!(
            info.step(0x4000, &mut context, &rs, &stack),
            Step::Root
        ));
        let empty = Stack {
            base: 0x2000,
            bytes: &[],
        };
        assert!(matches!(
            info.step(0x1010, &mut context, &rs, &empty),
            Step::MissingCfi
        ));
    }
}
