//! The fixed compiler/runtime carrier for owner flow operations.

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, serde::Serialize, serde::Deserialize)]
#[repr(u8)]
pub enum Op {
    New,
    Sink,
    Stage,
    Port,
    Push,
    Advance,
    End,
    Next,
    Finish,
}

impl Op {
    pub const ALL: &[Self] = &[
        Self::New,
        Self::Sink,
        Self::Stage,
        Self::Port,
        Self::Push,
        Self::Advance,
        Self::End,
        Self::Next,
        Self::Finish,
    ];

    pub const fn name(self) -> &'static str {
        match self {
            Self::New => "flow_new",
            Self::Sink => "flow_sink",
            Self::Stage => "flow_stage",
            Self::Port => "flow_port",
            Self::Push => "flow_push",
            Self::Advance => "flow_advance",
            Self::End => "flow_end",
            Self::Next => "flow_next",
            Self::Finish => "flow_finish",
        }
    }
}
