use super::*;

/// # Safety
/// Consumes the sole Socket carrier. No allocation, admission or replay point.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_net_split(
    context: *mut Context,
    key: u64,
    _site: u64,
) -> Outcome {
    unsafe {
        if let Some(provider) = served_socket(context, key) {
            crate::provide::split_socket(&provider, key);
            return Outcome {
                value: key | 1,
                written: key | 2,
                ..Outcome::default()
            };
        }
        let files = (*context).host_files;
        #[cfg(feature = "record")]
        let (read, write) = if let Some(tape) = (*files).net_tape {
            crate::tape::socket_parts(&tape.provider, tape.handle(key), 0b110);
            if let Some(source) = tape.source(key) {
                (*files).sockets.split(source);
            }
            (key | 1, key | 2)
        } else {
            (*files).sockets.split(key)
        };
        #[cfg(not(feature = "record"))]
        let (read, write) = (*files).sockets.split(key);
        Outcome {
            value: read,
            written: write,
            ..Outcome::default()
        }
    }
}

/// Reunite the halves of one plain TCP socket in the run's registry.
/// # Safety
/// Both keys are live, uniquely owned halves. Failure returns both unchanged.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_net_join(
    context: *mut Context,
    read: u64,
    write: u64,
    _site: u64,
) -> Outcome {
    let refused = || Outcome {
        value: read,
        written: write,
        error: IoError { tag: 1, code: 0 },
        ..Outcome::default()
    };
    unsafe {
        // A served socket's halves rejoin at its port; a TLS stream's never.
        if let Some(provider) = served_socket(context, read) {
            let unix = {
                (*(*context).host_files)
                    .unix_served
                    .contains_key(&(read & !3))
            };
            return if read & !3 == write & !3
                && !unix
                && !tls_stream(context, read)
                && crate::provide::join_socket(&provider, read)
            {
                Outcome {
                    value: read & !3,
                    ..Outcome::default()
                }
            } else {
                refused()
            };
        }
    }
    unsafe {
        let files = (*context).host_files;
        let key = read & !3;
        if key != write & !3
            || (*files).tls_writes.contains_key(&key)
            || !(*files).tcp_routes.get(&key).is_some_and(|route| {
                matches!(route.origin, TcpOrigin::Connected(_) | TcpOrigin::Accepted)
            })
        {
            return refused();
        }
        #[cfg(feature = "record")]
        let source = match (*files).net_tape {
            Some(tape) => {
                crate::tape::socket_parts(&tape.provider, tape.handle(key), 1);
                tape.source(key)
            }
            None => Some(key),
        };
        #[cfg(not(feature = "record"))]
        let source = Some(key);
        if let Some(source) = source {
            (*files).sockets.rejoin(source);
        }
        Outcome {
            value: key,
            ..Outcome::default()
        }
    }
}
