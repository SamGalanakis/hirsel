use super::*;
use crate::schedule_script::{FlowStep, PausePoint, ScheduleScript, StepBoundary};
use crate::tasks::{self, SchedulerProvider, hirsel_core_check_cancel, run_root};
use std::{
    ptr,
    sync::{Arc, Condvar, Mutex},
};

pub(crate) struct FlowFixture {
    context: *mut Context,
    flow: *mut u8,
}

impl FlowFixture {
    /// The owner pins and exclusively uses this context until the flow closes.
    pub(crate) unsafe fn new(context: *mut Context) -> Self {
        let mut result = [0; 4];
        assert_eq!(
            unsafe {
                hirsel_core_flow(
                    context,
                    ptr::null_mut(),
                    Op::New as u8,
                    ptr::null(),
                    &mut result,
                    0,
                )
            },
            0
        );
        Self {
            context,
            flow: ptr::with_exposed_provenance_mut(result[1] as usize),
        }
    }

    fn call(&self, op: Op, args: &[u64]) -> [u64; 4] {
        let mut result = [0; 4];
        assert_eq!(
            unsafe {
                hirsel_core_flow(
                    self.context,
                    self.flow,
                    op as u8,
                    args.as_ptr(),
                    &mut result,
                    0,
                )
            },
            0
        );
        result
    }

    pub(crate) fn sink(&self) -> u64 {
        let [0, sink, ..] = self.call(Op::Sink, &[]) else {
            panic!("sink")
        };
        sink
    }

    pub(crate) fn stage(&self, destination: u64, transform: Transform) -> u64 {
        let [0, stage, ..] = self.call(
            Op::Stage,
            &[
                destination,
                4,
                0,
                1000,
                0,
                0,
                0,
                8,
                8,
                0,
                transform as usize as u64,
            ],
        ) else {
            panic!("stage")
        };
        stage
    }

    pub(crate) fn port(&self, destination: u64) -> u64 {
        let [0, port, ..] = self.call(Op::Port, &[destination, 4, 8, 8]) else {
            panic!("port")
        };
        port
    }

    pub(crate) fn push(&self, port: u64, key: u64, value: u64) {
        assert_eq!(
            self.call(
                Op::Push,
                &[port, key, (&raw const value).expose_provenance() as u64, 8]
            ),
            [0; 4]
        );
    }

    pub(crate) fn advance(&self, port: u64, key: u64) {
        assert_eq!(self.call(Op::Advance, &[port, key]), [0; 4]);
    }

    pub(crate) fn end(&self, port: u64) {
        assert_eq!(self.call(Op::End, &[port]), [0; 4]);
    }

    pub(crate) fn next(&self, sink: u64) -> Pull<u64> {
        let mut value = 0u64;
        let [tag, key, from, _] = self.call(
            Op::Next,
            &[sink, (&raw mut value).expose_provenance() as u64],
        );
        match tag {
            0 => Pull::Item(key, from as usize, value),
            1 => Pull::Dropped(from as usize, key),
            2 => Pull::End,
            3 => Pull::Stalled,
            _ => panic!("next: {tag}"),
        }
    }

    pub(crate) fn finish(mut self) -> Result<(), (u64, u64, u64)> {
        let [tag, stage, key, halt] = self.call(Op::Finish, &[]);
        self.flow = ptr::null_mut();
        match tag {
            0 => Ok(()),
            1 => Err((stage, key, halt)),
            _ => panic!("finish: {tag}"),
        }
    }
}

impl Drop for FlowFixture {
    fn drop(&mut self) {
        if !self.flow.is_null() {
            unsafe { hirsel_core_flow_drop(self.flow) };
        }
    }
}

pub(crate) unsafe extern "C" fn add_one(
    _: *mut Context,
    _: *const u8,
    _: *mut u8,
    _: u64,
    input: *const u8,
    output: *mut u8,
    present: *mut u8,
    _: u64,
) -> u8 {
    unsafe {
        output
            .cast::<u64>()
            .write(input.cast::<u64>().read_unaligned() + 1);
        present.write(1);
    }
    0
}

unsafe extern "C" fn check_cancel_until_out_of_time(
    context: *mut Context,
    _: *const u8,
    _: *mut u8,
    _: u64,
    _: *const u8,
    _: *mut u8,
    _: *mut u8,
    _: u64,
) -> u8 {
    loop {
        unsafe {
            (*context).steps -= 1;
            if (*context).steps <= 0 && crate::steps::hirsel_core_step_out(context, 0) != 0 {
                return 1;
            }
            assert_eq!(hirsel_core_check_cancel(context, 0), 0);
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Law {
    Handoff,
    Mark,
    Terminal,
    CancelFinish,
    CancelDrop,
}

#[test]
fn owner_cuts_keep_the_flow_laws_and_refund_on_cancel() {
    unsafe extern "C" fn owner(context: *mut Context, _: *mut u8, input: *mut u8) -> u8 {
        // SAFETY: the root copies a single Law and owns its flow context.
        let law = unsafe { input.cast::<Law>().read() };
        let start = unsafe { (*context).available() };
        let flow = unsafe { FlowFixture::new(context) };
        let sink = flow.sink();
        match law {
            Law::Handoff => {
                let s0 = flow.stage(sink, add_one);
                let s1 = flow.stage(s0, add_one);
                let port = flow.port(s1);
                flow.push(port, 1, 40);
                assert_eq!(flow.next(sink), Pull::Item(1, 0, 42));
                flow.end(port);
                assert_eq!(flow.next(sink), Pull::End);
                assert_eq!(flow.finish(), Ok(()));
            }
            Law::Mark | Law::Terminal => {
                let stage = flow.stage(sink, add_one);
                let a = flow.port(stage);
                let b = flow.port(sink);
                flow.push(b, 3, 42);
                if law == Law::Mark {
                    flow.advance(a, 10);
                } else {
                    flow.end(a);
                    flow.end(b);
                }
                assert_eq!(flow.next(sink), Pull::Item(3, 1, 42));
                if law == Law::Terminal {
                    assert_eq!(flow.next(sink), Pull::End);
                }
                assert_eq!(flow.finish(), Ok(()));
            }
            Law::CancelFinish | Law::CancelDrop => {
                let stage = flow.stage(sink, check_cancel_until_out_of_time);
                let port = flow.port(stage);
                flow.push(port, 1, 1);
                tasks::request_flow_fixture_cancel();
                if law == Law::CancelFinish {
                    assert_eq!(flow.next(sink), Pull::Dropped(0, 1));
                    assert_eq!(flow.finish(), Err((0, 1, Halt::OutOfTime as u64)));
                } else {
                    drop(flow);
                }
                assert_eq!(unsafe { (*context).available() }, start);
                assert_eq!(unsafe { hirsel_core_check_cancel(context, 2) }, 1);
                assert_eq!(
                    unsafe { (*context).causes.iter().next().unwrap().kind() },
                    causes::CANCELLED
                );
                return 0;
            }
        }
        assert_eq!(unsafe { (*context).available() }, start);
        0
    }

    for law in [
        Law::Handoff,
        Law::Mark,
        Law::Terminal,
        Law::CancelFinish,
        Law::CancelDrop,
    ] {
        let mut context = Context::new(1 << 20);
        causes::task_start(&mut context).unwrap();
        let mut law = law;
        Arc::new(ScheduleScript::new(0)).install(|| unsafe {
            assert_eq!(
                run_root(
                    &mut context,
                    owner,
                    (&raw mut law).cast(),
                    ptr::null_mut(),
                    size_of::<Law>(),
                    0,
                    SchedulerProvider::default(),
                ),
                0
            );
        });
    }
}
