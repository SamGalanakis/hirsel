//! Binding of canonical core declarations to their call-site kernels.
//! Identity is resolved once when the program's declarations are published.
use super::*;
use hirsel_core_ir::{Counted, HostOp, RtOp};
use hirsel_core_layout::flow::Op as FlowOp;
mod flow;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Kernel {
    Stream(HostOp),
    CarrierMove,
    CarrierView,
    StringShorten,
    StringList(&'static str),
    Unique,
    Host(HostOp),
    Net(HostOp),
    ArgsLen,
    ArgsGet,
    NewOwner(Option<Counted>),
    Share(Counted),
    TryUnwrap(Counted),
    WithUnique(Counted),
    Unbox,
    Spawn,
    CheckCancel,
    Cancel,
    Join,
    ChannelNew,
    LatestNew,
    LatestHandle,
    LatestPublish,
    LatestRead,
    ChannelSender,
    Send,
    TrySend,
    Receive,
    TryReceive,
    LogNew,
    LogAppend,
    LogFinish,
    CodeIdentity,
    CodeRevision,
    CodeCallable,
    CodeDescribe,
    TapeOpen,
    /// `true` ends with returned Bytes, `false` with a Causes chain and its kind.
    TapeEnd(bool),
    /// The substitute of a capability of this `entry` kind.
    TapeGrant(u8),
    TapeAcross,
    Flow(FlowOp),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Lane {
    Numeric,
    Value,
    Language,
    PendingCode,
    Runtime(Kernel),
}
struct Entry {
    module: &'static str,
    name: &'static str,
    lane: Lane,
    #[cfg(test)]
    class: &'static str,
    #[cfg(test)]
    private: bool,
}
macro_rules! operations {
    ($(($module:literal, $name:literal, $lane:expr, $class:literal, $owner:literal, $private:literal)),* $(,)?) => {
        const OPERATIONS: &[Entry] = &[$(Entry { module: $module, name: $name, lane: $lane, #[cfg(test)] class: $class, #[cfg(test)] private: $private }),*];
    };
}

operations! {

    ("bytebuf", "append_inline", Lane::Value, "A", "http-b1-1204", false),
    ("bytebuf", "freeze", Lane::Value, "A", "http-b1-1204", false),
    ("bytebuf", "as_bytes", Lane::Value, "A", "http-b1-1204", false),
    ("flow", "new", Lane::Runtime(Kernel::Flow(FlowOp::New)), "B", "mc-f1-1180", false),
    ("flow", "sink", Lane::Runtime(Kernel::Flow(FlowOp::Sink)), "B", "mc-f1-1180", false),
    ("flow", "stage", Lane::Runtime(Kernel::Flow(FlowOp::Stage)), "B", "mc-f1-1180", false),
    ("flow", "port", Lane::Runtime(Kernel::Flow(FlowOp::Port)), "B", "mc-f1-1180", false),
    ("flow", "push", Lane::Runtime(Kernel::Flow(FlowOp::Push)), "B", "mc-f1-1180", false),
    ("flow", "advance", Lane::Runtime(Kernel::Flow(FlowOp::Advance)), "B", "mc-f1-1180", false),
    ("flow", "end", Lane::Runtime(Kernel::Flow(FlowOp::End)), "B", "mc-f1-1180", false),
    ("flow", "next", Lane::Runtime(Kernel::Flow(FlowOp::Next)), "B", "mc-f1-1180", false),
    ("flow", "finish", Lane::Runtime(Kernel::Flow(FlowOp::Finish)), "B", "mc-f1-1180", false),
    ("int", "parse", Lane::PendingCode, "C", "std-seed-api-889", false),
    ("core", "isolate", Lane::PendingCode, "A", "core-check-callables-913", false),
    ("latest", "new", Lane::Runtime(Kernel::LatestNew), "A", "core-ops-914", false),
    ("latest", "handle", Lane::Runtime(Kernel::LatestHandle), "A", "core-ops-914", false),
    ("latest", "publish", Lane::Runtime(Kernel::LatestPublish), "A", "core-ops-914", false),
    ("latest", "read", Lane::Runtime(Kernel::LatestRead), "A", "core-ops-914", false),
    ("log", "new", Lane::Runtime(Kernel::LogNew), "B", "core-ops-914", false),
    ("log", "append", Lane::Runtime(Kernel::LogAppend), "B", "core-ops-914", false),
    ("log", "finish", Lane::Runtime(Kernel::LogFinish), "B", "core-ops-914", false),
    ("code", "describe", Lane::Runtime(Kernel::CodeDescribe), "A", "core-check-callables-913", false),
    ("code", "identity", Lane::Runtime(Kernel::CodeIdentity), "A", "core-ops-914", false),
    ("code", "revision", Lane::Runtime(Kernel::CodeRevision), "A", "core-ops-914", false),
    ("code", "callable", Lane::Runtime(Kernel::CodeCallable), "A", "core-ops-914", false),
    ("host", "rename", Lane::Runtime(Kernel::Host(HostOp::Rename)), "B", "core-ops-914", false),
    ("host", "sleep_until", Lane::Runtime(Kernel::Host(HostOp::SleepUntil)), "B", "core-ops-914", false),
    ("host", "sync", Lane::Runtime(Kernel::Host(HostOp::Sync)), "B", "core-ops-914", false),
    ("host", "sync_dir", Lane::Runtime(Kernel::Host(HostOp::SyncDir)), "B", "core-ops-914", false),
    ("host", "random", Lane::Runtime(Kernel::Host(HostOp::Random)), "B", "core-ops-914", false),
    ("list", "sum", Lane::PendingCode, "C", "std-seed-api-889", false),
    ("list", "fill", Lane::PendingCode, "A", "core-check-calls-910", false),
    ("string", "from", Lane::PendingCode, "C", "std-seed-api-889", false),
    ("string", "from_utf8", Lane::PendingCode, "C", "std-seed-api-889", false),
    ("string", "is_utf8", Lane::PendingCode, "C", "std-seed-api-889", false),
    ("string", "into_bytes", Lane::PendingCode, "C", "std-seed-api-889", false),
    ("string", "next_scalar", Lane::PendingCode, "C", "std-seed-api-889", false),
    ("string", "push_scalar", Lane::PendingCode, "C", "std-seed-api-889", true),
    ("string", "push_char", Lane::PendingCode, "C", "std-seed-api-889", false),
    ("string", "push_u64", Lane::PendingCode, "C", "std-seed-api-889", false),
    ("string", "truncate", Lane::PendingCode, "C", "std-seed-api-889", false),
    ("string", "make_ascii_upper", Lane::PendingCode, "C", "std-seed-api-889", false),
    ("string", "make_ascii_lower", Lane::PendingCode, "C", "std-seed-api-889", false),
    ("arc", "new", Lane::Runtime(Kernel::NewOwner(Some(Counted::Arc))), "A", "core-ops-914", false),
    ("arc", "share", Lane::Runtime(Kernel::Share(Counted::Arc)), "A", "core-ops-914", false),
    ("arc", "try_unwrap", Lane::Runtime(Kernel::TryUnwrap(Counted::Arc)), "A", "core-ops-914", false),
    ("arc", "with_unique", Lane::Runtime(Kernel::WithUnique(Counted::Arc)), "A", "core-ops-914", false),
    ("args", "len", Lane::Runtime(Kernel::ArgsLen), "B", "core-ops-914", false),
    ("args", "get", Lane::Runtime(Kernel::ArgsGet), "B", "core-ops-914", false),
    ("box", "new", Lane::Runtime(Kernel::NewOwner(None)), "A", "core-ops-914", false),
    ("box", "unbox", Lane::Runtime(Kernel::Unbox), "A", "core-ops-914", false),
    ("channel", "new", Lane::Runtime(Kernel::ChannelNew), "B", "core-ops-914", false),
    ("channel", "sender", Lane::Runtime(Kernel::ChannelSender), "B", "core-ops-914", false),
    ("channel", "send", Lane::Runtime(Kernel::Send), "B", "core-ops-914", false),
    ("channel", "try_send", Lane::Runtime(Kernel::TrySend), "B", "core-ops-914", false),
    ("channel", "receive", Lane::Runtime(Kernel::Receive), "B", "core-ops-914", false),
    ("channel", "try_receive", Lane::Runtime(Kernel::TryReceive), "B", "core-ops-914", false),
    ("char", "from_u32", Lane::Numeric, "A", "core-check-index-912", false),
    ("char", "to_u32", Lane::Numeric, "A", "core-check-index-912", false),
    ("core", "fail", Lane::Language, "A", "core-check-895", false),
    ("core", "replace", Lane::Value, "A", "core-check-calls-910", false),
    ("core", "clone", Lane::Language, "A", "core-check-895", false),
    ("core", "clone_in", Lane::Language, "A", "core-check-895", false),
    ("core", "build_in", Lane::Language, "A", "core-check-895", false),
    ("core", "finish", Lane::Language, "A", "core-check-895", false),
    ("core", "finish_trim", Lane::Language, "A", "core-check-895", false),
    ("core", "build_stream_in", Lane::Language, "A", "core-check-895", false),
    ("core", "emit", Lane::Language, "A", "core-check-895", false),
    ("dest", "put", Lane::Language, "A", "core-check-895", false),
    ("float", "mul_add", Lane::Numeric, "A", "core-check-index-912", false),
    ("float", "eq", Lane::Numeric, "A", "float-cmp-1251", false),
    ("float", "lt", Lane::Numeric, "A", "float-cmp-1251", false),
    ("float", "le", Lane::Numeric, "A", "float-cmp-1251", false),
    ("float", "gt", Lane::Numeric, "A", "float-cmp-1251", false),
    ("float", "ge", Lane::Numeric, "A", "float-cmp-1251", false),
    ("float", "total_order", Lane::Numeric, "A", "core-check-index-912", false),
    ("float", "to_int", Lane::Numeric, "A", "core-check-index-912", false),
    ("float", "from_int", Lane::Numeric, "A", "core-check-index-912", false),
    ("float", "widen", Lane::Numeric, "A", "core-check-index-912", false),
    ("float", "narrow", Lane::Numeric, "A", "core-check-index-912", false),
    ("float", "to_bits32", Lane::Numeric, "A", "core-check-index-912", false),
    ("float", "to_bits64", Lane::Numeric, "A", "core-check-index-912", false),
    ("float", "from_bits32", Lane::Numeric, "A", "core-check-index-912", false),
    ("float", "from_bits64", Lane::Numeric, "A", "core-check-index-912", false),
    ("host", "open", Lane::Runtime(Kernel::Host(HostOp::Open)), "B", "core-ops-914", false),
    ("host", "read", Lane::Runtime(Kernel::Host(HostOp::Read)), "B", "core-ops-914", false),
    ("host", "write", Lane::Runtime(Kernel::Host(HostOp::Write)), "B", "core-ops-914", false),
    ("host", "close", Lane::Runtime(Kernel::Host(HostOp::Close)), "B", "core-ops-914", false),
    ("host", "try_lock", Lane::Runtime(Kernel::Host(HostOp::TryLock)), "B", "core-ops-914", false),
    ("host", "unlock", Lane::Runtime(Kernel::Host(HostOp::Unlock)), "B", "core-ops-914", false),
    ("host", "mkdir", Lane::Runtime(Kernel::Host(HostOp::Mkdir)), "B", "core-ops-914", false),
    ("host", "mkdtemp", Lane::Runtime(Kernel::Host(HostOp::Mkdtemp)), "B", "core-ops-914", false),
    ("host", "remove", Lane::Runtime(Kernel::Host(HostOp::Remove)), "B", "core-ops-914", false),
    ("host", "remove_tree", Lane::Runtime(Kernel::Host(HostOp::RemoveTree)), "B", "core-ops-914", false),
    ("host", "wall_ms", Lane::Runtime(Kernel::Host(HostOp::WallMs)), "B", "core-ops-914", false),
    ("host", "mono_ns", Lane::Runtime(Kernel::Host(HostOp::MonoNs)), "B", "core-ops-914", false),
    ("tape", "open", Lane::PendingCode, "C", "tape-build-1037", false),
    ("tape", "end", Lane::PendingCode, "C", "tape-build-1037", false),
    ("tape", "open_raw", Lane::Runtime(Kernel::TapeOpen), "B", "tape-build-1037", true),
    ("tape", "end_returned", Lane::Runtime(Kernel::TapeEnd(true)), "B", "tape-build-1037", true),
    ("tape", "end_causes", Lane::Runtime(Kernel::TapeEnd(false)), "B", "tape-build-1037", true),
    ("tape", "clock", Lane::Runtime(Kernel::TapeGrant(2)), "B", "tape-build-1037", false),
    ("tape", "dir", Lane::Runtime(Kernel::TapeGrant(0)), "B", "tape-build-1037", false),
    ("tape", "stdout", Lane::Runtime(Kernel::TapeGrant(4)), "B", "tape-build-1037", false),
    ("tape", "stderr", Lane::Runtime(Kernel::TapeGrant(5)), "B", "tape-build-1037", false),
    ("tape", "tasks", Lane::Runtime(Kernel::TapeGrant(9)), "B", "tape-s2-1074", false),
    ("tape", "args", Lane::Runtime(Kernel::TapeGrant(10)), "B", "tape-build-1037", false),
    ("tape", "across", Lane::Runtime(Kernel::TapeAcross), "B", "tape-build-1037", false),
    ("tape", "io_error", Lane::PendingCode, "C", "tape-build-1037", true),
    ("tape", "open_error", Lane::PendingCode, "C", "tape-build-1037", true),
    ("tape", "ending", Lane::PendingCode, "C", "tape-build-1037", true),
    ("indexed", "get", Lane::Language, "A", "core-check-895", false),
    ("int", "wrapping_add", Lane::Numeric, "A", "core-check-index-912", false),
    ("int", "wrapping_sub", Lane::Numeric, "A", "core-check-index-912", false),
    ("int", "wrapping_mul", Lane::Numeric, "A", "core-check-index-912", false),
    ("int", "wrapping_neg", Lane::Numeric, "A", "core-check-index-912", false),
    ("int", "wrapping_shl", Lane::Numeric, "A", "core-check-index-912", false),
    ("int", "checked_add", Lane::Numeric, "A", "core-check-index-912", false),
    ("int", "checked_sub", Lane::Numeric, "A", "core-check-index-912", false),
    ("int", "checked_mul", Lane::Numeric, "A", "core-check-index-912", false),
    ("int", "checked_div", Lane::Numeric, "A", "core-check-index-912", false),
    ("int", "saturating_add", Lane::Numeric, "A", "core-check-index-912", false),
    ("int", "saturating_sub", Lane::Numeric, "A", "core-check-index-912", false),
    ("int", "saturating_mul", Lane::Numeric, "A", "core-check-index-912", false),
    ("int", "mul_wide", Lane::Numeric, "A", "core-check-index-912", false),
    ("int", "leading_zeros", Lane::Numeric, "A", "core-check-index-912", false),
    ("int", "trailing_zeros", Lane::Numeric, "A", "core-check-index-912", false),
    ("int", "count_ones", Lane::Numeric, "A", "core-check-index-912", false),
    ("list", "word_le", Lane::Value, "A", "core-check-index-912", false),
    ("list", "put_word_le", Lane::Value, "A", "core-ops-914", false),
    ("int", "widen", Lane::Numeric, "A", "core-check-index-912", false),
    ("int", "narrow", Lane::Numeric, "A", "core-check-index-912", false),
    ("int", "try_narrow", Lane::Numeric, "A", "core-check-index-912", false),
    ("int", "truncate", Lane::Numeric, "A", "core-check-index-912", false),
    ("deque", "Deque", Lane::PendingCode, "C", "core-collections-918", false),
    ("deque", "Deque.$clone", Lane::PendingCode, "C", "core-collections-918", true),
    ("heap", "new", Lane::PendingCode, "C", "core-collections-918", false),
    ("heap", "len", Lane::PendingCode, "C", "core-collections-918", false),
    ("heap", "push", Lane::PendingCode, "C", "core-collections-918", false),
    ("heap", "peek", Lane::PendingCode, "C", "core-collections-918", false),
    ("heap", "pop", Lane::PendingCode, "C", "core-collections-918", false),
    ("heap", "grow", Lane::PendingCode, "C", "core-collections-918", true),
    ("heap", "slots_new", Lane::Value, "A", "core-collections-918", true),
    ("heap", "slots_move", Lane::Value, "A", "core-collections-918", true),
    ("heap", "slots_free", Lane::Value, "A", "core-collections-918", true),
    ("heap", "prepare_push", Lane::PendingCode, "C", "mem-b-1172", true),
    ("heap", "finish_prepared_push", Lane::PendingCode, "C", "mem-b-1172", true),
    ("heap", "slots_unmark_spare", Lane::Value, "A", "mem-b-1172", true),
    ("heap", "slots_publish_spare", Lane::Value, "A", "mem-b-1172", true),
    ("heap", "slots_put", Lane::Value, "A", "core-collections-918", true),
    ("heap", "slots_take", Lane::Value, "A", "core-collections-918", true),
    ("heap", "slots_view", Lane::Value, "A", "core-collections-918", true),
    ("heap", "slots_shift", Lane::Value, "A", "core-collections-918", true),
    ("deque", "new", Lane::PendingCode, "C", "core-collections-918", false),
    ("deque", "len", Lane::PendingCode, "C", "core-collections-918", false),
    ("deque", "push_back", Lane::PendingCode, "C", "core-collections-918", false),
    ("deque", "push_front", Lane::PendingCode, "C", "core-collections-918", false),
    ("deque", "pop_front", Lane::PendingCode, "C", "core-collections-918", false),
    ("deque", "pop_back", Lane::PendingCode, "C", "core-collections-918", false),
    ("deque", "grow", Lane::PendingCode, "C", "core-collections-918", true),
    ("deque", "reverse", Lane::PendingCode, "C", "core-collections-918", true),
    ("deque", "slots_needs_drop", Lane::Value, "A", "core-collections-918", true),
    ("deque", "slots_forget_ring", Lane::Value, "A", "core-collections-918", true),
    ("deque", "slots_empty", Lane::Value, "A", "core-collections-918", true),
    ("deque", "slots_new", Lane::Value, "A", "core-collections-918", true),
    ("deque", "prepare_push", Lane::PendingCode, "C", "mem-b-1172", true),
    ("deque", "finish_prepared_push", Lane::PendingCode, "C", "mem-b-1172", true),
    ("deque", "slots_unmark_spare", Lane::Value, "A", "mem-b-1172", true),
    ("deque", "slots_publish_spare", Lane::Value, "A", "mem-b-1172", true),
    ("deque", "slots_put", Lane::Value, "A", "core-collections-918", true),
    ("deque", "slots_take", Lane::Value, "A", "core-collections-918", true),
    ("deque", "slots_move", Lane::Value, "A", "core-collections-918", true),
    ("deque", "slots_shift", Lane::Value, "A", "core-collections-918", true),
    ("deque", "slots_view", Lane::Value, "A", "core-collections-918", true),
    ("deque", "slots_free", Lane::Value, "A", "core-collections-918", true),
    ("list", "Hole", Lane::PendingCode, "C", "core-collections-918", true),
    ("list", "slots_finish_list", Lane::Value, "A", "core-collections-918", true),
    ("list", "sort_stable", Lane::PendingCode, "C", "core-collections-918", false),
    ("list", "precedes", Lane::PendingCode, "C", "core-collections-918", true),
    ("list", "sorted_run_end", Lane::PendingCode, "C", "core-collections-918", true),
    ("list", "merge_top", Lane::PendingCode, "C", "core-collections-918", true),
    ("list", "move_run", Lane::PendingCode, "C", "core-collections-918", true),
    ("list", "merge_runs", Lane::PendingCode, "C", "core-collections-918", true),
    ("list", "empty_merge", Lane::PendingCode, "C", "core-collections-918", true),
    ("list", "slots_list_view", Lane::Value, "A", "core-collections-918", true),
    ("list", "slots_list_move", Lane::Value, "A", "core-collections-918", true),
    ("list", "slots_forget_ring", Lane::Value, "A", "core-collections-918", true),
    ("list", "slots_empty", Lane::Value, "A", "core-collections-918", true),
    ("list", "slots_new", Lane::Value, "A", "core-collections-918", true),
    ("list", "slots_put", Lane::Value, "A", "core-collections-918", true),
    ("list", "slots_take", Lane::Value, "A", "core-collections-918", true),
    ("list", "slots_view", Lane::Value, "A", "core-collections-918", true),
    ("list", "slots_move", Lane::Value, "A", "core-collections-918", true),
    ("list", "slots_shift", Lane::Value, "A", "core-collections-918", true),
    ("list", "slots_free", Lane::Value, "A", "core-collections-918", true),
    ("list", "new", Lane::Value, "A", "core-check-calls-910", false),
    ("list", "with_capacity", Lane::Value, "A", "core-check-calls-910", false),
    ("list", "len", Lane::Value, "A", "core-check-calls-910", false),
    ("list", "capacity", Lane::Value, "A", "core-check-calls-910", false),
    ("list", "reserve", Lane::Value, "A", "core-check-calls-910", false),
    ("list", "get", Lane::Value, "A", "core-check-calls-910", false),
    ("list", "slice", Lane::Value, "A", "core-check-calls-910", false),
    ("list", "push", Lane::Value, "A", "core-check-calls-910", false),
    ("list", "grow", Lane::PendingCode, "A", "core-check-calls-910", true),
    ("list", "pop", Lane::Value, "A", "core-check-calls-910", false),
    ("list", "set", Lane::Value, "A", "core-check-calls-910", false),
    ("list", "swap", Lane::Value, "A", "core-collections-918", false),
    ("list", "clear", Lane::Value, "A", "core-check-calls-910", false),
    ("list", "append", Lane::PendingCode, "A", "core-check-calls-910", false),
    ("map", "probe_insert", Lane::PendingCode, "C", "mem-b-1172", true),
    ("map", "push_entry", Lane::Value, "A", "core-check-callables-913", true),
    ("map", "pop_entry", Lane::Value, "A", "core-check-callables-913", true),
    ("map", "undo_clone", Lane::Value, "C", "core-check-callables-913", true),
    ("map", "Map.$clone", Lane::Value, "C", "core-check-callables-913", true),
    ("map", "regrow", Lane::Value, "A", "core-check-callables-913", true),
    ("map", "compact", Lane::Value, "A", "core-check-callables-913", true),
    ("map", "make_room", Lane::Value, "C", "core-check-callables-913", true),
    ("map", "reindex", Lane::Value, "C", "std-seed-api-889", true),
    ("map", "install_index", Lane::PendingCode, "C", "core-check-callables-913", true),
    ("map", "swap_value", Lane::Value, "A", "core-check-callables-913", true),
    ("map", "kill", Lane::Value, "A", "core-check-callables-913", true),
    ("map", "take_entry", Lane::Value, "A", "core-collections-918", true),
    ("map", "hash", Lane::Value, "A", "core-check-callables-913", true),
    ("map", "slots", Lane::Value, "A", "core-check-callables-913", true),
    ("map", "capacity", Lane::Value, "A", "core-check-callables-913", true),
    ("map", "slot_hash", Lane::Value, "A", "core-check-callables-913", true),
    ("map", "slot_live", Lane::Value, "A", "core-check-callables-913", true),
    ("map", "slot_key", Lane::Value, "A", "core-check-callables-913", true),
    ("map", "slot_value", Lane::Value, "A", "core-check-callables-913", true),
    ("map", "with_value", Lane::Value, "A", "core-check-callables-913", true),
    ("map", "index_len", Lane::Value, "A", "core-check-callables-913", true),
    ("map", "index_get", Lane::Value, "A", "core-check-callables-913", true),
    ("map", "index_set", Lane::Value, "A", "core-check-callables-913", true),
    ("map", "find", Lane::PendingCode, "C", "core-check-callables-913", true),
    ("map", "new", Lane::Value, "A", "core-check-callables-913", false),
    ("map", "len", Lane::Value, "A", "core-check-callables-913", false),
    ("map", "get", Lane::Value, "C", "core-check-callables-913", false),
    ("map", "insert", Lane::Value, "C", "core-check-callables-913", false),
    ("map", "remove", Lane::Value, "C", "core-check-callables-913", false),
    ("map", "update", Lane::Value, "C", "core-collections-918", false),
    ("map", "reserve", Lane::Value, "C", "std-seed-api-889", false),
    ("float", "sqrt", Lane::Numeric, "A", "core-check-895", false),
    ("float", "abs", Lane::Numeric, "A", "core-check-895", false),
    ("float", "floor", Lane::Numeric, "A", "core-check-895", false),
    ("float", "ceil", Lane::Numeric, "A", "core-check-895", false),
    ("float", "trunc", Lane::Numeric, "A", "core-check-895", false),
    ("float", "round", Lane::Numeric, "A", "core-check-895", false),
    ("float", "round_even", Lane::Numeric, "A", "core-check-895", false),
    ("option", "unwrap", Lane::Value, "A", "core-check-calls-910", false),
    ("rc", "new", Lane::Runtime(Kernel::NewOwner(Some(Counted::Rc))), "A", "core-ops-914", false),
    ("rc", "share", Lane::Runtime(Kernel::Share(Counted::Rc)), "A", "core-ops-914", false),
    ("rc", "try_unwrap", Lane::Runtime(Kernel::TryUnwrap(Counted::Rc)), "A", "core-ops-914", false),
    ("rc", "with_unique", Lane::Runtime(Kernel::WithUnique(Counted::Rc)), "A", "core-ops-914", false),
    ("result", "unwrap", Lane::Value, "A", "core-check-calls-910", false),
    ("seq", "len", Lane::Language, "A", "core-check-895", false),
    ("seq", "nth", Lane::Language, "A", "core-check-895", false),
    ("seq", "index_in", Lane::Language, "A", "core-check-895", false),
    ("seq", "get", Lane::Language, "A", "core-check-895", false),
    ("span", "builder", Lane::Language, "A", "core-check-895", false),
    ("span", "with_capacity", Lane::Language, "A", "core-check-895", false),
    ("span", "push", Lane::Language, "A", "core-check-895", false),
    ("span", "finish", Lane::Language, "A", "core-check-895", false),
    ("span", "len", Lane::Language, "A", "core-check-895", false),
    ("span", "get", Lane::Language, "A", "core-check-895", false),
    ("stderr", "write", Lane::Runtime(Kernel::Stream(HostOp::Stderr)), "B", "core-ops-914", false),
    ("stdout", "write", Lane::Runtime(Kernel::Stream(HostOp::Stdout)), "B", "core-ops-914", false),
    ("stdout", "write_bytes", Lane::Runtime(Kernel::Stream(HostOp::Stdout)), "B", "core-ops-914", false),
    ("stderr", "write_bytes", Lane::Runtime(Kernel::Stream(HostOp::Stderr)), "B", "core-ops-914", false),
    ("host", "connect", Lane::Runtime(Kernel::Net(HostOp::Connect)), "B", "wire-w1a-1063", false),
    ("host", "listen", Lane::Runtime(Kernel::Net(HostOp::Listen)), "B", "wire-w1a-1063", false),
    ("host", "accept", Lane::Runtime(Kernel::Net(HostOp::Accept)), "B", "wire-w1a-1063", false),
    ("host", "recv_bounded", Lane::Runtime(Kernel::Net(HostOp::RecvBounded)), "B", "wire-w1a-1063", false),
    ("host", "send_pair", Lane::Runtime(Kernel::Net(HostOp::SendPair)), "B", "wire-w1a-1063", false),
    ("net", "narrow", Lane::Runtime(Kernel::Net(HostOp::NetNarrow)), "B", "wire-w1a-1063", false),
    ("net", "split", Lane::Runtime(Kernel::Net(HostOp::NetSplit)), "B", "wire-w1a-1063", false),
    ("string", "new", Lane::Runtime(Kernel::StringList("new")), "A", "core-ops-914", false),
    ("string", "as_bytes", Lane::Runtime(Kernel::CarrierView), "A", "core-ops-914", false),
    ("string", "slice", Lane::PendingCode, "C", "std-seed-api-889", false),
    ("string", "byte_len", Lane::Runtime(Kernel::StringList("len")), "A", "core-ops-914", false),
    ("string", "push", Lane::Runtime(Kernel::StringList("append")), "A", "core-ops-914", false),
    ("string", "reserve", Lane::Runtime(Kernel::StringList("reserve")), "A", "core-ops-914", false),
    ("string", "of_bytes", Lane::Runtime(Kernel::CarrierMove), "A", "core-ops-914", true),
    ("string", "to_bytes", Lane::Runtime(Kernel::CarrierMove), "A", "core-ops-914", true),
    ("string", "view_string", Lane::Runtime(Kernel::CarrierView), "A", "core-ops-914", true),
    ("string", "shorten", Lane::Runtime(Kernel::StringShorten), "A", "core-ops-914", true),
    ("task", "spawn", Lane::Runtime(Kernel::Spawn), "B", "core-ops-914", false),
    ("task", "join", Lane::Runtime(Kernel::Join), "B", "core-ops-914", false),
    ("task", "cancel", Lane::Runtime(Kernel::Cancel), "B", "core-ops-914", false),
    ("task", "check_cancel", Lane::Runtime(Kernel::CheckCancel), "B", "core-ops-914", false),
    ("text", "len", Lane::Language, "A", "core-check-895", false),
    ("text", "as_string", Lane::Language, "A", "core-check-895", false),
    ("text", "copy_in", Lane::Language, "A", "core-check-895", false),
    ("unique", "new", Lane::Runtime(Kernel::Unique), "B", "core-ops-914", false),
    ("writer", "append", Lane::Language, "A", "core-check-895", false),
    ("writer", "scalar", Lane::Language, "A", "core-check-895", false),
    ("writer", "push", Lane::Language, "A", "core-check-895", false),
    ("writer", "close", Lane::Language, "A", "core-check-895", false),
    ("writer", "completed", Lane::Language, "A", "core-check-895", false),
    ("writer", "last_completed", Lane::Language, "A", "core-check-895", false),
}

pub(crate) fn binding(f: &FnDecl) -> Option<u16> {
    if f.path.package_is("store", "1")
        && f.path.0.modules == ["runner"]
        && matches!(
            f.path.0.name.as_str(),
            "launch_word" | "launch_text" | "launch_emit"
        )
    {
        return Some(match f.path.0.name.as_str() {
            "launch_word" => u16::MAX,
            "launch_text" => u16::MAX - 1,
            _ => u16::MAX - 2,
        });
    }
    if !f.path.package_is("core", "1") || f.body.is_some() {
        return None;
    }
    OPERATIONS
        .iter()
        .position(|entry| {
            f.path.0.modules == [entry.module]
                && f.path.0.name == entry.name
                && matches!(entry.lane, Lane::Runtime(_) | Lane::PendingCode)
        })
        .map(|i| i as u16)
}

impl<'c, 'a> Cx<'c, 'a, '_> {
    pub(super) fn core_operation(
        &mut self,
        f: usize,
        types: &[TyId],
        vals: &[Option<ValueId>],
        result: TyId,
        n: NodeId,
    ) -> Option<Option<ValueId>> {
        if self.p.core_operations[f] == Some(u16::MAX - 2) {
            return Some(
                self.runtime_row(RtOp::DescribeEmit, &[Arg::Value(vals[0]?)], n)
                    .map(|v| v[0]),
            );
        }
        if self.p.core_operations[f] == Some(u16::MAX - 1) {
            let view = self.runtime_type(result, n)?;
            let output = self.new_slot(PlaceTy::Declared(view), n);
            self.runtime_row(RtOp::LaunchTarget { view }, &[Arg::Address(output)], n)?;
            return Some(Some(self.pointer(output, n)));
        }
        if self.p.core_operations[f] == Some(u16::MAX) {
            return Some(
                self.runtime_row(
                    RtOp::LaunchWord,
                    &[Arg::Value(vals[0]?), Arg::Value(vals[1]?)],
                    n,
                )
                .map(|v| v[0]),
            );
        }
        match OPERATIONS[self.p.core_operations[f]? as usize].lane {
            Lane::Runtime(kernel) => Some(self.runtime_kernel(kernel, f, types, vals, result, n)),
            Lane::PendingCode => self.pending_code(f, types, vals, result, n),
            _ => None,
        }
    }

    fn pending_code(
        &mut self,
        f: usize,
        _types: &[TyId],
        vals: &[Option<ValueId>],
        _result: TyId,
        n: NodeId,
    ) -> Option<Option<ValueId>> {
        let entry = &OPERATIONS[self.p.core_operations[f]? as usize];
        let args = self.kids(n);
        match (entry.module, entry.name) {
            ("core", "isolate") => {
                let argument = args.get(1).copied().unwrap_or(n);
                let forbidden = vals
                    .first()
                    .copied()
                    .flatten()
                    .is_some_and(|value| self.isolate_descriptor_forbidden(Some(value)))
                    || args.get(1).is_some_and(|arg| {
                        self.p
                            .target(self.at, *arg)
                            .is_some_and(|target| match target {
                                Target::Binder(slot) => {
                                    let var = self.var(*slot);
                                    matches!(var.origin, Origin::ViewParam | Origin::MutParam)
                                        && self.contains_callback_kind::<true>(var.ty)
                                }
                                _ => false,
                            })
                    });
                if forbidden {
                    self.refuse(
                        "FAIL-16",
                        argument,
                        "isolate's body may not capture a mutable place",
                    );
                } else {
                    let body_ty = self.p.fns[f]
                        .params
                        .first()
                        .map(|p| subst(&mut self.t, p.ty, _types))
                        .unwrap_or(ERROR);
                    if let Some(body) = vals.first().copied().flatten() {
                        return Some(self.lower_isolate(body_ty, body, _result, n));
                    }
                }
                Some(None)
            }
            _ => None,
        }
    }

    /// The function item `code.describe` names, refused unless it is a top-level
    /// function whose generics are all regions (PROG-9).
    fn code_describe_target(&mut self, n: NodeId) -> Option<usize> {
        let argument = self.kids(n).get(1).copied().unwrap_or(n);
        let item = self.p.target(self.at, argument).and_then(|target| {
            let Target::Definition(id) = target else {
                return None;
            };
            let Some(Def::Fn(index)) = self.p.by_definition[id.0 as usize] else {
                return None;
            };
            let function = &self.p.fns[index as usize];
            function
                .generics
                .iter()
                .all(|generic| generic.region)
                .then_some(index as usize)
        });
        if item.is_none() {
            self.refuse(
                "PROG-9",
                argument,
                "code.describe requires a top-level non-generic function name",
            );
        }
        item
    }

    /// Lower `core.isolate`'s value boundary (FORM #101).  The callback's failure
    /// successor enters `handler`, whose first instruction is the catch row; the
    /// ordinary result slot is initialized independently on each of the three exits.
    fn lower_isolate(
        &mut self,
        body_ty: TyId,
        body: ValueId,
        result: TyId,
        n: NodeId,
    ) -> Option<ValueId> {
        let Some((_, inner, Some(capacity))) = self.result_parts(result) else {
            self.gap(n, "isolate result shape is not built");
            return None;
        };
        let Some((_, payload, Some(causes))) = self.result_parts(inner) else {
            self.gap(n, "isolate result shape is not built");
            return None;
        };
        let (Ok(Rep::Place(result_id)), Ok(Rep::Place(inner_id)), Ok(Rep::Place(capacity_id))) =
            (self.rep(result), self.rep(inner), self.rep(capacity))
        else {
            self.gap(n, "isolate result representation is not built");
            return None;
        };
        if !self.live() {
            return None;
        }

        let to = self.new_slot(PlaceTy::Declared(result_id), n);
        let [entered, room, ..] = match self.runtime_row(RtOp::IsolateEnter, &[], n) {
            Some(values) => values,
            None => return None,
        };
        let body_block = self.new_block(&[]);
        let refused_block = self.new_block(&[]);
        let handler = self.new_block(&[]);
        let join = self.new_block(&[]);
        self.runtime_branch(entered, body_block, refused_block, n);

        self.start(body_block);
        let returned = self.call_lowered_fail(body_ty, body, n, handler);
        self.runtime_row(RtOp::IsolateLeave, &[], n);
        self.inst(
            Inst::SetCase {
                address: to,
                case: 0,
            },
            n,
        );
        let inside = self.address(
            AddressBase::Address(to),
            &[Step::CaseField { case: 0, field: 0 }],
            PlaceTy::Declared(inner_id),
            n,
        );
        self.inst(
            Inst::SetCase {
                address: inside,
                case: 0,
            },
            n,
        );
        if let Some(rep) = self.rep(payload).ok()
            && let Some(value) = returned
            && let Some(place) = rep.stored()
        {
            let at = self.address(
                AddressBase::Address(inside),
                &[Step::CaseField { case: 0, field: 0 }],
                place,
                n,
            );
            self.install(at, rep, Some(value), n);
        }
        self.jump(join, &[], n);

        self.start(handler);
        let [caught, ..] = match self.runtime_row(RtOp::IsolateCatch, &[], n) {
            Some(values) => values,
            None => return None,
        };
        self.inst(
            Inst::SetCase {
                address: to,
                case: 0,
            },
            n,
        );
        let inside = self.address(
            AddressBase::Address(to),
            &[Step::CaseField { case: 0, field: 0 }],
            PlaceTy::Declared(inner_id),
            n,
        );
        self.inst(
            Inst::SetCase {
                address: inside,
                case: 1,
            },
            n,
        );
        if let Some(at) =
            self.runtime_field(inside, &[Step::CaseField { case: 1, field: 0 }], causes, n)
        {
            self.runtime_store(at, &[Step::Field(0)], Ty::Ptr, caught, n);
        }
        self.jump(join, &[], n);

        self.start(refused_block);
        self.inst(
            Inst::SetCase {
                address: to,
                case: 1,
            },
            n,
        );
        let capacity_at = self.address(
            AddressBase::Address(to),
            &[Step::CaseField { case: 1, field: 0 }],
            PlaceTy::Declared(capacity_id),
            n,
        );
        let requested = match self.konst(
            Ty::Scalar(Scalar::U64),
            hirsel_core_layout::charge::CAUSE_CELL,
            n,
        ) {
            Some(value) => value,
            None => return None,
        };
        self.runtime_capacity(capacity_at, requested, room, n);
        self.jump(join, &[], n);

        self.start(join);
        let value = self.pointer(to, n);
        self.temp(result, Some(value));
        // The operation itself synthesizes a value; its caller owns the result slot.
        Some(value)
    }

    fn runtime_kernel(
        &mut self,
        kernel: Kernel,
        f: usize,
        types: &[TyId],
        vals: &[Option<ValueId>],
        result: TyId,
        n: NodeId,
    ) -> Option<ValueId> {
        match kernel {
            Kernel::Unique => self
                .runtime_row(RtOp::DestId, &[], n)
                .map(|values| values[0]),
            Kernel::StringShorten => {
                // A String's carrier is the List[u8] row's (stride 1, no bound).
                let carrier = self.list_carrier(scalar(Scalar::U8), n)?;
                self.set_carrier_word(vals[0]?, carrier, SeqWord::Len, vals[1]?, n);
                None
            }
            Kernel::CarrierView => self.runtime_carrier_view(vals[0]?, result, n),
            Kernel::CarrierMove => {
                self.consume(vals[0], n);
                self.temp(result, vals[0]);
                vals[0]
            }
            Kernel::StringList(name) => {
                if !self.live() {
                    return None;
                }
                match self.list_value(name, scalar(Scalar::U8), vals, result, n) {
                    Some(value) => value,
                    None => {
                        self.gap(
                            n,
                            match name {
                                "reserve" => "shared List reserve kernel is not built",
                                "append" => "shared List append kernel is not built",
                                _ => "shared List carrier kernel is not built",
                            },
                        );
                        None
                    }
                }
            }

            Kernel::NewOwner(kind) => self.runtime_new_owner(kind, types[0], vals[0], result, n),
            Kernel::Share(kind) => self.runtime_share(kind, vals[0]?, result, n),
            Kernel::TryUnwrap(kind) => self.runtime_try_unwrap(kind, types[0], vals[0]?, result, n),
            Kernel::WithUnique(kind) => self.runtime_with_unique(kind, types[0], vals, result, n),
            Kernel::Unbox => self.runtime_unbox(types[0], vals[0]?, n),
            Kernel::ArgsLen => self
                .runtime_row(RtOp::ArgsLen, &[Arg::Value(vals[0]?)], n)
                .map(|values| values[0]),
            Kernel::ArgsGet => self.runtime_args_get(vals, result, n),
            Kernel::Stream(op) => self.runtime_stream(op, vals, result, n),
            Kernel::Host(op) => self.runtime_host(op, f, vals, result, n),
            Kernel::Net(op) => self.runtime_net(op, f, vals, result, n),
            Kernel::Spawn => self.runtime_spawn(
                types[0], types[1], vals[0]?, vals[1]?, vals[2], vals[3]?, result, n,
            ),
            Kernel::CheckCancel => {
                self.runtime_row(RtOp::CheckCancel, &[], n)?;
                None
            }
            Kernel::Cancel => {
                let handle = self.runtime_handle(vals[0]?, n);
                self.runtime_row(RtOp::Cancel, &[Arg::Value(handle)], n)?;
                None
            }
            Kernel::ChannelSender => {
                let handle = self.runtime_handle(vals[0]?, n);
                let [handle, ..] =
                    self.runtime_row(RtOp::ChannelSender, &[Arg::Value(handle)], n)?;
                self.runtime_carrier(result, handle, n)
            }
            Kernel::ChannelNew => self.runtime_channel(types[0], vals[0]?, result, n),
            Kernel::LatestNew => self.runtime_latest_new(vals[0], result, n),
            Kernel::LatestHandle => {
                let handle = self.runtime_handle(vals[0]?, n);
                let [handle, ..] =
                    self.runtime_row(RtOp::LatestHandle, &[Arg::Value(handle)], n)?;
                self.runtime_carrier(result, handle, n)
            }
            Kernel::LatestPublish => {
                let handle = self.runtime_handle(vals[0]?, n);
                let node = self.runtime_handle(vals[1]?, n);
                self.runtime_row(
                    RtOp::LatestPublish,
                    &[Arg::Value(handle), Arg::Value(node)],
                    n,
                )?;
                self.consume(vals[1], n);
                None
            }
            Kernel::LatestRead => {
                let handle = self.runtime_handle(vals[0]?, n);
                let [node, ..] = self.runtime_row(RtOp::LatestRead, &[Arg::Value(handle)], n)?;
                self.runtime_carrier(result, node, n)
            }
            Kernel::Send | Kernel::TrySend => {
                self.runtime_send(kernel, types[0], vals[0]?, vals[1], result, n)
            }
            Kernel::Receive => self.runtime_receive(types[0], vals[0]?, result, n),
            Kernel::TryReceive => self.runtime_try_receive(types[0], vals[0]?, result, n),
            Kernel::Join => self.runtime_join(types[0], vals[0]?, result, n),
            Kernel::LogNew => self.runtime_log_new(types[0], result, n),
            Kernel::LogAppend => self.runtime_log_append(types[0], vals[0]?, vals[1], result, n),
            Kernel::LogFinish => self.runtime_log_finish(types[0], vals[0]?, result, n),
            Kernel::CodeIdentity => self.runtime_code_identity(vals[0]?, result, n),
            Kernel::CodeRevision => self.runtime_code_revision(vals[0]?, result, n),
            Kernel::CodeCallable => self.runtime_code_callable(vals[0]?, result, n),
            Kernel::CodeDescribe => self.runtime_code_describe(result, n),
            Kernel::Flow(op) => self.runtime_flow(op, types, vals, result, n),
            Kernel::TapeOpen => self.runtime_tape_open(f, vals, result, n),
            Kernel::TapeEnd(returned) => {
                let tape = self.runtime_key(vals[0]?, n);
                // END consumes the Tape on every path: the runtime takes its key.
                self.consume(vals[0], n);
                let (kind, operand) = if returned {
                    (self.konst(Ty::Scalar(Scalar::U8), 0, n)?, vals[1]?)
                } else {
                    (vals[1]?, self.runtime_handle(vals[2]?, n))
                };
                let [code, ..] = self.runtime_row(
                    RtOp::TapeEnd,
                    &[Arg::Value(tape), Arg::Value(kind), Arg::Value(operand)],
                    n,
                )?;
                Some(code)
            }
            Kernel::TapeGrant(kind) => {
                let tape = self.runtime_key(vals[0]?, n);
                let kind = self.konst(Ty::Scalar(Scalar::U8), u64::from(kind), n)?;
                let [value, ..] = self.runtime_row(
                    RtOp::TapeGrant,
                    &[Arg::Value(tape), Arg::Value(kind), Arg::Value(vals[1]?)],
                    n,
                )?;
                Some(value)
            }
            Kernel::TapeAcross => {
                let tape = self.runtime_key(vals[0]?, n);
                let [differs, ..] = self.runtime_row(RtOp::TapeAcross, &[Arg::Value(tape)], n)?;
                Some(differs)
            }
        }
    }

    /// Runtime rows use their own fixed ABI, with the context supplied by the engine.
    /// A call's failure edge is captured before any normal-result custody changes.
    pub(super) fn runtime_row(
        &mut self,
        op: RtOp,
        args: &[Arg],
        n: NodeId,
    ) -> Option<[ValueId; 4]> {
        self.runtime_outputs(op, args, n)
    }
    fn runtime_outputs<const N: usize>(
        &mut self,
        op: RtOp,
        args: &[Arg],
        n: NodeId,
    ) -> Option<[ValueId; N]> {
        if !self.live() {
            return None;
        }
        let signature = self.e.sig(op.signature());
        let mut output = [ValueId(0); N];
        let count = self.e.module.signatures[signature.index()].results.len();
        for (i, value) in output.iter_mut().enumerate().take(count) {
            *value = self.value(self.e.module.signatures[signature.index()].results[i]);
        }
        let args = PoolRange::push(&mut self.b.pools.args, args.iter().copied());
        let results = PoolRange::push(&mut self.b.pools.values, output[..count].iter().copied());
        let fail = if op.fails() { self.fail_to(n) } else { None };
        let call = CallId(self.b.calls.len() as u32);
        self.b.calls.push(CallSite {
            callee: Callee::Runtime(op),
            signature,
            args,
            results,
            fail,
            cold: false,
        });
        self.inst(Inst::Call { call }, n);
        Some(output)
    }

    fn runtime_handle(&mut self, value: ValueId, n: NodeId) -> ValueId {
        let field = self.address(AddressBase::Value(value), &[], PlaceTy::Value(Ty::Ptr), n);
        self.read_bits(field, Ty::Ptr, n)
    }

    pub(super) fn runtime_type(&mut self, ty: TyId, n: NodeId) -> Option<TypeId> {
        let id = self.e.type_row(self.p, &mut self.t, ty);
        if id.is_none() {
            self.gap(n, "runtime operand layout is not built");
        }
        id
    }

    fn runtime_place(
        &mut self,
        ty: TyId,
        value: Option<ValueId>,
        n: NodeId,
    ) -> Option<(TypeId, AddressId)> {
        let id = self.runtime_type(ty, n)?;
        let rep = match self.rep(ty) {
            Ok(rep) => rep,
            Err(what) => {
                self.gap(n, what);
                return None;
            }
        };
        let place = match rep {
            Rep::Place(_) => {
                self.address(AddressBase::Value(value?), &[], PlaceTy::Declared(id), n)
            }
            _ => {
                let slot = self.new_slot(PlaceTy::Declared(id), n);
                if let Some(stored) = rep.stored() {
                    let field =
                        self.address(AddressBase::Address(slot), &[Step::Field(0)], stored, n);
                    self.install(field, rep, value, n);
                }
                slot
            }
        };
        Some((id, place))
    }

    fn runtime_field(
        &mut self,
        to: AddressId,
        steps: &[Step],
        ty: TyId,
        n: NodeId,
    ) -> Option<AddressId> {
        let rep = match self.rep(ty) {
            Ok(rep) => rep,
            Err(what) => {
                self.gap(n, what);
                return None;
            }
        };
        // A zero-sized field has no address: its callers store nothing (FORM #120).
        let Some(place) = rep.stored() else {
            return None;
        };
        let at = self.address(AddressBase::Address(to), steps, place, n);
        // A builtin handle field is stored directly in its parent's recipe.
        // Rebase its carrier before addressing fields of that carrier.
        if let PlaceTy::Declared(_) = place {
            let pointer = self.pointer(at, n);
            Some(self.address(AddressBase::Value(pointer), &[], place, n))
        } else {
            Some(at)
        }
    }

    fn runtime_store(&mut self, to: AddressId, steps: &[Step], ty: Ty, value: ValueId, n: NodeId) {
        let address = self.address(AddressBase::Address(to), steps, PlaceTy::Value(ty), n);
        self.inst(Inst::Store { address, value }, n);
    }

    fn runtime_carrier(&mut self, ty: TyId, handle: ValueId, n: NodeId) -> Option<ValueId> {
        let id = self.runtime_type(ty, n)?;
        let to = self.new_slot(PlaceTy::Declared(id), n);
        self.runtime_store(to, &[Step::Field(0)], Ty::Ptr, handle, n);
        let value = self.pointer(to, n);
        self.temp(ty, Some(value));
        Some(value)
    }

    fn runtime_compare(&mut self, ty: Ty, value: ValueId, bits: u64, n: NodeId) -> Option<ValueId> {
        let right = self.konst(ty, bits, n)?;
        let result = self.value(Ty::Scalar(Scalar::Bool));
        self.inst(
            Inst::Compare {
                result,
                op: CompareOp::Eq,
                left: value,
                right,
            },
            n,
        );
        Some(result)
    }

    fn runtime_nonzero(&mut self, ty: Ty, value: ValueId, n: NodeId) -> Option<ValueId> {
        let right = self.konst(ty, 0, n)?;
        let result = self.value(Ty::Scalar(Scalar::Bool));
        self.inst(
            Inst::Compare {
                result,
                op: CompareOp::Ne,
                left: value,
                right,
            },
            n,
        );
        Some(result)
    }

    pub(super) fn runtime_branch(
        &mut self,
        condition: ValueId,
        yes: BlockId,
        no: BlockId,
        n: NodeId,
    ) {
        let yes = self.edge(yes, &[]);
        let no = self.edge(no, &[]);
        self.end(Terminator::Branch { condition, yes, no }, Some(n));
    }

    /// Both arms initialize a Result or Option's tag and active payload. Its owner
    /// starts only after the join, so failure before the call commits cleans inputs.
    pub(super) fn runtime_result(
        &mut self,
        ty: TyId,
        ok: ValueId,
        n: NodeId,
        success: impl FnOnce(&mut Self, AddressId, TyId),
        error: impl FnOnce(&mut Self, AddressId, TyId),
    ) -> Option<ValueId> {
        // A zero-sized result (`sleep_until`'s unit) has nothing to build: the call
        // already ran.
        if self.rep_at(ty, n)? == Rep::None {
            return None;
        }
        let TyKind::Adt(_, arguments) = self.t.kind(ty).clone() else {
            self.gap(
                n,
                "a runtime result that is not a declared type is not built",
            );
            return None;
        };
        let (yes_case, no_case) = if arguments.len() == 1 { (1, 0) } else { (0, 1) };
        let id = self.runtime_type(ty, n)?;
        let to = self.new_slot(PlaceTy::Declared(id), n);
        let yes = self.new_block(&[]);
        let no = self.new_block(&[]);
        let join = self.new_block(&[]);
        self.runtime_branch(ok, yes, no, n);
        self.start(yes);
        self.inst(
            Inst::SetCase {
                address: to,
                case: yes_case,
            },
            n,
        );
        success(self, to, arguments[0]);
        self.jump(join, &[], n);
        self.start(no);
        self.inst(
            Inst::SetCase {
                address: to,
                case: no_case,
            },
            n,
        );
        error(self, to, arguments.get(1).copied().unwrap_or(UNIT));
        self.jump(join, &[], n);
        self.start(join);
        if !self.live() {
            return None;
        }
        let value = self.pointer(to, n);
        self.temp(ty, Some(value));
        Some(value)
    }

    /// `latest.new`: the root admits `LATEST_ROOT` from the current limit; a
    /// refusal hands the taken `Arc[T]` back in the error (CONC-13).
    fn runtime_latest_new(
        &mut self,
        arc: Option<ValueId>,
        result: TyId,
        n: NodeId,
    ) -> Option<ValueId> {
        // The refusal pair `(Capacity, Arc[T])` names the root's item type.
        let TyKind::Adt(_, outcome) = self.t.kind(result).clone() else {
            self.gap(n, "a latest.new result that is not a Result is not built");
            return None;
        };
        let TyKind::Tuple(pair) = self.t.kind(outcome[1]).clone() else {
            self.gap(n, "a latest.new refusal that is not a pair is not built");
            return None;
        };
        let item = pair[1];
        let id = self.runtime_type(item, n)?;
        let rep = self.rep_at(item, n)?;
        let node = self.runtime_handle(arc?, n);
        let requested = self.konst(
            Ty::Scalar(Scalar::U64),
            hirsel_core_layout::charge::LATEST_ROOT,
            n,
        )?;
        let [latest, available, ..] =
            self.runtime_row(RtOp::LatestNew { item: id }, &[Arg::Value(node)], n)?;
        // The root owns the Arc on success; the error owns it on refusal.
        self.consume(arc, n);
        let ok = self.runtime_nonzero(Ty::Ptr, latest, n)?;
        self.runtime_result(
            result,
            ok,
            n,
            |cx, to, ty| {
                if let Some(at) =
                    cx.runtime_field(to, &[Step::CaseField { case: 0, field: 0 }], ty, n)
                {
                    cx.runtime_store(at, &[Step::Field(0)], Ty::Ptr, latest, n);
                }
            },
            |cx, to, pair| {
                let TyKind::Tuple(parts) = cx.t.kind(pair).clone() else {
                    return;
                };
                if let Some(to) =
                    cx.runtime_field(to, &[Step::CaseField { case: 1, field: 0 }], pair, n)
                {
                    if let Some(at) = cx.runtime_field(to, &[Step::Field(0)], parts[0], n) {
                        cx.runtime_capacity(at, requested, available, n);
                    }
                    if let Some(at) = cx.runtime_field(to, &[Step::Field(1)], item, n) {
                        cx.install(at, rep, arc, n);
                    }
                }
            },
        )
    }

    fn runtime_channel(
        &mut self,
        item: TyId,
        cap: ValueId,
        result: TyId,
        n: NodeId,
    ) -> Option<ValueId> {
        let id = self.runtime_type(item, n)?;
        // Charge-v1 uses logical size, relocated with the generic instance.
        let size = self.layout_size(item, true, n)?;
        let u = Ty::Scalar(Scalar::U64);
        let base = self.konst(u, hirsel_core_layout::charge::CHANNEL_BASE, n)?;
        let requested = self.runtime_charge(cap, size, base, n)?;
        let [sender, receiver, available, ..] =
            self.runtime_row(RtOp::ChannelNew { item: id }, &[Arg::Value(cap)], n)?;
        let ok = self.runtime_nonzero(Ty::Ptr, sender, n)?;
        self.runtime_result(
            result,
            ok,
            n,
            |cx, to, pair| {
                let TyKind::Tuple(types) = cx.t.kind(pair).clone() else {
                    return;
                };
                if let Some(to) =
                    cx.runtime_field(to, &[Step::CaseField { case: 0, field: 0 }], pair, n)
                {
                    for (i, (ty, handle)) in types.into_iter().zip([sender, receiver]).enumerate() {
                        if let Some(field) = cx.runtime_field(to, &[Step::Field(i as u32)], ty, n) {
                            cx.runtime_store(field, &[Step::Field(0)], Ty::Ptr, handle, n);
                        }
                    }
                }
            },
            |cx, to, capacity| {
                if let Some(to) =
                    cx.runtime_field(to, &[Step::CaseField { case: 1, field: 0 }], capacity, n)
                {
                    cx.runtime_capacity(to, requested, available, n);
                }
            },
        )
    }

    /// Saturated exact charge. Overflow is a recoverable refusal by the runtime;
    /// it is not a source Overflow failure while building the Capacity record.
    fn runtime_charge(
        &mut self,
        count: ValueId,
        size: ValueId,
        base: ValueId,
        n: NodeId,
    ) -> Option<ValueId> {
        self.runtime_charge_parts(count, size, base, false, n)
            .map(|p| p.0)
    }

    fn runtime_charge_parts(
        &mut self,
        count: ValueId,
        size: ValueId,
        base: ValueId,
        round: bool,
        n: NodeId,
    ) -> Option<(ValueId, ValueId)> {
        let u = Ty::Scalar(Scalar::U64);
        let product = self.value(u);
        let mul_overflow = self.value(Ty::Scalar(Scalar::Bool));
        self.inst(
            Inst::IntArith {
                result: product,
                flag: Some(mul_overflow),
                op: ArithOp::Mul,
                mode: ArithMode::Checked,
                left: count,
                right: size,
            },
            n,
        );
        let total = self.value(u);
        let add_overflow = self.value(Ty::Scalar(Scalar::Bool));
        self.inst(
            Inst::IntArith {
                result: total,
                flag: Some(add_overflow),
                op: ArithOp::Add,
                mode: ArithMode::Checked,
                left: product,
                right: base,
            },
            n,
        );
        let overflow = self.value(Ty::Scalar(Scalar::Bool));
        self.inst(
            Inst::Binary {
                result: overflow,
                op: BinaryOp::BitOr,
                left: mul_overflow,
                right: add_overflow,
                fail: None,
            },
            n,
        );
        let total = if round {
            let mask = self.konst(u, !7, n)?;
            let aligned = self.value(u);
            self.inst(
                Inst::Binary {
                    result: aligned,
                    op: BinaryOp::BitAnd,
                    left: total,
                    right: mask,
                    fail: None,
                },
                n,
            );
            aligned
        } else {
            total
        };
        let max = self.konst(u, u64::MAX, n)?;
        let yes = self.new_block(&[]);
        let no = self.new_block(&[]);
        let join = self.new_block(&[u]);
        self.runtime_branch(overflow, yes, no, n);
        self.start(yes);
        self.jump(join, &[max], n);
        self.start(no);
        self.jump(join, &[total], n);
        self.start(join);
        Some((
            self.b.blocks[join.index()]
                .params
                .get(&self.b.pools.values)
                .expect("the join block just built holds its parameter")[0],
            overflow,
        ))
    }

    pub(super) fn runtime_capacity(
        &mut self,
        to: AddressId,
        requested: ValueId,
        available: ValueId,
        n: NodeId,
    ) {
        for (field, value) in [requested, available].into_iter().enumerate() {
            self.runtime_store(
                to,
                &[Step::Field(field as u32)],
                Ty::Scalar(Scalar::U64),
                value,
                n,
            );
        }
    }

    fn runtime_send(
        &mut self,
        kernel: Kernel,
        item: TyId,
        sender: ValueId,
        value: Option<ValueId>,
        result: TyId,
        n: NodeId,
    ) -> Option<ValueId> {
        let sender = self.runtime_handle(sender, n);
        let (id, input) = self.runtime_place(item, value, n)?;
        let op = if kernel == Kernel::Send {
            RtOp::Send { item: id }
        } else {
            RtOp::TrySend { item: id }
        };
        let [status, ..] = self.runtime_row(op, &[Arg::Value(sender), Arg::Address(input)], n)?;
        let ok = if kernel == Kernel::Send {
            status
        } else {
            self.runtime_compare(Ty::Scalar(Scalar::U8), status, 0, n)?
        };
        // Both a published item and a returned error now own the taken value.
        self.consume(value, n);
        self.runtime_result(
            result,
            ok,
            n,
            |_, _, _| {},
            |cx, to, error| {
                if let Some(to) =
                    cx.runtime_field(to, &[Step::CaseField { case: 1, field: 0 }], error, n)
                {
                    if kernel == Kernel::Send {
                        cx.runtime_closed(to, item, value, n);
                    } else {
                        let full = cx.runtime_compare(Ty::Scalar(Scalar::U8), status, 1, n);
                        if let Some(full) = full {
                            let yes = cx.new_block(&[]);
                            let no = cx.new_block(&[]);
                            let join = cx.new_block(&[]);
                            cx.runtime_branch(full, yes, no, n);
                            cx.start(yes);
                            cx.runtime_case_payload(to, 0, item, value, n);
                            cx.jump(join, &[], n);
                            cx.start(no);
                            cx.runtime_case_payload(to, 1, item, value, n);
                            cx.jump(join, &[], n);
                            cx.start(join);
                        }
                    }
                }
            },
        )
    }

    fn runtime_case_payload(
        &mut self,
        to: AddressId,
        case: u32,
        ty: TyId,
        value: Option<ValueId>,
        n: NodeId,
    ) {
        self.inst(Inst::SetCase { address: to, case }, n);
        let rep = match self.rep(ty) {
            Ok(r) => r,
            Err(what) => {
                self.gap(n, what);
                return;
            }
        };
        if let Some(at) = self.runtime_field(to, &[Step::CaseField { case, field: 0 }], ty, n) {
            self.install(at, rep, value, n);
        }
    }

    fn runtime_closed(&mut self, to: AddressId, ty: TyId, value: Option<ValueId>, n: NodeId) {
        self.runtime_case_payload(to, 0, ty, value, n);
    }

    fn runtime_receive(
        &mut self,
        item: TyId,
        receiver: ValueId,
        result: TyId,
        n: NodeId,
    ) -> Option<ValueId> {
        let receiver = self.runtime_handle(receiver, n);
        let id = self.runtime_type(item, n)?;
        let output = self.new_slot(PlaceTy::Declared(id), n);
        let [ok, ..] = self.runtime_row(
            RtOp::Receive { item: id },
            &[Arg::Value(receiver), Arg::Address(output)],
            n,
        )?;
        self.runtime_result(
            result,
            ok,
            n,
            |cx, to, ty| {
                let value = cx.runtime_read(output, ty, n);
                cx.runtime_case_payload(to, 0, ty, value, n);
            },
            |cx, to, error| {
                if let Some(to) =
                    cx.runtime_field(to, &[Step::CaseField { case: 1, field: 0 }], error, n)
                {
                    cx.runtime_closed(to, UNIT, None, n);
                }
            },
        )
    }

    fn runtime_try_receive(
        &mut self,
        item: TyId,
        receiver: ValueId,
        result: TyId,
        n: NodeId,
    ) -> Option<ValueId> {
        let receiver = self.runtime_handle(receiver, n);
        let id = self.runtime_type(item, n)?;
        let output = self.new_slot(PlaceTy::Declared(id), n);
        let [status, ..] = self.runtime_row(
            RtOp::TryReceive { item: id },
            &[Arg::Value(receiver), Arg::Address(output)],
            n,
        )?;
        let ok = self.runtime_compare(Ty::Scalar(Scalar::U8), status, 0, n)?;
        self.runtime_result(
            result,
            ok,
            n,
            |cx, to, ty| {
                let value = cx.runtime_read(output, ty, n);
                cx.runtime_case_payload(to, 0, ty, value, n);
            },
            |cx, to, error| {
                let Some(error_to) =
                    cx.runtime_field(to, &[Step::CaseField { case: 1, field: 0 }], error, n)
                else {
                    return;
                };
                let empty = cx.runtime_compare(Ty::Scalar(Scalar::U8), status, 1, n);
                let Some(empty) = empty else { return };
                let yes = cx.new_block(&[]);
                let no = cx.new_block(&[]);
                let join = cx.new_block(&[]);
                cx.runtime_branch(empty, yes, no, n);
                cx.start(yes);
                cx.runtime_case_payload(error_to, 0, UNIT, None, n);
                cx.jump(join, &[], n);
                cx.start(no);
                cx.runtime_case_payload(error_to, 1, UNIT, None, n);
                cx.jump(join, &[], n);
                cx.start(join);
            },
        )
    }

    fn runtime_read(&mut self, from: AddressId, ty: TyId, n: NodeId) -> Option<ValueId> {
        match self.rep(ty) {
            Ok(Rep::Place(_)) => Some(self.pointer(from, n)),
            Ok(Rep::Scalar(t)) => {
                let field = self.address(
                    AddressBase::Address(from),
                    &[Step::Field(0)],
                    PlaceTy::Value(t),
                    n,
                );
                Some(self.read_bits(field, t, n))
            }
            Ok(Rep::None) => None,
            Err(what) => {
                self.gap(n, what);
                None
            }
        }
    }

    fn runtime_join(
        &mut self,
        item: TyId,
        task: ValueId,
        result: TyId,
        n: NodeId,
    ) -> Option<ValueId> {
        let handle = self.runtime_handle(task, n);
        let id = self.runtime_type(item, n)?;
        let output = self.new_slot(PlaceTy::Declared(id), n);
        let [status, causes, ..] = self.runtime_row(
            RtOp::Join { r: id },
            &[Arg::Value(handle), Arg::Address(output)],
            n,
        )?;
        self.consume(Some(task), n);
        let ok = self.runtime_compare(Ty::Scalar(Scalar::U8), status, 0, n)?;
        self.runtime_result(
            result,
            ok,
            n,
            |cx, to, ty| {
                let value = cx.runtime_read(output, ty, n);
                cx.runtime_case_payload(to, 0, ty, value, n);
            },
            |cx, to, error| {
                let TyKind::Adt(decl, _) = cx.t.kind(error).clone() else {
                    return;
                };
                let DeclBody::Variant(cases) = &cx.p.decls[decl.index()].body else {
                    return;
                };
                let cause_ty = cases[0].fields[0].ty;
                if let Some(to) =
                    cx.runtime_field(to, &[Step::CaseField { case: 1, field: 0 }], error, n)
                {
                    if let Some(failed) = cx.runtime_compare(Ty::Scalar(Scalar::U8), status, 1, n) {
                        let yes = cx.new_block(&[]);
                        let no = cx.new_block(&[]);
                        let join = cx.new_block(&[]);
                        cx.runtime_branch(failed, yes, no, n);
                        for (block, case) in [(yes, 0), (no, 1)] {
                            cx.start(block);
                            cx.inst(Inst::SetCase { address: to, case }, n);
                            if let Some(field) = cx.runtime_field(
                                to,
                                &[Step::CaseField { case, field: 0 }],
                                cause_ty,
                                n,
                            ) {
                                cx.runtime_store(field, &[Step::Field(0)], Ty::Ptr, causes, n);
                            }
                            cx.jump(join, &[], n);
                        }
                        cx.start(join);
                    }
                }
            },
        )
    }
}

/// CAP-6: a kernel that observes outside its operands (the host, the standard streams,
/// the process arguments) is reached only through an operation with an authority
/// operand: a capability, `Args`, or a core resource made from one (CAP-7). The match
/// has no wildcard, so a new kernel is classified before it can bind.
#[cfg(test)]
pub(crate) fn assert_no_ambient_input(p: &Program<'_>) {
    fn ambient(k: Kernel) -> bool {
        match k {
            Kernel::Stream(_)
            | Kernel::Host(_)
            | Kernel::Net(_)
            | Kernel::ArgsLen
            | Kernel::ArgsGet
            | Kernel::TapeOpen
            | Kernel::TapeEnd(_)
            | Kernel::TapeGrant(_)
            | Kernel::TapeAcross => true,
            Kernel::CarrierMove
            | Kernel::CarrierView
            | Kernel::StringShorten
            | Kernel::StringList(_)
            | Kernel::Unique
            | Kernel::NewOwner(_)
            | Kernel::Share(_)
            | Kernel::TryUnwrap(_)
            | Kernel::WithUnique(_)
            | Kernel::Unbox
            | Kernel::Spawn
            | Kernel::CheckCancel
            | Kernel::Cancel
            | Kernel::Join
            | Kernel::ChannelNew
            | Kernel::LatestNew
            | Kernel::LatestHandle
            | Kernel::LatestPublish
            | Kernel::LatestRead
            | Kernel::ChannelSender
            | Kernel::Send
            | Kernel::TrySend
            | Kernel::TryReceive
            | Kernel::Receive
            | Kernel::LogNew
            | Kernel::LogAppend
            | Kernel::LogFinish
            | Kernel::CodeIdentity
            | Kernel::CodeRevision
            | Kernel::CodeCallable
            | Kernel::CodeDescribe
            | Kernel::Flow(_) => false,
        }
    }
    for (f, fd) in p.fns.iter().enumerate() {
        let Some(i) = p.core_operations[f] else {
            continue;
        };
        if i >= u16::MAX - 2 {
            continue;
        }
        let Lane::Runtime(kernel) = OPERATIONS[i as usize].lane else {
            continue;
        };
        if !ambient(kernel) {
            continue;
        }
        let authority = fd.params.iter().any(|param| match p.types.kind(param.ty) {
            TyKind::Adt(d, _) => {
                let decl = &p.decls[d.index()];
                crate::layout::handle(decl)
                    || decl.repr == crate::decls::Repr::Resource
                        && decl.path.package_is("core", "1")
            }
            _ => false,
        });
        assert!(
            authority,
            "CAP-6: {} observes outside its operands without an authority operand",
            fd.path
        );
    }
}

#[cfg(test)]
pub(crate) fn assert_declarations(p: &Program<'_>) {
    use std::collections::BTreeSet;
    let actual: BTreeSet<_> = p
        .fns
        .iter()
        .filter(|f| f.path.package_is("core", "1"))
        .map(|f| (f.path.0.modules[0].as_str(), f.path.0.name.as_str()))
        .collect();
    for entry in OPERATIONS {
        assert!(matches!(entry.class, "A" | "B" | "C"));
        if entry.private {
            assert!(
                entry.module == "string"
                    && matches!(
                        entry.name,
                        "of_bytes" | "to_bytes" | "view_string" | "shorten" | "push_scalar"
                    )
                    || entry.module == "map"
                    || entry.module == "list"
                        && (entry.name.starts_with("slots_")
                            || matches!(
                                entry.name,
                                "Hole"
                                    | "grow"
                                    | "precedes"
                                    | "sorted_run_end"
                                    | "merge_top"
                                    | "move_run"
                                    | "merge_runs"
                                    | "empty_merge"
                            ))
                    || matches!(entry.module, "deque" | "heap" | "tape")
            );
        }
    }
    let recorded: BTreeSet<_> = OPERATIONS.iter().map(|e| (e.module, e.name)).collect();
    assert_eq!(
        recorded.len(),
        OPERATIONS.len(),
        "duplicate core operation binding"
    );
    assert_eq!(
        actual, recorded,
        "assign each core declaration, bodied function or private kernel to its owning lane"
    );
}

impl<'c, 'a> Cx<'c, 'a, '_> {
    #[allow(clippy::too_many_arguments)]
    fn runtime_spawn(
        &mut self,
        x: TyId,
        r: TyId,
        tasks: ValueId,
        bytes: ValueId,
        value: Option<ValueId>,
        function: ValueId,
        result: TyId,
        n: NodeId,
    ) -> Option<ValueId> {
        // Native task-entry glue follows x/r.carried. The fixed runtime C entry
        // still owns input and result buffers by address.
        let logical = self.layout_size(r, true, n)?;
        let xrep = self.rep_at(x, n)?;
        let rid = self.runtime_type(r, n)?;
        let (xid, input, function) = self.task_entry_adapter(x, r, function, value, n)?;
        let u = Ty::Scalar(Scalar::U64);
        let one = self.konst(u, 1, n)?;
        let overhead = self.konst(
            u,
            hirsel_core_layout::charge::TASK_RECORD + hirsel_core_layout::charge::CAUSE_CELL,
            n,
        )?;
        let base = self.runtime_charge(logical, one, overhead, n)?;
        let requested = self.runtime_charge(bytes, one, base, n)?;
        let [task, available, ..] = self.runtime_row(
            RtOp::Spawn { x: xid, r: rid },
            &[
                Arg::Value(bytes),
                Arg::Address(input),
                Arg::Value(function),
                Arg::Value(tasks),
            ],
            n,
        )?;
        self.consume(value, n);
        let ok = self.runtime_nonzero(Ty::Ptr, task, n)?;
        self.runtime_result(
            result,
            ok,
            n,
            |cx, to, ty| {
                if let Some(at) =
                    cx.runtime_field(to, &[Step::CaseField { case: 0, field: 0 }], ty, n)
                {
                    cx.runtime_store(at, &[Step::Field(0)], Ty::Ptr, task, n);
                }
            },
            |cx, to, pair| {
                let TyKind::Tuple(types) = cx.t.kind(pair).clone() else {
                    return;
                };
                if let Some(to) =
                    cx.runtime_field(to, &[Step::CaseField { case: 1, field: 0 }], pair, n)
                {
                    if let Some(capacity) = cx.runtime_field(to, &[Step::Field(0)], types[0], n) {
                        cx.runtime_capacity(capacity, requested, available, n);
                    }
                    if let Some(at) = cx.runtime_field(to, &[Step::Field(1)], x, n) {
                        cx.install(at, xrep, value, n);
                    }
                }
            },
        )
    }
}

impl<'c, 'a> Cx<'c, 'a, '_> {
    fn owner_charge(
        &mut self,
        ty: TyId,
        kind: Option<Counted>,
        n: NodeId,
    ) -> Option<(ValueId, ValueId)> {
        let logical = self.layout_size(ty, true, n)?;
        let u = Ty::Scalar(Scalar::U64);
        let one = self.konst(u, 1, n)?;
        // round8(payload + counted header); overflow remains Capacity, not Overflow.
        let extra = self.konst(u, if kind.is_some() { 15 } else { 7 }, n)?;
        self.runtime_charge_parts(logical, one, extra, true, n)
    }

    fn runtime_new_owner(
        &mut self,
        kind: Option<Counted>,
        item: TyId,
        value: Option<ValueId>,
        result: TyId,
        n: NodeId,
    ) -> Option<ValueId> {
        let (charge, overflow) = self.owner_charge(item, kind, n)?;
        let rep = self.rep_at(item, n)?;
        let id = self.runtime_type(item, n)?;
        let payload = self.layout_size(item, false, n)?;
        let u = Ty::Scalar(Scalar::U64);
        let size = if kind.is_some() {
            let one = self.konst(u, 1, n)?;
            let header = self.konst(u, 8, n)?;
            // Physical exhaustion is a host stop after logical admission.
            self.runtime_charge(payload, one, header, n)?
        } else {
            payload
        };
        let node = self.value(Ty::Ptr);
        let available = self.value(u);
        let fail = self.fail_to(n);
        let edge = match kind {
            None => FieldTy::Box(TypeRef(id.0)),
            Some(Counted::Rc) => FieldTy::Rc(TypeRef(id.0)),
            Some(Counted::Arc) => FieldTy::Arc(TypeRef(id.0)),
        };
        self.inst(
            Inst::Alloc {
                result: node,
                available,
                region_entry: None,
                kind: AllocKind::Own { edge },
                place: AllocPlace::Current,
                charge,
                overflow: Some(overflow),
                size,
                align: 8,
                fail,
            },
            n,
        );
        // Allocation commits normally to either the new owner or the handback
        // error. A grant failure enters cleanup before this consumption.
        self.consume(value, n);
        let ok = self.runtime_nonzero(Ty::Ptr, node, n)?;
        self.runtime_result(
            result,
            ok,
            n,
            |cx, to, owner| {
                if let Some(at) =
                    cx.runtime_field(to, &[Step::CaseField { case: 0, field: 0 }], owner, n)
                {
                    cx.runtime_store(at, &[Step::Field(0)], Ty::Ptr, node, n);
                    let at = cx.owner_payload(node, id, kind.is_some(), n);
                    if let Some(at) = at {
                        let at = match rep {
                            Rep::Scalar(_) => cx.address(
                                AddressBase::Address(at),
                                &[Step::Field(0)],
                                rep.stored().unwrap(),
                                n,
                            ),
                            _ => at,
                        };
                        cx.install(at, rep, value, n);
                    }
                }
            },
            |cx, to, pair| {
                let TyKind::Tuple(parts) = cx.t.kind(pair).clone() else {
                    return;
                };
                if let Some(to) =
                    cx.runtime_field(to, &[Step::CaseField { case: 1, field: 0 }], pair, n)
                {
                    if let Some(at) = cx.runtime_field(to, &[Step::Field(0)], parts[0], n) {
                        cx.runtime_capacity(at, charge, available, n);
                    }
                    if let Some(at) = cx.runtime_field(to, &[Step::Field(1)], item, n) {
                        cx.install(at, rep, value, n);
                    }
                }
            },
        )
    }

    pub(super) fn list_ty(&mut self, item: TyId, n: NodeId) -> Option<TyId> {
        let list = self.prelude("List", n)?;
        Some(self.t.mk(TyKind::Adt(list, vec![item])))
    }

    fn log_ty(&mut self, item: TyId, n: NodeId) -> Option<TyId> {
        let log = self
            .p
            .decls
            .iter()
            .position(|d| d.role == Some(crate::decls::Role::Log) && d.path.0.name == "Log");
        let log = self.named(log, "a log without the prelude's Log is not built", n)?;
        Some(
            self.t
                .mk(TyKind::Adt(crate::types::DeclId(log as u32), vec![item])),
        )
    }

    /// MEM-29: the log node is a box allocation whose payload is the List carrier.
    fn runtime_log_new(&mut self, item: TyId, result: TyId, n: NodeId) -> Option<ValueId> {
        let list = self.list_ty(item, n)?;
        let (charge, overflow) = self.owner_charge(list, None, n)?;
        let id = self.runtime_type(list, n)?;
        let carrier = self.list_carrier(item, n)?;
        let payload = self.layout_size(list, false, n)?;
        let node = self.value(Ty::Ptr);
        let available = self.value(Ty::Scalar(Scalar::U64));
        let edge = FieldTy::Box(TypeRef(id.0));
        let fail = self.fail_to(n);
        self.inst(
            Inst::Alloc {
                result: node,
                available,
                region_entry: None,
                kind: AllocKind::Own { edge },
                place: AllocPlace::Current,
                charge,
                overflow: Some(overflow),
                size: payload,
                align: 8,
                fail,
            },
            n,
        );
        let ok = self.runtime_nonzero(Ty::Ptr, node, n)?;
        let null = self.konst(Ty::Ptr, 0, n)?;
        let zero = self.konst(Ty::Scalar(Scalar::U64), 0, n)?;
        self.runtime_result(
            result,
            ok,
            n,
            |cx, to, log| {
                // The node itself is the List carrier: {data, len, capacity}.
                cx.set_carrier_word(node, carrier, SeqWord::Data, null, n);
                cx.set_carrier_word(node, carrier, SeqWord::Cap, zero, n);
                cx.set_carrier_word(node, carrier, SeqWord::Len, zero, n);
                if let Some(at) =
                    cx.runtime_field(to, &[Step::CaseField { case: 0, field: 0 }], log, n)
                {
                    cx.runtime_store(at, &[Step::Field(0)], Ty::Ptr, node, n);
                }
            },
            |cx, to, cap| {
                if let Some(at) =
                    cx.runtime_field(to, &[Step::CaseField { case: 1, field: 0 }], cap, n)
                {
                    cx.runtime_capacity(at, charge, available, n);
                }
            },
        )
    }

    /// Append through the pointer loaded from the Log carrier. The temporary
    /// reserve result is adapted to the public `(Capacity, E)` refusal so the
    /// taken element remains available until the reserve decision.
    fn runtime_log_append(
        &mut self,
        item: TyId,
        log: ValueId,
        value: Option<ValueId>,
        result: TyId,
        n: NodeId,
    ) -> Option<ValueId> {
        let log_ty = self.log_ty(item, n)?;
        let log_id = self.runtime_type(log_ty, n)?;
        let log_base = self.address(AddressBase::Value(log), &[], PlaceTy::Declared(log_id), n);
        let node_at = self.address(
            AddressBase::Address(log_base),
            &[Step::Field(0)],
            PlaceTy::Value(Ty::Ptr),
            n,
        );
        let node = self.read_bits(node_at, Ty::Ptr, n);
        let one = self.konst(Ty::Scalar(Scalar::U64), 1, n)?;
        let cap = self.prelude("Capacity", n)?;
        let cap = self.t.mk(TyKind::Adt(cap, vec![]));
        let reserve = self.prelude("Result", n)?;
        let reserve = self.t.mk(TyKind::Adt(reserve, vec![UNIT, cap]));
        let reserved =
            self.list_reserve(item, node, one, super::lists::Ask::Bounded, reserve, n)?;
        let reserve_id =
            self.place_row(reserve, "a log reserve without a place is not built", n)?;
        let tag = self.value(Ty::Scalar(Scalar::U32));
        let from = self.address(
            AddressBase::Value(reserved),
            &[],
            PlaceTy::Declared(reserve_id),
            n,
        );
        self.inst(
            Inst::Case {
                result: tag,
                address: from,
            },
            n,
        );
        let zero = self.konst(Ty::Scalar(Scalar::U32), 0, n)?;
        let ok = self.list_compare(CompareOp::Eq, tag, zero, n);
        let out_id = match self.rep(result) {
            Ok(Rep::Place(id)) => id,
            Err(what) => {
                self.gap(n, what);
                return None;
            }
            _ => return None,
        };
        let out = self.new_slot(PlaceTy::Declared(out_id), n);
        let yes = self.new_block(&[]);
        let no = self.new_block(&[]);
        let done = self.new_block(&[]);
        self.list_branch(ok, yes, no, n);
        let item_rep = self.rep_at(item, n)?;
        self.start(yes);
        let carrier = self.list_carrier(item, n)?;
        let len = self.carrier_word(node, carrier, SeqWord::Len, n);
        let element = self.list_element(item, node, len, n)?;
        if let Some(place) = item_rep.stored() {
            let at = self.address(AddressBase::Value(element), &[], place, n);
            self.install(at, item_rep, value, n);
        }
        let (new_len, _) = self.list_arith(ArithOp::Add, ArithMode::Wrapping, len, one, n);
        self.set_carrier_word(node, carrier, SeqWord::Len, new_len, n);
        self.inst(
            Inst::SetCase {
                address: out,
                case: 0,
            },
            n,
        );
        self.consume(value, n);
        self.jump(done, &[], n);
        self.start(no);
        self.inst(
            Inst::SetCase {
                address: out,
                case: 1,
            },
            n,
        );
        let error_ty = match self.t.kind(result).clone() {
            TyKind::Adt(_, args) => args[1],
            _ => {
                self.gap(n, "a log append that is not a declared type is not built");
                return None;
            }
        };
        let cap_at = self.runtime_field(out, &[Step::CaseField { case: 1, field: 0 }], error_ty, n);
        let cap_at = self.named(cap_at, "a log refusal without a place is not built", n)?;
        let cap_id = self.runtime_type(cap, n)?;
        let old_cap = self.address(
            AddressBase::Address(from),
            &[Step::CaseField { case: 1, field: 0 }],
            PlaceTy::Declared(cap_id),
            n,
        );
        for field in 0..2 {
            let src = self.address(
                AddressBase::Address(old_cap),
                &[Step::Field(field)],
                PlaceTy::Value(Ty::Scalar(Scalar::U64)),
                n,
            );
            let value = self.read_bits(src, Ty::Scalar(Scalar::U64), n);
            self.runtime_store(
                cap_at,
                &[Step::Field(field)],
                Ty::Scalar(Scalar::U64),
                value,
                n,
            );
        }
        if let Some(at) = self.runtime_field(
            out,
            &[Step::CaseField { case: 1, field: 0 }, Step::Field(1)],
            item,
            n,
        ) {
            self.install(at, item_rep, value, n);
        }
        self.jump(done, &[], n);
        self.start(done);
        let out = self.pointer(out, n);
        self.temp(result, Some(out));
        Some(out)
    }

    fn runtime_log_finish(
        &mut self,
        item: TyId,
        log: ValueId,
        result: TyId,
        n: NodeId,
    ) -> Option<ValueId> {
        let log_ty = self.log_ty(item, n)?;
        let log_id = self.runtime_type(log_ty, n)?;
        let log_base = self.address(AddressBase::Value(log), &[], PlaceTy::Declared(log_id), n);
        let node_at = self.address(
            AddressBase::Address(log_base),
            &[Step::Field(0)],
            PlaceTy::Value(Ty::Ptr),
            n,
        );
        let node = self.read_bits(node_at, Ty::Ptr, n);
        let list = self.list_ty(item, n)?;
        let list_id = self.runtime_type(list, n)?;
        let to = self.new_slot(PlaceTy::Declared(list_id), n);
        self.copy_to(to, node, list_id, n);
        self.consume(Some(log), n);
        let (charge, _) = self.owner_charge(list, None, n)?;
        self.inst(
            Inst::Free {
                block: node,
                charge,
            },
            n,
        );
        let value = self.pointer(to, n);
        self.temp(result, Some(value));
        Some(value)
    }

    /// PROG-9: a StaticFn is the address of its function's static descriptor,
    /// `{code, length, revision}` then the identity bytes (`Inst::DescriptorRef`). The
    /// described function is the item's closed adapter, whatever body describes
    /// it, so a program has one descriptor per function and StaticFn equality is
    /// word equality.
    fn runtime_code_describe(&mut self, result: TyId, n: NodeId) -> Option<ValueId> {
        let item = self.code_describe_target(n)?;
        let regions = (0..self.p.fns[item].generics.len())
            .map(|i| self.t.mk(TyKind::Region(Root::Generic(i as u32))))
            .collect::<Vec<_>>();
        let callable = subst(&mut self.t, self.p.fns[item].callable, &regions);
        let function = self.item_adapter_closed(item, &regions, callable, n)?;
        // A closed adapter is no template of an enclosing generic body: carried
        // under it, specialization would make a fresh row per instance.
        self.nested.remove(&function);
        let Some(bytes) = self.code_identity_bytes(function) else {
            self.gap(n, "StaticFn identity source is not built");
            return None;
        };
        let identity = self.e.static_bytes(&bytes);
        let descriptor = self.value(Ty::Ptr);
        self.inst(
            Inst::DescriptorRef {
                result: descriptor,
                function,
                identity,
            },
            n,
        );
        let word = self.reinterpret(descriptor, Ty::Ptr, Ty::Scalar(Scalar::U64), n);
        if let Some(word) = word {
            self.static_fns
                .insert(word, (function, identity, bytes.len() as u64));
        }
        self.temp(result, word);
        word
    }

    /// The descriptor of `value`, a StaticFn, and its identity length: a
    /// StaticFn this body described has both here; any other has them in its
    /// word.
    fn code_descriptor(&mut self, value: ValueId, n: NodeId) -> Option<(ValueId, ValueId)> {
        if let Some(&(function, identity, length)) = self.static_fns.get(&value) {
            let descriptor = self.value(Ty::Ptr);
            self.inst(
                Inst::DescriptorRef {
                    result: descriptor,
                    function,
                    identity,
                },
                n,
            );
            return Some((descriptor, self.konst(Ty::Scalar(Scalar::U64), length, n)?));
        }
        let Some(descriptor) = self.static_fn_pointer(value, n) else {
            self.gap(n, "StaticFn identity carriage is not built");
            return None;
        };
        let length = self.descriptor_word(descriptor, 8, Ty::Scalar(Scalar::U64), n)?;
        Some((descriptor, length))
    }

    /// The word at `offset` in `descriptor`.
    fn descriptor_word(
        &mut self,
        descriptor: ValueId,
        offset: u64,
        ty: Ty,
        n: NodeId,
    ) -> Option<ValueId> {
        let offset = self.konst(Ty::Scalar(Scalar::U64), offset, n)?;
        let at = self.address(
            AddressBase::Value(descriptor),
            &[Step::Byte(offset)],
            PlaceTy::Value(ty),
            n,
        );
        Some(self.read_bits(at, ty, n))
    }

    /// `code.identity(s)`: the bytes after `s`'s descriptor's three words, as
    /// a view.
    fn runtime_code_identity(
        &mut self,
        value: ValueId,
        result: TyId,
        n: NodeId,
    ) -> Option<ValueId> {
        let (descriptor, length) = self.code_descriptor(value, n)?;
        let offset = self.konst(Ty::Scalar(Scalar::U64), 24, n)?;
        let at = self.address(
            AddressBase::Value(descriptor),
            &[Step::Byte(offset)],
            PlaceTy::Value(Ty::Scalar(Scalar::U8)),
            n,
        );
        let bytes = self.pointer(at, n);
        let Some(value) = self.identity_carrier(bytes, length, result, n) else {
            self.gap(n, "StaticFn identity carrier is not built");
            return None;
        };
        self.temp(result, Some(value));
        Some(value)
    }

    /// `code.revision(s)`: the descriptor's third word addresses the
    /// function's revision carrier `{bytes, 32, 0}`, which the link defines
    /// from the program's reach; a unit linked without it keeps its own
    /// `{0, 0, 0}`, and the result is None.
    fn runtime_code_revision(
        &mut self,
        value: ValueId,
        result: TyId,
        n: NodeId,
    ) -> Option<ValueId> {
        let (descriptor, _) = self.code_descriptor(value, n)?;
        let carrier = self.descriptor_word(descriptor, 16, Ty::Ptr, n)?;
        let bytes = self.descriptor_word(carrier, 0, Ty::Ptr, n)?;
        let TyKind::Adt(_, arguments) = self.t.kind(result).clone() else {
            self.gap(
                n,
                "a code revision that is not a declared type is not built",
            );
            return None;
        };
        let payload = arguments[0];
        let id = self.runtime_type(result, n)?;
        let to = self.new_slot(PlaceTy::Declared(id), n);
        let none = self.runtime_compare(Ty::Ptr, bytes, 0, n)?;
        let yes = self.new_block(&[]);
        let no = self.new_block(&[]);
        let join = self.new_block(&[]);
        self.runtime_branch(none, no, yes, n);
        self.start(yes);
        self.inst(
            Inst::SetCase {
                address: to,
                case: 1,
            },
            n,
        );
        let at = self.runtime_field(to, &[Step::CaseField { case: 1, field: 0 }], payload, n)?;
        // The carrier is static and has capacity 0: the payload is its 24-byte
        // non-owning value.
        self.install_view(at, self.bare(payload), carrier, n)?;
        self.jump(join, &[], n);
        self.start(no);
        self.inst(
            Inst::SetCase {
                address: to,
                case: 0,
            },
            n,
        );
        self.jump(join, &[], n);
        self.start(join);
        Some(self.pointer(to, n))
    }

    fn code_identity_bytes(&self, function: FuncId) -> Option<Vec<u8>> {
        let instance = self.e.module.functions[function.index()].instance;
        let path = &self.e.module.instances[instance.index()].definition;
        // Function items pass through a callable adapter before reaching this
        // kernel. Its `$callable[...]` definition is an ABI row, not the source
        // item whose PROG-9 identity is published.
        let source_name = path
            .0
            .name
            .split_once("$callable")
            .map_or(path.0.name.as_str(), |(name, _)| name);
        let f = self.p.fns.iter().position(|fd| {
            fd.path.0.package == path.0.package
                && fd.path.0.modules == path.0.modules
                && fd.path.0.name == source_name
        })?;
        let fd = &self.p.fns[f];
        crate::identity::code_identity(&self.p.decls, &self.t, fd)
    }

    /// A `Bytes` view `{bytes, length, 0}` of static identity bytes.
    fn identity_carrier(
        &mut self,
        bytes: ValueId,
        length: ValueId,
        result: TyId,
        n: NodeId,
    ) -> Option<ValueId> {
        // A generic `identity[F]` result keeps its anchor on the StaticFn
        // parameter.  The carrier layout is still the ordinary Bytes row, so
        // erase only that semantic anchor when asking the emitter for storage.
        let bare = self.bare(result);
        let id = self
            .e
            .type_row(self.p, &mut self.t, result)
            .or_else(|| self.e.type_row(self.p, &mut self.t, bare));
        let Some(id) = id else {
            self.gap(n, "runtime operand layout is not built");
            return None;
        };
        let to = self.new_slot(PlaceTy::Declared(id), n);
        let zero = self.konst(Ty::Scalar(Scalar::U64), 0, n)?;
        let carrier = self.pointer(to, n);
        self.sequence_word(carrier, 0, Ty::Ptr, bytes, n);
        self.sequence_word(carrier, 8, Ty::Scalar(Scalar::U64), length, n);
        self.sequence_word(carrier, 16, Ty::Scalar(Scalar::U64), zero, n);
        Some(self.pointer(to, n))
    }

    /// `code.callable(s)`: the code word of `s`'s descriptor, or the function
    /// itself when this body described `s`.
    fn runtime_code_callable(
        &mut self,
        value: ValueId,
        result: TyId,
        n: NodeId,
    ) -> Option<ValueId> {
        let code = if let Some(&(function, _, _)) = self.static_fns.get(&value) {
            let code = self.value(Ty::Ptr);
            self.inst(
                Inst::FunctionRef {
                    result: code,
                    function,
                },
                n,
            );
            code
        } else {
            let Some(descriptor) = self.static_fn_pointer(value, n) else {
                self.gap(n, "StaticFn callable carriage is not built");
                return None;
            };
            let at = self.address(
                AddressBase::Value(descriptor),
                &[],
                PlaceTy::Value(Ty::Ptr),
                n,
            );
            self.read_bits(at, Ty::Ptr, n)
        };
        self.temp(result, Some(code));
        Some(code)
    }

    /// A StaticFn's descriptor address: the value itself when it is carried as a
    /// pointer, else its word reinterpreted.
    fn static_fn_pointer(&mut self, value: ValueId, n: NodeId) -> Option<ValueId> {
        (self.b.values.get(value.index()) == Some(&Ty::Ptr))
            .then_some(value)
            .or_else(|| self.reinterpret(value, Ty::Scalar(Scalar::U64), Ty::Ptr, n))
    }

    fn reinterpret(&mut self, value: ValueId, from: Ty, to: Ty, n: NodeId) -> Option<ValueId> {
        if self.b.values.get(value.index()) != Some(&from) {
            return None;
        }
        let slot = self.new_slot(PlaceTy::Value(from), n);
        self.inst(
            Inst::Store {
                address: slot,
                value,
            },
            n,
        );
        let pointer = self.pointer(slot, n);
        let at = self.address(AddressBase::Value(pointer), &[], PlaceTy::Value(to), n);
        Some(self.read_bits(at, to, n))
    }

    fn owner_payload(
        &mut self,
        node: ValueId,
        id: TypeId,
        counted: bool,
        n: NodeId,
    ) -> Option<AddressId> {
        let place = PlaceTy::Declared(id);
        if counted {
            let offset = self.konst(Ty::Scalar(Scalar::U64), 8, n)?;
            let at = self.address(AddressBase::Value(node), &[Step::Byte(offset)], place, n);
            let pointer = self.pointer(at, n);
            Some(self.address(AddressBase::Value(pointer), &[], place, n))
        } else {
            Some(self.address(AddressBase::Value(node), &[], place, n))
        }
    }

    fn runtime_share(
        &mut self,
        kind: Counted,
        value: ValueId,
        result: TyId,
        n: NodeId,
    ) -> Option<ValueId> {
        let node = self.runtime_handle(value, n);
        self.inst(Inst::Share { node, kind }, n);
        self.runtime_carrier(result, node, n)
    }

    fn runtime_with_unique(
        &mut self,
        kind: Counted,
        item: TyId,
        vals: &[Option<ValueId>],
        result: TyId,
        n: NodeId,
    ) -> Option<ValueId> {
        let id = self.runtime_type(item, n)?;
        // A mut holder argument points at its carrier, whose first word is the node.
        let node = self.runtime_handle(vals[0]?, n);
        let callback = vals[1]?;
        let ok = self.value(Ty::Scalar(Scalar::Bool));
        self.inst(
            Inst::TryUnwrap {
                result: ok,
                node,
                kind,
            },
            n,
        );
        self.runtime_result(
            result,
            ok,
            n,
            |cx, to, ty| {
                if let Some(payload) = cx.owner_payload(node, id, true, n) {
                    let pointer = cx.pointer(payload, n);
                    let callable = cx.t.mk(TyKind::Fn(
                        vec![(Mode::Mut, item)],
                        ty,
                        false,
                        Callable::Scoped,
                    ));
                    let value = cx.call_lowered(callable, callback, &[Some(pointer)], n);
                    cx.runtime_case_payload(to, 1, ty, value, n);
                }
            },
            |_, _, _| {},
        )
    }

    fn runtime_try_unwrap(
        &mut self,
        kind: Counted,
        item: TyId,
        value: ValueId,
        result: TyId,
        n: NodeId,
    ) -> Option<ValueId> {
        // A failed unwrap returns this holder unchanged, without sharing it.
        let (charge, _) = self.owner_charge(item, Some(kind), n)?;
        let id = self.runtime_type(item, n)?;
        let node = self.runtime_handle(value, n);
        let ok = self.value(Ty::Scalar(Scalar::Bool));
        self.inst(
            Inst::TryUnwrap {
                result: ok,
                node,
                kind,
            },
            n,
        );
        self.consume(Some(value), n);
        self.runtime_result(
            result,
            ok,
            n,
            |cx, to, ty| {
                if let Some(payload) = cx.owner_payload(node, id, true, n) {
                    let value = cx.runtime_read(payload, ty, n);
                    cx.runtime_case_payload(to, 0, ty, value, n);
                    cx.inst(
                        Inst::Free {
                            block: node,
                            charge,
                        },
                        n,
                    );
                }
            },
            |cx, to, ty| {
                cx.runtime_case_payload(to, 1, ty, Some(value), n);
            },
        )
    }

    fn runtime_unbox(&mut self, item: TyId, value: ValueId, n: NodeId) -> Option<ValueId> {
        let (charge, _) = self.owner_charge(item, None, n)?;
        let rep = self.rep_at(item, n)?;
        let id = self.runtime_type(item, n)?;
        let node = self.runtime_handle(value, n);
        let payload = self.owner_payload(node, id, false, n)?;
        self.consume(Some(value), n);
        let value = self.runtime_read(payload, item, n);
        let output = match rep {
            Rep::Place(_) => {
                let to = self.new_slot(PlaceTy::Declared(id), n);
                self.install(to, rep, value, n);
                let value = self.pointer(to, n);
                self.temp(item, Some(value));
                Some(value)
            }
            _ => value,
        };
        self.inst(
            Inst::Free {
                block: node,
                charge,
            },
            n,
        );
        output
    }
}

impl<'c, 'a> Cx<'c, 'a, '_> {
    fn runtime_carrier_view(&mut self, value: ValueId, result: TyId, n: NodeId) -> Option<ValueId> {
        let id = match self.rep(result) {
            Ok(Rep::Place(id)) => id,
            Ok(_) => {
                self.gap(n, "a carrier view needs its value place");
                return None;
            }
            Err(what) => {
                self.gap(n, what);
                return None;
            }
        };
        let to = self.new_slot(PlaceTy::Declared(id), n);
        // FORM #56: copy data, length and the owner's capacity, without an owner.
        self.install_view(to, self.bare(result), value, n)?;
        Some(self.pointer(to, n))
    }

    fn runtime_args_get(
        &mut self,
        vals: &[Option<ValueId>],
        result: TyId,
        n: NodeId,
    ) -> Option<ValueId> {
        let [value, ..] = self.runtime_row(
            RtOp::ArgsGet,
            &[Arg::Value(vals[0]?), Arg::Value(vals[1]?)],
            n,
        )?;
        let TyKind::Adt(_, arguments) = self.t.kind(result).clone() else {
            self.gap(
                n,
                "an argument lookup that is not a declared type is not built",
            );
            return None;
        };
        let payload = arguments[0];
        let id = self.runtime_type(result, n)?;
        let to = self.new_slot(PlaceTy::Declared(id), n);
        let none = self.runtime_compare(Ty::Ptr, value, 0, n)?;
        let yes = self.new_block(&[]);
        let no = self.new_block(&[]);
        let join = self.new_block(&[]);
        self.runtime_branch(none, no, yes, n);
        self.start(yes);
        self.inst(
            Inst::SetCase {
                address: to,
                case: 1,
            },
            n,
        );
        let at = self.runtime_field(to, &[Step::CaseField { case: 1, field: 0 }], payload, n)?;
        // The provider pointer pins a capacity-0 carrier. The Some payload is its
        // 24-byte non-owning value, not the address of that run-owned carrier.
        self.install_view(at, self.bare(payload), value, n)?;
        self.jump(join, &[], n);
        self.start(no);
        self.inst(
            Inst::SetCase {
                address: to,
                case: 0,
            },
            n,
        );
        self.jump(join, &[], n);
        self.start(join);
        Some(self.pointer(to, n))
    }
}

impl<'c, 'a> Cx<'c, 'a, '_> {
    fn runtime_stream(
        &mut self,
        op: HostOp,
        vals: &[Option<ValueId>],
        result: TyId,
        n: NodeId,
    ) -> Option<ValueId> {
        let [_, error, code, written] = self.runtime_row(
            RtOp::Host(op),
            &[Arg::Value(vals[0]?), Arg::Value(vals[1]?)],
            n,
        )?;
        let ok = self.runtime_compare(Ty::Scalar(Scalar::U8), error, 0, n)?;
        self.runtime_result(
            result,
            ok,
            n,
            |_, _, _| {},
            |cx, to, ty| {
                if let Some(to) =
                    cx.runtime_field(to, &[Step::CaseField { case: 1, field: 0 }], ty, n)
                {
                    cx.runtime_write_error(to, ty, written, error, code, n);
                }
            },
        )
    }

