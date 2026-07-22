use crate::token::{Span, Token, TokenKind};
use crate::ast::*;
use crate::error::CompileError;

pub struct Parser {
    tokens: Vec<Token>,
    source: String,
    pos: usize,
}

impl Parser {
    pub fn new(tokens: Vec<Token>, source: &str) -> Self {
        Self { tokens, source: source.to_string(), pos: 0 }
    }

    fn peek(&self) -> &Token {
        self.tokens.get(self.pos).unwrap_or(&EOF_TOKEN)
    }

    fn peek_kind(&self) -> &TokenKind {
        &self.peek().kind
    }

    fn advance(&mut self) -> &Token {
        self.pos += 1;
        self.tokens.get(self.pos - 1).unwrap_or(&EOF_TOKEN)
    }

    fn consume(&mut self, expected: TokenKind) -> Result<&Token, CompileError> {
        let actual = self.peek_kind().clone();
        if std::mem::discriminant(&actual) == std::mem::discriminant(&expected) {
            Ok(self.advance())
        } else {
            Err(CompileError::parse(
                format!("Expected '{}', got '{}'", expected, actual),
                self.peek().span,
            ))
        }
    }

    fn current_span(&self) -> Span {
        self.peek().span
    }

    // ─── Module ───────────────────────────────────────────────────────────────

    pub fn parse_module(&mut self, name: String) -> Result<Module, CompileError> {
        // Optional module declaration
        if matches!(self.peek_kind(), TokenKind::Module) {
            self.advance();
            if let TokenKind::Ident(n) = &self.peek_kind().clone() {
                let _module_name = n.clone();
                self.advance();
                let _ = self.consume(TokenKind::Semicolon);
            }
        }

        let mut imports = Vec::new();
        let mut items = Vec::new();

        while !matches!(self.peek_kind(), TokenKind::Eof) {
            if matches!(self.peek_kind(), TokenKind::Import) {
                imports.push(self.parse_import()?);
            } else {
                items.push(self.parse_item()?);
            }
        }

        Ok(Module { name, imports, items })
    }

    fn parse_import(&mut self) -> Result<Import, CompileError> {
        let start = self.current_span();
        self.advance(); // import
        let mut path = Vec::new();

        // Parse module path: e.g., std.io or std.collections.list
        loop {
            if let TokenKind::Ident(s) = &self.peek_kind().clone() {
                path.push(s.clone());
                self.advance();
            } else {
                break;
            }
            if matches!(self.peek_kind(), TokenKind::Dot) {
                self.advance();
            } else {
                break;
            }
        }

        // Parse import items (optional braces for selective imports)
        let items = if matches!(self.peek_kind(), TokenKind::LBrace) {
            self.advance(); // {
            let mut items = Vec::new();
            while !matches!(self.peek_kind(), TokenKind::RBrace) && !matches!(self.peek_kind(), TokenKind::Eof) {
                let name = self.parse_ident()?;
                let alias = if matches!(self.peek_kind(), TokenKind::As) {
                    self.advance();
                    Some(self.parse_ident()?)
                } else {
                    None
                };
                items.push(ImportItem::Single { name, alias });
                if matches!(self.peek_kind(), TokenKind::Comma) {
                    self.advance();
                }
            }
            let _ = self.consume(TokenKind::RBrace);
            items
        } else {
            // import foo.bar — import everything
            vec![ImportItem::All]
        };

        let _ = self.consume(TokenKind::Semicolon);
        Ok(Import { path, items, span: start })
    }

    fn parse_item(&mut self) -> Result<Item, CompileError> {
        let is_pub = matches!(self.peek_kind(), TokenKind::Pub);
        if is_pub { self.advance(); }

        match self.peek_kind().clone() {
            TokenKind::Func => self.parse_function(is_pub).map(Item::Function),
            TokenKind::Struct => self.parse_struct(is_pub).map(Item::Struct),
            TokenKind::Enum => self.parse_enum(is_pub).map(Item::Enum),
            TokenKind::Macro => self.parse_macro_def().map(Item::Macro),
            TokenKind::Trait => self.parse_trait_def().map(Item::Trait),
            TokenKind::Impl => self.parse_impl_block().map(Item::Impl),
            TokenKind::Type => self.parse_type_alias(is_pub).map(Item::TypeAlias),
            TokenKind::Let | TokenKind::Var => self.parse_var_decl(is_pub).map(Item::VarDecl),
            TokenKind::At => {
                let start = self.current_span();
                self.advance();
                // Accept keywords as macro names (e.g., @import)
                let name = if matches!(self.peek_kind(), TokenKind::Import) {
                    self.advance();
                    "import".to_string()
                } else {
                    self.parse_ident()?
                };
                let args = if matches!(self.peek_kind(), TokenKind::LParen) {
                    self.parse_call_args()?
                } else {
                    vec![self.parse_expr()?]
                };
                Ok(Item::MacroCall(MacroCallItem { name, args, span: start }))
            }
            _ => Err(CompileError::parse(
                format!("Expected declaration, got '{}'", self.peek_kind()),
                self.peek().span,
            )),
        }
    }

    // ─── Function ─────────────────────────────────────────────────────────────

    fn parse_function(&mut self, is_pub: bool) -> Result<Function, CompileError> {
        let start = self.current_span();
        self.advance(); // func

        let name = self.parse_ident()?;
        let generics = self.parse_generic_params()?;
        let _ = self.consume(TokenKind::LParen);
        let params = self.parse_delimited(
            TokenKind::RParen,
            TokenKind::Comma,
            |p| p.parse_param(),
        )?;

        let return_type = if matches!(self.peek_kind(), TokenKind::Arrow) {
            self.advance();
            self.parse_type()?
        } else {
            Type::Unit
        };

        let body = self.parse_block()?;

        Ok(Function {
            name,
            generics,
            params,
            return_type,
            body,
            is_pub,
            span: start,
        })
    }

