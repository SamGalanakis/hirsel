//! The canonical object stream, shared by artifacts and generated launchers.
use cranelift_object::{ObjectProduct, object};
use object::read::elf::{FileHeader, Rela, SectionHeader};
use object::{LittleEndian, SectionIndex, elf};

pub(crate) fn finish(product: ObjectProduct) -> Result<Vec<u8>, String> {
    let mut bytes = product.emit().map_err(|e| e.to_string())?;
    relax_got_loads(&mut bytes).map_err(|e| e.to_string())?;
    Ok(bytes)
}

fn relax_got_loads(bytes: &mut [u8]) -> object::read::Result<()> {
    // Only x86-64 ELF has relaxable GOT loads; a Mach-O object is final.
    if object::FileKind::parse(&*bytes)? != object::FileKind::Elf64 {
        return Ok(());
    }
    let endian = LittleEndian;
    let file = object::read::elf::ElfFile64::<LittleEndian>::parse(&*bytes)?;
    if file.elf_header().e_machine(endian) != elf::EM_X86_64 {
        return Ok(());
    }
    let sections = file.elf_section_table();
    let mut patches = Vec::new();
    for section in sections.iter() {
        let Some((relocations, _)) = section.rela(endian, &*bytes)? else {
            continue;
        };
        let code = sections.section(SectionIndex(section.sh_info(endian) as usize))?;
        if !code.sh_flags(endian).contains(elf::SHF_EXECINSTR) {
            continue;
        }
        let data = code.data(endian, &*bytes)?;
        for (index, relocation) in relocations.iter().enumerate() {
            if relocation.r_type(endian, false) != elf::R_X86_64_GOTPCREL
                || relocation.r_addend(endian) != -4
            {
                continue;
            }
            let offset = relocation.r_offset(endian) as usize;
            let Some(kind) = relaxable_instruction(data, offset) else {
                continue;
            };
            // ELF64 r_info's low word is the type; retain the symbol, addend,
            // section order and every code byte. ObjectFile reads these bytes
            // only after normalization, so all consumers see the same object.
            let info =
                section.sh_offset(endian) as usize + index * std::mem::size_of_val(relocation) + 8;
            patches.push((info, kind));
        }
    }
    for (offset, kind) in patches {
        bytes[offset..offset + 4].copy_from_slice(&kind.0.to_le_bytes());
    }
    Ok(())
}

fn relaxable_instruction(code: &[u8], displacement: usize) -> Option<elf::RelocationType> {
    // Only the pinned backend's RIP-relative MOV loads and indirect CALL/JMP
    // forms have the psABI relaxation contract. Other GOT references stay plain.
    if displacement.checked_add(4)? > code.len() || displacement < 2 {
        return None;
    }
    let (opcode, modrm) = (code[displacement - 2], code[displacement - 1]);
    if modrm & 0xc7 != 0x05 {
        return None;
    }
    if opcode == 0x8b && displacement >= 3 && matches!(code[displacement - 3], 0x48 | 0x4c) {
        Some(elf::R_X86_64_REX_GOTPCRELX)
    } else if opcode == 0xff && matches!(modrm, 0x15 | 0x25) {
        Some(elf::R_X86_64_GOTPCRELX)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_validated_got_load_call_and_jump_encodings_are_relaxable() {
        for rex in [0x48, 0x4c] {
            for reg in 0..8 {
                assert_eq!(
                    relaxable_instruction(&[rex, 0x8b, 5 | reg << 3, 0, 0, 0, 0], 3),
                    Some(elf::R_X86_64_REX_GOTPCRELX)
                );
            }
        }
        for opcode in [0x15, 0x25] {
            assert_eq!(
                relaxable_instruction(&[0xff, opcode, 0, 0, 0, 0], 2),
                Some(elf::R_X86_64_GOTPCRELX)
            );
        }
        for code in [
            &[0x48, 0x89, 5, 0, 0, 0, 0][..],    // store
            &[0x48, 0x8b, 4, 0, 0, 0, 0][..],    // SIB
            &[0x48, 0x8b, 0xc5, 0, 0, 0, 0][..], // register
            &[0x48, 0x8b, 5, 0, 0, 0][..],       // truncated displacement
        ] {
            assert_eq!(relaxable_instruction(code, 3), None);
        }
    }
}