    fn runtime_write_error(
        &mut self,
        to: AddressId,
        ty: TyId,
        written: ValueId,
        error: ValueId,
        code: ValueId,
        n: NodeId,
    ) {
        self.runtime_store(to, &[Step::Field(0)], Ty::Scalar(Scalar::U64), written, n);
        let TyKind::Adt(decl, _) = self.t.kind(ty) else {
            return;
        };
        let DeclBody::Record(fields) = &self.p.decls[decl.index()].body else {
            return;
        };
        let io = fields[1].ty;
        if let Some(to) = self.runtime_field(to, &[Step::Field(1)], io, n) {
            self.runtime_io_error(to, error, code, n);
        }
    }

    fn runtime_io_error(&mut self, to: AddressId, error: ValueId, code: ValueId, n: NodeId) {
        let join = self.new_block(&[]);
        // Provider tags are 1-based, source variant indices 0-based. The fifth
        // variant alone has a payload. The runtime has already classified errno.
        for case in 0..4 {
            let Some(on) =
                self.runtime_compare(Ty::Scalar(Scalar::U8), error, u64::from(case + 1), n)
            else {
                return;
            };
            let yes = self.new_block(&[]);
            let no = self.new_block(&[]);
            self.runtime_branch(on, yes, no, n);
            self.start(yes);
            self.inst(Inst::SetCase { address: to, case }, n);
            self.jump(join, &[], n);
            self.start(no);
        }
        self.inst(
            Inst::SetCase {
                address: to,
                case: 4,
            },
            n,
        );
        self.runtime_store(
            to,
            &[Step::CaseField { case: 4, field: 0 }],
            Ty::Scalar(Scalar::U32),
            code,
            n,
        );
        self.jump(join, &[], n);
        self.start(join);
    }
}