    fn parse_param(&mut self) -> Result<Param, CompileError> {
        let start = self.current_span();
        // Check for named parameter: ~name
        let named = matches!(self.peek_kind(), TokenKind::Tilde);
        if named { self.advance(); }
        let name = self.parse_ident()?;
        let _ = self.consume(TokenKind::Colon);
        let ty = self.parse_type()?;
        let default = if matches!(self.peek_kind(), TokenKind::Eq) {
            self.advance();
            Some(self.parse_expr()?)
        } else {
            None
        };
        Ok(Param { name, ty, named, default, span: start })
    }

    // ─── Struct ───────────────────────────────────────────────────────────────

    fn parse_struct(&mut self, is_pub: bool) -> Result<Struct, CompileError> {
        let start = self.current_span();
        self.advance(); // struct

        let name = self.parse_ident()?;
        let generics = self.parse_generic_params()?;
        let _ = self.consume(TokenKind::LBrace);

        let mut fields = Vec::new();
        while !matches!(self.peek_kind(), TokenKind::RBrace) && !matches!(self.peek_kind(), TokenKind::Eof) {
            let field_start = self.current_span();
            let field_name = self.parse_ident()?;
            let _ = self.consume(TokenKind::Colon);
            let field_ty = self.parse_type()?;
            fields.push(StructField { name: field_name, ty: field_ty, span: field_start });
            if matches!(self.peek_kind(), TokenKind::Comma) {
                self.advance();
            }
        }
        let _ = self.consume(TokenKind::RBrace);

        Ok(Struct { name, generics, fields, is_pub, span: start })
    }

    // ─── Enum ─────────────────────────────────────────────────────────────────

    fn parse_enum(&mut self, is_pub: bool) -> Result<Enum, CompileError> {
        let start = self.current_span();
        self.advance(); // enum

        let name = self.parse_ident()?;
        let generics = self.parse_generic_params()?;
        let _ = self.consume(TokenKind::LBrace);

        let mut cases = Vec::new();
        while !matches!(self.peek_kind(), TokenKind::RBrace) && !matches!(self.peek_kind(), TokenKind::Eof) {
            let case_start = self.current_span();
            let _ = self.consume(TokenKind::Case);
            let case_name = self.parse_ident()?;

            let payload = if matches!(self.peek_kind(), TokenKind::LParen) {
                self.advance();
                let ty = self.parse_type()?;
                let _ = self.consume(TokenKind::RParen);
                Some(ty)
            } else {
                None
            };

            cases.push(EnumCase { name: case_name, payload, span: case_start });

            if matches!(self.peek_kind(), TokenKind::Comma) {
                self.advance();
            }
        }
        let _ = self.consume(TokenKind::RBrace);

        Ok(Enum { name, generics, cases, is_pub, span: start })
    }

    // ─── Type Alias ───────────────────────────────────────────────────────────

    fn parse_type_alias(&mut self, _is_pub: bool) -> Result<TypeAlias, CompileError> {
        let start = self.current_span();
        self.advance(); // type

        let name = self.parse_ident()?;
        let generics = self.parse_generic_params()?;
        let _ = self.consume(TokenKind::Eq);
        let ty = self.parse_type()?;
        let _ = self.consume(TokenKind::Semicolon);

        Ok(TypeAlias { name, generics, ty, span: start })
    }

    // ─── Variable Declaration ─────────────────────────────────────────────────


    fn parse_trait_def(&mut self) -> Result<TraitDef, CompileError> {
        let start = self.current_span();
        self.advance(); // trait
        let name = self.parse_ident()?;
        let generics = self.parse_generic_params()?;
        let self_alias = if matches!(self.peek_kind(), TokenKind::As) { self.advance(); Some(self.parse_ident()?) } else { None };
        let _ = self.consume(TokenKind::LBrace);
        let mut methods = Vec::new();
        while !matches!(self.peek_kind(), TokenKind::RBrace) && !matches!(self.peek_kind(), TokenKind::Eof) {
            methods.push(self.parse_trait_method()?);
        }
        let _ = self.consume(TokenKind::RBrace);
        Ok(TraitDef { name, generics, self_alias, methods, span: start })
    }

    /// Parse a trait method: signature with optional body or ;
    fn parse_trait_method(&mut self) -> Result<Function, CompileError> {
        let start = self.current_span();
        let _ = self.consume(TokenKind::Func);
        let name = self.parse_ident()?;
        let generics = self.parse_generic_params()?;
        let _ = self.consume(TokenKind::LParen);
        let mut params = Vec::new();
        if !matches!(self.peek_kind(), TokenKind::RParen) {
            params.push(self.parse_param()?);
            while matches!(self.peek_kind(), TokenKind::Comma) {
                self.advance();
                params.push(self.parse_param()?);
            }
        }
        let _ = self.consume(TokenKind::RParen);
        let return_type = if matches!(self.peek_kind(), TokenKind::Arrow) {
            self.advance();
            self.parse_type()?
        } else { Type::Unit };
        // Body: { ... } or just ;
        let body = if matches!(self.peek_kind(), TokenKind::LBrace) {
            self.parse_block()?
        } else {
            let _ = self.consume(TokenKind::Semicolon);
            Block { stmts: vec![], span: start }
        };
        Ok(Function { name, generics, params, return_type, body, is_pub: false, span: start })
    }

    fn parse_impl_block(&mut self) -> Result<ImplBlock, CompileError> {
        let start = self.current_span();
        self.advance();
        let trait_name = self.parse_ident()?;
        let generics = self.parse_generic_params()?;
        let _ = self.consume(TokenKind::For);
        let target_type = self.parse_type()?;
        let _ = self.consume(TokenKind::LBrace);
        let mut methods = Vec::new();
        while !matches!(self.peek_kind(), TokenKind::RBrace) && !matches!(self.peek_kind(), TokenKind::Eof) { methods.push(self.parse_function(false)?); }
        let _ = self.consume(TokenKind::RBrace);
        Ok(ImplBlock { trait_name, generics, target_type, methods, span: start })
    }
    fn parse_var_decl(&mut self, _is_pub: bool) -> Result<VarDecl, CompileError> {
        let start = self.current_span();
        let is_mut = matches!(self.peek_kind(), TokenKind::Var);
        self.advance(); // let or var

        let name = self.parse_ident()?;
        let ty = if matches!(self.peek_kind(), TokenKind::Colon) {
            self.advance();
            Some(self.parse_type()?)
        } else {
            None
        };

        let value = if matches!(self.peek_kind(), TokenKind::Eq) {
            self.advance();
            Some(self.parse_expr()?)
        } else {
            None
        };

        let _ = self.consume(TokenKind::Semicolon);

        Ok(VarDecl { name, ty, value, is_mut, span: start })
    }

