//! Provider operation codes shared by the native ABI and the tape codec.

macro_rules! host_ops {
    ($($(#[$attr:meta])* $name:ident = $code:literal),+ $(,)?) => {
        $($(#[$attr])* pub const $name: u8 = $code;)+

        /// Every provider operation code in declaration order.
        pub const OPERATIONS: &[u8] = &[$($code),+];

        const _: () = {
            let mut used = [false; 256];
            $(
                assert!(!used[$code], concat!("duplicate host ABI request number ", stringify!($code)));
                used[$code] = true;
            )+
        };
    };
}

host_ops! {
    OPEN = 0,
    READ = 1,
    WRITE = 2,
    CLOSE = 3,
    TRY_LOCK = 4,
    UNLOCK = 5,
    MKDIR = 6,
    MKDTEMP = 7,
    REMOVE = 8,
    REMOVE_TREE = 9,
    WALL_MS = 10,
    MONO_NS = 11,
    STDOUT = 12,
    STDERR = 13,
    RENAME = 14,
    SLEEP_UNTIL = 15,
    /// A wait until the clock's wall time reaches an absolute millisecond reading.
    SLEEP_UNTIL_WALL = 77,
    SYNC = 16,
    SYNC_DIR = 17,
    RANDOM = 18,
    CONNECT = 19,
    LISTEN = 20,
    ACCEPT = 21,
    TLS_CONNECT = 22,
    SEND_SECRET = 23,
    RECV_BOUNDED = 27,
    /// A wait until a read half has bytes, end of stream or an error to report (NET-1).
    RECV_READY = 78,
    /// TLS-7 credential fetch: the provider seals the secret key itself.
    CRED_FETCH = 80,
    SEND_PAIR = 26,
    STOPPED = 28,
    STDIN_READ = 32,
    READ_AT = 37,
    WRITE_AT = 38,
    SECRET_MAC = 39,
    SECRET_KDF = 79,
    RESOLVE = 40,
    OPEN_DIR = 43,
    ENTRIES = 44,
    NEXT_ENTRIES = 45,
    STAT = 46,
    STAT_FILE = 47,
    CLOSE_ENTRIES = 48,
    // Virtual providers (core.md, Provide). The constructors and hand operations
    // change run-local state only; CLOSE_CELL is a hand's or trigger's cleanup.
    PROVIDE_CLOCK = 49,
    PROVIDE_RANDOM = 50,
    PROVIDE_PROCESS = 51,
    CLOCK_ADVANCE = 52,
    CLOCK_SET_WALL = 53,
    CLOCK_IDLE = 54,
    PROCESS_TRIGGER = 55,
    CLOSE_CELL = 56,
    // Served ports (core.md, Provide). The constructors admit the port's room;
    // PORT_NEXT waits for a call; CALL_WORD and CALL_PAYLOAD read a taken call;
    // PORT_REPLY commits it. CLOSE_SERVED seals a port and CLOSE_CALL settles an
    // unanswered call as sealed.
    PROVIDE_STDOUT = 57,
    PROVIDE_STDERR = 58,
    PORT_NEXT = 59,
    CALL_WORD = 60,
    CALL_PAYLOAD = 61,
    PORT_REPLY = 62,
    CLOSE_SERVED = 63,
    CLOSE_CALL = 64,
    // A Dir port (PROVIDE_DIR); RELEASE is a derived handle's cleanup, which
    // queues its Released notice and never waits; PORT_FILL stages reply bytes.
    PROVIDE_DIR = 65,
    RELEASE = 66,
    PORT_FILL = 67,
    // The SCRAM sink issues proof bytes on the live stream and retains a verdict key.
    SCRAM = 68,
    SCRAM_VERIFY = 69,
    // A Net port: its sockets and listeners are the port's (core.md, Provide).
    PROVIDE_NET = 70,
    CALL_SECRET = 71,
    EXEC_SPAWN = 72,
    EXEC_WAIT = 73,
    EXEC_SIGNAL = 74,
    EXEC_NARROW = 75,
    PROVIDE_EXEC = 76,
    // Unix endpoints and datagrams (NET-12 through NET-14).
    UNIX_CONNECT = 106,
    UNIX_LISTEN = 107,
    UNIX_ACCEPT = 108,
    DATAGRAM_OPEN = 109,
    DATAGRAM_BIND = 110,
    DATAGRAM_CONNECT = 111,
    DATAGRAM_SEND = 112,
    DATAGRAM_SEND_TO = 113,
    DATAGRAM_RECV = 114,
    NET_NARROW_UNIX = 115,
    LOCAL_ADDR = 116,
    PEER_ADDR = 117,
    CALL_PAYLOAD_PREFIX = 118,
    MKSTEMP = 119,
    ZERO_EXTENSION = 124,
    PROVIDE_RANDOM_TAPE = 131,
    MEDIA_AGREEMENT = 132,
    UDP_OPEN = 133,
    UDP_BIND = 134,
    UDP_BIND_GROUP = 135,
    UDP_RECV = 136,
    UDP_SEND = 137,
    UDP_LOCAL_ADDR = 138,
    UDP_FILL_PACKETS = 139,
    MEDIA_P256_KEY = 140,
    MEDIA_KEY_SHARE = 141,
    SCHEDULE_QUIC_INITIAL = 142,
    SCHEDULE_SEALER = 143,
    SCHEDULE_OPENER = 144,
    SCHEDULE_HEADER_MASK = 145,
    PACKET_SEAL = 146,
    PACKET_SEAL_INTO = 147,
    PACKET_OPEN = 148,
    PACKET_OPEN_INTO = 149,
    PACKET_MASK = 150,
    MEDIA_CAPTURE = 151,
    SCHEDULE_IMPORT_TRAFFIC = 152,
    SCHEDULE_TLS13 = 153,
    SCHEDULE_APPLICATION = 154,
    SCHEDULE_KEY_UPDATE = 155,
    SCHEDULE_RETIRE = 156,
    SCHEDULE_FINISHED = 157,
    SCHEDULE_VERIFY_FINISHED = 158,
    SCHEDULE_DTLS12 = 159,
    SCHEDULE_IMPORT_SRTP = 160,
    SCHEDULE_SRTP = 161,
    SCHEDULE_SPACE_INFO = 162,
    SCHEDULE_SEALER_PAIR = 163,
    UNIT_SPAWN = 164,
    CONN_SEND = 165,
    CONN_TRY_SEND = 166,
    UNIT_CANCEL = 167,
    TRY_LOCK_SHARED = 172,
    ZERO_AT = 173,
    /// TLS-9: read a ClientHello on an accepted socket and stop.
    TLS_HELLO = 174,
    /// TLS-9: complete a server handshake with a chain and a `tls_serve` key.
    TLS_ACCEPT = 175,
    /// TLS-8 T16: import SFrame base-key bytes into a schedule.
    SCHEDULE_IMPORT_SFRAME = 177,
    /// TLS-8 T16: a sealer for one KID at a schedule's step.
    SCHEDULE_SFRAME_SEALER = 178,
    /// TLS-8 T16: an opener for one KID at a schedule's step.
    SCHEDULE_SFRAME_OPENER = 179,
    /// TLS-8 T16: advance a schedule one ratchet step.
    SCHEDULE_SFRAME_RATCHET = 180,
    /// A granted terminal stream's size (`stdout.size`, `stderr.size`). The value is
    /// `cols | rows << 16`, zero when the descriptor gives no size.
    TERM_SIZE = 181,
    /// `stdin.read_secret`: one echo-off terminal line, sealed by the host.
    /// The value is the Secret's designation.
    STDIN_SECRET = 182,
    /// TLS-8: issue an opener for the next QUIC receive key phase.
    SCHEDULE_NEXT_OPENER = 183,
    /// A suspend-inclusive elapsed clock reading.
    BOOT_NS = 184,
    /// Advance a virtual clock's monotonic and suspend-inclusive readings together.
    CLOCK_ADVANCE_BOOT = 185,
    CHECK_LIBRARY = 186,
    COMPILER_READ_SOURCE = 187,
}

// OpenMode's appended case numbers. Existing cases and tape records keep
// their ordinals.
pub const MODE_DURABLE: u8 = 5;
pub const MODE_DIRECT: u8 = 6;
pub const MODE_READ_DEVICE: u8 = 7;
pub const DEVICE_READ_UNAVAILABLE: i32 = 0x40000001;
pub const DEVICE_READ_EXTENT: u32 = 65536;
pub const DIRECT_ALIGNMENT: u64 = 4096;

/// A bit of the guest's Dir designation. Providers and tapes use only the underlying handle.
pub const DIR_READ_ONLY: u64 = 1 << 62;

/// Directory operations that need write authority and the writing modes of open.
pub fn needs_writable_dir(op: u8, mode: u8) -> bool {
    matches!(
        op,
        MKDIR | MKDTEMP | MKSTEMP | REMOVE | REMOVE_TREE | RENAME
    ) || (op == OPEN && !matches!(mode, 0 | MODE_READ_DEVICE))
}

/// A temporary name suffix must fit the 32 bytes guaranteed at entry.
pub fn valid_temp_suffix(suffix: &[u8]) -> bool {
    (1..=32).contains(&suffix.len()) && suffix.iter().all(u8::is_ascii_alphanumeric)
}

pub fn unix_operation(op: u8) -> bool {
    (UNIX_CONNECT..=DATAGRAM_RECV).contains(&op)
}

pub fn udp_operation(op: u8) -> bool {
    (UDP_OPEN..=UDP_LOCAL_ADDR).contains(&op)
}