impl<'c, 'a> Cx<'c, 'a, '_> {
    fn runtime_host(
        &mut self,
        op: HostOp,
        f: usize,
        vals: &[Option<ValueId>],
        result: TyId,
        n: NodeId,
    ) -> Option<ValueId> {
        let u64t = Ty::Scalar(Scalar::U64);
        let file = matches!(
            op,
            HostOp::Read
                | HostOp::Write
                | HostOp::Close
                | HostOp::TryLock
                | HostOp::Unlock
                | HostOp::Sync
        );
        let handle = if file {
            self.runtime_key(vals[0]?, n)
        } else {
            vals[0]?
        };
        let mut args = [Arg::Value(handle); 3];
        let len = match op {
            HostOp::Rename => {
                args[1] = Arg::Value(vals[1]?);
                args[2] = Arg::Value(vals[2]?);
                3
            }
            HostOp::SleepUntil => {
                args[1] = Arg::Value(vals[1]?);
                2
            }
            HostOp::Open => {
                args[1] = Arg::Value(vals[1]?);
                args[2] = Arg::Value(self.runtime_mode(f, 2, vals[2]?, n)?);
                3
            }
            HostOp::Read
            | HostOp::Write
            | HostOp::Mkdir
            | HostOp::Mkdtemp
            | HostOp::Remove
            | HostOp::RemoveTree
            | HostOp::Stdout
            | HostOp::Stderr
            | HostOp::Random => {
                args[1] = Arg::Value(vals[1]?);
                2
            }
            _ => 1,
        };
        if op == HostOp::Random {
            let _ = self.runtime_row(RtOp::Host(op), &args[..2], n)?;
            return None;
        }
        if op == HostOp::Close {
            // Explicit CLOSE consumes on error and pre-issue cancellation too.
            // Its failure cleanup must not close the same key a second time.
            self.consume(vals[0], n);
        }
        let [value, error, code, written] = self.runtime_row(RtOp::Host(op), &args[..len], n)?;
        if matches!(op, HostOp::WallMs | HostOp::MonoNs) {
            return Some(value);
        }
        let ok = self.runtime_compare(Ty::Scalar(Scalar::U8), error, 0, n)?;
        self.runtime_result(
            result,
            ok,
            n,
            |cx, to, ty| match op {
                HostOp::Open => {
                    if let Some(to) =
                        cx.runtime_field(to, &[Step::CaseField { case: 0, field: 0 }], ty, n)
                    {
                        cx.runtime_store(to, &[], u64t, value, n);
                    }
                }
                HostOp::Read | HostOp::Mkdtemp | HostOp::Random => {
                    cx.runtime_case_payload(to, 0, ty, Some(value), n)
                }
                HostOp::TryLock => {
                    if let Some(boolean) = cx.runtime_nonzero(u64t, value, n) {
                        cx.runtime_case_payload(to, 0, ty, Some(boolean), n);
                    }
                }
                _ => {}
            },
            |cx, to, ty| {
                if let Some(to) =
                    cx.runtime_field(to, &[Step::CaseField { case: 1, field: 0 }], ty, n)
                {
                    if matches!(op, HostOp::Write | HostOp::Stdout | HostOp::Stderr) {
                        cx.runtime_write_error(to, ty, written, error, code, n);
                    } else {
                        cx.runtime_io_error(to, error, code, n);
                    }
                }
            },
        )
    }

    /// A HostFile's or Tape's entire carrier is the opaque u64 key into the run's
    /// registry, not provider metadata.
    fn runtime_key(&mut self, value: ValueId, n: NodeId) -> ValueId {
        let u64t = Ty::Scalar(Scalar::U64);
        let at = self.address(AddressBase::Value(value), &[], PlaceTy::Value(u64t), n);
        self.read_bits(at, u64t, n)
    }

    /// A fieldless enum operand (`OpenMode`, `tape.Mode`) passes its case as a u8.
    fn runtime_mode(
        &mut self,
        f: usize,
        param: usize,
        value: ValueId,
        n: NodeId,
    ) -> Option<ValueId> {
        let id = self.runtime_type(self.p.fns[f].params[param].ty, n)?;
        let at = self.address(AddressBase::Value(value), &[], PlaceTy::Declared(id), n);
        let tag = self.value(Ty::Scalar(Scalar::U32));
        self.inst(
            Inst::Case {
                result: tag,
                address: at,
            },
            n,
        );
        let mode = self.value(Ty::Scalar(Scalar::U8));
        self.inst(
            Inst::Convert {
                result: mode,
                flag: None,
                operand: tag,
                mode: ConvertMode::Truncate,
            },
            n,
        );
        Some(mode)
    }

    /// `tape.open_raw`: Ok carries the Tape's key, Error the runtime's open word.
    fn runtime_tape_open(
        &mut self,
        f: usize,
        vals: &[Option<ValueId>],
        result: TyId,
        n: NodeId,
    ) -> Option<ValueId> {
        let u64t = Ty::Scalar(Scalar::U64);
        let mode = self.runtime_mode(f, 2, vals[2]?, n)?;
        let mut args = [Arg::Value(mode); 8];
        for i in [0, 1, 3, 4, 5, 6, 7] {
            args[i] = Arg::Value(vals[i]?);
        }
        let [tape, code, ..] = self.runtime_row(RtOp::TapeOpen, &args, n)?;
        let ok = self.runtime_compare(u64t, code, 0, n)?;
        self.runtime_result(
            result,
            ok,
            n,
            |cx, to, ty| {
                if let Some(to) =
                    cx.runtime_field(to, &[Step::CaseField { case: 0, field: 0 }], ty, n)
                {
                    cx.runtime_store(to, &[], u64t, tape, n);
                }
            },
            |cx, to, ty| cx.runtime_case_payload(to, 1, ty, Some(code), n),
        )
    }
}

