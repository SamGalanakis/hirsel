//! `descriptor`: a function's static descriptor, with its spawn rows when
//! the function is made a plain `fn(take X) -> R` value.
use super::*;

/// `vN = descriptor fF, dD` then `, spawn tX, tR` when it has spawn rows.
pub(super) fn print(result: String, inst: &Inst) -> String {
    let Inst::DescriptorRef {
        function,
        identity,
        spawn,
        ..
    } = inst
    else {
        unreachable!("a descriptor")
    };
    let spawn = spawn.map_or_else(String::new, |s| format!(", spawn t{}, t{}", s.x.0, s.r.0));
    format!(
        "{result} = descriptor f{}, d{}{spawn}",
        function.0, identity.0
    )
}

/// [`print`]'s operands after `descriptor`.
pub(super) fn parse(l: &mut Line, result: ValueId) -> R<Inst> {
    let function = FuncId(l.id('f')?);
    l.expect(",")?;
    let identity = StaticId(l.id('d')?);
    let spawn = if l.eat(",") {
        l.expect("spawn")?;
        let x = TypeId(l.id('t')?);
        l.expect(",")?;
        let r = TypeId(l.id('t')?);
        Some(SpawnTypes { x, r })
    } else {
        None
    };
    Ok(Inst::DescriptorRef {
        result,
        function,
        identity,
        spawn,
    })
}
