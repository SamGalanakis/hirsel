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
            RtOp::Join { r } => RtOp::Join { r: self.ty(r) },
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
