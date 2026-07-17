use crate::token::{Span, Token, TokenKind};
use crate::error::CompileError;

pub struct Lexer {
    source: Vec<char>,
    pos: usize,
    line: usize,
    col: usize,
}

impl Lexer {
    pub fn new(source: &str) -> Self {
        Self {
            source: source.chars().collect(),
            pos: 0,
            line: 1,
            col: 1,
        }
    }

    fn current(&self) -> Option<char> {
        self.source.get(self.pos).copied()
    }

    fn peek_next(&self) -> Option<char> {
        self.source.get(self.pos + 1).copied()
    }

    fn advance(&mut self) -> Option<char> {
        let ch = self.source.get(self.pos).copied();
        if let Some(c) = ch {
            self.pos += 1;
            if c == '\n' {
                self.line += 1;
                self.col = 1;
            } else {
                self.col += 1;
            }
        }
        ch
    }

    fn span_start(&self) -> usize {
        self.pos
    }

    fn mk_span(&self, start: usize) -> Span {
        Span::new(start, self.pos, self.line, self.col)
    }

    fn skip_whitespace_and_comments(&mut self) {
        loop {
            match self.current() {
                Some(' ') | Some('\t') | Some('\r') | Some('\n') => {
                    self.advance();
                }
                Some('/') => {
                    if self.peek_next() == Some('/') {
                        // Line comment
                        while let Some(c) = self.current() {
                            if c == '\n' { break; }
                            self.advance();
                        }
                    } else if self.peek_next() == Some('*') {
                        // Block comment
                        self.advance(); // /
                        self.advance(); // *
                        let mut depth: i32 = 1;
                        while depth > 0 {
                            match (self.current(), self.peek_next()) {
                                (Some('/'), Some('*')) => {
                                    self.advance();
                                    self.advance();
                                    depth += 1;
                                }
                                (Some('*'), Some('/')) => {
                                    self.advance();
                                    self.advance();
                                    depth -= 1;
                                }
                                (None, _) => break,
                                _ => { self.advance(); }
                            }
                        }
                    } else {
                        break;
                    }
                }
                _ => break,
            }
        }
    }

    fn read_number(&mut self, first: char, start: usize) -> Token {
        let mut is_float = false;
        let mut num_str = String::new();
        num_str.push(first);

        while let Some(c) = self.current() {
            if c.is_ascii_digit() {
                num_str.push(c);
                self.advance();
            } else if c == '.' && !is_float {
                is_float = true;
                num_str.push(c);
                self.advance();
            } else if c == '_' {
                self.advance(); // skip underscores in numbers
            } else {
                break;
            }
        }

        let kind = if is_float {
            TokenKind::FloatLiteral(num_str.parse().unwrap_or(0.0))
        } else {
            TokenKind::IntLiteral(num_str.parse().unwrap_or(0))
        };

        Token::new(kind, self.mk_span(start))
    }

    fn read_string(&mut self, quote_char: char, start: usize) -> Result<Token, CompileError> {
        let mut s = String::new();
        loop {
            match self.current() {
                None => {
                    return Err(CompileError::lex(
                        "Unterminated string literal",
                        self.mk_span(start),
                    ));
                }
                Some('\\') => {
                    self.advance();
                    match self.current() {
                        Some('n') => { s.push('\n'); self.advance(); }
                        Some('t') => { s.push('\t'); self.advance(); }
                        Some('r') => { s.push('\r'); self.advance(); }
                        Some('\\') => { s.push('\\'); self.advance(); }
                        Some('"') => { s.push('"'); self.advance(); }
                        Some('\'') => { s.push('\''); self.advance(); }
                        Some('0') => { s.push('\0'); self.advance(); }
                        Some(c) => {
                            s.push('\\');
                            s.push(c);
                            self.advance();
                        }
                        None => {
                            return Err(CompileError::lex(
                                "Unterminated escape sequence",
                                self.mk_span(start),
                            ));
                        }
                    }
                }
                Some(c) if c == quote_char => {
                    self.advance();
                    break;
                }
                Some(c) => {
                    s.push(c);
                    self.advance();
                }
            }
        }

        let kind = if quote_char == '"' {
            TokenKind::StringLiteral(s)
        } else {
            if s.chars().count() != 1 {
                return Err(CompileError::lex(
                    "Character literal must contain exactly one character",
                    self.mk_span(start),
                ));
            }
            TokenKind::CharLiteral(s.chars().next().unwrap())
        };

        Ok(Token::new(kind, self.mk_span(start)))
    }

    fn read_ident(&mut self, first: char, start: usize) -> Token {
        let mut s = String::new();
        s.push(first);

        while let Some(c) = self.current() {
            if c.is_alphanumeric() || c == '_' {
                s.push(c);
                self.advance();
            } else {
                break;
            }
        }

        let kind = match s.as_str() {
            "func" => TokenKind::Func,
            "let" => TokenKind::Let,
            "var" => TokenKind::Var,
            "return" => TokenKind::Return,
            "if" => TokenKind::If,
            "else" => TokenKind::Else,
            "match" => TokenKind::Match,
            "case" => TokenKind::Case,
            "enum" => TokenKind::Enum,
            "struct" => TokenKind::Struct,
            "macro" => TokenKind::Macro,
            "quote" => TokenKind::Quote,
            "while" => TokenKind::While,
            "for" => TokenKind::For,
            "in" => TokenKind::In,
            "as" => TokenKind::As,
            "is" => TokenKind::Is,
            "true" => TokenKind::True,
            "false" => TokenKind::False,
            "nil" => TokenKind::Nil,
            "type" => TokenKind::Type,
            "trait" => TokenKind::Trait,
            "impl" => TokenKind::Impl,
            "import" => TokenKind::Import,
            "module" => TokenKind::Module,
            "pub" => TokenKind::Pub,
            _ => TokenKind::Ident(s),
        };

        Token::new(kind, self.mk_span(start))
    }