impl<'c, 'a> Cx<'c, 'a, '_> {
    fn net_field(
        &mut self,
        ty: TyId,
        at: AddressId,
        i: usize,
        n: NodeId,
    ) -> Option<(TyId, AddressId)> {
        let child = match self.t.kind(ty) {
            TyKind::Tuple(xs) => xs[i],
            TyKind::Adt(d, _) => match &self.p.decls[d.index()].body {
                DeclBody::Record(fs) => fs[i].ty,
                _ => return None,
            },
            _ => return None,
        };
        let field = self.runtime_field(at, &[Step::Field(i as u32)], child, n)?;
        Some((child, field))
    }
    fn net_read_field(&mut self, ty: TyId, at: AddressId, i: usize, n: NodeId) -> Option<ValueId> {
        let (child, field) = self.net_field(ty, at, i, n)?;
        match self.rep(child).ok()? {
            Rep::Scalar(t) => Some(self.read_bits(field, t, n)),
            Rep::Place(_) => Some(self.pointer(field, n)),
            Rep::None => None,
        }
    }
    fn net_case(&mut self, at: AddressId, n: NodeId) -> ValueId {
        let tag = self.value(Ty::Scalar(Scalar::U32));
        self.inst(
            Inst::Case {
                result: tag,
                address: at,
            },
            n,
        );
        tag
    }
    fn net_ip(&mut self, at: AddressId, n: NodeId) -> Option<[ValueId; 3]> {
        let u = Ty::Scalar(Scalar::U64);
        let tag = self.net_case(at, n);
        let family = self.value(Ty::Scalar(Scalar::U8));
        self.inst(
            Inst::Convert {
                result: family,
                flag: None,
                operand: tag,
                mode: ConvertMode::Truncate,
            },
            n,
        );
        let v4 = self.runtime_compare(Ty::Scalar(Scalar::U32), tag, 0, n)?;
        let yes = self.new_block(&[]);
        let no = self.new_block(&[]);
        let join = self.new_block(&[u, u]);
        self.runtime_branch(v4, yes, no, n);
        self.start(yes);
        let at4 = self.address(
            AddressBase::Address(at),
            &[Step::CaseField { case: 0, field: 0 }],
            PlaceTy::Value(Ty::Scalar(Scalar::U32)),
            n,
        );
        let lo4 = self.read_bits(at4, Ty::Scalar(Scalar::U32), n);
        let lo = self.value(u);
        self.inst(
            Inst::Convert {
                result: lo,
                flag: None,
                operand: lo4,
                mode: ConvertMode::Widen,
            },
            n,
        );
        let zero = self.konst(u, 0, n)?;
        self.jump(join, &[zero, lo], n);
        self.start(no);
        let hi_at = self.address(
            AddressBase::Address(at),
            &[Step::CaseField { case: 1, field: 0 }],
            PlaceTy::Value(u),
            n,
        );
        let lo_at = self.address(
            AddressBase::Address(at),
            &[Step::CaseField { case: 1, field: 1 }],
            PlaceTy::Value(u),
            n,
        );
        let hi = self.read_bits(hi_at, u, n);
        let lo = self.read_bits(lo_at, u, n);
        self.jump(join, &[hi, lo], n);
        self.start(join);
        let values = self.block_params(join);
        Some([family, values[0], values[1]])
    }
    fn net_until(&mut self, ty: TyId, value: ValueId, n: NodeId) -> Option<[ValueId; 3]> {
        let (_, at) = self.runtime_place(ty, Some(value), n)?;
        let tag = self.net_case(at, n);
        let never = self.runtime_compare(Ty::Scalar(Scalar::U32), tag, 0, n)?;
        let u = Ty::Scalar(Scalar::U64);
        let k = Ty::Scalar(Scalar::U8);
        let yes = self.new_block(&[]);
        let no = self.new_block(&[]);
        let join = self.new_block(&[k, u, u]);
        self.runtime_branch(never, yes, no, n);
        self.start(yes);
        let zero = self.konst(u, 0, n)?;
        let off = self.konst(k, 0, n)?;
        self.jump(join, &[off, zero, zero], n);
        self.start(no);
        let mut fields = Vec::new();
        for field in 0..2 {
            let field = self.address(
                AddressBase::Address(at),
                &[Step::CaseField { case: 1, field }],
                PlaceTy::Value(u),
                n,
            );
            fields.push(self.read_bits(field, u, n));
        }
        let on = self.konst(k, 1, n)?;
        self.jump(join, &[on, fields[0], fields[1]], n);
        self.start(join);
        let values = self.block_params(join);
        Some([values[0], values[1], values[2]])
    }
    fn net_store_address(&mut self, to: AddressId, ty: TyId, values: &[ValueId], n: NodeId) {
        let Some((_, ip)) = self.net_field(ty, to, 0, n) else {
            return;
        };
        let v4 = self
            .runtime_compare(Ty::Scalar(Scalar::U8), values[0], 0, n)
            .unwrap();
        let yes = self.new_block(&[]);
        let no = self.new_block(&[]);
        let join = self.new_block(&[]);
        self.runtime_branch(v4, yes, no, n);
        self.start(yes);
        self.inst(
            Inst::SetCase {
                address: ip,
                case: 0,
            },
            n,
        );
        let lo = self.value(Ty::Scalar(Scalar::U32));
        self.inst(
            Inst::Convert {
                result: lo,
                flag: None,
                operand: values[2],
                mode: ConvertMode::Truncate,
            },
            n,
        );
        self.runtime_store(
            ip,
            &[Step::CaseField { case: 0, field: 0 }],
            Ty::Scalar(Scalar::U32),
            lo,
            n,
        );
        self.jump(join, &[], n);
        self.start(no);
        self.inst(
            Inst::SetCase {
                address: ip,
                case: 1,
            },
            n,
        );
        for field in 0..2 {
            self.runtime_store(
                ip,
                &[Step::CaseField { case: 1, field }],
                Ty::Scalar(Scalar::U64),
                values[field as usize + 1],
                n,
            );
        }
        self.jump(join, &[], n);
        self.start(join);
        self.runtime_store(to, &[Step::Field(1)], Ty::Scalar(Scalar::U16), values[3], n);
    }
    fn runtime_net(
        &mut self,
        op: HostOp,
        f: usize,
        vals: &[Option<ValueId>],
        result: TyId,
        n: NodeId,
    ) -> Option<ValueId> {
        let u = Ty::Scalar(Scalar::U64);
        let key = if matches!(
            op,
            HostOp::Accept | HostOp::RecvBounded | HostOp::SendPair | HostOp::NetSplit
        ) {
            self.runtime_key(vals[0]?, n)
        } else {
            vals[0]?
        };
        let mut operands = vec![key];
        match op {
            HostOp::Connect | HostOp::Listen | HostOp::NetNarrow => {
                let ty = self.p.fns[f].params[1].ty;
                let (_, at) = self.runtime_place(ty, vals[1], n)?;
                let ip_index = usize::from(op == HostOp::NetNarrow);
                if op == HostOp::NetNarrow {
                    let (_, dir) = self.net_field(ty, at, 0, n)?;
                    let d = self.net_case(dir, n);
                    let d8 = self.value(Ty::Scalar(Scalar::U8));
                    self.inst(
                        Inst::Convert {
                            result: d8,
                            flag: None,
                            operand: d,
                            mode: ConvertMode::Truncate,
                        },
                        n,
                    );
                    operands.push(d8);
                }
                let (_, ip) = self.net_field(ty, at, ip_index, n)?;
                operands.extend(self.net_ip(ip, n)?);
                if op == HostOp::NetNarrow {
                    operands.push(self.net_read_field(ty, at, 2, n)?);
                    let (pair, ports) = self.net_field(ty, at, 3, n)?;
                    operands.push(self.net_read_field(pair, ports, 0, n)?);
                    operands.push(self.net_read_field(pair, ports, 1, n)?);
                } else {
                    operands.push(self.net_read_field(ty, at, 1, n)?);
                    if op == HostOp::Connect {
                        operands.extend(self.net_until(self.p.fns[f].params[2].ty, vals[2]?, n)?);
                    } else {
                        operands.push(vals[2]?);
                    }
                }
            }
            HostOp::Accept => {
                operands.extend(self.net_until(self.p.fns[f].params[1].ty, vals[1]?, n)?)
            }
            HostOp::RecvBounded | HostOp::SendPair => {
                operands.extend([vals[1]?, vals[2]?]);
                operands.extend(self.net_until(self.p.fns[f].params[3].ty, vals[3]?, n)?);
            }
            HostOp::NetSplit => {}
            _ => unreachable!(),
        }
        let args: Vec<_> = operands.into_iter().map(Arg::Value).collect();
        let values = self.runtime_outputs::<8>(RtOp::Host(op), &args, n)?;
        let (value, error, code, written) = (values[0], values[1], values[2], values[3]);
        if op == HostOp::NetSplit {
            self.consume(vals[0], n);
            let id = self.runtime_type(result, n)?;
            let to = self.new_slot(PlaceTy::Declared(id), n);
            self.runtime_store(to, &[Step::Field(0)], u, value, n);
            self.runtime_store(to, &[Step::Field(1)], u, written, n);
            return Some(self.pointer(to, n));
        }
        let ok = self.runtime_compare(Ty::Scalar(Scalar::U8), error, 0, n)?;
        self.runtime_result(
            result,
            ok,
            n,
            |cx, to, ty| {
                if op == HostOp::NetNarrow || matches!(op, HostOp::RecvBounded | HostOp::SendPair) {
                    cx.runtime_case_payload(to, 0, ty, Some(value), n);
                } else if op == HostOp::Connect {
                    if let Some(at) =
                        cx.runtime_field(to, &[Step::CaseField { case: 0, field: 0 }], ty, n)
                    {
                        cx.runtime_store(at, &[], u, value, n);
                    }
                } else if matches!(op, HostOp::Listen | HostOp::Accept) {
                    if let Some(pair) =
                        cx.runtime_field(to, &[Step::CaseField { case: 0, field: 0 }], ty, n)
                    {
                        cx.runtime_store(pair, &[Step::Field(0)], u, value, n);
                        if let Some((addr_ty, addr)) = cx.net_field(ty, pair, 1, n) {
                            cx.net_store_address(addr, addr_ty, &values[4..], n);
                        }
                    }
                }
            },
            |cx, to, ty| {
                if let Some(at) =
                    cx.runtime_field(to, &[Step::CaseField { case: 1, field: 0 }], ty, n)
                {
                    if op == HostOp::NetNarrow {
                        let invalid = cx
                            .runtime_compare(Ty::Scalar(Scalar::U8), error, 1, n)
                            .unwrap();
                        let yes = cx.new_block(&[]);
                        let no = cx.new_block(&[]);
                        let join = cx.new_block(&[]);
                        cx.runtime_branch(invalid, yes, no, n);
                        cx.start(yes);
                        cx.inst(
                            Inst::SetCase {
                                address: at,
                                case: 0,
                            },
                            n,
                        );
                        cx.jump(join, &[], n);
                        cx.start(no);
                        cx.inst(
                            Inst::SetCase {
                                address: at,
                                case: 1,
                            },
                            n,
                        );
                        let TyKind::Adt(d, _) = cx.t.kind(ty) else {
                            return;
                        };
                        let DeclBody::Variant(cases) = &cx.p.decls[d.index()].body else {
                            return;
                        };
                        let cap = cases[1].fields[0].ty;
                        if let Some(cap) =
                            cx.runtime_field(at, &[Step::CaseField { case: 1, field: 0 }], cap, n)
                        {
                            cx.runtime_store(cap, &[Step::Field(0)], u, value, n);
                            cx.runtime_store(cap, &[Step::Field(1)], u, written, n);
                        }
                        cx.jump(join, &[], n);
                        cx.start(join);
                    } else {
                        cx.runtime_net_error(at, error, code, n);
                    }
                }
            },
        )
    }
    fn runtime_net_error(&mut self, to: AddressId, error: ValueId, code: ValueId, n: NodeId) {
        let join = self.new_block(&[]);
        for case in 0..7 {
            let Some(on) =
                self.runtime_compare(Ty::Scalar(Scalar::U8), error, u64::from(case + 1), n)
            else {
                return;
            };
            let yes = self.new_block(&[]);
            let no = self.new_block(&[]);
            self.runtime_branch(on, yes, no, n);
            self.start(yes);
            self.inst(Inst::SetCase { address: to, case }, n);
            self.jump(join, &[], n);
            self.start(no);
        }
        self.inst(
            Inst::SetCase {
                address: to,
                case: 7,
            },
            n,
        );
        self.runtime_store(
            to,
            &[Step::CaseField { case: 7, field: 0 }],
            Ty::Scalar(Scalar::U32),
            code,
            n,
        );
        self.jump(join, &[], n);
        self.start(join);
    }
}
