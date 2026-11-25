use super::*;
use crate::abi_layout::*;
use std::mem::{offset_of, size_of};

#[test]
fn published_entry_layout_matches_c_abi() {
    assert_eq!(size_of::<OsConfig>(), OS_CONFIG_BYTES as usize);
    for (actual, published) in [
        (offset_of!(OsConfig, revision), OS_CONFIG_REVISION),
        (offset_of!(OsConfig, place), OS_CONFIG_PLACE),
        (offset_of!(OsConfig, root_limit), OS_CONFIG_ROOT_LIMIT),
        (offset_of!(OsConfig, args), OS_CONFIG_ARGS),
        (offset_of!(OsConfig, args_len), OS_CONFIG_ARGS_LEN),
        (offset_of!(OsConfig, directories), OS_CONFIG_DIRECTORIES),
        (
            offset_of!(OsConfig, directories_len),
            OS_CONFIG_DIRECTORIES_LEN,
        ),
        (offset_of!(OsConfig, grant_mask), OS_CONFIG_GRANT_MASK),
        (offset_of!(OsConfig, stdout_fd), OS_CONFIG_STREAMS),
        (offset_of!(OsConfig, parameters), OS_CONFIG_PARAMETERS),
        (offset_of!(OsConfig, target), OS_CONFIG_TARGET),
        (offset_of!(OsConfig, directory_ops), OS_CONFIG_DIRECTORY_OPS),
        (offset_of!(OsConfig, process_ops), OS_CONFIG_PROCESS_OPS),
        (
            offset_of!(OsConfig, socket_activation),
            OS_CONFIG_SOCKET_ACTIVATION,
        ),
        (offset_of!(OsConfig, secret_ops), OS_CONFIG_SECRET_OPS),
        (offset_of!(OsConfig, env_patterns), OS_CONFIG_ENV_PATTERNS),
        (offset_of!(OsConfig, env_ops), OS_CONFIG_ENV_OPS),
        (offset_of!(OsConfig, exec_bindings), OS_CONFIG_EXEC_BINDINGS),
        (
            offset_of!(OsConfig, exec_bindings_len),
            OS_CONFIG_EXEC_BINDINGS_LEN,
        ),
        (offset_of!(OsConfig, exec_ops), OS_CONFIG_EXEC_OPS),
        (offset_of!(OsConfig, cred_grants), OS_CONFIG_CRED_GRANTS),
        (
            offset_of!(OsConfig, cred_grants_len),
            OS_CONFIG_CRED_GRANTS_LEN,
        ),
        (
            offset_of!(OsConfig, parameters_len),
            OS_CONFIG_PARAMETERS_LEN,
        ),
        (
            offset_of!(crate::process::Config, grants),
            PROCESS_CONFIG_GRANTS,
        ),
        (
            offset_of!(crate::process::MainResult, kind),
            MAIN_RESULT_KIND,
        ),
        (
            offset_of!(crate::process::MainResult, exit),
            MAIN_RESULT_EXIT,
        ),
    ] {
        assert_eq!(actual, published as usize);
    }
}
