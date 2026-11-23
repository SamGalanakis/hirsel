//! Native entry for the erased owner graph. The compiler supplies each plain
//! value's layout and adapts the stage's callable to the fixed transform ABI.
use super::*;
use crate::{Context, causes, tasks::Storage};
use hirsel_core_layout::flow::Op;

#[cfg(test)]
#[path = "fixtures.rs"]
pub(crate) mod fixtures;

type Transform = unsafe extern "C" fn(
    *mut Context,
    *const u8,
    *mut u8,
    u64,
    *const u8,
    *mut u8,
    *mut u8,
    u64,
) -> u8;

struct Plain {
    bytes: usize,
    at: *mut u8,
    _storage: Option<Storage>,
}

impl Plain {
    fn new(bytes: u64) -> Self {
        let bytes = host_size(bytes);
        let storage = Storage::new(bytes, 8);
        Self {
            bytes,
            at: storage.as_ptr(),
            _storage: Some(storage),
        }
    }

    fn span(at: *mut u8, bytes: usize) -> Self {
        Self {
            bytes,
            at,
            _storage: None,
        }
    }

    fn address(&mut self) -> *mut u8 {
        self.at
    }

    unsafe fn copy(bytes: u64, from: *const u8) -> Self {
        let value = Self::new(bytes);
        if value.bytes != 0 {
            // SAFETY: the compiler supplies a live plain operand of this layout.
            unsafe { from.copy_to_nonoverlapping(value.at, value.bytes) };
        }
        value
    }

    unsafe fn write(&self, to: *mut u8) {
        if self.bytes != 0 {
            // SAFETY: the compiler supplies fresh destination storage of this layout.
            unsafe { self.at.copy_to_nonoverlapping(to, self.bytes) };
        }
    }
}

fn host_size(size: u64) -> usize {
    usize::try_from(size).unwrap_or_else(|_| crate::process::stop(b"HostMemory"))
}

struct Ring {
    storage: Storage,
    cap: usize,
    bytes: usize,
    stride: usize,
    head: usize,
    len: usize,
}

impl Ring {
    fn slot(&self, offset: usize) -> *mut u8 {
        let index = (self.head + offset) % self.cap;
        // SAFETY: the ring owns all its initialized and vacant slots.
        unsafe { self.storage.as_ptr().add(index * self.stride) }
    }

    fn payload(&self, slot: *mut u8) -> Plain {
        // SAFETY: two key words precede the payload in the chosen slot.
        Plain::span(unsafe { slot.add(16) }, self.bytes)
    }
}

impl Fifo<Plain> for Ring {
    fn new(cap: usize, size: usize) -> Self {
        let stride = size
            .checked_add(16)
            .unwrap_or_else(|| crate::process::stop(b"HostMemory"));
        let bytes = cap
            .checked_mul(stride)
            .unwrap_or_else(|| crate::process::stop(b"HostMemory"));
        Self {
            storage: Storage::new(bytes, 8),
            cap,
            bytes: size,
            stride,
            head: 0,
            len: 0,
        }
    }

    fn len(&self) -> usize {
        self.len
    }

    fn first(&self) -> Option<u64> {
        // SAFETY: a nonempty ring's head has an initialized key word.
        (self.len != 0).then(|| unsafe { self.slot(0).cast::<u64>().read_unaligned() })
    }

    fn value<R>(&self, read: impl FnOnce(&Plain) -> R) -> R {
        read(&self.payload(self.slot(0)))
    }

    fn push(&mut self, key: u64, value: Plain) {
        let slot = self.slot(self.len);
        // SAFETY: the interpreter supplied a free slot and a live plain value
        // with this port's compiler-selected payload layout.
        unsafe {
            slot.cast::<u64>().write_unaligned(key);
            slot.add(8).cast::<u64>().write_unaligned(key);
            value.write(slot.add(16));
        }
        self.len += 1;
    }

    fn pop(&mut self) -> Slot<Plain> {
        let slot = self.slot(0);
        // SAFETY: the merge just selected this initialized head. Its data
        // stays readable until the owner copies it out or the stage retires it.
        let (key, following) = unsafe {
            (
                slot.cast::<u64>().read_unaligned(),
                slot.add(8).cast::<u64>().read_unaligned(),
            )
        };
        self.head = (self.head + 1) % self.cap;
        self.len -= 1;
        Slot {
            key,
            following,
            value: self.payload(slot),
        }
    }

