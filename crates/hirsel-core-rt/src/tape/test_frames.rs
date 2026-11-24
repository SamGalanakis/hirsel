use super::*;

/// The code revision the tests open tapes as.
pub(crate) const REVISION: [u8; 32] = {
    let mut b = [0; 32];
    let mut i = 0;
    while i < 32 {
        b[i] = 0x20 + i as u8;
        i += 1;
    }
    b
};

pub(super) const HAND: &[u8] = include_bytes!("testdata/hand-tape.bin");
pub(super) const MODEL: &[u8] = include_bytes!("testdata/model-f4-tape.bin");
pub(super) const XXH3: &[u8] = include_bytes!("testdata/xxh3.bin");

pub(super) fn tape_frames(mut bytes: &[u8]) -> Vec<Frame> {
    let (mut blocks, _) = Blocks::open(|buf| bytes.read(buf)).unwrap();
    let mut decoder = Decoder::default();
    let mut frames = Vec::new();
    while let Some((flags, payload)) = blocks.next(|buf| bytes.read(buf)).unwrap() {
        frames.extend(decoder.block(payload, flags & format::END != 0).unwrap());
    }
    frames
}

/// Runs `body` as the root task of a scheduler run; Record under the
/// recording schedule script, as the S2 cases do.
pub(super) fn in_task(record: bool, body: &mut dyn FnMut(*mut crate::Context)) {
    type Body<'a> = &'a mut dyn FnMut(*mut crate::Context);
    unsafe extern "C" fn root(context: *mut crate::Context, _: *mut u8, input: *mut u8) -> u8 {
        // SAFETY: `in_task` pins the body until the run returns.
        unsafe { (**input.cast::<*mut Body<'_>>())(context) };
        0
    }
    let mut context = crate::Context::new(1 << 20);
    let mut files = crate::host_file::Files::default();
    context.host_files = &mut files;
    crate::causes::task_start(&mut context).unwrap();
    let mut body: Body<'_> = body;
    let mut input: *mut Body<'_> = &mut body;
    // SAFETY: the body and its borrows are pinned for the cooperative run.
    let mut execute = || unsafe {
        crate::tasks::run_root(
            &mut context,
            root,
            (&mut input as *mut *mut Body<'_>).cast(),
            ptr::null_mut(),
            size_of::<*mut Body<'_>>(),
            0,
            crate::tasks::SchedulerProvider::default(),
        )
    };
    let status = if record {
        std::sync::Arc::new(crate::schedule_script::ScheduleScript::last()).install(execute)
    } else {
        execute()
    };
    assert_eq!(status, 0);
}
