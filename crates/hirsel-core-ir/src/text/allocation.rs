use super::*;

pub(super) fn parse(l: &mut Line<'_>, defs: &[ValueId]) -> R<Inst> {
    let (result, available, region_entry) = match defs {
        [a, b] => (*a, *b, None),
        [a, b, c] => (*a, *b, Some(*c)),
        _ => return Err("alloc takes pointer, room and optionally region entry".into()),
    };
    let kind = match l.next()? {
        "for" => AllocKind::own(l.field_ty()?),
        "env" => AllocKind::Environment { ty: l.tref()? },
        "region" => AllocKind::Region,
        "resource" => {
            l.expect("grant")?;
            AllocKind::Resource { grant: l.num()? }
        }
        k => return Err(format!("unknown allocation kind `{k}`")),
    };
    let size = l.value()?;
    l.expect("align")?;
    let align = l.num()?;
    l.expect("charge")?;
    let charge = l.value()?;
    let place = if l.eat("like") {
        AllocPlace::Like(l.value()?)
    } else if l.eat("in") {
        AllocPlace::Region(l.value()?)
    } else if l.eat("lent_frame") {
        AllocPlace::LentFrame(l.value()?)
    } else if l.eat("file_buffer") {
        AllocPlace::FileBuffer
    } else if l.eat("file_partner") {
        AllocPlace::FilePartner(l.value()?)
    } else if l.eat("byte_head") {
        AllocPlace::ByteHead
    } else if l.eat("byte_tail") {
        AllocPlace::ByteTail(l.value()?)
    } else {
        AllocPlace::Current
    };
    let overflow = if l.eat("overflow") {
        Some(l.value()?)
    } else {
        None
    };
    Ok(Inst::Alloc {
        result,
        available,
        region_entry,
        kind,
        place,
        charge,
        overflow,
        size,
        align,
    })
}

pub(super) fn format_place(place: AllocPlace) -> String {
    match place {
        AllocPlace::Current => String::new(),
        AllocPlace::Like(b) => format!(" like v{}", b.0),
        AllocPlace::Region(r) => format!(" in v{}", r.0),
        AllocPlace::LentFrame(r) => format!(" lent_frame v{}", r.0),
        AllocPlace::FileBuffer => " file_buffer".into(),
        AllocPlace::FilePartner(r) => format!(" file_partner v{}", r.0),
        AllocPlace::ByteHead => " byte_head".into(),
        AllocPlace::ByteTail(r) => format!(" byte_tail v{}", r.0),
    }
}