    // ─── Macro Definition ─────────────────────────────────────────────────────

    fn parse_macro_def(&mut self) -> Result<MacroDef, CompileError> {
        let start = self.current_span();
        self.advance(); // macro

        let name = self.parse_ident()?;
        let _ = self.consume(TokenKind::LParen);
        let mut params = Vec::new();
        while !matches!(self.peek_kind(), TokenKind::RParen) && !matches!(self.peek_kind(), TokenKind::Eof) {
            params.push(self.parse_ident()?);
            if matches!(self.peek_kind(), TokenKind::Comma) {
                self.advance();
            }
        }
        let _ = self.consume(TokenKind::RParen);

        let body = if matches!(self.peek_kind(), TokenKind::FatArrow) {
            self.advance();
            Box::new(self.parse_expr()?)
        } else {
            Box::new(Expr::block(self.parse_block()?.stmts, start))
        };

        Ok(MacroDef { name, params, body, span: start })
    }

    // ─── Types ────────────────────────────────────────────────────────────────

    fn parse_type(&mut self) -> Result<Type, CompileError> {
        // Check for @T (GC reference)
        if matches!(self.peek_kind(), TokenKind::At) {
            self.advance();
            let inner = self.parse_type()?;
            return Ok(Type::GcRef(Box::new(inner)));
        }

        // Check for function type: (params) -> ret
        if matches!(self.peek_kind(), TokenKind::LParen) {
            self.advance();

            // Try to parse as tuple/function type
            let mut params = Vec::new();
            if !matches!(self.peek_kind(), TokenKind::RParen) {
                params.push(self.parse_type()?);
                while matches!(self.peek_kind(), TokenKind::Comma) {
                    self.advance();
                    params.push(self.parse_type()?);
                }
            }
            let _ = self.consume(TokenKind::RParen);

            if matches!(self.peek_kind(), TokenKind::Arrow) {
                self.advance();
                let ret = self.parse_type()?;
                return Ok(Type::Function(FunctionType {
                    params,
                    ret: Box::new(ret),
                }));
            }

            // Tuple type
            if params.len() == 1 {
                return Ok(params.into_iter().next().unwrap());
            }
            if params.is_empty() {
                return Ok(Type::Unit);
            }
            return Ok(Type::Tuple(params));
        }

        // Path type
        let mut segments = Vec::new();
        loop {
            let seg_name = self.parse_ident()?;
            let args = self.parse_generic_args()?;
            segments.push(PathSegment { name: seg_name, args });

            if matches!(self.peek_kind(), TokenKind::Colon) && self.peek_next_is_colon() {
                self.advance(); // :
                self.advance(); // :
            } else {
                break;
            }
        }

        Ok(Type::Path(Path { segments }))
    }

    fn peek_next_is_colon(&self) -> bool {
        if self.pos + 1 < self.tokens.len() {
            matches!(self.tokens[self.pos + 1].kind, TokenKind::Colon)
        } else {
            false
        }
    }

    // ─── Expressions ──────────────────────────────────────────────────────────

    pub fn parse_expr(&mut self) -> Result<Expr, CompileError> {
        self.parse_assignment()
    }

    fn parse_assignment(&mut self) -> Result<Expr, CompileError> {
        let expr = self.parse_or()?;

        // Check for assignment operators
        match self.peek_kind() {
            TokenKind::Eq => {
                self.advance();
                let rhs = self.parse_assignment()?;
                let rhs_span = rhs.span;
                let expr_span = expr.span;
                return Ok(Expr::assign(expr, rhs, expr_span.merge(&rhs_span)));
            }
            TokenKind::PlusEq | TokenKind::MinusEq | TokenKind::StarEq |
            TokenKind::SlashEq | TokenKind::PercentEq => {
                let op = match self.advance().kind {
                    TokenKind::PlusEq => BinOp::Add,
                    TokenKind::MinusEq => BinOp::Sub,
                    TokenKind::StarEq => BinOp::Mul,
                    TokenKind::SlashEq => BinOp::Div,
                    TokenKind::PercentEq => BinOp::Mod,
                    _ => unreachable!(),
                };
                let rhs = self.parse_assignment()?;
                let span = expr.span.merge(&rhs.span);
                return Ok(Expr::new(ExprKind::AssignOp {
                    target: Box::new(expr),
                    op,
                    value: Box::new(rhs),
                }, span));
            }
            _ => {}
        }
        Ok(expr)
    }

    fn parse_or(&mut self) -> Result<Expr, CompileError> {
        let mut left = self.parse_and()?;
        while matches!(self.peek_kind(), TokenKind::OrOr) {
            self.advance();
            let right = self.parse_and()?;
            let span = left.span.merge(&right.span);
            left = Expr::new(ExprKind::Binary {
                op: BinOp::Or,
                left: Box::new(left),
                right: Box::new(right),
            }, span);
        }
        Ok(left)
    }

    fn parse_and(&mut self) -> Result<Expr, CompileError> {
        let mut left = self.parse_comparison()?;
        while matches!(self.peek_kind(), TokenKind::AndAnd) {
            self.advance();
            let right = self.parse_comparison()?;
            let span = left.span.merge(&right.span);
            left = Expr::new(ExprKind::Binary {
                op: BinOp::And,
                left: Box::new(left),
                right: Box::new(right),
            }, span);
        }
        Ok(left)
    }

