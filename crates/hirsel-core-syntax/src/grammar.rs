//! The revised grammar's terminal tables. All precedence and boundary decisions live here.
use crate::{Symbol, sym};
pub const ITEM_START: &[Symbol] = &[
    sym::AT,
    sym::PUB,
    sym::FN,
    sym::TYPE,
    sym::STABLE,
    sym::RESOURCE,
    sym::SCOPED,
    sym::CONST,
];
pub const PREFIX: &[Symbol] = &[
    sym::MINUS,
    sym::BANG,
    sym::TILDE,
    sym::TAKE,
    sym::MUT,
    sym::COPY,
    sym::VIEW,
];
pub const BOUNDS: &[Symbol] = &[
    sym::PLAIN,
    sym::SHAREABLE,
    sym::SEND,
    sym::KEY,
    sym::ORDERED,
    sym::INTEGER,
    sym::FLOAT,
    sym::PACKED,
    sym::STREAMABLE,
];
pub const ASSIGN: &[Symbol] = &[
    sym::EQ,
    sym::PLUSEQ,
    sym::MINUSEQ,
    sym::STAREQ,
    sym::SLASHEQ,
    sym::PERCENTEQ,
    sym::SHLEQ,
    sym::SHREQ,
    sym::AMPEQ,
    sym::PIPEEQ,
    sym::CARETEQ,
];
/// Operator binding powers and nonassociativity, dispatched by interned terminal.
pub enum Continuation {
    Call,
    Index,
    Projection,
    Try,
    Placement,
    Binary(u8, bool),
}
#[inline]
pub fn continuation(word: Symbol) -> Option<Continuation> {
    Some(match word {
        sym::LPAREN => Continuation::Call,
        sym::LBRACKET => Continuation::Index,
        sym::DOT => Continuation::Projection,
        sym::QUESTION => Continuation::Try,
        sym::IN => Continuation::Placement,
        sym::DOTDOT => Continuation::Binary(1, true),
        sym::DOTDOTEQ => Continuation::Binary(1, true),
        sym::OROR => Continuation::Binary(2, false),
        sym::ANDAND => Continuation::Binary(3, false),
        sym::EQEQ => Continuation::Binary(4, true),
        sym::NE => Continuation::Binary(4, true),
        sym::LT => Continuation::Binary(4, true),
        sym::LE => Continuation::Binary(4, true),
        sym::GT => Continuation::Binary(4, true),
        sym::GE => Continuation::Binary(4, true),
        sym::PIPE => Continuation::Binary(5, false),
        sym::CARET => Continuation::Binary(6, false),
        sym::AMP => Continuation::Binary(7, false),
        sym::SHL => Continuation::Binary(8, false),
        sym::SHR => Continuation::Binary(8, false),
        sym::PLUS => Continuation::Binary(9, false),
        sym::MINUS => Continuation::Binary(9, false),
        sym::STAR => Continuation::Binary(10, false),
        sym::SLASH => Continuation::Binary(10, false),
        sym::PERCENT => Continuation::Binary(10, false),
        _ => return None,
    })
}
