//! Receive bounds shared by the driver's WebSocket transports.

pub(crate) const UPGRADE_HEAD_LIMIT: usize = 8192;
pub(crate) const MESSAGE_LIMIT_MIB: usize = 64;
pub(crate) const MESSAGE_LIMIT: usize = MESSAGE_LIMIT_MIB << 20;

/// The end of a complete HTTP upgrade head within the receive limit.
pub(crate) fn upgrade_head_end(input: &[u8]) -> Option<usize> {
    input[..input.len().min(UPGRADE_HEAD_LIMIT)]
        .windows(4)
        .position(|bytes| bytes == b"\r\n\r\n")
        .map(|start| start + 4)
}
