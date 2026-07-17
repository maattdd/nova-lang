use std::fmt;

#[allow(dead_code)]
/// A position in source code
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Span {
    pub start: usize,
    pub end: usize,
    pub line: usize,
    pub col: usize,
}

impl Span {
    pub fn new(start: usize, end: usize, line: usize, col: usize) -> Self {
        Self { start, end, line, col }
    }

    pub fn zero() -> Self {
        Self { start: 0, end: 0, line: 0, col: 0 }
    }

    pub fn merge(&self, other: &Span) -> Span {
        Span {
            start: self.start.min(other.start),
            end: self.end.max(other.end),
            line: self.line.min(other.line),
            col: self.col.min(other.col),
        }
    }
}

impl fmt::Display for Span {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.line, self.col)
    }
}

#[allow(dead_code)]
/// All token kinds in Nova
#[derive(Debug, Clone, PartialEq)]
pub enum TokenKind {
    // Keywords
    Func,
    Let,
    Var,
    Return,
    If,
    Else,
    Match,
    Case,
    Enum,
    Struct,
    Macro,
    Quote,
    While,
    For,
    In,
    As,
    Is,
    True,
    False,
    Nil,
    Type,
    Trait,
    Impl,
    Import,
    Module,
    Pub,

    // Identifiers and literals
    Ident(String),
    IntLiteral(i64),
    FloatLiteral(f64),
    StringLiteral(String),
    CharLiteral(char),

    // Operators
    Plus,          // +
    Minus,         // -
    Star,          // *
    Slash,         // /
    Percent,       // %
    Eq,            // =
    EqEq,          // ==
    NotEq,         // !=
    Lt,            // <
    Gt,            // >
    LtEq,          // <=
    GtEq,          // >=
    AndAnd,        // &&
    OrOr,          // ||
    Not,           // !
    Arrow,         // ->
    FatArrow,      // =>
    Dot,           // .
    DotDot,        // ..
    Colon,         // :
    Semicolon,     // ;
    Comma,         // ,
    At,            // @
    Dollar,        // $
    Underscore,    // _
    Question,      // ?
    Tilde,         // ~
    Hash,          // #

    // Delimiters
    LParen,        // (
    RParen,        // )
    LBrace,        // {
    RBrace,        // }
    LBracket,      // [
    RBracket,      // ]

    // Compound assignment
    PlusEq,        // +=
    MinusEq,       // -=
    StarEq,        // *=
    SlashEq,       // /=
    PercentEq,     // %=

    // Intrinsics (#-prefixed compiler builtins)
    HashCpp,       // #cpp
    HashInclude,   // #include
    HashParse,     // #parse
    HashError,     // #error
    HashSplice,    // #splice
    HashFilterPub, // #filter_pub
    RawCpp(String), // raw C++ text captured by #cpp { ... }

    // End of file
    Eof,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Token {
    pub kind: TokenKind,
    pub span: Span,
}

impl Token {
    pub fn new(kind: TokenKind, span: Span) -> Self {
        Self { kind, span }
    }
}

impl fmt::Display for TokenKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TokenKind::Func => write!(f, "func"),
            TokenKind::Let => write!(f, "let"),
            TokenKind::Var => write!(f, "var"),
            TokenKind::Return => write!(f, "return"),
            TokenKind::If => write!(f, "if"),
            TokenKind::Else => write!(f, "else"),
            TokenKind::Match => write!(f, "match"),
            TokenKind::Case => write!(f, "case"),
            TokenKind::Enum => write!(f, "enum"),
            TokenKind::Struct => write!(f, "struct"),
            TokenKind::Macro => write!(f, "macro"),
            TokenKind::Quote => write!(f, "quote"),
            TokenKind::While => write!(f, "while"),
            TokenKind::For => write!(f, "for"),
            TokenKind::In => write!(f, "in"),
            TokenKind::As => write!(f, "as"),
            TokenKind::Is => write!(f, "is"),
            TokenKind::True => write!(f, "true"),
            TokenKind::False => write!(f, "false"),
            TokenKind::Nil => write!(f, "nil"),
            TokenKind::Type => write!(f, "type"),
            TokenKind::Trait => write!(f, "trait"),
            TokenKind::Impl => write!(f, "impl"),
            TokenKind::Import => write!(f, "import"),
            TokenKind::Module => write!(f, "module"),
            TokenKind::Pub => write!(f, "pub"),
            TokenKind::Ident(s) => write!(f, "{}", s),
            TokenKind::IntLiteral(n) => write!(f, "{}", n),
            TokenKind::FloatLiteral(n) => write!(f, "{}", n),
            TokenKind::StringLiteral(s) => write!(f, "\"{}\"", s),
            TokenKind::CharLiteral(c) => write!(f, "'{}'", c),
            TokenKind::Plus => write!(f, "+"),
            TokenKind::Minus => write!(f, "-"),
            TokenKind::Star => write!(f, "*"),
            TokenKind::Slash => write!(f, "/"),
            TokenKind::Percent => write!(f, "%"),
            TokenKind::Eq => write!(f, "="),
            TokenKind::EqEq => write!(f, "=="),
            TokenKind::NotEq => write!(f, "!="),
            TokenKind::Lt => write!(f, "<"),
            TokenKind::Gt => write!(f, ">"),
            TokenKind::LtEq => write!(f, "<="),
            TokenKind::GtEq => write!(f, ">="),
            TokenKind::AndAnd => write!(f, "&&"),
            TokenKind::OrOr => write!(f, "||"),
            TokenKind::Not => write!(f, "!"),
            TokenKind::Arrow => write!(f, "->"),
            TokenKind::FatArrow => write!(f, "=>"),
            TokenKind::Dot => write!(f, "."),
            TokenKind::DotDot => write!(f, ".."),
            TokenKind::Colon => write!(f, ":"),
            TokenKind::Semicolon => write!(f, ";"),
            TokenKind::Comma => write!(f, ","),
            TokenKind::At => write!(f, "@"),
            TokenKind::Dollar => write!(f, "$"),
            TokenKind::Underscore => write!(f, "_"),
            TokenKind::Question => write!(f, "?"),
            TokenKind::Tilde => write!(f, "~"),
            TokenKind::Hash => write!(f, "#"),
            TokenKind::LParen => write!(f, "("),
            TokenKind::RParen => write!(f, ")"),
            TokenKind::LBrace => write!(f, "{{"),
            TokenKind::RBrace => write!(f, "}}"),
            TokenKind::LBracket => write!(f, "["),
            TokenKind::RBracket => write!(f, "]"),
            TokenKind::PlusEq => write!(f, "+="),
            TokenKind::MinusEq => write!(f, "-="),
            TokenKind::StarEq => write!(f, "*="),
            TokenKind::SlashEq => write!(f, "/="),
            TokenKind::PercentEq => write!(f, "%="),
            TokenKind::HashCpp => write!(f, "#cpp"),
            TokenKind::HashInclude => write!(f, "#include"),
            TokenKind::HashParse => write!(f, "#parse"),
            TokenKind::HashError => write!(f, "#error"),
            TokenKind::HashSplice => write!(f, "#splice"),
            TokenKind::HashFilterPub => write!(f, "#filter_pub"),
            TokenKind::RawCpp(ref s) => write!(f, "#cpp {{ {} }}", s),
            TokenKind::Eof => write!(f, "<eof>"),
        }
    }
}
