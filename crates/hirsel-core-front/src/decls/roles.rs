use super::*;
use hirsel_core_layout::Handle;

/// A core type whose kind and layout are fixed by its definition path (LANG-7, OWN-3).
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, serde::Serialize, serde::Deserialize)]
pub enum Role {
    View,
    Box,
    Rc,
    Arc,
    /// `Shared[T]` (CONC-15): a counted node many units read.
    Shared,
    Latest,
    Client,
    Inbox,
    List,
    /// `Columns[T]` (MEM-24): List's rows and charge, stored by column.
    Columns,
    String,
    Text,
    Map,
    Region,
    Capability,
    /// `Secret` (TLS-5): a capability that is a resource, one holder of a designation.
    Secret,
    Media,
    Task,
    Sender,
    Receiver,
    HostFile,
    FixedSpan,
    SeqSpan,
    IndexedSpan,
    SeqIndex,
    Link,
    Dest,
    Writer,
    Child,
    SpanBuilder,
    Builder,
    Args,
    Unique,
    Site,
    Causes,
    Cell,
    StaticFn,
    Code,
    Held,
    CodeSlot,
    Socket,
    Flow,
    FlowEndpoint,
    FrozenBytes,
    ForeignHandle,
}
impl Role {
    pub(super) fn of(path: &DefPath) -> Option<Role> {
        if path.package_is("core", "1")
            && (path.0.modules == ["secret"] && path.0.name == "KeyShare"
                || path.0.modules == ["schedule"] && path.0.name == "Schedule"
                || path.0.modules == ["packet"]
                    && matches!(path.0.name.as_str(), "Sealer" | "Opener" | "HeaderMask"))
        {
            return Some(Role::Media);
        }
        if path.package_is("core", "1") && path.0.modules == ["secret"] && path.0.name == "Scram" {
            return Some(Role::HostFile);
        }
        // TLS-9: a Hello holds its accepted socket; its drop declines it.
        if path.package_is("core", "1") && path.0.modules == ["tls"] && path.0.name == "Hello" {
            return Some(Role::Socket);
        }
        if path.is_core("core.Held") || path.is_core("code.TransferableHeld") {
            return Some(Role::Held);
        }
        if path.is_core("foreign.Ptr") {
            return Some(Role::ForeignHandle);
        }
        if !is_core_prelude(path, &[]) {
            return None;
        }
        Some(match path.0.name.as_str() {
            "View" => Role::View,
            "Box" => Role::Box,
            "Rc" => Role::Rc,
            "Arc" => Role::Arc,
            "Shared" => Role::Shared,
            "Latest" => Role::Latest,
            "Client" => Role::Client,
            "Inbox" => Role::Inbox,
            "List" => Role::List,
            "Columns" => Role::Columns,
            "String" => Role::String,
            "FrozenBytes" => Role::FrozenBytes,
            "Text" => Role::Text,
            "Map" => Role::Map,
            "Region" => Role::Region,
            "Task" => Role::Task,
            "Sender" => Role::Sender,
            "Receiver" => Role::Receiver,
            "HostFile" | "HostEntries" | "ClockHand" | "StopTrigger" | "Served" | "Call" => {
                Role::HostFile
            }
            "Socket" | "ReadHalf" | "WriteHalf" | "Listener" | "UnixListener" | "UnixDatagram"
            | "UdpSocket" | "Proc" => Role::Socket,
            "FixedSpan" => Role::FixedSpan,
            "SeqSpan" => Role::SeqSpan,
            "IndexedSpan" => Role::IndexedSpan,
            "SeqIndex" => Role::SeqIndex,
            "Link" => Role::Link,
            "Dest" => Role::Dest,
            "Writer" => Role::Writer,
            "Child" => Role::Child,
            "SpanBuilder" => Role::SpanBuilder,
            "Builder" => Role::Builder,
            "Args" => Role::Args,
            "Unique" => Role::Unique,
            "Site" => Role::Site,
            "Causes" => Role::Causes,
            "Cell" => Role::Cell,
            "StaticFn" => Role::StaticFn,
            "Code" => Role::Code,
            "CodeSlot" => Role::CodeSlot,
            "Flow" => Role::Flow,
            "Into" | "Port" | "Sink" => Role::FlowEndpoint,

            "Loader" | "Dir" | "Net" | "Clock" | "Random" | "Stdout" | "Stderr" | "Stdin"
            | "Env" | "Process" | "Tasks" | "Foreign" | "Exec" | "Cred" | "Compiler" => {
                Role::Capability
            }
            "Secret" => Role::Secret,
            _ => return None,
        })
    }
    pub fn kind(self) -> Kind {
        match self {
            Role::Box
            | Role::List
            | Role::Columns
            | Role::String
            | Role::FrozenBytes
            | Role::Map
            | Role::SpanBuilder
            | Role::Builder
            | Role::Causes
            | Role::Cell => Kind::Own,
            Role::Rc | Role::Arc | Role::Shared => Kind::Counted,
            Role::View | Role::Region | Role::Dest | Role::Writer => Kind::Scoped,
            Role::Text | Role::FixedSpan | Role::SeqSpan | Role::IndexedSpan | Role::SeqIndex => {
                Kind::Region
            }
            Role::Capability
            | Role::Args
            | Role::Unique
            | Role::Site
            | Role::Child
            | Role::Link
            | Role::StaticFn
            | Role::FlowEndpoint
            | Role::ForeignHandle => Kind::Plain,
            Role::Task
            | Role::Sender
            | Role::Receiver
            | Role::Socket
            | Role::HostFile
            | Role::Latest
            | Role::Client
            | Role::Inbox
            | Role::Flow
            | Role::Code
            | Role::Held
            | Role::CodeSlot
            | Role::Secret
            | Role::Media => Kind::Resource,
        }
    }
}

