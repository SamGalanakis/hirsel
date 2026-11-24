// ---------------------------------------------------------------------------------------------
// The prelude bindings: what an extracted body may call outside the extracted set.

/// A store or codec operation bound by `Store.lean` (trusted model).
pub(super) struct PreludeFn {
    pub(super) path: &'static str,
    pub(super) lean: &'static str,
    /// Runs in `Hirsel.Prog` and yields the Hirsel result value (it never throws a typed error).
    pub(super) effect: bool,
    /// The argument positions that are the transaction (dropped: it is the state).
    pub(super) tx_args: &'static [usize],
    /// A pure operation that updates this `mut` argument: it returns `(result, new value)`.
    pub(super) mut_out: Option<usize>,
}

pub(super) const PRELUDE_FNS: &[PreludeFn] = &[
    PreludeFn {
        path: "store@1/store/info",
        lean: "Hirsel.store.info",
        effect: true,
        tx_args: &[0],
        mut_out: None,
    },
    PreludeFn {
        path: "store@1/store/fetch",
        lean: "Hirsel.store.fetch",
        effect: true,
        tx_args: &[0],
        mut_out: None,
    },
    PreludeFn {
        path: "store@1/store/put",
        lean: "Hirsel.store.put",
        effect: true,
        tx_args: &[0],
        mut_out: None,
    },
    PreludeFn {
        path: "store@1/store/delete",
        lean: "Hirsel.store.delete",
        effect: true,
        tx_args: &[0],
        mut_out: None,
    },
    PreludeFn {
        path: "std@1/codec/encode",
        lean: "Hirsel.std.codec.encode",
        effect: false,
        tx_args: &[],
        mut_out: Some(2),
    },
];

/// Builtins, by the definition the IR's `BuiltinUse` names.
pub(super) const BUILTINS: &[(&str, &str)] = &[
    ("core@1/int/checked_add", "Hirsel.int.checked_add"),
    ("core@1/int/checked_sub", "Hirsel.int.checked_sub"),
    ("core@1/int/checked_mul", "Hirsel.int.checked_mul"),
    ("core@1/list/new", "[]"),
    ("core@1/list/len", "Hirsel.list.len"),
    ("core@1/list/slice", "Hirsel.list.slice"),
    ("core@1/arc/share", "id"),
    ("core@1/list/push", "Hirsel.list.push"),
    ("core@1/core/clone", "clone"),
];

/// Constructors of prelude types a body may name.
pub(super) const PRELUDE_CTORS: &[(&str, &str, usize)] = &[
    ("Ok", "Except.ok", 1),
    ("Error", "Except.error", 1),
    ("Some", "some", 1),
    ("None", "none", 0),
    ("Limit", "Hirsel.store.Error.Limit", 1),
    ("Missing", "Hirsel.store.Error.Missing", 1),
    ("Exists", "Hirsel.store.Error.Exists", 1),
    ("Key", "Hirsel.store.Limit.Key", 0),
    ("Value", "Hirsel.store.Limit.Value", 0),
    ("Transaction", "Hirsel.store.Limit.Transaction", 0),
    ("Name", "Hirsel.store.Limit.Name", 0),
    ("Ids", "Hirsel.store.Limit.Ids", 0),
    ("Manifest", "Hirsel.store.Limit.Manifest", 0),
    ("Triggers", "Hirsel.store.Limit.Triggers", 0),
];
