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
            "Flow" => Role::Flow,
            "Into" | "Port" | "Sink" => Role::FlowEndpoint,

            "Dir" | "Net" | "Clock" | "Random" | "Stdout" | "Stderr" | "Stdin" | "Env"
            | "Process" | "Tasks" | "Foreign" | "Exec" | "Cred" => Role::Capability,
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
            Self::Secret => Handle::Secret,
            Self::Media => Handle::Media,
            Self::Causes => Handle::Causes,
            _ => return None,
        })
    }
}