impl Role {
    /// One-word opaque carriers whose values have runtime cleanup.
    pub(crate) fn owned_handle(self) -> Option<Handle> {
        Some(match self {
            Self::Sender => Handle::Sender,
            Self::Receiver => Handle::Receiver,
            Self::Task => Handle::Task,
            Self::Latest => Handle::Latest,
            Self::Client => Handle::Client,
            Self::Inbox => Handle::Inbox,
            Self::HostFile => Handle::HostFile,
            Self::Socket => Handle::Socket,
            Self::Flow => Handle::Flow,
            Self::Code => Handle::Code,
            Self::Held => Handle::Held,
            Self::CodeSlot => Handle::CodeSlot,
            Self::Secret => Handle::Secret,
            Self::Media => Handle::Media,
            Self::Causes => Handle::Causes,
            _ => return None,
        })
    }
}

pub(super) fn nominal_bounds(
    decl: &TypeDecl,
    meet: impl Fn() -> u16,
    at: impl Fn(usize) -> u16,
) -> u16 {
    let declared = match decl.role {
        // L2 (mc-units-lang): an owner crosses as a flat copy of its content,
        // so only a list of plain elements and a string are `cross`.
        Some(Role::List) => {
            let elems = meet();
            elems & (KEY | ORDERED | SHAREABLE | SEND | CODEC)
                | if elems & (PLAIN | CROSS) == PLAIN | CROSS {
                    CROSS
                } else {
                    0
                }
        }
        Some(Role::String) => KEY | ORDERED | SHAREABLE | SEND | CROSS | CODEC,
        Some(Role::FrozenBytes) => SHAREABLE | SEND,
        Some(Role::Text) => KEY | ORDERED,
        Some(Role::FixedSpan | Role::SeqSpan | Role::IndexedSpan) => meet() & (KEY | ORDERED),
        Some(Role::Child | Role::Unique | Role::Site) => PLAIN | SHAREABLE | SEND | CROSS | KEY,
        Some(Role::Args) => PLAIN | SHAREABLE | SEND | CROSS,
        Some(Role::Held) => {
            if decl.path.is_core("code.TransferableHeld") {
                SEND
            } else {
                0
            }
        }
        Some(Role::Causes) => SEND,
        Some(Role::StaticFn) => PLAIN | SHAREABLE | SEND | CROSS | KEY,
        Some(Role::Rc) => SHAREABLE | (meet() & (KEY | ORDERED)),
        Some(Role::Arc) => SHAREABLE | SEND | (meet() & (KEY | ORDERED)),
        // CONC-15: a holder crosses as its pointer; the payload stays put.
        Some(Role::Shared) => SHAREABLE | SEND | CROSS | (meet() & (KEY | ORDERED)),
        Some(Role::Box) => meet() & (SHAREABLE | SEND | KEY | ORDERED),
        Some(
            Role::Sender
            | Role::Receiver
            | Role::HostFile
            | Role::Latest
            | Role::Inbox
            | Role::Code
            | Role::CodeSlot,
        ) => SEND,
        Some(Role::Client | Role::Secret | Role::Media) => SEND | CROSS,
        Some(Role::Socket) => match decl.path.0.name.as_str() {
            "Socket" | "Listener" | "UdpSocket" => SEND | CROSS,
            _ => SEND,
        },
        Some(Role::Map) => meet() & (SHAREABLE | SEND | CODEC),
        // MEM-24: a Columns sends and shares as its rows do. It does not cross a unit
        // boundary (CONC-15), which copies one contiguous run per sequence.
        Some(Role::Columns) => meet() & (SHAREABLE | SEND),
        Some(Role::Capability) => PLAIN | SHAREABLE | SEND | CROSS,
        Some(Role::ForeignHandle) => PLAIN | SEND,
        Some(_) => 0,
        None if decl.structural => meet(),
        None => {
            // CONC-2: a record or variant type is `cross` when it declares the bound
            // and each field of each case is `cross`, which `check_declared_bounds` checks.
            let mut bounds = decl.declared & !PLAIN;
            for &i in &decl.summary.bound_params {
                bounds &= at(i as usize);
            }
            if let Some(codec) = decl.codec() {
                let mut codec_bound = decl.declared & CODEC;
                for &i in &codec.params {
                    codec_bound &= at(i as usize);
                }
                bounds = bounds & !CODEC | codec_bound;
            }
            bounds
        }
    };
    match decl.repr {
        Repr::Region => declared & !(SEND | CROSS | SHAREABLE | CODEC),
        Repr::Scoped => declared & !(SEND | CROSS | SHAREABLE | KEY | ORDERED | CODEC),
        // SEM-7: a resource is never compared, whatever it declares.
        Repr::Resource => declared & !(KEY | ORDERED | CODEC),
        Repr::Own => declared,
    }
}

