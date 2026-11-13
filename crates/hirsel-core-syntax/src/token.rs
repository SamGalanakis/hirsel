//! One-byte lexical tags. Fixed terminal tags have their interned symbol number.
use crate::{Span, Symbol, sym};
#[repr(u8)]
#[derive(serde::Serialize, serde::Deserialize, Clone, Copy, Debug, Eq, PartialEq)]
pub enum TokenKind {
    KwFn = 1,
    KwLet = 2,
    KwMut = 3,
    KwMatch = 4,
    KwIf = 5,
    KwElse = 6,
    KwLoop = 7,
    KwWhile = 8,
    KwFor = 9,
    KwIn = 10,
    KwBreak = 11,
    KwContinue = 12,
    KwReturn = 13,
    KwTake = 14,
    KwCopy = 15,
    KwView = 16,
    KwType = 17,
    KwStable = 18,
    KwResource = 19,
    KwScoped = 20,
    KwRegion = 21,
    KwBound = 22,
    KwPub = 23,
    KwOpaque = 24,
    KwConst = 25,
    KwWhere = 26,
    KwTrue = 27,
    KwFalse = 28,
    KwBytes = 29,
    DotDotEq = 30,
    ShlEq = 31,
    ShrEq = 32,
    DotDot = 33,
    Arrow = 34,
    FatArrow = 35,
    AndAnd = 36,
    OrOr = 37,
    Shl = 38,
    Shr = 39,
    EqEq = 40,
    Ne = 41,
    Le = 42,
    Ge = 43,
    PlusEq = 44,
    MinusEq = 45,
    StarEq = 46,
    SlashEq = 47,
    PercentEq = 48,
    AmpEq = 49,
    PipeEq = 50,
    CaretEq = 51,
    LParen = 52,
    RParen = 53,
    LBracket = 54,
    RBracket = 55,
    LBrace = 56,
    RBrace = 57,
    Comma = 58,
    Colon = 59,
    Semicolon = 60,
    Dot = 61,
    At = 62,
    Question = 63,
    Plus = 64,
    Minus = 65,
    Star = 66,
    Slash = 67,
    Percent = 68,
    Bang = 69,
    Tilde = 70,
    Amp = 71,
    Pipe = 72,
    Caret = 73,
    Eq = 74,
    Lt = 75,
    Gt = 76,
    Ident = 128,
    Label,
    Integer,
    Float,
    Character,
    Byte,
    String,
    Bytes,
    LineComment,
    BlockComment,
    DocLine,
    DocBlock,
    Invalid,
    Eof,
}
impl TokenKind {
    #[inline]
    pub fn symbol(self) -> Symbol {
        if (self as u8) <= 76 {
            Symbol(self as u32)
        } else {
            sym::NONE
        }
    }
    pub fn identifier(symbol: Symbol) -> Self {
        match symbol {
            Symbol(1) => Self::KwFn,
            Symbol(2) => Self::KwLet,
            Symbol(3) => Self::KwMut,
            Symbol(4) => Self::KwMatch,
            Symbol(5) => Self::KwIf,
            Symbol(6) => Self::KwElse,
            Symbol(7) => Self::KwLoop,
            Symbol(8) => Self::KwWhile,
            Symbol(9) => Self::KwFor,
            Symbol(10) => Self::KwIn,
            Symbol(11) => Self::KwBreak,
            Symbol(12) => Self::KwContinue,
            Symbol(13) => Self::KwReturn,
            Symbol(14) => Self::KwTake,
            Symbol(15) => Self::KwCopy,
            Symbol(16) => Self::KwView,
            Symbol(17) => Self::KwType,
            Symbol(18) => Self::KwStable,
            Symbol(19) => Self::KwResource,
            Symbol(20) => Self::KwScoped,
            Symbol(21) => Self::KwRegion,
            Symbol(22) => Self::KwBound,
            Symbol(23) => Self::KwPub,
            Symbol(24) => Self::KwOpaque,
            Symbol(25) => Self::KwConst,
            Symbol(26) => Self::KwWhere,
            Symbol(27) => Self::KwTrue,
            Symbol(28) => Self::KwFalse,
            Symbol(29) => Self::KwBytes,
            _ => Self::Ident,
        }
    }
    pub fn is_keyword(self) -> bool {
        (self as u8) <= 29
    }
    pub fn is_punctuation(self) -> bool {
        (30..=76).contains(&(self as u8))
    }
    pub fn is_trivia(self) -> bool {
        matches!(
            self,
            Self::LineComment | Self::BlockComment | Self::DocLine | Self::DocBlock
        )
    }
}
#[derive(serde::Serialize, serde::Deserialize, Clone, Copy, Debug, Eq, PartialEq)]
pub struct Token {
    pub kind: TokenKind,
    pub span: Span,
    pub symbol: Symbol,
    pub line_before: bool,
}