    fn parse_comparison(&mut self) -> Result<Expr, CompileError> {
        let mut left = self.parse_term()?;
        while matches!(self.peek_kind(),
            TokenKind::EqEq | TokenKind::NotEq | TokenKind::Lt | TokenKind::Gt |
            TokenKind::LtEq | TokenKind::GtEq
        ) {
            let op = BinOp::from_token(self.peek_kind()).unwrap();
            self.advance();
            let right = self.parse_term()?;
            let span = left.span.merge(&right.span);
            left = Expr::new(ExprKind::Binary {
                op,
                left: Box::new(left),
                right: Box::new(right),
            }, span);
        }
        Ok(left)
    }

    fn parse_term(&mut self) -> Result<Expr, CompileError> {
        let mut left = self.parse_factor()?;
        while matches!(self.peek_kind(), TokenKind::Plus | TokenKind::Minus) {
            let op = BinOp::from_token(self.peek_kind()).unwrap();
            self.advance();
            let right = self.parse_factor()?;
            let span = left.span.merge(&right.span);
            left = Expr::new(ExprKind::Binary {
                op,
                left: Box::new(left),
                right: Box::new(right),
            }, span);
        }
        Ok(left)
    }

    fn parse_factor(&mut self) -> Result<Expr, CompileError> {
        let mut left = self.parse_unary()?;
        while matches!(self.peek_kind(), TokenKind::Star | TokenKind::Slash | TokenKind::Percent) {
            let op = BinOp::from_token(self.peek_kind()).unwrap();
            self.advance();
            let right = self.parse_unary()?;
            let span = left.span.merge(&right.span);
            left = Expr::new(ExprKind::Binary {
                op,
                left: Box::new(left),
                right: Box::new(right),
            }, span);
        }
        Ok(left)
    }

    fn parse_unary(&mut self) -> Result<Expr, CompileError> {
        match self.peek_kind() {
            TokenKind::Minus => {
                let start = self.current_span();
                self.advance();
                let expr = self.parse_unary()?;
                let expr_span = expr.span;
                Ok(Expr::new(ExprKind::Unary { op: UnaryOp::Neg, expr: Box::new(expr) }, start.merge(&expr_span)))
            }
            TokenKind::Not => {
                let start = self.current_span();
                self.advance();
                let expr = self.parse_unary()?;
                let expr_span = expr.span;
                Ok(Expr::new(ExprKind::Unary { op: UnaryOp::Not, expr: Box::new(expr) }, start.merge(&expr_span)))
            }
            _ => self.parse_postfix(),
        }
    }

    fn parse_postfix(&mut self) -> Result<Expr, CompileError> {
        let mut expr = self.parse_primary()?;

        loop {
            match self.peek_kind() {
                TokenKind::Dot => {
                    self.advance();
                    let field = self.parse_ident()?;
                    let span = expr.span;
                    // UFCS: obj.method(args) → method(obj, args)
                    if matches!(self.peek_kind(), TokenKind::LParen) {
                        let mut args = self.parse_call_args()?;
                        args.insert(0, expr);
                        expr = Expr::new(ExprKind::Call {
                            func: Box::new(Expr::ident(&field, span)),
                            args,
                        }, span);
                    // p.debug with no args → DotAccess (resolved by type checker)
                    } else {
                        expr = Expr::new(ExprKind::DotAccess {
                            object: Box::new(expr),
                            field,
                        }, span);
                    }
                }
                TokenKind::LParen => {
                    let args = self.parse_call_args()?;
                    let span = expr.span;
                    expr = Expr::new(ExprKind::Call {
                        func: Box::new(expr),
                        args,
                    }, span);
                }
                TokenKind::LBracket => {
                    self.advance();
                    let index = self.parse_expr()?;
                    let _ = self.consume(TokenKind::RBracket);
                    let span = expr.span;
                    expr = Expr::new(ExprKind::Index {
                        object: Box::new(expr),
                        index: Box::new(index),
                    }, span);
                }
                TokenKind::Colon if self.peek_next_is_colon() => {
                    // Path continuation: expr :: rest
                    self.advance(); // :
                    self.advance(); // :
                    let rest = self.parse_ident()?;
                    // Transform into path
                    match expr.kind {
                        ExprKind::Ident(name) => {
                            expr = Expr::new(ExprKind::Path(vec![name, rest]), expr.span);
                        }
                        ExprKind::Path(mut segments) => {
                            segments.push(rest);
                            expr = Expr::new(ExprKind::Path(segments), expr.span);
                        }
                        _ => {
                            return Err(CompileError::parse(
                                "Invalid path expression",
                                expr.span,
                            ));
                        }
                    }
                }
                _ => break,
            }
        }

        Ok(expr)
    }

    fn parse_call_args(&mut self) -> Result<Vec<Expr>, CompileError> {
        self.advance(); // (
        self.parse_delimited(TokenKind::RParen, TokenKind::Comma, |p| p.parse_expr())
    }

