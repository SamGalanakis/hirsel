use super::{RtOp, TypeId};

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
}
