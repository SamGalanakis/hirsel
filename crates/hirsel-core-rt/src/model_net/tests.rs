use super::*;
use crate::{Context, host_file::Files, net::*};
use hirsel_core_net::{
    Rule,
    model::{Config, Fault},
};
use std::{fs::File, ptr};
struct Fixture {
    model: Model,
}
unsafe extern "C" fn diagnostic(_: *mut u8, _: *const u8, _: usize) {}
unsafe extern "C" fn wire(context: *mut Context, _: *mut u8, input: *mut u8) -> u8 {
    // SAFETY: the fixture pins its grants, provider, scalar input and byte carriers.
    unsafe {
        let fixture = &**input.cast::<*mut Fixture>();
        let l = hirsel_core_host_listen(context, 1, 0, 0, 0x7f000001, 0, 1, 1);
        assert_eq!(l.error, IoError::OK);
        let c = hirsel_core_host_connect(context, 1, 0, 0, l.lo, l.port, 0, 0, 0, 2);
        assert_eq!(c.error, IoError::OK);
        let s = hirsel_core_host_accept(context, l.value, 0, 0, 0, 3);
        assert_eq!(s.error, IoError::OK);
        assert_ne!(s.port, 0);
        let client = hirsel_core_net_split(context, c.value, 4);
        let server = hirsel_core_net_split(context, s.value, 5);
        let message = b"abc";
        for sent in 0..message.len() {
            let first = usize::from(sent == 0);
            let prefix = Buffer {
                data: message[sent..].as_ptr().cast_mut(),
                len: first,
                capacity: 0,
            };
            let tail = Buffer {
                data: message[sent + first..].as_ptr().cast_mut(),
                len: message.len() - sent - first,
                capacity: 0,
            };
            let result =
                hirsel_core_host_send_pair(context, client.written, &prefix, &tail, 0, 0, 0, 6);
            assert_eq!((result.error, result.value), (IoError::OK, 1));
        }
        let mut bytes = [0; 3];
        let mut buffer = Buffer {
            data: bytes.as_mut_ptr(),
            len: 0,
            capacity: 3,
        };
        let zero = hirsel_core_host_recv_bounded(context, server.value, &mut buffer, 0, 0, 0, 0, 7);
        assert_eq!((zero.error, zero.value, buffer.len), (IoError::OK, 0, 0));
        while buffer.len < 3 {
            let r = hirsel_core_host_recv_bounded(
                context,
                server.value,
                &mut buffer,
                u64::MAX,
                0,
                0,
                0,
                7,
            );
            assert_eq!(r.error, IoError::OK);
            assert!(r.value > 0);
        }
        assert_eq!(&bytes, b"abc");
        let mut byte = 0;
        let mut buffer = Buffer {
            data: &mut byte,
            len: 0,
            capacity: 1,
        };
        let r = hirsel_core_host_recv_bounded(
            context,
            server.value,
            &mut buffer,
            u64::MAX,
            1,
            2,
            100,
            8,
        );
        assert_eq!((r.error.tag, buffer.len), (6, 0));
        assert_eq!(
            fixture.model.now(),
            100,
            "a silent wait expires only after virtual time reaches its deadline"
        );
        let mut at = 200u64;
        let mut sleep = Request {
            operation: SLEEP_UNTIL,
            mode: 0,
            handle: 0,
            path: ptr::null(),
            path_len: 0,
            buffer: Buffer {
                data: (&mut at as *mut u64).cast(),
                len: 8,
                capacity: 0,
            },
            site: 9,
        };
        assert_eq!(
            hirsel_core_host(context, (*(*context).run).provider, &mut sleep).error,
            IoError::OK
        );
        assert_eq!(fixture.model.now(), 200);
        fixture.model.fault(Fault::Partition {
            socket: c.value,
            blocked: true,
        });
        let output = Buffer {
            data: b"x".as_ptr().cast_mut(),
            len: 1,
            capacity: 0,
        };
        assert_eq!(
            hirsel_core_host_send_pair(
                context,
                client.written,
                &output,
                &empty_buffer(),
                0,
                0,
                0,
                10
            )
            .value,
            1
        );
        fixture.model.schedule(
            294,
            Fault::Partition {
                socket: c.value,
                blocked: false,
            },
        );
        let r = hirsel_core_host_recv_bounded(
            context,
            server.value,
            &mut buffer,
            u64::MAX,
            1,
            2,
            300,
            11,
        );
        assert_eq!(
            (r.error.tag, r.value, byte),
            (0, 1, b'x'),
            "ready beats the tied virtual timer"
        );
        assert_eq!(fixture.model.now(), 300);
        hirsel_core_socket_drop(context, client.written);
        buffer.len = 0;
        assert_eq!(
            hirsel_core_host_recv_bounded(
                context,
                server.value,
                &mut buffer,
                u64::MAX,
                0,
                0,
                0,
                12
            )
            .value,
            0
        );
        for key in [client.value, server.value, server.written, l.value] {
            hirsel_core_socket_drop(context, key);
        }
        assert_eq!(
            (fixture.model.live_handles(), fixture.model.pending()),
            (0, 0)
        );
    }
    0
}
#[test]
fn model_native_provider_uses_virtual_timers_with_and_without_scheduler() {
    let mut recorded =
        std::collections::BTreeMap::<_, Vec<hirsel_core_net::model::Decision>>::new();
    for (scheduled, replaying) in [(false, false), (true, false), (true, true)] {
        let prefix = if replaying {
            recorded
                .iter()
                .map(|(&kind, choices)| {
                    (kind, choices.iter().map(|choice| choice.selected).collect())
                })
                .collect()
        } else {
            Default::default()
        };
        let model = Model::new(Config {
            prefix,
            queue_bytes: 4,
            max_prefix: 1,
            latency_ns: 3,
            ..Config::default()
        });
        let mut os = crate::os::OsProvider::new(
            File::create("/dev/null").unwrap().into(),
            File::create("/dev/null").unwrap().into(),
        );
        let mut adapter = ModelProvider::new(model.clone(), os.provider());
        let provider = adapter.provider();
        let rules = [
            Rule::parse("Connect 127.0.0.0/8 1-65535").unwrap(),
            Rule::parse("Listen 127.0.0.0/8 0-65535").unwrap(),
        ]
        .map(hirsel_core_net::RuleView::address);
        let view = Rules {
            data: rules.as_ptr(),
            len: rules.len(),
        };
        let grants = [
            crate::entry::Grant {
                kind: crate::entry::NET,
                provider: &provider,
                handle: (&view as *const Rules) as u64,
            },
            crate::entry::Grant {
                kind: crate::entry::CLOCK,
                provider: &provider,
                handle: 0,
            },
        ];
        let config = crate::process::Config {
            root_limit: 1 << 20,
            args: ptr::null(),
            args_len: 0,
            grants: ptr::null(),
            grants_len: 0,
            provider: &provider,
            state: ptr::null_mut(),
            diagnostic,
            describe: None,
            scheduler: Default::default(),
            stop_fd: 2,
            capabilities: grants.as_ptr(),
            capabilities_len: grants.len(),
            close_grants: None,
            launch: ptr::null(),
            describe_output: None,
        };
        let mut context = Context::new(1 << 20);
        let mut files = Files::default();
        context.host_files = &mut files;
        context.run = &config;
        let mut fixture = Fixture { model };
        let mut input = &mut fixture as *mut Fixture;
        // SAFETY: run pins all configuration and the fixture through teardown.
        unsafe {
            if scheduled {
                crate::causes::task_start(&mut context).unwrap();
                assert_eq!(
                    crate::tasks::run_root(
                        &mut context,
                        wire,
                        (&mut input as *mut *mut Fixture).cast(),
                        ptr::null_mut(),
                        size_of::<*mut Fixture>(),
                        0,
                        Default::default()
                    ),
                    0
                );
            } else {
                assert_eq!(
                    wire(
                        &mut context,
                        ptr::null_mut(),
                        (&mut input as *mut *mut Fixture).cast()
                    ),
                    0
                );
            }
            files.close_all(&mut context);
        }
        assert!(files.sockets.is_empty());
        if replaying {
            assert_eq!(fixture.model.decisions(), recorded);
        } else if scheduled {
            recorded = fixture.model.decisions();
        }
    }
}