    fn advance(&mut self, mark: u64) -> bool {
        if self.len == 0 {
            return false;
        }
        // SAFETY: this is the mark word of the newest live item.
        unsafe {
            self.slot(self.len - 1)
                .add(8)
                .cast::<u64>()
                .write_unaligned(mark)
        };
        true
    }
}

type NativeFlow = Reference<Plain, Ring>;

fn wire(result: Result<Endpoint, WireError>) -> [u64; 4] {
    match result {
        Ok(endpoint) => [0, endpoint.0, 0, 0],
        Err(WireError::Foreign) => [1, 0, 0, 0],
        Err(WireError::Started) => [2, 0, 0, 0],
        Err(WireError::Zero) => [3, 0, 0, 0],
        Err(WireError::Refused(capacity)) => [4, capacity.requested, capacity.available, 0],
    }
}

/// # Safety
/// The current fiber exclusively owns `context` and `flow`. The compiler
/// supplies the command's word arguments and four writable result words.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_flow(
    context: *mut Context,
    flow: *mut u8,
    command: u8,
    args: *const u64,
    output: *mut [u64; 4],
    site: u64,
) -> u8 {
    // SAFETY: command packets are compiler-produced. An endpoint may belong
    // to another flow and must be checked before addressing its port or merge.
    unsafe {
        let command = Op::ALL[command as usize];
        if command == Op::New {
            let flow = Box::new(NativeFlow::default());
            output.write([0, Box::into_raw(flow).expose_provenance() as u64, 0, 0]);
            return 0;
        }
        if command == Op::Finish {
            let finish = Box::from_raw(flow.cast::<NativeFlow>()).finish();
            let result = match finish {
                Ok(()) => [0, 0, 0, 0],
                Err(error) => [1, error.stage as u64, error.key, error.halt as u64],
            };
            output.write(result);
            return 0;
        }
        let ctx = &mut *context;
        let flow = &mut *flow.cast::<NativeFlow>();
        let word = |i| args.add(i).read();
        let endpoint = || Endpoint(word(0));
        let foreign = |ctx: &mut Context| {
            causes::record(ctx, causes::Cause::new(causes::BOUNDS, site));
            1
        };
        let response = match command {
            Op::New | Op::Finish => unreachable!(),
            Op::Sink => wire(flow.sink()),
            Op::Port => wire(flow.port_with_size(
                ctx,
                endpoint(),
                word(1) as u32,
                word(2),
                host_size(word(3)),
            )),
            Op::Stage => {
                let output_size = word(7);
                let code = std::ptr::with_exposed_provenance(word(9) as usize);
                let adapter: Transform = std::mem::transmute(word(10) as usize);
                wire(flow.stage_with_size(
                    ctx,
                    endpoint(),
                    word(1) as u32,
                    StageCharge {
                        output: word(8),
                        working: word(2),
                        state: word(6),
                        item_steps: word(3),
                    },
                    host_size(output_size),
                    || {
                        let mut state = Plain::copy(
                            word(5),
                            std::ptr::with_exposed_provenance(word(4) as usize),
                        );
                        move |stage: &mut Context, key, value: &Plain| {
                            let out = Plain::new(output_size);
                            let mut present = 0u8;
                            let status = adapter(
                                stage,
                                code,
                                state.address(),
                                key,
                                value.at,
                                out.at,
                                &mut present,
                                site,
                            );
                            if status != 0 {
                                Err(Halt::Failed)
                            } else {
                                Ok((present != 0).then_some(out))
                            }
                        }
                    },
                ))
            }
            Op::Push => {
                let item = Plain::span(
                    std::ptr::with_exposed_provenance_mut(word(2) as usize),
                    host_size(word(3)),
                );
                let result = match flow.push(endpoint(), word(1), item) {
                    Ok(()) => 0,
                    Err(PushError::Ended(_)) => 1,
                    Err(PushError::Order(_)) => 2,
                    Err(PushError::Closed(_)) => 3,
                    Err(PushError::Stalled(_)) => 4,
                    Err(PushError::Foreign(_)) => return foreign(ctx),
                };
                [result, 0, 0, 0]
            }
            Op::Advance => {
                if flow.advance(endpoint(), word(1)).is_err() {
                    return foreign(ctx);
                }
                [0; 4]
            }
            Op::End => {
                if flow.end(endpoint()).is_err() {
                    return foreign(ctx);
                }
                [0; 4]
            }
            Op::Next => {
                let write = |item: &Plain| {
                    item.write(std::ptr::with_exposed_provenance_mut(word(1) as usize))
                };
                let pulled = flow.next(endpoint()).map(|pull| match pull {
                    Pull::Item(key, from, item) => {
                        write(&item);
                        Pull::Item(key, from, ())
                    }
                    Pull::Dropped(from, at) => Pull::Dropped(from, at),
                    Pull::End => Pull::End,
                    Pull::Stalled => Pull::Stalled,
                });
                match pulled {
                    Ok(Pull::Item(key, from, ())) => [0, key, from as u64, 0],
                    Ok(Pull::Dropped(from, at)) => [1, at, from as u64, 0],
                    Ok(Pull::End) => [2, 0, 0, 0],
                    Ok(Pull::Stalled) => [3, 0, 0, 0],
                    Err(Foreign) => return foreign(ctx),
                }
            }
        };
        output.write(response);
        0
    }
}

