use super::{HostOp, RtOp, TypeId};

impl RtOp {
    pub fn types(self) -> Vec<TypeId> {
        match self {
            RtOp::Returnable(wait) => wait.row().types(),
            RtOp::Spawn { x, r } | RtOp::SpawnUnit { x, r } | RtOp::Transfer { x, r } => vec![x, r],
            RtOp::Join { r } => vec![r],
            RtOp::ConnNew { item }
            | RtOp::ConnSend { item }
            | RtOp::ConnTrySend { item }
            | RtOp::ConnReceive { item }
            | RtOp::ConnTryReceive { item }
            | RtOp::ChannelNew { item }
            | RtOp::Send { item }
            | RtOp::TrySend { item }
            | RtOp::Receive { item }
            | RtOp::ReceiveRec { item }
            | RtOp::TryReceive { item }
            | RtOp::LatestNew { item }
            | RtOp::SharedNew { item }
            | RtOp::SharedRelease { item } => vec![item],
            RtOp::LaunchTarget { view } => vec![view],
            RtOp::Cancel
            | RtOp::CheckCancel
            | RtOp::ChannelSender
            | RtOp::ConnClient
            | RtOp::LatestHandle
            | RtOp::LatestPublish
            | RtOp::LatestRead
            | RtOp::SharedShare
            | RtOp::DestId
            | RtOp::CompareBytes
            | RtOp::MapHashBytes
            | RtOp::MapHashWord
            | RtOp::MapHashStart
            | RtOp::MapHashAddWord
            | RtOp::MapHashAddBytes
            | RtOp::MapHashFinish
            | RtOp::MapPlaceWord
            | RtOp::MapPlaceBytes
            | RtOp::MapStrongWord
            | RtOp::MapStrongBytes
            | RtOp::MapStrongStart
            | RtOp::Xxh3Short
            | RtOp::Xxh3Stripes
            | RtOp::Sha1Blocks
            | RtOp::Sha1Compress
            | RtOp::ZeroBytes
            | RtOp::Sha256Blocks
            | RtOp::Sha512Blocks
            | RtOp::Crc32c
            | RtOp::Crc64Nvme
            | RtOp::Crc32cCombine
            | RtOp::Crc64NvmeCombine
            | RtOp::Ed25519Verify
            | RtOp::P256Verify
            | RtOp::P256VerifyDer
            | RtOp::P384Verify
            | RtOp::P384VerifyDer
            | RtOp::RsaVerifyPkcs1Sha256
            | RtOp::RsaVerifyPssSha256
            | RtOp::Argon2id
            | RtOp::CauseNext
            | RtOp::CauseValue
            | RtOp::ArgsLen
            | RtOp::ArgsGet
            | RtOp::RootLimit
            | RtOp::LaunchWord
            | RtOp::LaunchHandle
            | RtOp::SecretShare
            | RtOp::DescribeEmit
            | RtOp::Flow(_)
            | RtOp::Host(_)
            | RtOp::FrameRecord
            | RtOp::IsolateEnter
            | RtOp::IsolateLeave
            | RtOp::IsolateCatch
            | RtOp::StepEnter
            | RtOp::StepLeave
            | RtOp::StepCatch => vec![],
        }
    }

    /// Whether a call of this row can fail, and so returns the failure status
    /// (crate docs, "Native ABI").
    pub fn fails(self) -> bool {
        matches!(
            self,
            RtOp::Join { .. }
                | RtOp::Spawn { .. }
                | RtOp::CheckCancel
                | RtOp::Argon2id
                | RtOp::ChannelNew { .. }
                | RtOp::Send { .. }
                | RtOp::Receive { .. }
                | RtOp::ReceiveRec { .. }
                | RtOp::ConnNew { .. }
                | RtOp::ConnSend { .. }
                | RtOp::ConnTrySend { .. }
                | RtOp::ConnReceive { .. }
                | RtOp::SpawnUnit { .. }
                | RtOp::IsolateCatch
                | RtOp::Flow(_)
        ) || matches!(self, RtOp::Host(_) | RtOp::Returnable(_))
    }
}

/// The waits with a returnable row ([`RtOp::Returnable`]), each the stackful
/// row [`Wait::row`] with the child word in front.
#[derive(serde::Serialize, serde::Deserialize, Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum Wait {
    /// An owned provider wait (one with [`HostOp::returnable_symbol`]).
    Host(HostOp),
    Join {
        r: TypeId,
    },
    ConnSend {
        item: TypeId,
    },
    ConnReceive {
        item: TypeId,
    },
}

impl Wait {
    /// The stackful row this wait wraps.
    pub fn row(self) -> RtOp {
        match self {
            Wait::Host(op) => RtOp::Host(op),
            Wait::Join { r } => RtOp::Join { r },
            Wait::ConnSend { item } => RtOp::ConnSend { item },
            Wait::ConnReceive { item } => RtOp::ConnReceive { item },
        }
    }

    /// The returnable wait of a stackful row, if it has one.
    pub fn of(row: RtOp) -> Option<Wait> {
        Some(match row {
            RtOp::Host(op) if op.returnable_symbol().is_some() => Wait::Host(op),
            RtOp::Join { r } => Wait::Join { r },
            RtOp::ConnSend { item } => Wait::ConnSend { item },
            RtOp::ConnReceive { item } => Wait::ConnReceive { item },
            _ => return None,
        })
    }
}
