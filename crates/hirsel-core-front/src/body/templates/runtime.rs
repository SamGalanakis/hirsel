use super::*;

impl Substitution<'_> {
    pub(super) fn runtime(&mut self, op: hirsel_core_ir::RtOp) -> hirsel_core_ir::RtOp {
        use hirsel_core_ir::RtOp;
        match op {
            RtOp::Spawn { x, r } => RtOp::Spawn {
                x: self.ty(x),
                r: self.ty(r),
            },
            RtOp::SpawnUnit { x, r } => RtOp::SpawnUnit {
                x: self.ty(x),
                r: self.ty(r),
            },
            RtOp::ConnNew { item } => RtOp::ConnNew {
                item: self.ty(item),
            },
            RtOp::ConnSend { item } => RtOp::ConnSend {
                item: self.ty(item),
            },
            RtOp::ConnTrySend { item } => RtOp::ConnTrySend {
                item: self.ty(item),
            },
            RtOp::ConnReceive { item } => RtOp::ConnReceive {
                item: self.ty(item),
            },
            RtOp::ConnTryReceive { item } => RtOp::ConnTryReceive {
                item: self.ty(item),
            },
            RtOp::ConnReceiveUntil { item } => RtOp::ConnReceiveUntil {
                item: self.ty(item),
            },
            RtOp::DataDecode {
                witness,
                bytes,
                output,
            } => RtOp::DataDecode {
                witness: self.ty(witness),
                bytes: self.ty(bytes),
                output: self.ty(output),
            },
            RtOp::Join { r } => RtOp::Join { r: self.ty(r) },
            RtOp::HeldNew { item } => RtOp::HeldNew {
                item: self.ty(item),
            },
            RtOp::HeldTake { item } => RtOp::HeldTake {
                item: self.ty(item),
            },
            RtOp::ResourceOrigin { item } => RtOp::ResourceOrigin {
                item: self.ty(item),
            },
            RtOp::ChannelNew { item } => RtOp::ChannelNew {
                item: self.ty(item),
            },
            RtOp::LatestNew { item } => RtOp::LatestNew {
                item: self.ty(item),
            },
            RtOp::SharedNew { item } => RtOp::SharedNew {
                item: self.ty(item),
            },
            RtOp::SharedRelease { item } => RtOp::SharedRelease {
                item: self.ty(item),
            },
            RtOp::LaunchTarget { view } => RtOp::LaunchTarget {
                view: self.ty(view),
            },
            RtOp::Send { item } => RtOp::Send {
                item: self.ty(item),
            },
            RtOp::TrySend { item } => RtOp::TrySend {
                item: self.ty(item),
            },
            RtOp::Receive { item } => RtOp::Receive {
                item: self.ty(item),
            },
            RtOp::ReceiveUntil { item } => RtOp::ReceiveUntil {
                item: self.ty(item),
            },
            RtOp::TryReceive { item } => RtOp::TryReceive {
                item: self.ty(item),
            },
            op => op,
        }
    }
}
