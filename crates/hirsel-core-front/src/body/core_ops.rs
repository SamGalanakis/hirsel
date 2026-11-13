//! Binding of canonical core declarations to their call-site kernels.
//! Identity is resolved once when the program's declarations are published.
use super::*;
use hirsel_core_ir::{Counted, HostOp, RtOp};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Kernel {
    Stream(HostOp),
    CarrierMove,
    CarrierView,
    StringShorten,
    StringList(&'static str),
    Unique,
    Host(HostOp),
    ArgsLen,
    ArgsGet,
    NewOwner(Option<Counted>),
    Share(Counted),
    TryUnwrap(Counted),
    Unbox,
    Spawn,
    CheckCancel,
    Cancel,
    Join,
    ChannelNew,
    ChannelSender,
    Send,
    TrySend,
    Receive,
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
    owner: &'static str,
    #[cfg(test)]
    private: bool,
}
macro_rules! operations {
    ($(($module:literal, $name:literal, $lane:expr, $class:literal, $owner:literal, $private:literal)),* $(,)?) => {
        const OPERATIONS: &[Entry] = &[$(Entry { module: $module, name: $name, lane: $lane, #[cfg(test)] class: $class, #[cfg(test)] owner: $owner, #[cfg(test)] private: $private }),*];
    };
}

operations! {
    ("int", "parse", Lane::PendingCode, "C", "std-seed-api-889", false),
    ("list", "sum", Lane::PendingCode, "C", "std-seed-api-889", false),
    ("list", "fill", Lane::PendingCode, "C", "std-seed-api-889", false),
    ("string", "from", Lane::PendingCode, "C", "std-seed-api-889", false),
    ("string", "from_utf8", Lane::PendingCode, "C", "std-seed-api-889", false),
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
    ("args", "len", Lane::Runtime(Kernel::ArgsLen), "B", "core-ops-914", false),
    ("args", "get", Lane::Runtime(Kernel::ArgsGet), "B", "core-ops-914", false),
    ("box", "new", Lane::Runtime(Kernel::NewOwner(None)), "A", "core-ops-914", false),
    ("box", "unbox", Lane::Runtime(Kernel::Unbox), "A", "core-ops-914", false),
    ("channel", "new", Lane::Runtime(Kernel::ChannelNew), "B", "core-ops-914", false),
    ("channel", "sender", Lane::Runtime(Kernel::ChannelSender), "B", "core-ops-914", false),
    ("channel", "send", Lane::Runtime(Kernel::Send), "B", "core-ops-914", false),
    ("channel", "try_send", Lane::Runtime(Kernel::TrySend), "B", "core-ops-914", false),
    ("channel", "receive", Lane::Runtime(Kernel::Receive), "B", "core-ops-914", false),
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
    ("int", "widen", Lane::Numeric, "A", "core-check-index-912", false),
    ("int", "narrow", Lane::Numeric, "A", "core-check-index-912", false),
    ("int", "try_narrow", Lane::Numeric, "A", "core-check-index-912", false),
    ("int", "truncate", Lane::Numeric, "A", "core-check-index-912", false),
    ("deque", "Deque", Lane::PendingCode, "C", "core-collections-918", false),
    ("deque", "Deque.$clone", Lane::PendingCode, "C", "core-collections-918", true),
    ("deque", "new", Lane::PendingCode, "C", "core-collections-918", false),
    ("deque", "len", Lane::PendingCode, "C", "core-collections-918", false),
    ("deque", "push_back", Lane::PendingCode, "C", "core-collections-918", false),
    ("deque", "push_front", Lane::PendingCode, "C", "core-collections-918", false),
    ("deque", "pop_front", Lane::PendingCode, "C", "core-collections-918", false),
    ("deque", "pop_back", Lane::PendingCode, "C", "core-collections-918", false),
    ("deque", "grow", Lane::PendingCode, "C", "core-collections-918", true),
    ("deque", "reverse", Lane::PendingCode, "C", "core-collections-918", true),
    ("deque", "slots_empty", Lane::Value, "A", "core-collections-918", true),
    ("deque", "slots_new", Lane::Value, "A", "core-collections-918", true),
    ("deque", "slots_put", Lane::Value, "A", "core-collections-918", true),
    ("deque", "slots_take", Lane::Value, "A", "core-collections-918", true),
    ("deque", "slots_move", Lane::Value, "A", "core-collections-918", true),
    ("deque", "slots_shift", Lane::Value, "A", "core-collections-918", true),
    ("deque", "slots_view", Lane::Value, "A", "core-collections-918", true),
    ("deque", "slots_free", Lane::Value, "A", "core-collections-918", true),
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
    ("list", "pop", Lane::Value, "A", "core-check-calls-910", false),
    ("list", "set", Lane::Value, "A", "core-check-calls-910", false),
    ("list", "swap", Lane::Value, "A", "core-collections-918", false),
    ("list", "clear", Lane::Value, "A", "core-check-calls-910", false),
    ("list", "append", Lane::PendingCode, "A", "core-check-calls-910", false),
    ("map", "push_entry", Lane::Value, "A", "core-check-callables-913", true),
    ("map", "regrow", Lane::Value, "A", "core-check-callables-913", true),
    ("map", "compact", Lane::Value, "A", "core-check-callables-913", true),
    ("map", "make_room", Lane::Value, "C", "core-check-callables-913", true),
    ("map", "install_index", Lane::PendingCode, "C", "core-check-callables-913", true),
    ("map", "swap_value", Lane::Value, "A", "core-check-callables-913", true),
    ("map", "kill", Lane::Value, "A", "core-check-callables-913", true),
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
    ("map", "set", Lane::Value, "C", "core-check-callables-913", false),
    ("map", "remove", Lane::Value, "C", "core-check-callables-913", false),
    ("map", "update", Lane::Value, "C", "core-collections-918", false),
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
    if !f.path.package_is("core", "1") || f.body.is_some() {
        return None;
    }
    OPERATIONS
        .iter()
        .position(|entry| {
            f.path.0.modules == [entry.module]
                && f.path.0.name == entry.name
                && matches!(entry.lane, Lane::Runtime(_))
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
        let Lane::Runtime(kernel) = OPERATIONS[self.p.core_operations[f]? as usize].lane else {
            unreachable!()
        };
        Some(self.runtime_kernel(kernel, f, types, vals, result, n))
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
                if self.live() {
                    self.sequence_word(vals[0]?, 8, Ty::Scalar(Scalar::U64), vals[1]?, n);
                }
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
            Kernel::Unbox => self.runtime_unbox(types[0], vals[0]?, n),
            Kernel::ArgsLen => self
                .runtime_row(RtOp::ArgsLen, &[Arg::Value(vals[0]?)], n)
                .map(|values| values[0]),
            Kernel::ArgsGet => self.runtime_args_get(vals, result, n),
            Kernel::Stream(op) => self.runtime_stream(op, vals, result, n),
            Kernel::Host(op) => self.runtime_host(op, f, vals, result, n),
            Kernel::Spawn => {
                self.runtime_spawn(types[0], types[1], vals[1]?, vals[2], vals[3]?, result, n)
            }
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
            Kernel::Send | Kernel::TrySend => {
                self.runtime_send(kernel, types[0], vals[0]?, vals[1], result, n)
            }
            Kernel::Receive => self.runtime_receive(types[0], vals[0]?, result, n),
            Kernel::Join => self.runtime_join(types[0], vals[0]?, result, n),
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
        if !self.live() {
            return None;
        }
        let signature = self.e.sig(op.signature());
        let mut output = [ValueId(0); 4];
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
        });
        self.inst(Inst::Call { call }, n);
        Some(output)
    }

    fn runtime_handle(&mut self, value: ValueId, n: NodeId) -> ValueId {
        let field = self.address(AddressBase::Value(value), &[], PlaceTy::Value(Ty::Ptr), n);
        self.read_bits(field, Ty::Ptr, n)
    }

    fn runtime_type(&mut self, ty: TyId, n: NodeId) -> Option<TypeId> {
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
        let place = rep.stored()?;
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

    fn runtime_branch(&mut self, condition: ValueId, yes: BlockId, no: BlockId, n: NodeId) {
        let yes = self.edge(yes, &[]);
        let no = self.edge(no, &[]);
        self.end(Terminator::Branch { condition, yes, no }, Some(n));
    }

    /// Both arms initialize an ordinary Result's tag and active payload. Its owner
    /// starts only after the join, so failure before the call commits cleans inputs.
    pub(super) fn runtime_result(
        &mut self,
        ty: TyId,
        ok: ValueId,
        n: NodeId,
        success: impl FnOnce(&mut Self, AddressId, TyId),
        error: impl FnOnce(&mut Self, AddressId, TyId),
    ) -> Option<ValueId> {
        let TyKind::Adt(_, arguments) = self.t.kind(ty).clone() else {
            return None;
        };
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
                case: 0,
            },
            n,
        );
        success(self, to, arguments[0]);
        self.jump(join, &[], n);
        self.start(no);
        self.inst(
            Inst::SetCase {
                address: to,
                case: 1,
            },
            n,
        );
        error(self, to, arguments[1]);
        self.jump(join, &[], n);
        self.start(join);
        if !self.live() {
            return None;
        }
        let value = self.pointer(to, n);
        self.temp(ty, Some(value));
        Some(value)
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
                .get(&self.b.pools.values)?[0],
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
            Kernel::Stream(_) | Kernel::Host(_) | Kernel::ArgsLen | Kernel::ArgsGet => true,
            Kernel::CarrierMove
            | Kernel::CarrierView
            | Kernel::StringShorten
            | Kernel::StringList(_)
            | Kernel::Unique
            | Kernel::NewOwner(_)
            | Kernel::Share(_)
            | Kernel::TryUnwrap(_)
            | Kernel::Unbox
            | Kernel::Spawn
            | Kernel::CheckCancel
            | Kernel::Cancel
            | Kernel::Join
            | Kernel::ChannelNew
            | Kernel::ChannelSender
            | Kernel::Send
            | Kernel::TrySend
            | Kernel::Receive => false,
        }
    }
    for (f, fd) in p.fns.iter().enumerate() {
        let Some(i) = p.core_operations[f] else {
            continue;
        };
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
        assert!(matches!(
            entry.owner,
            "core-ops-914"
                | "core-check-index-912"
                | "core-check-calls-910"
                | "core-check-895"
                | "core-check-callables-913"
                | "std-seed-api-889"
                | "core-collections-918"
        ));
        if entry.private {
            assert!(
                entry.module == "string"
                    && matches!(
                        entry.name,
                        "of_bytes" | "to_bytes" | "view_string" | "shorten" | "push_scalar"
                    )
                    || entry.module == "map"
                    || entry.module == "list" && entry.name.starts_with("slots_")
                    || entry.module == "deque"
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
        bytes: ValueId,
        value: Option<ValueId>,
        function: ValueId,
        result: TyId,
        n: NodeId,
    ) -> Option<ValueId> {
        // Native task-entry glue follows x/r.carried. The fixed runtime C entry
        // still owns input and result buffers by address.
        let logical = self.layout_size(r, true, n)?;
        let xrep = self.rep(x).ok()?;
        let rid = self.runtime_type(r, n)?;
        let (xid, input) = self.runtime_place(x, value, n)?;
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
            &[Arg::Value(bytes), Arg::Address(input), Arg::Value(function)],
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
        let rep = self.rep(item).ok()?;
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
        self.inst(
            Inst::Alloc {
                result: node,
                available,
                region_entry: None,
                kind: kind.map_or(AllocKind::Own, AllocKind::Counted),
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
        let rep = self.rep(item).ok()?;
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
            HostOp::Read | HostOp::Write | HostOp::Close | HostOp::TryLock | HostOp::Unlock
        );
        // HostFile's entire carrier is the opaque u64 key, not provider metadata.
        let handle = if file {
            let at = self.address(AddressBase::Value(vals[0]?), &[], PlaceTy::Value(u64t), n);
            self.read_bits(at, u64t, n)
        } else {
            vals[0]?
        };
        let mut args = [Arg::Value(handle); 3];
        let len = match op {
            HostOp::Open => {
                args[1] = Arg::Value(vals[1]?);
                let ty = self.p.fns[f].params[2].ty;
                let id = self.runtime_type(ty, n)?;
                let at = self.address(AddressBase::Value(vals[2]?), &[], PlaceTy::Declared(id), n);
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
                args[2] = Arg::Value(mode);
                3
            }
            HostOp::Read
            | HostOp::Write
            | HostOp::Mkdir
            | HostOp::Mkdtemp
            | HostOp::Remove
            | HostOp::RemoveTree
            | HostOp::Stdout
            | HostOp::Stderr => {
                args[1] = Arg::Value(vals[1]?);
                2
            }
            _ => 1,
        };
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
                HostOp::Read | HostOp::Mkdtemp => {
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
}