    fn parse_primary(&mut self) -> Result<Expr, CompileError> {
        let start = self.current_span();

        match self.peek_kind().clone() {
            // Literals
            TokenKind::IntLiteral(n) => {
                self.advance();
                Ok(Expr::int_literal(n, start))
            }
            TokenKind::FloatLiteral(n) => {
                self.advance();
                Ok(Expr::new(ExprKind::FloatLiteral(n), start))
            }
            TokenKind::StringLiteral(s) => {
                self.advance();
                Ok(Expr::string_literal(&s, start))
            }
            TokenKind::CharLiteral(c) => {
                self.advance();
                Ok(Expr::new(ExprKind::CharLiteral(c), start))
            }
            TokenKind::True => {
                self.advance();
                Ok(Expr::new(ExprKind::BoolLiteral(true), start))
            }
            TokenKind::False => {
                self.advance();
                Ok(Expr::new(ExprKind::BoolLiteral(false), start))
            }
            TokenKind::Nil => {
                self.advance();
                Ok(Expr::new(ExprKind::NilLiteral, start))
            }

            // Quote — must come before the keyword-as-ident catch-all below,
            // which would otherwise swallow `quote` as an identifier.
            TokenKind::Quote => self.parse_quote_expr(),

            // .case or .case(expr) — enum constructor, mirroring pattern syntax
            TokenKind::Dot => {
                self.advance();
                let case = self.parse_ident()?;
                let arg = if matches!(self.peek_kind(), TokenKind::LParen) {
                    self.advance();
                    let inner = self.parse_expr()?;
                    let _ = self.consume(TokenKind::RParen);
                    Some(Box::new(inner))
                } else {
                    None
                };
                Ok(Expr::new(ExprKind::EnumCtor { path: vec![], case, arg }, start))
            }

            // Identifiers and paths (keywords usable as idents in expressions)
            _ if self.is_ident_or_keyword() => {
                let name = self.consume_ident_or_keyword();

                // Check for :: (path separator)
                if matches!(self.peek_kind(), TokenKind::Colon) && self.peek_next_is_colon() {
                    self.advance(); // :
                    self.advance(); // :
                    let mut segments = vec![name];
                    loop {
                        segments.push(self.parse_ident()?);
                        if matches!(self.peek_kind(), TokenKind::Colon) && self.peek_next_is_colon() {
                            self.advance();
                            self.advance();
                        } else {
                            break;
                        }
                    }
                    Ok(Expr::new(ExprKind::Path(segments), start))
                // Check for struct literal: TypeName { field: value, ... }
                // Only for capitalized identifiers (types are PascalCase)
                } else if matches!(self.peek_kind(), TokenKind::LBrace) && name.chars().next().map_or(false, |c| c.is_uppercase()) {
                    let fields = self.parse_struct_literal_fields()?;
                    Ok(Expr::new(ExprKind::StructLit { path: vec![name], fields }, start))
                } else {
                    Ok(Expr::ident(&name, start))
                }
            }

            // @ — macro invocation, cpp block, or GC allocation
            TokenKind::At => {
                self.advance();

                // Check for @cpp { ... } — raw C++ interop
                if let TokenKind::Ident(ref s) = &self.peek_kind().clone() {
                    if s == "cpp" {
                        self.advance(); // consume 'cpp'
                        // Find the { in the source
                        let brace_start = self.peek().span.start;
                        // Skip whitespace in source to find {
                        let src_after_cpp = &self.source[brace_start..];
                        if let Some(open_brace) = src_after_cpp.find('{') {
                            let raw_start = brace_start + open_brace + 1;
                            // Find matching }
                            let raw_text = self.extract_matching_brace(raw_start)?;
                            let mut brace_depth: i32 = 0;
                            while self.pos < self.tokens.len() {
                                match self.peek_kind() {
                                    TokenKind::LBrace => { brace_depth += 1; self.advance(); }
                                    TokenKind::RBrace => {
                                        brace_depth -= 1;
                                        self.advance();
                                        if brace_depth == 0 { break; }
                                    }
                                    TokenKind::Eof => break,
                                    _ => { self.advance(); }
                                }
                            }
                            return Ok(Expr::new(ExprKind::CppBlock(raw_text), start));
                        }
                    }
                }

                // Check if it's a macro invocation
                if let TokenKind::Ident(macro_name) = &self.peek_kind().clone() {
                    let macro_name = macro_name.clone();
                    let lookahead = self.pos + 1;
                    let next_is_lbrace = self.tokens.get(lookahead)
                        .map(|t| matches!(t.kind, TokenKind::LBrace))
                        .unwrap_or(false);
                    
                    // If next token after ident is `{`, it's a GC allocation: @Type { fields }
                    if next_is_lbrace {
                        let ty = self.parse_type()?;
                        let fields = self.parse_struct_literal_fields()?;
                        return Ok(Expr::gc_new(ty, fields, start));
                    }

                    self.advance();

                    // Check for arguments
                    if matches!(self.peek_kind(), TokenKind::LParen) {
                        let args = self.parse_call_args()?;
                        return Ok(Expr::new(ExprKind::MacroCall { name: macro_name, args }, start));
                    } else {
                        // No parens — macro with no args, or single expression argument
                        let arg = self.parse_primary()?;
                        return Ok(Expr::new(ExprKind::MacroCall { name: macro_name, args: vec![arg] }, start));
                    }
                }

                // Otherwise it's a GC allocation: @Type { fields... } or @Type(...)
                let ty = self.parse_type()?;

                if matches!(self.peek_kind(), TokenKind::LBrace) {
                    // Struct-like initialization
                    let fields = self.parse_struct_literal_fields()?;
                    Ok(Expr::gc_new(ty, fields, start))
                } else if matches!(self.peek_kind(), TokenKind::LParen) {
                    // Enum constructor: @Type(case_value)
                    self.advance();
                    let inner = self.parse_expr()?;
                    let _ = self.consume(TokenKind::RParen);
                    // Extract type path
                    match ty {
                        Type::Path(path) => {
                            let segments: Vec<String> = path.segments.into_iter().map(|s| s.name).collect();
                            Ok(Expr::new(ExprKind::EnumCtor {
                                path: segments,
                                case: String::new(), // filled in by the inner expr
                                arg: Some(Box::new(inner)),
                            }, start))
                        }
                        _ => Err(CompileError::parse("Expected type path after @", start)),
                    }
                } else {
                    Err(CompileError::parse("Expected { or ( after @type", start))
                }
            }

            // $ — unquote
            TokenKind::Dollar => {
                self.advance();
                if let TokenKind::Ident(name) = &self.peek_kind().clone() {
                    let name = name.clone();
                    self.advance();
                    if matches!(self.peek_kind(), TokenKind::LParen) {
                        // $(expr) — splice expression result
                        let args = self.parse_call_args()?;
                        if args.len() == 1 {
                            Ok(Expr::new(ExprKind::Unquote(Box::new(args.into_iter().next().unwrap())), start))
                        } else {
                            Err(CompileError::parse("$(...) takes exactly one argument", start))
                        }
                    } else {
                        Ok(Expr::new(ExprKind::UnquoteIdent(name), start))
                    }
                } else {
                    Err(CompileError::parse("Expected identifier after $", start))
                }
            }

            // Blocks
            TokenKind::LBrace => Ok(Expr::block(self.parse_block()?.stmts, start)),

            // Parenthesized expression or tuple
            TokenKind::LParen => {
                self.advance();
                let expr = self.parse_expr()?;
                if matches!(self.peek_kind(), TokenKind::Comma) {
                    // Tuple
                    let mut exprs = vec![expr];
                    while matches!(self.peek_kind(), TokenKind::Comma) {
                        self.advance();
                        exprs.push(self.parse_expr()?);
                    }
                    let _ = self.consume(TokenKind::RParen);
                    // Tuples as expressions
                    Ok(Expr::new(ExprKind::Ident("<tuple>".into()), start)) // Placeholder
                } else {
                    let _ = self.consume(TokenKind::RParen);
                    Ok(expr)
                }
            }

            // Control flow
            TokenKind::If => self.parse_if_expr(),
            TokenKind::Match => self.parse_match_expr(),
            TokenKind::While => self.parse_while_expr(),
            TokenKind::For => self.parse_for_expr(),
            TokenKind::Return => {
                self.advance();
                let expr = if !matches!(self.peek_kind(), TokenKind::Semicolon) && !matches!(self.peek_kind(), TokenKind::RBrace) {
                    Some(self.parse_expr()?)
                } else {
                    None
                };
                Ok(Expr::return_expr(expr, start))
            }
            TokenKind::Let | TokenKind::Var => {
                let is_mut = matches!(self.peek_kind(), TokenKind::Var);
                self.advance();
                let name = self.parse_ident()?;
                let ty = if matches!(self.peek_kind(), TokenKind::Colon) {
                    self.advance();
                    Some(self.parse_type()?)
                } else {
                    None
                };
                let value = if matches!(self.peek_kind(), TokenKind::Eq) {
                    self.advance();
                    self.parse_expr()?
                } else {
                    // No initializer
                    return Ok(Expr::let_binding(&name, ty, Expr::new(ExprKind::NilLiteral, start), is_mut, start));
                };
                // Don't consume ; here — parse_block handles it
                Ok(Expr::let_binding(&name, ty, value, is_mut, start))
            }

            TokenKind::Func => {
                // Function definition in expression position (e.g. inside quote blocks)
                let func = self.parse_function(false)?;
                Ok(Expr::new(ExprKind::FuncDef(func), start))
            }

            TokenKind::Tilde => {
                // ~name: value — named argument
                self.advance();
                let name = self.parse_ident()?;
                let _ = self.consume(TokenKind::Colon);
                let value = self.parse_expr()?;
                Ok(Expr::new(ExprKind::NamedArg { name, value: Box::new(value) }, start))
            }

            // Unexpected
            _ => Err(CompileError::parse(
                format!("Unexpected token: {}", self.peek_kind()),
                self.peek().span,
            )),
        }
    }

