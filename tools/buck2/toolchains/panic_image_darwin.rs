use crate::backtrace_rs::{self, PrintFmt};
use crate::{fmt, slice};

// Darwin's trace-only output: the PCs, the executable's LC_UUID and its slide,
// as panic_image.rs prints the ELF build ID and load bias on Linux.
pub(super) unsafe fn write(out: &mut fmt::Formatter<'_>, format: PrintFmt) -> fmt::Result {
    out.write_str("stack backtrace:\n")?;
    write_image(out)?;
    let short = format == PrintFmt::Short;
    let mut index = 0;
    let mut result = Ok(());
    // SAFETY: std's BacktraceLock serializes this trace just as it does the
    // symbolizing formatter.
    unsafe {
        backtrace_rs::trace_unsynchronized(|frame| {
            if short && index > super::MAX_NB_FRAMES {
                return false;
            }
            const HEX_WIDTH: usize = 2 + 2 * size_of::<usize>();
            let pc = frame.ip();
            result = writeln!(out, "{index:4}: {pc:HEX_WIDTH$?}");
            index += 1;
            result.is_ok()
        });
    }
    result?;
    if short {
        out.write_str(
            "note: Some details are omitted, \
             run with `RUST_BACKTRACE=full` for a verbose backtrace.\n",
        )?;
    }
    Ok(())
}

unsafe extern "C" {
    // libSystem's dyld image calls (tools/native-toolchain/darwin/libSystem.symbols).
    // Image 0 is the main executable.
    fn _dyld_get_image_header(image_index: u32) -> *const u8;
    fn _dyld_get_image_vmaddr_slide(image_index: u32) -> isize;
}

const MH_MAGIC_64: u32 = 0xfeed_facf;
const MACH_HEADER_64: usize = 32;
const LC_UUID: u32 = 0x1b;

fn write_image(out: &mut fmt::Formatter<'_>) -> fmt::Result {
    // SAFETY: dyld keeps the main executable's header and slide for process life.
    let (header, slide) = unsafe { (_dyld_get_image_header(0), _dyld_get_image_vmaddr_slide(0)) };
    let slide = slide as usize;
    // SAFETY: a non-null header is the mapped executable's mach_header_64,
    // followed by its sizeofcmds bytes of load commands.
    let uuid = if header.is_null() { None } else { unsafe { uuid(header) } };
    let Some(uuid) = uuid else {
        return writeln!(out, "executable load bias: {slide:#x}");
    };
    out.write_str("executable uuid: ")?;
    for byte in uuid {
        write!(out, "{byte:02x}")?;
    }
    writeln!(out, " load bias: {slide:#x}")
}

unsafe fn uuid(header: *const u8) -> Option<&'static [u8]> {
    // SAFETY: see write_image; Mach-O fields have no alignment contract here.
    let fixed = unsafe { slice::from_raw_parts(header, MACH_HEADER_64) };
    let word = |bytes: &[u8], at: usize| Some(u32::from_ne_bytes(bytes.get(at..at + 4)?.try_into().ok()?));
    if word(fixed, 0)? != MH_MAGIC_64 {
        return None;
    }
    let size = word(fixed, 20)? as usize;
    // SAFETY: the load commands follow the header inside the mapped image.
    let mut commands = unsafe { slice::from_raw_parts(header.add(MACH_HEADER_64), size) };
    while commands.len() >= 8 {
        let (command, length) = (word(commands, 0)?, word(commands, 4)? as usize);
        if length < 8 || length > commands.len() {
            return None;
        }
        if command == LC_UUID {
            return commands.get(8..24);
        }
        commands = &commands[length..];
    }
    None
}