impl TypeDecl {
    pub(crate) fn pack_callable_carrier(&self) -> bool {
        matches!(self.path.0.name.as_str(), "Code" | "CodeSlot") && is_core_prelude(&self.path, &[])
    }

    pub fn all_fields(&self) -> impl Iterator<Item = &FieldDecl> {
        let (r, v): (&[FieldDecl], &[CaseDecl]) = match &self.body {
            DeclBody::Record(f) => (f, &[]),
            DeclBody::Variant(c) => (&[], c),
            DeclBody::Alias(_) => (&[], &[]),
        };
        r.iter().chain(v.iter().flat_map(|c| c.fields.iter()))
    }
}

impl Program<'_> {
    pub(crate) fn held_send_type(&self, at: ItemRef, children: &[NodeId]) -> Option<DeclId> {
        let [_, bound] = *children else {
            return None;
        };
        if self.target(at, bound) != Some(&Target::Builtin(hirsel_core_syntax::sym::SEND)) {
            return None;
        }
        match self.def(&DefPath::core("code.TransferableHeld"))? {
            Def::Type(id) => Some(id),
            _ => None,
        }
    }

    pub(super) fn invalid_pack_carrier(&mut self, decl: DeclId, args: &[TyId], site: Site) -> bool {
        if self.decls[decl.index()].pack_callable_carrier()
            && args.first().is_some_and(|&arg| {
                !matches!(
                    self.types.kind(arg),
                    TyKind::Fn(.., Callable::Plain) | TyKind::Param(_) | TyKind::Error
                )
            })
        {
            self.refusals.push(refusal(
                "PACK-14",
                site,
                "a code handle needs a named entry's function type",
            ));
            true
        } else {
            false
        }
    }
}

pub(super) fn write_adt_path(decl: &TypeDecl, layout: bool, s: &mut String) {
    if !layout && decl.path.is_core("code.TransferableHeld") {
        s.push_str("Held[send]");
    } else {
        let _ = decl.path.write_to(s);
    }
}