    // ─── Block ────────────────────────────────────────────────────────────────

    fn parse_block(&mut self) -> Result<Block, CompileError> {
        let start = self.current_span();
        let _ = self.consume(TokenKind::LBrace);
        let mut stmts = Vec::new();

        while !matches!(self.peek_kind(), TokenKind::RBrace) && !matches!(self.peek_kind(), TokenKind::Eof) {
            stmts.push(self.parse_expr()?);
            // Optional semicolons
            if matches!(self.peek_kind(), TokenKind::Semicolon) {
                self.advance();
            }
        }

        let end = self.current_span();
        let _ = self.consume(TokenKind::RBrace);

        Ok(Block { stmts, span: start.merge(&end) })
    }

    // ─── If ───────────────────────────────────────────────────────────────────

    fn parse_if_expr(&mut self) -> Result<Expr, CompileError> {
        let start = self.current_span();
        self.advance(); // if
        let cond = self.parse_expr()?;
        let then_branch = self.parse_block()?;

        let else_branch = if matches!(self.peek_kind(), TokenKind::Else) {
            self.advance();
            if matches!(self.peek_kind(), TokenKind::If) {
                // else if
                Some(Block {
                    stmts: vec![self.parse_if_expr()?],
                    span: self.current_span(),
                })
            } else {
                Some(self.parse_block()?)
            }
        } else {
            None
        };

        Ok(Expr::r#if(cond, then_branch, else_branch, start))
    }

    // ─── Match ────────────────────────────────────────────────────────────────

    fn parse_match_expr(&mut self) -> Result<Expr, CompileError> {
        let start = self.current_span();
        self.advance(); // match
        let expr = self.parse_expr()?;
        let _ = self.consume(TokenKind::LBrace);

        let mut arms = Vec::new();
        while !matches!(self.peek_kind(), TokenKind::RBrace) && !matches!(self.peek_kind(), TokenKind::Eof) {
            let _ = self.consume(TokenKind::Case);
            let pattern = self.parse_pattern()?;

            let guard = if matches!(self.peek_kind(), TokenKind::If) {
                self.advance();
                Some(Box::new(self.parse_expr()?))
            } else {
                None
            };

            let _ = self.consume(TokenKind::FatArrow);
            let body = self.parse_expr()?;

            arms.push(MatchArm { pattern, guard, body: Box::new(body) });

            if matches!(self.peek_kind(), TokenKind::Comma) {
                self.advance();
            }
        }
        let _ = self.consume(TokenKind::RBrace);

        Ok(Expr::r#match(expr, arms, start))
    }

    // ─── While ────────────────────────────────────────────────────────────────

    fn parse_while_expr(&mut self) -> Result<Expr, CompileError> {
        let start = self.current_span();
        self.advance(); // while
        let cond = self.parse_expr()?;
        let body = self.parse_block()?;
        Ok(Expr::new(ExprKind::While {
            cond: Box::new(cond),
            body,
        }, start))
    }

    // ─── For ──────────────────────────────────────────────────────────────────

    fn parse_for_expr(&mut self) -> Result<Expr, CompileError> {
        let start = self.current_span();
        self.advance(); // for
        let var = self.parse_ident()?;
        let _ = self.consume(TokenKind::In);
        let iter = self.parse_expr()?;
        let body = self.parse_block()?;
        Ok(Expr::new(ExprKind::For {
            var,
            iter: Box::new(iter),
            body,
        }, start))
    }

    // ─── Quote ────────────────────────────────────────────────────────────────

    fn parse_quote_expr(&mut self) -> Result<Expr, CompileError> {
        let start = self.current_span();
        self.advance(); // quote
        let _ = self.consume(TokenKind::LBrace);
        let mut exprs = Vec::new();

        while !matches!(self.peek_kind(), TokenKind::RBrace) && !matches!(self.peek_kind(), TokenKind::Eof) {
            // Parse either an item or an expression
            if matches!(self.peek_kind(), TokenKind::Func)
                || matches!(self.peek_kind(), TokenKind::Struct)
                || matches!(self.peek_kind(), TokenKind::Enum)
                || matches!(self.peek_kind(), TokenKind::Let)
                || matches!(self.peek_kind(), TokenKind::Var)
            {
                // Parse the item and wrap it as an expression-like node
                // For now, keep parsing as expression since we need AST compatibility
                exprs.push(self.parse_expr()?);
            } else {
                exprs.push(self.parse_expr()?);
            }
            if matches!(self.peek_kind(), TokenKind::Semicolon) {
                self.advance();
            }
        }
        let _ = self.consume(TokenKind::RBrace);

        Ok(Expr::new(ExprKind::Quote(exprs), start))
    }

    // ─── Patterns ─────────────────────────────────────────────────────────────

    fn parse_pattern(&mut self) -> Result<Pattern, CompileError> {
        let start = self.current_span();

        match self.peek_kind().clone() {
            TokenKind::Underscore => {
                self.advance();
                Ok(Pattern { kind: PatternKind::Wildcard, span: start })
            }
            TokenKind::IntLiteral(n) => {
                self.advance();
                Ok(Pattern { kind: PatternKind::Literal(LiteralPat::Int(n)), span: start })
            }
            TokenKind::FloatLiteral(n) => {
                self.advance();
                Ok(Pattern { kind: PatternKind::Literal(LiteralPat::Float(n)), span: start })
            }
            TokenKind::StringLiteral(s) => {
                self.advance();
                Ok(Pattern { kind: PatternKind::Literal(LiteralPat::String(s)), span: start })
            }
            TokenKind::CharLiteral(c) => {
                self.advance();
                Ok(Pattern { kind: PatternKind::Literal(LiteralPat::Char(c)), span: start })
            }
            TokenKind::True => {
                self.advance();
                Ok(Pattern { kind: PatternKind::Literal(LiteralPat::Bool(true)), span: start })
            }
            TokenKind::False => {
                self.advance();
                Ok(Pattern { kind: PatternKind::Literal(LiteralPat::Bool(false)), span: start })
            }
            TokenKind::Nil => {
                self.advance();
                Ok(Pattern { kind: PatternKind::Literal(LiteralPat::Nil), span: start })
            }
            TokenKind::Dot => {
                // .case_name or .case_name(pattern)
                self.advance();
                let case = self.parse_ident()?;
                let inner = if matches!(self.peek_kind(), TokenKind::LParen) {
                    self.advance();
                    let pat = self.parse_pattern()?;
                    let _ = self.consume(TokenKind::RParen);
                    Some(Box::new(pat))
                } else {
                    None
                };
                Ok(Pattern {
                    kind: PatternKind::EnumCtor { path: vec![], case, inner },
                    span: start,
                })
            }
            TokenKind::Ident(name) => {
                let name = name.clone();
                self.advance();

                // Could be a variable binding or a struct/enum pattern
                if matches!(self.peek_kind(), TokenKind::LBrace) {
                    // Struct pattern: Name { field1: pat1, field2: pat2 }
                    self.advance();
                    let mut fields = Vec::new();
                    while !matches!(self.peek_kind(), TokenKind::RBrace) {
                        let fname = self.parse_ident()?;
                        let _ = self.consume(TokenKind::Colon);
                        let pat = self.parse_pattern()?;
                        fields.push((fname, pat));
                        if matches!(self.peek_kind(), TokenKind::Comma) {
                            self.advance();
                        }
                    }
                    let _ = self.consume(TokenKind::RBrace);
                    Ok(Pattern {
                        kind: PatternKind::Struct { path: vec![name], fields },
                        span: start,
                    })
                } else if matches!(self.peek_kind(), TokenKind::Colon) && self.peek_next_is_colon() {
                    // Path pattern
                    let mut path = vec![name];
                    while matches!(self.peek_kind(), TokenKind::Colon) && self.peek_next_is_colon() {
                        self.advance();
                        self.advance();
                        path.push(self.parse_ident()?);
                    }
                    Ok(Pattern {
                        kind: PatternKind::EnumCtor { path, case: String::new(), inner: None },
                        span: start,
                    })
                } else {
                    // Variable binding
                    Ok(Pattern {
                        kind: PatternKind::Variable { name, is_mut: false },
                        span: start,
                    })
                }
            }
            _ => Err(CompileError::parse(
                format!("Expected pattern, got '{}'", self.peek_kind()),
                self.peek().span,
            )),
        }
    }

    // ─── Helpers ──────────────────────────────────────────────────────────────

    fn parse_ident(&mut self) -> Result<String, CompileError> {
        match self.peek_kind().clone() {
            TokenKind::Ident(s) => { self.advance(); Ok(s) }
            // All keywords can be used as identifiers in appropriate contexts
            TokenKind::Module => { self.advance(); Ok("module".into()) }
            TokenKind::Macro => { self.advance(); Ok("macro".into()) }
            TokenKind::Quote => { self.advance(); Ok("quote".into()) }
            TokenKind::Import => { self.advance(); Ok("import".into()) }
            TokenKind::Type => { self.advance(); Ok("type".into()) }
            TokenKind::Func => { self.advance(); Ok("func".into()) }
            TokenKind::Let => { self.advance(); Ok("let".into()) }
            TokenKind::Var => { self.advance(); Ok("var".into()) }
            TokenKind::If => { self.advance(); Ok("if".into()) }
            TokenKind::Else => { self.advance(); Ok("else".into()) }
            TokenKind::Match => { self.advance(); Ok("match".into()) }
            TokenKind::Case => { self.advance(); Ok("case".into()) }
            TokenKind::Enum => { self.advance(); Ok("enum".into()) }
            TokenKind::Struct => { self.advance(); Ok("struct".into()) }
            TokenKind::While => { self.advance(); Ok("while".into()) }
            TokenKind::For => { self.advance(); Ok("for".into()) }
            TokenKind::Return => { self.advance(); Ok("return".into()) }
            TokenKind::Dollar => {
                // $ident — unquote identifier (for use in quote blocks)
                // Return it as a special marker that the macro expander will handle
                self.advance();
                if let TokenKind::Ident(s) = &self.peek_kind().clone() {
                    let s = s.clone();
                    self.advance();
                    Ok(format!("${}", s))
                } else {
                    Err(CompileError::parse(
                        "Expected identifier after $",
                        self.peek().span,
                    ))
                }
            }
            _ => Err(CompileError::parse(
                format!("Expected identifier, got '{}'", self.peek_kind()),
                self.peek().span,
            )),
        }
    }

    fn parse_generic_params(&mut self) -> Result<Vec<GenericParam>, CompileError> {
        if matches!(self.peek_kind(), TokenKind::LBracket) {
            self.advance();
            let mut params = Vec::new();
            loop {
                let name = self.parse_ident()?;
                params.push(GenericParam { name, span: self.current_span() });
                if matches!(self.peek_kind(), TokenKind::RBracket) {
                    self.advance();
                    break;
                }
                let _ = self.consume(TokenKind::Comma);
            }
            Ok(params)
        } else {
            Ok(vec![])
        }
    }

    fn parse_generic_args(&mut self) -> Result<Vec<Type>, CompileError> {
        if matches!(self.peek_kind(), TokenKind::LBracket) {
            self.advance();
            let mut args = Vec::new();
            loop {
                args.push(self.parse_type()?);
                if matches!(self.peek_kind(), TokenKind::RBracket) {
                    self.advance();
                    break;
                }
                let _ = self.consume(TokenKind::Comma);
            }
            Ok(args)
        } else {
            Ok(vec![])
        }
    }

    fn peek_is(&self, kind: &TokenKind) -> bool {
        std::mem::discriminant(self.peek_kind()) == std::mem::discriminant(kind)
    }

    fn parse_delimited<T>(
        &mut self,
        end: TokenKind,
        sep: TokenKind,
        parse_item: fn(&mut Self) -> Result<T, CompileError>,
    ) -> Result<Vec<T>, CompileError> {
        let mut items = Vec::new();
        if !self.peek_is(&end) {
            items.push(parse_item(self)?);
            while self.peek_is(&sep) {
                self.advance();
                items.push(parse_item(self)?);
            }
        }
        let _ = self.consume(end);
        Ok(items)
    }

    fn is_at_stmt_end(&self) -> bool {
        matches!(self.peek_kind(), TokenKind::Semicolon | TokenKind::RBrace | TokenKind::Eof)
    }

    fn is_ident_or_keyword(&self) -> bool {
        matches!(self.peek_kind(),
            TokenKind::Ident(_) | TokenKind::Module | TokenKind::Macro
            | TokenKind::Quote | TokenKind::Type | TokenKind::Import
        )
    }

    fn consume_ident_or_keyword(&mut self) -> String {
        match self.advance().kind.clone() {
            TokenKind::Ident(s) => s,
            TokenKind::Module => "module".into(),
            TokenKind::Macro => "macro".into(),
            TokenKind::Quote => "quote".into(),
            TokenKind::Type => "type".into(),
            TokenKind::Import => "import".into(),
            TokenKind::Func => "func".into(),
            TokenKind::Let => "let".into(),
            TokenKind::Var => "var".into(),
            TokenKind::Return => "return".into(),
            TokenKind::If => "if".into(),
            TokenKind::Else => "else".into(),
            TokenKind::Match => "match".into(),
            TokenKind::Case => "case".into(),
            TokenKind::Enum => "enum".into(),
            TokenKind::While => "while".into(),
            TokenKind::For => "for".into(),
            _ => unreachable!(),
        }
    }

    fn parse_struct_literal_fields(&mut self) -> Result<Vec<(String, Expr)>, CompileError> {
        let _ = self.consume(TokenKind::LBrace);
        let mut fields = Vec::new();
        while !matches!(self.peek_kind(), TokenKind::RBrace) && !matches!(self.peek_kind(), TokenKind::Eof) {
            let name = self.parse_ident()?;
            let _ = self.consume(TokenKind::Colon);
            let value = self.parse_expr()?;
            fields.push((name, value));
            if matches!(self.peek_kind(), TokenKind::Comma) {
                self.advance();
            }
        }
        let _ = self.consume(TokenKind::RBrace);
        Ok(fields)
    }

    /// Extract text between matching braces starting at source position `start`.
    /// Handles nested braces and string/char literals.
    fn extract_matching_brace(&self, start: usize) -> Result<String, CompileError> {
        let mut depth: i32 = 1;
        let bytes = self.source.as_bytes();
        let mut pos = start;
        let mut result = String::new();

        while pos < bytes.len() && depth > 0 {
            let ch = bytes[pos] as char;
            match ch {
                '{' => {
                    depth += 1;
                    result.push('{');
                }
                '}' => {
                    depth -= 1;
                    if depth > 0 {
                        result.push('}');
                    }
                }
                _ => {
                    result.push(ch);
                }
            }
            pos += 1;
        }

        if depth > 0 {
            return Err(CompileError::parse(
                "Unterminated cpp block — missing '}'",
                Span::zero(),
            ));
        }

        Ok(result)
    }
}

// Static EOF token for bounds safety
static EOF_SPAN: Span = Span { start: 0, end: 0, line: 0, col: 0 };
static EOF_TOKEN: Token = Token { kind: TokenKind::Eof, span: EOF_SPAN };