    fn read_operator(&mut self, first: char, start: usize) -> Token {
        let kind = match (first, self.current()) {
            ('+', Some('=')) => { self.advance(); TokenKind::PlusEq }
            ('-', Some('>')) => { self.advance(); TokenKind::Arrow }
            ('-', Some('=')) => { self.advance(); TokenKind::MinusEq }
            ('*', Some('=')) => { self.advance(); TokenKind::StarEq }
            ('/', Some('=')) => { self.advance(); TokenKind::SlashEq }
            ('%', Some('=')) => { self.advance(); TokenKind::PercentEq }
            ('=', Some('>')) => { self.advance(); TokenKind::FatArrow }
            ('=', Some('=')) => { self.advance(); TokenKind::EqEq }
            ('!', Some('=')) => { self.advance(); TokenKind::NotEq }
            ('<', Some('=')) => { self.advance(); TokenKind::LtEq }
            ('>', Some('=')) => { self.advance(); TokenKind::GtEq }
            ('&', Some('&')) => { self.advance(); TokenKind::AndAnd }
            ('|', Some('|')) => { self.advance(); TokenKind::OrOr }
            ('.', Some('.')) => { self.advance(); TokenKind::DotDot }

            ('+', _) => TokenKind::Plus,
            ('-', _) => TokenKind::Minus,
            ('*', _) => TokenKind::Star,
            ('/', _) => TokenKind::Slash,
            ('%', _) => TokenKind::Percent,
            ('=', _) => TokenKind::Eq,
            ('<', _) => TokenKind::Lt,
            ('>', _) => TokenKind::Gt,
            ('!', _) => TokenKind::Not,
            ('.', _) => TokenKind::Dot,

            _ => unreachable!(),
        };

        Token::new(kind, self.mk_span(start))
    }

    pub fn next_token(&mut self) -> Result<Token, CompileError> {
        self.skip_whitespace_and_comments();
        let start = self.span_start();

        match self.current() {
            None => Ok(Token::new(TokenKind::Eof, self.mk_span(start))),
            Some(c) => {
                match c {
                    '(' => { self.advance(); Ok(Token::new(TokenKind::LParen, self.mk_span(start))) }
                    ')' => { self.advance(); Ok(Token::new(TokenKind::RParen, self.mk_span(start))) }
                    '{' => { self.advance(); Ok(Token::new(TokenKind::LBrace, self.mk_span(start))) }
                    '}' => { self.advance(); Ok(Token::new(TokenKind::RBrace, self.mk_span(start))) }
                    '[' => { self.advance(); Ok(Token::new(TokenKind::LBracket, self.mk_span(start))) }
                    ']' => { self.advance(); Ok(Token::new(TokenKind::RBracket, self.mk_span(start))) }
                    ':' => { self.advance(); Ok(Token::new(TokenKind::Colon, self.mk_span(start))) }
                    ';' => { self.advance(); Ok(Token::new(TokenKind::Semicolon, self.mk_span(start))) }
                    ',' => { self.advance(); Ok(Token::new(TokenKind::Comma, self.mk_span(start))) }
                    '@' => { self.advance(); Ok(Token::new(TokenKind::At, self.mk_span(start))) }
                    '$' => { self.advance(); Ok(Token::new(TokenKind::Dollar, self.mk_span(start))) }
                    '?' => { self.advance(); Ok(Token::new(TokenKind::Question, self.mk_span(start))) }
                    '~' => { self.advance(); Ok(Token::new(TokenKind::Tilde, self.mk_span(start))) }
                    '#' => { self.advance(); Ok(Token::new(TokenKind::Hash, self.mk_span(start))) }
                    '_' => {
                        self.advance();
                        if let Some(c) = self.current() {
                            if c.is_alphanumeric() || c == '_' {
                                // It's an identifier starting with underscore
                                return Ok(self.read_ident('_', start));
                            }
                        }
                        Ok(Token::new(TokenKind::Underscore, self.mk_span(start)))
                    }
                    '"' | '\'' => {
                        self.advance();
                        self.read_string(c, start)
                    }
                    c if c.is_ascii_digit() => {
                        self.advance();
                        Ok(self.read_number(c, start))
                    }
                    c if c.is_alphabetic() => {
                        self.advance();
                        Ok(self.read_ident(c, start))
                    }
                    c if "+-*/%=!<>&|.".contains(c) => {
                        self.advance();
                        Ok(self.read_operator(c, start))
                    }
                    c => Err(CompileError::lex(
                        format!("Unexpected character: '{}'", c),
                        self.mk_span(start),
                    )),
                }
            }
        }
    }

    /// Collect all tokens into a Vec (useful for parser)
    pub fn tokenize(&mut self) -> Result<Vec<Token>, CompileError> {
        let mut tokens = Vec::new();
        loop {
            let token = self.next_token()?;
            let is_eof = token.kind == TokenKind::Eof;
            tokens.push(token);
            if is_eof { break; }
        }
        Ok(tokens)
    }
}
