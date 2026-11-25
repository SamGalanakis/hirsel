//! A served Dir port's operations and the replies that answer each (core.md,
//! Provide). Each row is one `DirOp` variant in declaration order: its call
//! word, the provider operation a caller issues it with, and every reply
//! variant besides `Failed` that answers it. The runtime and the evaluator
//! translate a caller's operation, check a server's reply and lay out a call
//! from this table alone, so an operation cannot be served without its
//! replies.

/// `DirReply` variants as the tag word of a `Reply[DirOp]` (core.md, Provide).
pub mod reply {
    pub const DONE: u64 = 0;
    pub const FAILED: u64 = 1;
    pub const WRITE_FAILED: u64 = 2;
    pub const COUNT: u64 = 3;
    pub const LOCKED: u64 = 4;
    pub const STAT: u64 = 5;
    pub const FILE: u64 = 6;
    pub const DIR: u64 = 7;
    pub const ENTRIES: u64 = 8;
    pub const ZERO_EXTENSION: u64 = 9;
}

macro_rules! dir_ops {
    ($($name:ident = $word:literal $(from $host:ident)? => [$($reply:ident),*],)+) => {
        /// The `DirOp` variants; the discriminant is the call's first word.
        #[derive(Clone, Copy, Debug, Eq, PartialEq)]
        #[repr(u64)]
        pub enum DirOp {
            $($name = $word,)+
        }

        impl DirOp {
            /// The variant a call's first word names.
            pub const fn from_word(word: u64) -> Option<DirOp> {
                match word {
                    $($word => Some(DirOp::$name),)+
                    _ => None,
                }
            }

            /// The variant a caller's provider operation issues on a Dir port.
            pub const fn from_host(code: u8) -> Option<DirOp> {
                match code {
                    $($(crate::host_op::$host => Some(DirOp::$name),)?)+
                    _ => None,
                }
            }

            /// The reply variants besides `Failed` that answer this operation.
            pub const fn replies(self) -> &'static [u64] {
                match self {
                    $(DirOp::$name => &[$(reply::$reply),*],)+
                }
            }
        }

        // Declaration order: the words are 0, 1, 2, ... with no gap.
        const _: () = {
            let mut next = 0;
            $(
                assert!($word == next, concat!("DirOp ", stringify!($name), " is out of order"));
                next += 1;
            )+
        };
    };
}

dir_ops! {
    Open = 0 from OPEN => [FILE],
    OpenDir = 1 from OPEN_DIR => [DIR],
    Mkdtemp = 2 from MKDTEMP => [DIR],
    Stat = 3 from STAT => [STAT],
    Mkdir = 4 from MKDIR => [DONE],
    Remove = 5 from REMOVE => [DONE],
    RemoveTree = 6 from REMOVE_TREE => [DONE],
    Rename = 7 from RENAME => [DONE],
    SyncDir = 8 from SYNC_DIR => [DONE],
    Entries = 9 from ENTRIES => [ENTRIES],
    Read = 10 from READ => [COUNT],
    ReadAt = 11 from READ_AT => [COUNT],
    Write = 12 from WRITE => [DONE, WRITE_FAILED],
    WriteAt = 13 from WRITE_AT => [DONE, WRITE_FAILED],
    StatFile = 14 from STAT_FILE => [STAT],
    Sync = 15 from SYNC => [DONE],
    TryLock = 16 from TRY_LOCK => [LOCKED],
    Unlock = 17 from UNLOCK => [DONE],
    Close = 18 from CLOSE => [DONE],
    NextEntries = 19 from NEXT_ENTRIES => [COUNT],
    // A handle's notice, queued at its drop: it takes no reply at all.
    Released = 20 => [],
    Mkstemp = 21 from MKSTEMP => [FILE],
    ZeroExtension = 22 from ZERO_EXTENSION => [ZERO_EXTENSION],
    TryLockShared = 23 from TRY_LOCK_SHARED => [LOCKED],
    ZeroAt = 24 from ZERO_AT => [DONE],
}

impl DirOp {
    pub const fn word(self) -> u64 {
        self as u64
    }
}

/// Whether reply tag `reply` answers the call whose first word is `op`:
/// `Failed` answers every operation but `Released`, and nothing answers a
/// word that names no operation.
pub fn answers(op: u64, reply: u64) -> bool {
    DirOp::from_word(op).is_some_and(|op| {
        (reply == reply::FAILED && op != DirOp::Released) || op.replies().contains(&reply)
    })
}
