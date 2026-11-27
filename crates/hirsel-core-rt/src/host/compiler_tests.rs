use super::*;
use crate::provider::{Buffer, STEP_PENDING};

#[test]
fn compiler_issue_parks_and_abort_releases_the_call() {
    let binding = Binding::new("/absent-hirsel-worker".into());
    let mut call = Call::default();
    let mut row = Request {
        operation: crate::provider::CHECK_LIBRARY,
        mode: 0,
        handle: binding.handle(),
        site: 0,
        path: (&raw mut call).cast(),
        path_len: 0,
        buffer: Buffer {
            data: std::ptr::null_mut(),
            len: 0,
            capacity: 0,
        },
    };
    // The provider borrows this call only through delivery or abort.
    unsafe {
        assert_eq!(
            crate::os::issue(std::ptr::null_mut(), &mut row, 7).kind,
            STEP_PENDING
        );
        abort(&mut row);
    }
    assert!(call.pending.is_none());
    assert!(call.reply.is_empty());
}
