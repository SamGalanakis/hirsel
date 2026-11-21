//! The revised grammar's terminal tables. All precedence and boundary decisions live here.
use crate::{Symbol, sym};
pub const ITEM_START: &[Symbol] = &[
    sym::AT,
    sym::PUB,
    sym::FN,
    sym::TYPE,
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
    sym::CROSS,
    sym::KEY,
    sym::ORDERED,
    sym::INTEGER,
    sym::FLOAT,
    sym::PACKED,
    sym::STREAMABLE,
    sym::CODEC,
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
    Binary(u8, bool),
}
/// LANG-4's closed set for a complete expression at a newline. Assignment
/// operators are handled only by the statement grammar, never by expressions.
#[inline]
pub fn line_continuation(word: Symbol) -> bool {
    matches!(
        word,
        sym::DOTDOT
            | sym::DOTDOTEQ
            | sym::OROR
            | sym::ANDAND
            | sym::EQEQ
            | sym::NE
            | sym::LT
            | sym::LE
            | sym::GT
            | sym::GE
            | sym::PIPE
            | sym::CARET
            | sym::AMP
            | sym::SHL
            | sym::SHR
            | sym::PLUS
            | sym::STAR
            | sym::SLASH
            | sym::PERCENT
            | sym::DOT
            | sym::QUESTION
    )
}
#[inline(always)]
pub fn continuation(word: Symbol) -> Option<Continuation> {
    Some(match word {
        sym::LPAREN => Continuation::Call,
        sym::LBRACKET => Continuation::Index,
        sym::DOT => Continuation::Projection,
        sym::QUESTION => Continuation::Try,
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
