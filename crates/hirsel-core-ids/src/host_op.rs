//! Provider operation codes shared by the native ABI and the tape codec.

pub const OPEN: u8 = 0;
pub const READ: u8 = 1;
pub const WRITE: u8 = 2;
pub const CLOSE: u8 = 3;
pub const TRY_LOCK: u8 = 4;
pub const UNLOCK: u8 = 5;
pub const MKDIR: u8 = 6;
pub const MKDTEMP: u8 = 7;
pub const REMOVE: u8 = 8;
pub const REMOVE_TREE: u8 = 9;
pub const WALL_MS: u8 = 10;
pub const MONO_NS: u8 = 11;
pub const STDOUT: u8 = 12;
pub const STDERR: u8 = 13;
pub const RENAME: u8 = 14;
pub const SLEEP_UNTIL: u8 = 15;
pub const SYNC: u8 = 16;
pub const SYNC_DIR: u8 = 17;
pub const RANDOM: u8 = 18;
pub const CONNECT: u8 = 19;
pub const LISTEN: u8 = 20;
pub const ACCEPT: u8 = 21;
pub const TLS_CONNECT: u8 = 22;
pub const SEND_SECRET: u8 = 23;
pub const RECV_BOUNDED: u8 = 27;
pub const SEND_PAIR: u8 = 26;
pub const STOPPED: u8 = 28;
pub const STDIN_READ: u8 = 32;
pub const READ_AT: u8 = 37;
pub const WRITE_AT: u8 = 38;
pub const SECRET_MAC: u8 = 39;
pub const RESOLVE: u8 = 40;
pub const OPEN_DIR: u8 = 43;
pub const ENTRIES: u8 = 44;
pub const NEXT_ENTRIES: u8 = 45;
pub const STAT: u8 = 46;
pub const STAT_FILE: u8 = 47;
pub const CLOSE_ENTRIES: u8 = 48;
// Virtual providers (core.md, Provide). The constructors and hand operations
// change run-local state only; CLOSE_CELL is a hand's or trigger's cleanup.
pub const PROVIDE_CLOCK: u8 = 49;
pub const PROVIDE_RANDOM: u8 = 50;
pub const PROVIDE_PROCESS: u8 = 51;
pub const CLOCK_ADVANCE: u8 = 52;
pub const CLOCK_SET_WALL: u8 = 53;
pub const CLOCK_IDLE: u8 = 54;
pub const PROCESS_TRIGGER: u8 = 55;
pub const CLOSE_CELL: u8 = 56;
// Served ports (core.md, Provide). The constructors admit the port's room;
// PORT_NEXT waits for a call; CALL_WORD and CALL_PAYLOAD read a taken call;
// PORT_REPLY commits it. CLOSE_SERVED seals a port and CLOSE_CALL settles an
// unanswered call as sealed.
pub const PROVIDE_STDOUT: u8 = 57;
pub const PROVIDE_STDERR: u8 = 58;
pub const PORT_NEXT: u8 = 59;
pub const CALL_WORD: u8 = 60;
pub const CALL_PAYLOAD: u8 = 61;
pub const PORT_REPLY: u8 = 62;
pub const CLOSE_SERVED: u8 = 63;
pub const CLOSE_CALL: u8 = 64;
// A Dir port (PROVIDE_DIR); RELEASE is a derived handle's cleanup, which
// queues its Released notice and never waits; PORT_FILL stages reply bytes.
pub const PROVIDE_DIR: u8 = 65;
pub const RELEASE: u8 = 66;
pub const PORT_FILL: u8 = 67;
// The SCRAM sink issues proof bytes on the live stream and retains a verdict key.
pub const SCRAM: u8 = 68;
pub const SCRAM_VERIFY: u8 = 69;
// A Net port: its sockets and listeners are the port's (core.md, Provide).
pub const PROVIDE_NET: u8 = 70;
pub const CALL_SECRET: u8 = 71;
pub const EXEC_SPAWN: u8 = 72;
pub const EXEC_WAIT: u8 = 73;
pub const EXEC_SIGNAL: u8 = 74;

/// A bit of the guest's Dir designation. Providers and tapes use only the underlying handle.
pub const DIR_READ_ONLY: u64 = 1 << 62;

/// Directory operations that need write authority and the writing modes of open.
pub fn needs_writable_dir(op: u8, mode: u8) -> bool {
    matches!(op, MKDIR | MKDTEMP | REMOVE | REMOVE_TREE | RENAME) || (op == OPEN && mode != 0)
}

pub const EXEC_NARROW: u8 = 75;
pub const PROVIDE_EXEC: u8 = 76;
// Unix endpoints and datagrams (NET-12 through NET-14).
pub const UNIX_CONNECT: u8 = 106;
pub const UNIX_LISTEN: u8 = 107;
pub const UNIX_ACCEPT: u8 = 108;
pub const DATAGRAM_OPEN: u8 = 109;
pub const DATAGRAM_BIND: u8 = 110;
pub const DATAGRAM_CONNECT: u8 = 111;
pub const DATAGRAM_SEND: u8 = 112;
pub const DATAGRAM_SEND_TO: u8 = 113;
pub const DATAGRAM_RECV: u8 = 114;
pub const NET_NARROW_UNIX: u8 = 115;

pub fn unix_operation(op: u8) -> bool {
    (UNIX_CONNECT..=DATAGRAM_RECV).contains(&op)
}
