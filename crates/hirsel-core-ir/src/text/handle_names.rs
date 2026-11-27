use hirsel_core_layout::Handle;

pub(super) const HANDLE_NAMES: &[(&str, Handle)] = &[
    ("task", Handle::Task),
    ("sender", Handle::Sender),
    ("receiver", Handle::Receiver),
    ("latest", Handle::Latest),
    ("client", Handle::Client),
    ("inbox", Handle::Inbox),
    ("cap", Handle::Capability),
    ("causes", Handle::Causes),
    ("hostfile", Handle::HostFile),
    ("socket", Handle::Socket),
    ("flow", Handle::Flow),
    ("secret", Handle::Secret),
    ("media", Handle::Media),
    ("code", Handle::Code),
    ("codeslot", Handle::CodeSlot),
];