/// # Safety
/// `flow` is the unique live carrier being mechanically cleaned.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_flow_drop(flow: *mut u8) {
    // SAFETY: the compiler transfers the live resource to its mechanical drop.
    unsafe { drop(Box::from_raw(flow.cast::<NativeFlow>())) };
}

#[cfg(feature = "testing")]
type FlowCases = unsafe extern "C" fn(*mut Context, *mut u8);

/// # Safety
/// The fixture pins its state through the synchronous callback.
#[cfg(feature = "testing")]
unsafe fn run_flow_cases(bytes: u64, state: *mut u8, body: FlowCases) {
    #[derive(Clone, Copy)]
    struct Call {
        state: *mut u8,
        body: FlowCases,
    }
    unsafe extern "C" fn invoke(context: *mut Context, _: *mut u8, input: *mut u8) -> u8 {
        // SAFETY: run_root copies Call into aligned storage on entry.
        unsafe {
            let call = input.cast::<Call>().read();
            (call.body)(context, call.state);
        }
        0
    }
    let mut context = Context::new(bytes);
    causes::task_start(&mut context).unwrap();
    let mut call = Call { state, body };
    // SAFETY: the root and callback state are pinned until the run ends.
    unsafe {
        crate::tasks::run_root(
            &mut context,
            invoke,
            (&raw mut call).cast(),
            std::ptr::null_mut(),
            size_of::<Call>(),
            0,
            crate::tasks::SchedulerProvider::default(),
        );
    }
}

/// # Safety
/// The fixture pins its state through the synchronous callback.
#[cfg(feature = "testing")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_test_flow_run(
    lazy: bool,
    bytes: u64,
    state: *mut u8,
    body: FlowCases,
) {
    use crate::schedule_script::{FlowMode, ScheduleScript};
    let mode = if lazy {
        FlowMode::Lazy
    } else {
        FlowMode::Eager
    };
    let script = std::sync::Arc::new(ScheduleScript::new(0).with_flow_mode(mode));
    // SAFETY: by the contract.
    script.install(|| unsafe { run_flow_cases(bytes, state, body) });
}

/// The cases unscripted, as the root unit of a `workers`-thread pool, so a
/// flow's stage calls run on helper units. A process launches one pool.
/// # Safety
/// The fixture pins its state through the synchronous callback.
#[cfg(feature = "testing")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_test_flow_run_pool(
    workers: u64,
    bytes: u64,
    state: *mut u8,
    body: FlowCases,
) {
    let state = state.expose_provenance();
    // SAFETY: lengths, masks, descriptors and nullable pointers; the root
    // reads none of them.
    let config: crate::entry::OsConfig = unsafe { std::mem::zeroed() };
    crate::units::launch(1, host_size(workers), &config, &|_| {
        // SAFETY: by the contract.
        unsafe { run_flow_cases(bytes, std::ptr::with_exposed_provenance_mut(state), body) };
        0
    });
}
