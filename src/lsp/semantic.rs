//! Semantic token computation for Nova sources.
//!
//! Walks the token stream and assigns each token a semantic type and modifiers.
//! Output is delta-encoded per the LSP specification.

#![allow(dead_code)]

use crate::token::{Token, TokenKind};

/// Index in the legend (must match order in mod.rs::SEMANTIC_TOKEN_TYPES).
#[derive(Debug, Clone, Copy)]
#[repr(u32)]
pub enum NovaSemanticToken {
    Keyword = 0,
    String = 1,
    Number = 2,
    Comment = 3,
    Operator = 4,
    Type = 5,
    Function = 6,
    Variable = 7,
    Parameter = 8,
    EnumMember = 9,
    Macro = 10,
    Struct = 11,
    TypeParameter = 12,
    Property = 13,
}

/// Bit-flag modifier index (must match order in mod.rs::SEMANTIC_TOKEN_MODIFIERS).
pub mod modifier {
    pub const DECLARATION: u32 = 1 << 0;
    pub const DEFINITION: u32 = 1 << 1;
    pub const READONLY: u32 = 1 << 2;
    pub const DEFAULT_LIBRARY: u32 = 1 << 3;
    pub const MODIFICATION: u32 = 1 << 4;
}

/// Compute delta-encoded semantic tokens for a source file.
///
/// Returns a flat list of `[delta_line, delta_start, len, token_type, modifiers]` tuples.
pub fn compute_semantic_tokens(_source: &str, tokens: &[Token]) -> Vec<u32> {
    let mut result = Vec::new();
    let mut prev_line: u32 = 0;
    let mut prev_start: u32 = 0;

    // First pass: figure out which identifiers are definitions
    let mut definitions: Vec<String> = Vec::new();
    for w in tokens.windows(2) {
        match &w[0].kind {
            TokenKind::Func | TokenKind::Let | TokenKind::Var | TokenKind::Struct
            | TokenKind::Enum | TokenKind::Trait | TokenKind::Macro | TokenKind::Type => {
                if let TokenKind::Ident(ref name) = &w[1].kind {
                    definitions.push(name.clone());
                }
            }
            TokenKind::Case => {
                if let TokenKind::Ident(ref name) = &w[1].kind {
                    definitions.push(name.clone());
                }
            }
            _ => {}
        }
    }

    for token in tokens {
        if token.kind == TokenKind::Eof {
            continue;
        }

        let (token_type, modifiers) = classify_token(token, &definitions);

        if let Some(tt) = token_type {
            let line = (token.span.line.max(1) - 1) as u32;
            let start = (token.span.col.max(1) - 1) as u32;
            let len = (token.span.end - token.span.start) as u32;

            let delta_line = line.saturating_sub(prev_line);
            let delta_start = if delta_line == 0 {
                start.saturating_sub(prev_start)
            } else {
                start
            };

            result.extend_from_slice(&[delta_line, delta_start, len, tt as u32, modifiers]);

            prev_line = line;
            prev_start = start;
        }
    }

    result
}

/// Classify a token into a semantic token type + modifiers.
fn classify_token(token: &Token, definitions: &[String]) -> (Option<NovaSemanticToken>, u32) {
    use TokenKind::*;

    match &token.kind {
        // Keywords
        Func | Let | Var | Return | If | Else | Match | Case | Enum | Struct
        | Macro | Quote | While | For | In | As | Is | Import | Module | Pub
        | Trait | Impl | Type => {
            (Some(NovaSemanticToken::Keyword), 0)
        }

        // Literals
        True | False | Nil => (Some(NovaSemanticToken::Keyword), 0),

        // Strings
        StringLiteral(_) | CharLiteral(_) => (Some(NovaSemanticToken::String), 0),

        // Numbers
        IntLiteral(_) | FloatLiteral(_) => (Some(NovaSemanticToken::Number), 0),

        // Comments are filtered out by the lexer (not in token stream), but
        // we handle RawCpp as comment-like
        RawCpp(_) => (Some(NovaSemanticToken::Comment), 0),

        // Operators
        Plus | Minus | Star | Slash | Percent | Eq | EqEq | NotEq | Lt | Gt
        | LtEq | GtEq | AndAnd | OrOr | Not | Arrow | FatArrow | Dot | DotDot
        | Colon | Semicolon | Comma | At | Dollar | Underscore | Question
        | Tilde | Hash | LParen | RParen | LBrace | RBrace | LBracket | RBracket
        | PlusEq | MinusEq | StarEq | SlashEq | PercentEq
        | HashCpp | HashInclude | HashParse | HashError | HashSplice | HashFilterPub => {
            (Some(NovaSemanticToken::Operator), 0)
        }

        // Identifiers — context-dependent classification
        Ident(name) => {
            let is_def = definitions.contains(name);
            let mods = if is_def {
                modifier::DECLARATION
            } else {
                0
            };

            // Heuristic: capitalized names are types/structs/enums
            let first_char = name.chars().next().unwrap_or('_');
            if first_char.is_uppercase() {
                (Some(NovaSemanticToken::Type), mods)
            } else {
                (Some(NovaSemanticToken::Variable), mods)
            }
        }

        Eof => (None, 0),
    }
}
