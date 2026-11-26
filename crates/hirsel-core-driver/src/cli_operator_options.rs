//! The options of `hirsel host`, `register`, `deploy` and `decide` (launch.md "The
//! operator CLI").
use super::{OptionId, OptionSpec};

pub(super) static OPTIONS: &[OptionSpec] = &[
    option!(
        MaxPlaces,
        ["--max-places"],
        Some("N"),
        ["host", "launcher"],
        "Groups the platform host keeps live at once."
    ),
    option!(
        IdleMs,
        ["--idle-ms"],
        Some("MS"),
        ["host", "launcher"],
        "Idle time after which a serving group is stopped."
    ),
    option!(
        TapeRetention,
        ["--tape-retention"],
        Some("DAYS"),
        ["register"],
        "Retain a recording's pinned image for this many days. Default 30."
    ),
    option!(
        Workdir,
        ["--workdir"],
        Some("DIR"),
        ["register"],
        "Working directory for the group's children. Default: the current directory."
    ),
    option!(
        Listen,
        ["--listen"],
        Some("ADDR"),
        ["register"],
        "TCP address of the group's inherited listener."
    ),
    option!(
        FrontHttp,
        ["--front-http"],
        Some("NAME"),
        ["register"],
        "Route this TLS SNI name to the group's HTTP listener. Repeat for more names."
    ),
    option!(
        FrontStream,
        ["--front-stream"],
        Some("NAME"),
        ["register"],
        "Route this TLS SNI name to the group's stream listener. Repeat for more names."
    ),
    option!(
        ApproveGrant,
        ["--approve-grant"],
        Some("+LINE|-LINE"),
        ["deploy", "decide"],
        "Approve one line of the grant diff a request awaits a decision on: +LINE for an authority the release gains, -LINE for one it loses."
    ),
    option!(
        Refuse,
        ["--refuse"],
        None,
        ["decide"],
        "Refuse the decision the request awaits; the request fails Denied."
    ),
    option!(
        HeldAll,
        ["--held"],
        Some("release|cancel"),
        ["deploy", "decide"],
        "The fate of every run the deploy holds that no --held-entry names."
    ),
    option!(
        HeldEntry,
        ["--held-entry"],
        Some("PATH=release|cancel"),
        ["deploy", "decide"],
        "The fate of the held runs of one entry path, as the trigger report prints it. Repeat for more paths."
    ),
    option!(
        SourceDir,
        ["--source"],
        Some("DIR"),
        ["register", "deploy"],
        "Deploy the source tree under DIR, sent by content: the host compiles it. The positional names the package within DIR. Default \".\"."
    ),
    option!(
        PackageRoot,
        ["--package-root"],
        Some("ROOT"),
        ["register"],
        "A package root the group's source may link from the host's package store. Repeat for more roots."
    ),
    option!(
        HostMemory,
        ["--memory"],
        Some("BYTES"),
        ["register"],
        "Memory allowance for a group's release."
    ),
];
