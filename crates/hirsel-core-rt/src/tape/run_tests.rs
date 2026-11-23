use super::*;

pub(in crate::tape) fn check_launch_replay() {
    #[derive(Debug, PartialEq)]
    struct Snapshot {
        digest: Vec<u8>,
        target: Vec<u8>,
        n_place: u64,
        explicit: u64,
        revision: Vec<u8>,
        describe: u64,
        emitted: bool,
    }
    type HashLanes = [u64; hirsel_core_layout::map_hash::STATE_WORDS];
    thread_local! {
        static OBSERVED: std::cell::RefCell<Vec<(Snapshot, HashLanes)>> = const { std::cell::RefCell::new(Vec::new()) };
    }
    unsafe extern "C" fn entry(context: *mut Context, _: *const Config, _: *mut MainResult) -> u8 {
        // SAFETY: process startup pins the context and launch views through entry.
        unsafe {
            let word = |field, index| process::hirsel_core_launch_word(context, field, index);
            let mut target = empty_buffer();
            process::hirsel_core_launch_target(context, &mut target);
            let snapshot = Snapshot {
                digest: (0..word(1, 0)).map(|i| word(2, i) as u8).collect(),
                target: std::slice::from_raw_parts(target.data, target.len).to_vec(),
                n_place: word(0, 0),
                explicit: word(16, 0),
                revision: (0..word(17, 0)).map(|i| word(18, i) as u8).collect(),
                describe: word(5, 0),
                emitted: process::hirsel_core_describe_emit(
                    context,
                    &Buffer {
                        data: b"describe\n".as_ptr().cast_mut(),
                        len: 9,
                        capacity: 0,
                    },
                ),
            };
            OBSERVED
                .with_borrow_mut(|seen| seen.push((snapshot, (*context).map_hash_key.state().0)));
        }
        0
    }
    let scratch = tempfile::tempdir().unwrap();
    let root = scratch.path();
    use std::os::fd::AsRawFd;
    let output = std::fs::File::create(root.join("describe")).unwrap();
    let stderr = std::fs::File::create(root.join("stderr")).unwrap();
    let path = root.join("run.tape");
    for (mode, previous_format) in [(0, false), (1, false), (1, true)] {
        if previous_format {
            // Keep the Run frames and rewrite block zero in format 11, including
            // the trailing key and a valid checksum.
            let bytes = std::fs::read(&path).unwrap();
            let header_end =
                format::BLOCK_HEAD + u32::from_le_bytes(bytes[12..16].try_into().unwrap()) as usize;
            let mut header = bytes[format::BLOCK_HEAD..header_end].to_vec();
            header[..2].copy_from_slice(&11u16.to_le_bytes());
            let tape_id = header[format::TAPE_ID_AT..format::TAPE_ID_AT + 16]
                .try_into()
                .unwrap();
            header.extend_from_slice(&[0; 16]);
            let mut old = format::block(&tape_id, 0, 0, &header);
            old.extend_from_slice(&bytes[header_end + format::BLOCK_CHECK..]);
            std::fs::write(&path, old).unwrap();
        }
        let (target, arguments): (&[u8], &[&[u8]]) = if mode == 0 {
            (
                b"record-target",
                &[b"--hirsel-describe", b"--hirsel-n-place=73"],
            )
        } else {
            (b"replay-target", &[b"--hirsel-n-place=1"])
        };
        let args: Vec<_> = arguments
            .iter()
            .map(|a| View {
                data: a.as_ptr(),
                len: a.len(),
            })
            .collect();
        let input = OsConfig {
            root_limit: 1 << 20,
            args: args.as_ptr(),
            args_len: args.len(),
            directories: ptr::null(),
            directories_len: 0,
            grant_mask: 0,
            stdout_fd: output.as_raw_fd(),
            stderr_fd: stderr.as_raw_fd(),
            parameters: ptr::null(),
            parameters_len: 0,
            net_rules: ptr::null(),
            net_rules_len: 0,
            directory_ops: std::ptr::null(),
            process_ops: std::ptr::null(),
            socket_activation: std::ptr::null(),
            secret_grants: core::ptr::null(),
            secret_grants_len: 0,
            env_patterns: crate::process::View {
                data: std::ptr::null(),
                len: 0,
            },
            secret_ops: std::ptr::null(),
            env_ops: std::ptr::null(),
            exec_bindings: std::ptr::null(),
            exec_bindings_len: 0,
            exec_ops: std::ptr::null(),
            cred_grants: std::ptr::null(),
            cred_grants_len: 0,
            revision: &[7; 32],
            target: View {
                data: target.as_ptr(),
                len: target.len(),
            },
        };
        // SAFETY: all startup tables and names remain pinned until the run ends.
        let status = unsafe {
            run_taped(
                &input,
                ptr::null(),
                ptr::null(),
                entry,
                path.as_os_str().as_bytes().to_vec(),
                mode,
                c"launch-case".as_ptr(),
                &[7; 32],
                None,
            )
        };
        assert_eq!(status, u8::from(previous_format));
    }
    let seen = OBSERVED.with(|seen| seen.take());
    let [(recorded, record_key), (replayed, replay_key)] = seen.as_slice() else {
        panic!("expected entry once during recording and once during replay");
    };
    assert_eq!(recorded.digest.len(), 32);
    assert_eq!(recorded.target, b"record-target");
    assert_eq!(recorded.n_place, 73);
    assert_eq!(recorded.explicit, 1);
    assert_eq!(recorded.revision, [7; 32]);
    assert_eq!(recorded.describe, 1);
    assert!(recorded.emitted);
    assert!(
        record_key != replay_key,
        "replay reused the recording's Map key"
    );
    assert_eq!(replayed, recorded);
    assert_eq!(
        std::fs::read_to_string(root.join("stderr")).unwrap(),
        "replay: refused: unsupported tape format 11 (expected 12)\n"
    );
    assert_eq!(std::fs::read(root.join("describe")).unwrap(), b"describe\n");
}
