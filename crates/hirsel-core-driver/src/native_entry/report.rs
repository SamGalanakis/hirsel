//! Preserve diagnostic-only report I/O and its existing process status.
use super::*;
use cranelift_codegen::ir::{InstBuilder, condcodes::IntCC};

pub(super) fn emit(f: &mut Code<'_, '_>, path: &[u8]) -> Result<()> {
    let path = f.string(path)?;
    let mode = f.string(b"w")?;
    let file = f.value_call("fopen", I64, &[path, mode])?;
    let (opened, stopped) = f.if_nonzero(file);
    f.b.switch_to_block(stopped);
    failed(f, None)?;
    f.b.switch_to_block(opened);
    let len = f.stack(8, 3);
    let zero = f.constant(0);
    f.store(zero, len, 0);
    let data = f.value_call("hirsel_core_memory_report", I64, &[len])?;
    let len = f.load(I64, len, 0);
    let one = f.constant(1);
    let written = f.value_call("fwrite", I64, &[data, one, len, file])?;
    let mismatch = f.b.ins().icmp(IntCC::NotEqual, written, len);
    let (stopped, written) = f.if_nonzero(mismatch);
    f.b.switch_to_block(stopped);
    failed(f, Some(file))?;
    f.b.switch_to_block(written);
    let closed = f.value_call("fclose", I32, &[file])?;
    let (stopped, done) = f.if_nonzero(closed);
    f.b.switch_to_block(stopped);
    failed(f, None)?;
    f.b.switch_to_block(done);
    Ok(())
}
fn failed(f: &mut Code<'_, '_>, file: Option<ir::Value>) -> Result<()> {
    let label = f.string(b"logical charge report")?;
    f.void_call("perror", &[label])?;
    if let Some(file) = file {
        f.value_call("fclose", I32, &[file])?;
    }
    f.return_i32(2);
    Ok(())
}
