//! Nova LSP server — provides IDE features via the Language Server Protocol.
//!
//! Features:
//! - Diagnostics (errors/warnings from the compiler frontend)
//! - Hover (type info, documentation)
//! - Go-to-definition
//! - Completion (keywords, identifiers in scope)
//! - Semantic tokens (syntax highlighting)
//! - Formatting

mod analysis;
mod semantic;

use crate::ast::*;
use crate::error::CompileError;
use crate::lexer::Lexer;
use crate::parser::Parser;
use crate::mono::Mono;
use crate::typeck::TypeChecker;

use analysis::Document;
use dashmap::DashMap;
use tower_lsp::jsonrpc::Result as LspResult;
use tower_lsp::lsp_types::*;
use tower_lsp::{Client, LanguageServer, LspService, Server};

// ─── Semantic token legend ────────────────────────────────────────────────────
// Must match NovaSemanticToken::kind_to_index()

const SEMANTIC_TOKEN_TYPES: &[&str] = &[
    "keyword",       // 0
    "string",        // 1
    "number",        // 2
    "comment",       // 3
    "operator",      // 4
    "type",          // 5
    "function",      // 6
    "variable",      // 7
    "parameter",     // 8
    "enumMember",    // 9
    "macro",         // 10
    "struct",        // 11
    "typeParameter", // 12
    "property",      // 13
];

const SEMANTIC_TOKEN_MODIFIERS: &[&str] = &[
    "declaration",    // 0
    "definition",     // 1
    "readonly",       // 2
    "defaultLibrary", // 3
    "modification",   // 4
];

// ─── Backend ──────────────────────────────────────────────────────────────────

pub struct Backend {
    client: Client,
    /// Open documents: URI → Document
    documents: DashMap<String, Document>,
}

impl Backend {
    pub fn new(client: Client) -> Self {
        Self {
            client,
            documents: DashMap::new(),
        }
    }

    /// Parse a source file, returning the module + tokens + source.
    /// Errors are returned as a string.
    fn parse_source(
        source: &str,
        uri: &str,
    ) -> Result<(Module, Vec<crate::token::Token>), String> {
        let mut lex = Lexer::new(source);
        let tokens = lex.tokenize().map_err(|e| format!("{}", e))?;
        let mut parser = Parser::new(tokens.clone(), source);
        let module_name = Self::file_stem(uri);
        let mut module = parser
            .parse_module(module_name)
            .map_err(|e| format!("{}", e))?;
        // Best-effort macro expansion so hover/completion see generated items
        let _ = Self::expand_macros(&mut module, uri);
        Ok((module, tokens))
    }

    fn file_stem(uri: &str) -> String {
        let path = uri.strip_prefix("file://").unwrap_or(uri);
        std::path::Path::new(path)
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("main")
            .to_string()
    }

    /// Module search paths for @import, mirroring the compile pipeline:
    /// the file's parent directory, then the current directory.
    fn search_paths(uri: &str) -> Vec<std::path::PathBuf> {
        let path = uri.strip_prefix("file://").unwrap_or(uri);
        let mut paths = Vec::new();
        if let Some(parent) = std::path::Path::new(path).parent() {
            paths.push(parent.to_path_buf());
        }
        paths.push(std::path::PathBuf::from("."));
        paths
    }

    /// Expand macros (including @import) in place, mirroring the compile pipeline.
    fn expand_macros(module: &mut Module, uri: &str) -> Result<(), CompileError> {
        let search_paths = Self::search_paths(uri);
        let import_macro = crate::import_macro::ImportMacro::new(search_paths.clone());
        let interpreter = crate::interpreter::Interpreter::new(search_paths);
        let mut expander = crate::macro_expand::MacroExpander::new(import_macro, interpreter);
        expander.register_structs(module);
        expander.expand_module(module, None)
    }

    /// Compute diagnostics for a document
    fn compute_diagnostics(
        uri: &str,
        source: &str,
    ) -> Vec<Diagnostic> {
        let mut diags = Vec::new();

        // Lex
        let mut lex = Lexer::new(source);
        let tokens = match lex.tokenize() {
            Ok(t) => t,
            Err(e) => {
                diags.push(Self::compile_error_to_diagnostic(&e, source));
                return diags;
            }
        };

        // Parse
        let mut parser = Parser::new(tokens, source);
        let module_name = Self::file_stem(uri);
        let mut module = match parser.parse_module(module_name) {
            Ok(m) => m,
            Err(e) => {
                diags.push(Self::compile_error_to_diagnostic(&e, source));
                return diags;
            }
        };

        // Expand macros (including @import)
        if let Err(e) = Self::expand_macros(&mut module, uri) {
            diags.push(Self::compile_error_to_diagnostic(&e, source));
            return diags;
        }

        // Monomorphize generics — the compiler type-checks the lowered module,
        // so the LSP must do the same or generic code (e.g. `swap(Pair...)`)
        // shows spurious type errors.
        if let Err(e) = Mono::lower(&module).map(|m| module = m) {
            diags.push(Self::compile_error_to_diagnostic(&e, source));
            return diags;
        }

        // Type check
        let mut checker = TypeChecker::new();
        checker.register_types(&module);
        match checker.check_module(&module) {
            Err(e) => {
                diags.push(Self::compile_error_to_diagnostic(&e, source));
            }
            Ok(_) => {}
        }

        diags
    }

    fn compile_error_to_diagnostic(e: &CompileError, _source: &str) -> Diagnostic {
        let (message, span) = match e {
            CompileError::LexError { message, span }
            | CompileError::ParseError { message, span }
            | CompileError::TypeError { message, span }
            | CompileError::MacroError { message, span } => (message.clone(), *span),
            CompileError::IoError(io) => (format!("I/O error: {}", io), crate::token::Span::zero()),
            CompileError::Generic(msg) => (msg.clone(), crate::token::Span::zero()),
        };

        let (line, col, end_line, end_col) = if span.line > 0 {
            // span.line and span.col are 1-based
            let line = span.line.max(1) as u32 - 1;
            let col = span.col.max(1) as u32 - 1;
            (line, col, line, col + (span.end - span.start).max(1) as u32)
        } else {
            (0u32, 0u32, 0u32, 1u32)
        };

        Diagnostic {
            range: Range {
                start: Position { line, character: col },
                end: Position { line: end_line, character: end_col },
            },
            severity: Some(DiagnosticSeverity::ERROR),
            source: Some("nova".into()),
            message,
            ..Default::default()
        }
    }

    /// Find the token at a position in the source. Lexer spans record the
    /// line/col *after* the token, so the token occupies columns
    /// `[span.col - len, span.col - 1]` (1-based).
    fn token_at_position(tokens: &[crate::token::Token], pos: Position) -> Option<usize> {
        let target_line = pos.line as usize + 1;
        let target_col = pos.character as usize + 1;
        tokens.iter().position(|t| {
            let len = t.span.end - t.span.start;
            len > 0
                && t.span.line == target_line
                && t.span.col > len
                && t.span.col - len <= target_col
                && target_col <= t.span.col - 1
        })
    }

    /// Build completions at a given position
    fn build_completions(
        module: &Module,
        _tokens: &[crate::token::Token],
        _pos: Position,
    ) -> Vec<CompletionItem> {
        let mut items = Vec::new();

        // Keywords
        for kw in &[
            "func", "let", "var", "return", "if", "else", "match", "case",
            "enum", "struct", "macro", "quote", "while", "for", "in",
            "as", "is", "true", "false", "nil", "type", "trait", "impl",
            "import", "module", "pub",
        ] {
            items.push(CompletionItem {
                label: kw.to_string(),
                kind: Some(CompletionItemKind::KEYWORD),
                insert_text: Some(format!("{} ", kw)),
                ..Default::default()
            });
        }

        // Top-level items
        for item in &module.items {
            match item {
                Item::Function(f) => {
                    items.push(CompletionItem {
                        label: f.name.clone(),
                        kind: Some(CompletionItemKind::FUNCTION),
                        detail: Some(format!(
                            "({}) -> {}",
                            f.params
                                .iter()
                                .map(|p| p.ty.to_string())
                                .collect::<Vec<_>>()
                                .join(", "),
                            f.return_type
                        )),
                        ..Default::default()
                    });
                }
                Item::Struct(s) => {
                    items.push(CompletionItem {
                        label: s.name.clone(),
                        kind: Some(CompletionItemKind::STRUCT),
                        detail: Some(format!("struct ({} fields)", s.fields.len())),
                        ..Default::default()
                    });
                }
                Item::Enum(e) => {
                    items.push(CompletionItem {
                        label: e.name.clone(),
                        kind: Some(CompletionItemKind::ENUM),
                        detail: Some(format!("enum ({} cases)", e.cases.len())),
                        ..Default::default()
                    });
                    for case in &e.cases {
                        items.push(CompletionItem {
                            label: format!(".{}", case.name),
                            kind: Some(CompletionItemKind::ENUM_MEMBER),
                            detail: Some(format!("case of {}", e.name)),
                            ..Default::default()
                        });
                    }
                }
                Item::TypeAlias(t) => {
                    items.push(CompletionItem {
                        label: t.name.clone(),
                        kind: Some(CompletionItemKind::TYPE_PARAMETER),
                        detail: Some(format!("type = {}", t.ty)),
                        ..Default::default()
                    });
                }
                _ => {}
            }
        }

        // Built-in types
        for bt in &["Int", "Float", "Bool", "String", "Char", "Void"] {
            items.push(CompletionItem {
                label: bt.to_string(),
                kind: Some(CompletionItemKind::TYPE_PARAMETER),
                detail: Some("built-in type".into()),
                ..Default::default()
            });
        }

        items
    }

    /// Lex + parse, returning None on any error (no diagnostics wanted).
    fn try_parse(source: &str, uri: &str) -> Option<Module> {
        let mut lex = Lexer::new(source);
        let tokens = lex.tokenize().ok()?;
        let mut parser = Parser::new(tokens, source);
        parser.parse_module(Self::file_stem(uri)).ok()
    }

    /// Convert an LSP position to a byte offset in the source
    fn offset_of_position(source: &str, pos: Position) -> usize {
        let mut offset = 0usize;
        for (i, line) in source.split('\n').enumerate() {
            if i as u32 == pos.line {
                offset += (pos.character as usize).min(line.len());
                while offset > 0 && !source.is_char_boundary(offset) {
                    offset -= 1;
                }
                return offset;
            }
            offset += line.len() + 1;
        }
        source.len()
    }

    /// If the cursor sits after `ident.` (optionally mid-word: `ident.par`),
    /// return the receiver identifier.
    fn dot_receiver(source: &str, offset: usize) -> Option<String> {
        let bytes = source.as_bytes();
        let is_ident = |b: u8| b.is_ascii_alphanumeric() || b == b'_';
        let mut i = offset.min(bytes.len());
        // Skip back over the partially typed member name
        while i > 0 && is_ident(bytes[i - 1]) {
            i -= 1;
        }
        if i == 0 || bytes[i - 1] != b'.' {
            return None;
        }
        let dot = i - 1;
        let mut j = dot;
        while j > 0 && is_ident(bytes[j - 1]) {
            j -= 1;
        }
        if j == dot {
            return None; // no identifier before the dot (e.g. enum case `.Foo`)
        }
        let recv = &source[j..dot];
        if recv.as_bytes()[0].is_ascii_digit() {
            return None; // float literal like `1.5`
        }
        Some(recv.to_string())
    }

    /// Base type name for member lookup: last path segment, through @T refs.
    fn type_base_name(ty: &Type) -> Option<String> {
        match ty {
            Type::Path(p) => p.segments.last().map(|s| s.name.clone()),
            Type::GcRef(inner) => Self::type_base_name(inner),
            _ => None,
        }
    }

    // ─── Hover ────────────────────────────────────────────────────────────────

    fn hover_md(markdown: &str, range: Option<Range>) -> Hover {
        Hover {
            contents: HoverContents::Markup(MarkupContent {
                kind: MarkupKind::Markdown,
                value: markdown.to_string(),
            }),
            range,
        }
    }

    fn token_range(token: &crate::token::Token) -> Option<Range> {
        if token.span.line == 0 {
            return None;
        }
        let len = token.span.end - token.span.start;
        if len == 0 || token.span.col <= len {
            return None;
        }
        let line = (token.span.line.max(1) - 1) as u32;
        let start_col = (token.span.col - len - 1) as u32;
        let end_col = (token.span.col - 1) as u32;
        Some(Range {
            start: Position { line, character: start_col },
            end: Position {
                line,
                character: end_col,
            },
        })
    }

    /// Function/method signature as written in the source.
    fn fn_signature(f: &Function) -> String {
        let generics = if f.generics.is_empty() {
            String::new()
        } else {
            format!(
                "[{}]",
                f.generics
                    .iter()
                    .map(|g| g.name.clone())
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        };
        format!(
            "func {}{}({}) -> {}",
            f.name,
            generics,
            f.params
                .iter()
                .map(|p| format!("{}: {}", p.name, p.ty))
                .collect::<Vec<_>>()
                .join(", "),
            f.return_type
        )
    }

    /// All function-like items (free functions, impl and trait methods).
    fn all_functions(module: &Module) -> Vec<&Function> {
        let mut fns = Vec::new();
        for item in &module.items {
            match item {
                Item::Function(f) => fns.push(f),
                Item::Impl(i) => fns.extend(i.methods.iter()),
                Item::Trait(t) => fns.extend(t.methods.iter()),
                _ => {}
            }
        }
        fns
    }

    /// Hover content for a position — no document store needed.
    fn hover_at(source: &str, uri: &str, pos: Position) -> Option<Hover> {
        let (module, tokens) = Self::parse_source(source, uri).ok()?;
        let idx = Self::token_at_position(&tokens, pos)?;
        let token = &tokens[idx];
        let crate::token::TokenKind::Ident(name) = &token.kind else {
            return None;
        };
        let range = Self::token_range(token);
        let offset = token.span.start;

        // Enum case member: `.some` — the previous token is a Dot.
        if idx > 0 && matches!(tokens[idx - 1].kind, crate::token::TokenKind::Dot) {
            let mut cases = Vec::new();
            for item in &module.items {
                if let Item::Enum(e) = item {
                    if let Some(c) = e.cases.iter().find(|c| &c.name == name) {
                        cases.push(match &c.payload {
                            Some(p) => format!("case {}({}) of enum {}", c.name, p, e.name),
                            None => format!("case {} of enum {}", c.name, e.name),
                        });
                    }
                }
            }
            if !cases.is_empty() {
                return Some(Self::hover_md(&format!("```nova\n.{}```\n\n{}", name, cases.join("\n\n")), range));
            }
            // Fall through: a field named like the token is not an enum case.
        }

        // Declarations: whichever comes first in the module wins.
        for item in &module.items {
            match item {
                Item::Function(f) if f.name == *name => {
                    let note = if f.is_pub { "\n\n*(public)*" } else { "" };
                    return Some(Self::hover_md(
                        &format!("```nova\n{}```{}", Self::fn_signature(f), note),
                        range,
                    ));
                }
                Item::Struct(s) if s.name == *name => {
                    let body = s
                        .fields
                        .iter()
                        .map(|f| format!("  {}: {}", f.name, f.ty))
                        .collect::<Vec<_>>()
                        .join(",\n");
                    let generics = if s.generics.is_empty() {
                        String::new()
                    } else {
                        format!(
                            "[{}]",
                            s.generics
                                .iter()
                                .map(|g| g.name.clone())
                                .collect::<Vec<_>>()
                                .join(", ")
                        )
                    };
                    return Some(Self::hover_md(
                        &format!("```nova\nstruct {}{} {{\n{}\n}}\n```", s.name, generics, body),
                        range,
                    ));
                }
                Item::Enum(e) if e.name == *name => {
                    let cases = e
                        .cases
                        .iter()
                        .map(|c| match &c.payload {
                            Some(p) => format!("  case {}({})", c.name, p),
                            None => format!("  case {}", c.name),
                        })
                        .collect::<Vec<_>>()
                        .join(",\n");
                    let generics = if e.generics.is_empty() {
                        String::new()
                    } else {
                        format!(
                            "[{}]",
                            e.generics
                                .iter()
                                .map(|g| g.name.clone())
                                .collect::<Vec<_>>()
                                .join(", ")
                        )
                    };
                    return Some(Self::hover_md(
                        &format!("```nova\nenum {}{} {{\n{}\n}}\n```", e.name, generics, cases),
                        range,
                    ));
                }
                Item::Trait(t) if t.name == *name => {
                    let methods = t
                        .methods
                        .iter()
                        .map(Self::fn_signature)
                        .collect::<Vec<_>>()
                        .join("\n");
                    return Some(Self::hover_md(
                        &format!("```nova\ntrait {} {{\n{}\n}}\n```", t.name, methods),
                        range,
                    ));
                }
                Item::TypeAlias(a) if a.name == *name => {
                    let generics = if a.generics.is_empty() {
                        String::new()
                    } else {
                        format!(
                            "[{}]",
                            a.generics
                                .iter()
                                .map(|g| g.name.clone())
                                .collect::<Vec<_>>()
                                .join(", ")
                        )
                    };
                    return Some(Self::hover_md(
                        &format!("```nova\ntype {}{} = {}\n```", a.name, generics, a.ty),
                        range,
                    ));
                }
                Item::VarDecl(v) if v.name == *name => {
                    let kw = if v.is_mut { "var" } else { "let" };
                    let value = match &v.ty {
                        Some(t) => format!("{} {}: {}", kw, v.name, t),
                        None => format!("{} {}", kw, v.name),
                    };
                    return Some(Self::hover_md(&format!("```nova\n{}\n```", value), range));
                }
                Item::Macro(m) if m.name == *name => {
                    return Some(Self::hover_md(
                        &format!("```nova\nmacro {}({})\n```", m.name, m.params.join(", ")),
                        range,
                    ));
                }
                _ => {}
            }
        }
        // Impl/trait methods are not top-level items; scan them too.
        for f in Self::all_functions(&module) {
            if f.name == *name {
                let note = if f.is_pub { "\n\n*(public)*" } else { "" };
                return Some(Self::hover_md(
                    &format!("```nova\n{}```{}", Self::fn_signature(f), note),
                    range,
                ));
            }
        }

        // Built-in types and runtime functions.
        let builtin = match name.as_str() {
            "Int" => Some("Built-in: integer (`int` in C++)"),
            "Float" => Some("Built-in: double-precision float (`double` in C++)"),
            "Bool" => Some("Built-in: boolean (`bool` in C++)"),
            "String" => Some("Built-in: string (`std::string` in C++)"),
            "Char" => Some("Built-in: character (`char` in C++)"),
            "print" => Some("Runtime builtin: writes its argument to stdout (no newline)."),
            "println" => Some("Runtime builtin: writes its argument to stdout followed by a newline."),
            "print_int" => Some("Runtime builtin: writes an integer to stdout followed by a newline."),
            _ => None,
        };
        if let Some(info) = builtin {
            return Some(Self::hover_md(&format!("**{}**\n\n{}", name, info), range));
        }

        // Local variables and parameters: inferred over the original AST so
        // generics keep their source shape (e.g. `Pair[Bool, Int]`).
        let enclosing: Vec<&Function> = Self::all_functions(&module)
            .into_iter()
            .filter(|f| f.body.span.start < offset && offset <= f.body.span.end)
            .collect();
        for f in enclosing {
            if let Some(ty) = crate::mono::Mono::infer_var_type_at(&module, f, name, offset) {
                let kind = if f.params.iter().any(|p| p.name == *name) {
                    "parameter"
                } else {
                    "local variable"
                };
                let md = format!("```nova\n{}: {}\n```\n\n*{}*", name, ty, kind);
                return Some(Self::hover_md(&md, range));
            }
        }
        // Parameter name in the signature: show the declared type as written.
        for f in Self::all_functions(&module) {
            if f.span.start <= offset
                && offset < f.body.span.start
                && f.params.iter().any(|p| p.name == *name)
            {
                let p = f.params.iter().find(|p| p.name == *name).unwrap();
                let md = format!(
                    "```nova\n{}: {}\n```\n\n*parameter of `{}`*",
                    p.name, p.ty, f.name
                );
                return Some(Self::hover_md(&md, range));
            }
        }

        None
    }

    /// Completions after `recv.`: struct fields plus every function callable
    /// via UFCS — first parameter matching the receiver's type directly, or a
    /// trait the receiver implements (trait methods, impl methods, free funcs).
    fn dot_completions(source: &str, uri: &str, offset: usize) -> Option<Vec<CompletionItem>> {
        let receiver = Self::dot_receiver(source, offset)?;

        // A bare `recv.` usually doesn't parse; retry with a placeholder
        // identifier inserted at the cursor.
        let mut module = Self::try_parse(source, uri).or_else(|| {
            let patched = format!("{}__nova_cursor{}", &source[..offset], &source[offset..]);
            Self::try_parse(&patched, uri)
        })?;
        let _ = Self::expand_macros(&mut module, uri);
        // Same lowering as the compiler: receiver inference needs concrete
        // (monomorphic) types, otherwise generics resolve to their params.
        if Mono::lower(&module).map(|m| module = m).is_err() {
            return None;
        }

        let mut checker = TypeChecker::new();
        checker.register_types(&module);

        // Resolve the receiver's type: param/local in the enclosing function
        // body, else a module-level variable.
        let mut enclosing: Vec<&Function> = Vec::new();
        for item in &module.items {
            match item {
                Item::Function(f) => enclosing.push(f),
                Item::Impl(i) => enclosing.extend(i.methods.iter()),
                Item::Trait(t) => enclosing.extend(t.methods.iter()),
                _ => {}
            }
        }
        let recv_ty = enclosing
            .iter()
            .filter(|f| f.body.span.start < offset && offset <= f.body.span.end)
            .find_map(|f| checker.infer_var_type_at(f, &receiver, offset))
            .or_else(|| {
                module.items.iter().find_map(|item| match item {
                    Item::VarDecl(v) if v.name == receiver => v.ty.clone(),
                    _ => None,
                })
            })?;

        let items = Self::build_dot_completions(&module, &recv_ty);
        if items.is_empty() { None } else { Some(items) }
    }

    fn build_dot_completions(module: &Module, recv_ty: &Type) -> Vec<CompletionItem> {
        let mut items = Vec::new();
        let Some(ty_name) = Self::type_base_name(recv_ty) else {
            return items;
        };

        // Type names a UFCS first parameter may have: the receiver's own type
        // plus every trait it implements.
        let mut recv_names = vec![ty_name.clone()];
        for item in &module.items {
            if let Item::Impl(i) = item {
                if Self::type_base_name(&i.target_type).as_deref() == Some(ty_name.as_str())
                    && !recv_names.contains(&i.trait_name)
                {
                    recv_names.push(i.trait_name.clone());
                }
            }
        }

        // Struct fields
        for item in &module.items {
            if let Item::Struct(s) = item {
                if s.name == ty_name {
                    for f in &s.fields {
                        items.push(CompletionItem {
                            label: f.name.clone(),
                            kind: Some(CompletionItemKind::FIELD),
                            detail: Some(f.ty.to_string()),
                            sort_text: Some(format!("0_{}", f.name)),
                            ..Default::default()
                        });
                    }
                }
            }
        }

        // Methods: free functions, impl methods, and trait methods (with the
        // trait's self-alias standing for the trait type itself).
        let mut fns: Vec<(&Function, Option<&TraitDef>)> = Vec::new();
        for item in &module.items {
            match item {
                Item::Function(f) => fns.push((f, None)),
                Item::Impl(i) => fns.extend(i.methods.iter().map(|m| (m, None))),
                Item::Trait(t) => fns.extend(t.methods.iter().map(|m| (m, Some(t)))),
                _ => {}
            }
        }

        let mut seen = std::collections::HashSet::new();
        for (f, trait_ctx) in fns {
            let Some(first) = f.params.first() else { continue };
            let mut first_ty = Self::type_base_name(&first.ty);
            if let Some(t) = trait_ctx {
                let alias = t.self_alias.as_deref().unwrap_or("Self");
                if first_ty.as_deref() == Some(alias) {
                    first_ty = Some(t.name.clone());
                }
            }
            let Some(first_ty) = first_ty else { continue };
            if !recv_names.contains(&first_ty) {
                continue;
            }
            // Dedupe e.g. a trait method against its impls, but keep overloads
            // that differ in the remaining parameters.
            let tail: Vec<String> = f.params.iter().skip(1).map(|p| p.ty.to_string()).collect();
            if !seen.insert(format!("{}({}) -> {}", f.name, tail.join(","), f.return_type)) {
                continue;
            }
            items.push(CompletionItem {
                label: f.name.clone(),
                kind: Some(CompletionItemKind::METHOD),
                detail: Some(format!(
                    "func {}({}) -> {}",
                    f.name,
                    f.params
                        .iter()
                        .map(|p| format!("{}: {}", p.name, p.ty))
                        .collect::<Vec<_>>()
                        .join(", "),
                    f.return_type
                )),
                sort_text: Some(format!("1_{}", f.name)),
                ..Default::default()
            });
        }

        items
    }

    /// Convert a byte offset in the source to an LSP position
    fn offset_to_position(source: &str, offset: usize) -> Position {
        let mut line = 0u32;
        let mut col = 0u32;
        for (i, ch) in source.char_indices() {
            if i >= offset {
                break;
            }
            if ch == '\n' {
                line += 1;
                col = 0;
            } else {
                col += 1;
            }
        }
        Position { line, character: col }
    }

    fn span_to_range(source: &str, span: &crate::token::Span) -> Range {
        Range {
            start: Self::offset_to_position(source, span.start),
            end: Self::offset_to_position(source, span.end),
        }
    }

    /// Range of `name` within the item's span (for precise outline selection),
    /// falling back to the whole span if not found.
    fn name_selection_range(source: &str, span: &crate::token::Span, name: &str) -> Range {
        let start = span.start.min(source.len());
        let end = span.end.min(source.len()).max(start);
        if let Some(rel) = source[start..end].find(name) {
            let name_start = start + rel;
            Range {
                start: Self::offset_to_position(source, name_start),
                end: Self::offset_to_position(source, name_start + name.len()),
            }
        } else {
            Self::span_to_range(source, span)
        }
    }

    #[allow(deprecated)] // DocumentSymbol::deprecated must be initialized
    fn make_symbol(
        source: &str,
        span: &crate::token::Span,
        name: String,
        detail: Option<String>,
        kind: SymbolKind,
        children: Vec<DocumentSymbol>,
    ) -> DocumentSymbol {
        DocumentSymbol {
            name: name.clone(),
            detail,
            kind,
            tags: None,
            deprecated: None,
            range: Self::span_to_range(source, span),
            selection_range: Self::name_selection_range(source, span, &name),
            children: if children.is_empty() { None } else { Some(children) },
        }
    }

    fn function_symbol(f: &Function, source: &str, kind: SymbolKind) -> DocumentSymbol {
        let detail = format!(
            "({}) -> {}",
            f.params
                .iter()
                .map(|p| format!("{}: {}", p.name, p.ty))
                .collect::<Vec<_>>()
                .join(", "),
            f.return_type
        );
        Self::make_symbol(source, &f.span, f.name.clone(), Some(detail), kind, Vec::new())
    }

    /// Build the document outline (textDocument/documentSymbol).
    /// Uses the unexpanded module so only symbols written in this file appear.
    fn build_document_symbols(module: &Module, source: &str) -> Vec<DocumentSymbol> {
        let mut symbols = Vec::new();
        for item in &module.items {
            match item {
                Item::Function(f) => {
                    symbols.push(Self::function_symbol(f, source, SymbolKind::FUNCTION));
                }
                Item::Struct(s) => {
                    let fields = s
                        .fields
                        .iter()
                        .map(|fld| {
                            Self::make_symbol(
                                source,
                                &fld.span,
                                fld.name.clone(),
                                Some(fld.ty.to_string()),
                                SymbolKind::FIELD,
                                Vec::new(),
                            )
                        })
                        .collect();
                    symbols.push(Self::make_symbol(
                        source,
                        &s.span,
                        s.name.clone(),
                        None,
                        SymbolKind::STRUCT,
                        fields,
                    ));
                }
                Item::Enum(e) => {
                    let cases = e
                        .cases
                        .iter()
                        .map(|c| {
                            Self::make_symbol(
                                source,
                                &c.span,
                                c.name.clone(),
                                c.payload.as_ref().map(|p| p.to_string()),
                                SymbolKind::ENUM_MEMBER,
                                Vec::new(),
                            )
                        })
                        .collect();
                    symbols.push(Self::make_symbol(
                        source,
                        &e.span,
                        e.name.clone(),
                        None,
                        SymbolKind::ENUM,
                        cases,
                    ));
                }
                Item::Trait(t) => {
                    let methods = t
                        .methods
                        .iter()
                        .map(|m| Self::function_symbol(m, source, SymbolKind::METHOD))
                        .collect();
                    symbols.push(Self::make_symbol(
                        source,
                        &t.span,
                        t.name.clone(),
                        None,
                        SymbolKind::INTERFACE,
                        methods,
                    ));
                }
                Item::Impl(i) => {
                    let methods = i
                        .methods
                        .iter()
                        .map(|m| Self::function_symbol(m, source, SymbolKind::METHOD))
                        .collect();
                    symbols.push(Self::make_symbol(
                        source,
                        &i.span,
                        format!("impl {} for {}", i.trait_name, i.target_type),
                        None,
                        SymbolKind::OBJECT,
                        methods,
                    ));
                }
                Item::TypeAlias(t) => {
                    symbols.push(Self::make_symbol(
                        source,
                        &t.span,
                        t.name.clone(),
                        Some(format!("type = {}", t.ty)),
                        SymbolKind::TYPE_PARAMETER,
                        Vec::new(),
                    ));
                }
                Item::VarDecl(v) => {
                    let kind = if v.is_mut {
                        SymbolKind::VARIABLE
                    } else {
                        SymbolKind::CONSTANT
                    };
                    symbols.push(Self::make_symbol(
                        source,
                        &v.span,
                        v.name.clone(),
                        v.ty.as_ref().map(|t| t.to_string()),
                        kind,
                        Vec::new(),
                    ));
                }
                Item::Macro(m) => {
                    symbols.push(Self::make_symbol(
                        source,
                        &m.span,
                        m.name.clone(),
                        Some(format!("macro({})", m.params.join(", "))),
                        SymbolKind::FUNCTION,
                        Vec::new(),
                    ));
                }
                Item::MacroCall(_) => {}
            }
        }
        symbols
    }
}

// ─── LanguageServer implementation ────────────────────────────────────────────

#[tower_lsp::async_trait]
impl LanguageServer for Backend {
    async fn initialize(&self, _: InitializeParams) -> LspResult<InitializeResult> {
        Ok(InitializeResult {
            capabilities: ServerCapabilities {
                text_document_sync: Some(TextDocumentSyncCapability::Kind(
                    TextDocumentSyncKind::FULL,
                )),
                hover_provider: Some(HoverProviderCapability::Simple(true)),
                completion_provider: Some(CompletionOptions {
                    trigger_characters: Some(vec![
                        ".".into(),
                        ":".into(),
                        "(".into(),
                        "@".into(),
                    ]),
                    ..Default::default()
                }),
                definition_provider: Some(OneOf::Left(true)),
                document_symbol_provider: Some(OneOf::Left(true)),
                semantic_tokens_provider: Some(
                    SemanticTokensServerCapabilities::SemanticTokensOptions(
                        SemanticTokensOptions {
                            legend: SemanticTokensLegend {
                                token_types: SEMANTIC_TOKEN_TYPES
                                    .iter()
                                    .map(|&s| s.into())
                                    .collect(),
                                token_modifiers: SEMANTIC_TOKEN_MODIFIERS
                                    .iter()
                                    .map(|&s| s.into())
                                    .collect(),
                            },
                            full: Some(SemanticTokensFullOptions::Bool(true)),
                            ..Default::default()
                        },
                    ),
                ),
                document_formatting_provider: Some(OneOf::Left(true)),
                diagnostic_provider: Some(DiagnosticServerCapabilities::Options(
                    DiagnosticOptions {
                        identifier: Some("nova".into()),
                        workspace_diagnostics: false,
                        inter_file_dependencies: false,
                        ..Default::default()
                    },
                )),
                ..Default::default()
            },
            server_info: Some(ServerInfo {
                name: "nova-lsp".into(),
                version: Some("0.1.0".into()),
            }),
        })
    }

    async fn initialized(&self, _: InitializedParams) {
        self.client
            .log_message(MessageType::INFO, "Nova LSP server started")
            .await;
    }

    async fn shutdown(&self) -> LspResult<()> {
        Ok(())
    }

    async fn did_open(&self, params: DidOpenTextDocumentParams) {
        let uri = params.text_document.uri.to_string();
        let source = params.text_document.text;
        self.documents.insert(
            uri.clone(),
            Document::new(source.clone()),
        );

        // Publish diagnostics
        let diags = Self::compute_diagnostics(&uri, &source);
        self.client
            .publish_diagnostics(
                params.text_document.uri.clone(),
                diags,
                None,
            )
            .await;
    }

    async fn did_change(&self, params: DidChangeTextDocumentParams) {
        let uri = params.text_document.uri.to_string();
        if let Some(change) = params.content_changes.into_iter().next() {
            let source = change.text;
            self.documents.insert(
                uri.clone(),
                Document::new(source.clone()),
            );

            let diags = Self::compute_diagnostics(&uri, &source);
            self.client
                .publish_diagnostics(params.text_document.uri.clone(), diags, None)
                .await;
        }
    }

    async fn did_close(&self, params: DidCloseTextDocumentParams) {
        self.documents.remove(&params.text_document.uri.to_string());
    }

    async fn hover(&self, params: HoverParams) -> LspResult<Option<Hover>> {
        let uri = params.text_document_position_params.text_document.uri.to_string();
        let pos = params.text_document_position_params.position;

        if let Some(doc) = self.documents.get(&uri) {
            let source = doc.source.clone();
            return Ok(Self::hover_at(&source, &uri, pos));
        }

        Ok(None)
    }

    async fn completion(
        &self,
        params: CompletionParams,
    ) -> LspResult<Option<CompletionResponse>> {
        let uri = params.text_document_position.text_document.uri.to_string();
        let pos = params.text_document_position.position;

        if let Some(doc) = self.documents.get(&uri) {
            let source = doc.source.clone();

            // Member completion after `recv.`
            let offset = Self::offset_of_position(&source, pos);
            if let Some(items) = Self::dot_completions(&source, &uri, offset) {
                return Ok(Some(CompletionResponse::Array(items)));
            }

            let (module, tokens) = match Self::parse_source(&source, &uri) {
                Ok(v) => v,
                Err(_) => return Ok(None),
            };

            let items = Self::build_completions(&module, &tokens, pos);
            return Ok(Some(CompletionResponse::Array(items)));
        }

        Ok(None)
    }

    async fn goto_definition(
        &self,
        params: GotoDefinitionParams,
    ) -> LspResult<Option<GotoDefinitionResponse>> {
        let uri = params
            .text_document_position_params
            .text_document
            .uri
            .to_string();
        let pos = params.text_document_position_params.position;

        if let Some(doc) = self.documents.get(&uri) {
            let source = doc.source.clone();
            let (_module, tokens) = match Self::parse_source(&source, &uri) {
                Ok(v) => v,
                Err(_) => return Ok(None),
            };

            // Find identifier under cursor
            if let Some(idx) = Self::token_at_position(&tokens, pos) {
                let name = match &tokens[idx].kind {
                    crate::token::TokenKind::Ident(s) => s.clone(),
                    _ => return Ok(None),
                };

                // Search for the definition in the same file
                for t in &tokens {
                    if let crate::token::TokenKind::Ident(ref s) = t.kind {
                        if s == &name && t.span.start != tokens[idx].span.start {
                            // Check if this is a definition (preceded by func, let, var, struct, etc.)
                            if let Some(range) = Self::token_range(t) {
                                return Ok(Some(GotoDefinitionResponse::Scalar(
                                    Location {
                                        uri: params
                                            .text_document_position_params
                                            .text_document
                                            .uri
                                            .clone(),
                                        range,
                                    },
                                )));
                            }
                        }
                    }
                }
            }
        }

        Ok(None)
    }

    async fn document_symbol(
        &self,
        params: DocumentSymbolParams,
    ) -> LspResult<Option<DocumentSymbolResponse>> {
        let uri = params.text_document.uri.to_string();

        if let Some(doc) = self.documents.get(&uri) {
            let source = doc.source.clone();
            // Parse without macro expansion: the outline should only show
            // symbols written in this file, not imported/generated ones.
            let module = match Self::try_parse(&source, &uri) {
                Some(m) => m,
                None => return Ok(None),
            };

            let symbols = Self::build_document_symbols(&module, &source);
            return Ok(Some(DocumentSymbolResponse::Nested(symbols)));
        }

        Ok(None)
    }

    async fn semantic_tokens_full(
        &self,
        params: SemanticTokensParams,
    ) -> LspResult<Option<SemanticTokensResult>> {
        let uri = params.text_document.uri.to_string();

        if let Some(doc) = self.documents.get(&uri) {
            let source = doc.source.clone();
            let tokens = match {
                let mut lex = Lexer::new(&source);
                lex.tokenize()
            } {
                Ok(t) => t,
                Err(_) => return Ok(None),
            };

            let raw_data = semantic::compute_semantic_tokens(&source, &tokens);
            // Convert flat u32 array to SemanticToken structs (groups of 5)
            let data: Vec<tower_lsp::lsp_types::SemanticToken> = raw_data
                .chunks_exact(5)
                .map(|chunk| tower_lsp::lsp_types::SemanticToken {
                    delta_line: chunk[0],
                    delta_start: chunk[1],
                    length: chunk[2],
                    token_type: chunk[3],
                    token_modifiers_bitset: chunk[4],
                })
                .collect();
            return Ok(Some(SemanticTokensResult::Tokens(SemanticTokens {
                result_id: None,
                data,
            })));
        }

        Ok(None)
    }

    async fn formatting(
        &self,
        params: DocumentFormattingParams,
    ) -> LspResult<Option<Vec<TextEdit>>> {
        let uri = params.text_document.uri.to_string();

        if let Some(doc) = self.documents.get(&uri) {
            let source = doc.source.clone();

            // Simple formatting: re-parse and pretty-print
            // For now, basic indentation normalization
            let formatted = Self::format_source(&source);

            let line_count = source.lines().count();
            let last_line_len = source.lines().last().map(|l| l.len()).unwrap_or(0);

            let edit = TextEdit {
                range: Range {
                    start: Position {
                        line: 0,
                        character: 0,
                    },
                    end: Position {
                        line: line_count as u32,
                        character: last_line_len as u32,
                    },
                },
                new_text: formatted,
            };

            return Ok(Some(vec![edit]));
        }

        Ok(None)
    }

    async fn diagnostic(
        &self,
        params: DocumentDiagnosticParams,
    ) -> LspResult<DocumentDiagnosticReportResult> {
        let uri = params.text_document.uri.to_string();

        if let Some(doc) = self.documents.get(&uri) {
            let source = doc.source.clone();
            let diags = Self::compute_diagnostics(&uri, &source);

            return Ok(DocumentDiagnosticReportResult::Report(
                DocumentDiagnosticReport::Full(RelatedFullDocumentDiagnosticReport {
                    related_documents: None,
                    full_document_diagnostic_report: FullDocumentDiagnosticReport {
                        result_id: None,
                        items: diags,
                    },
                }),
            ));
        }

        Ok(DocumentDiagnosticReportResult::Report(
            DocumentDiagnosticReport::Full(RelatedFullDocumentDiagnosticReport {
                related_documents: None,
                full_document_diagnostic_report: FullDocumentDiagnosticReport {
                    result_id: None,
                    items: vec![],
                },
            }),
        ))
    }
}

impl Backend {
    /// Basic source formatter: normalizes indentation.
    fn format_source(source: &str) -> String {
        let mut result = Vec::new();
        let mut indent: isize = 0;

        for line in source.lines() {
            let trimmed = line.trim();

            if trimmed.is_empty() {
                result.push(String::new());
                continue;
            }

            // Decrease indent for closing braces
            if trimmed.starts_with('}') || trimmed.starts_with("},") {
                indent = (indent - 1).max(0);
            }

            // Add indentation
            let indented = format!("{}{}", "    ".repeat(indent as usize), trimmed);

            // Increase indent for opening braces
            let open_braces = trimmed.matches('{').count() as isize;
            let close_braces = trimmed.matches('}').count() as isize;
            indent = (indent + open_braces - close_braces).max(0);

            result.push(indented);
        }

        result.join("\n")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diagnostics_expand_macros_like_the_compiler() {
        // matrix.nv uses @import + @debug macros; it compiles cleanly, so the
        // LSP must not report errors on the macro-generated `debug` functions.
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/examples/matrix.nv");
        let source = std::fs::read_to_string(path).unwrap();
        let uri = format!("file://{}", path);
        let diags = Backend::compute_diagnostics(&uri, &source);
        assert!(
            diags.is_empty(),
            "expected no diagnostics, got: {:?}",
            diags.iter().map(|d| &d.message).collect::<Vec<_>>()
        );
    }

    #[test]
    fn document_symbols_list_top_level_definitions() {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/examples/matrix.nv");
        let source = std::fs::read_to_string(path).unwrap();

        let mut lex = Lexer::new(&source);
        let tokens = lex.tokenize().unwrap();
        let mut parser = Parser::new(tokens, &source);
        let module = parser.parse_module("matrix".into()).unwrap();

        let symbols = Backend::build_document_symbols(&module, &source);
        let names: Vec<&str> = symbols.iter().map(|s| s.name.as_str()).collect();

        // Trait, structs, impls, and functions written in the file — nothing imported
        assert!(names.contains(&"Matrix"), "missing trait Matrix in {:?}", names);
        assert!(names.contains(&"Dense"), "missing struct Dense in {:?}", names);
        assert!(names.contains(&"HotVector"), "missing struct HotVector in {:?}", names);
        assert!(names.contains(&"impl Matrix for Dense"), "missing impl in {:?}", names);
        assert!(names.contains(&"main"), "missing func main in {:?}", names);
        assert!(!names.contains(&"debug"), "macro-generated debug leaked into {:?}", names);
        assert!(!names.contains(&"assert_eq"), "imported assert_eq leaked into {:?}", names);

        // Struct fields appear as children
        let dense = symbols.iter().find(|s| s.name == "Dense").unwrap();
        let fields: Vec<&str> = dense
            .children
            .as_deref()
            .unwrap_or(&[])
            .iter()
            .map(|c| c.name.as_str())
            .collect();
        assert_eq!(fields, vec!["sz"], "Dense fields: {:?}", fields);

        // Selection range points at the name, inside the full range
        let main_sym = symbols.iter().find(|s| s.name == "main").unwrap();
        assert!(main_sym.selection_range.start >= main_sym.range.start);
        assert!(main_sym.selection_range.end <= main_sym.range.end);
    }

    fn matrix_example() -> (String, String) {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/examples/matrix.nv");
        let source = std::fs::read_to_string(path).unwrap();
        (source, format!("file://{}", path))
    }

    fn generic_example() -> (String, String) {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/examples/test_generic.nv");
        let source = std::fs::read_to_string(path).unwrap();
        (source, format!("file://{}", path))
    }

    fn completion_labels(source: &str, uri: &str, offset: usize) -> Vec<String> {
        Backend::dot_completions(source, uri, offset)
            .unwrap_or_default()
            .into_iter()
            .map(|i| i.label)
            .collect()
    }

    /// Byte offset of `needle` → LSP position (first occurrence).
    fn pos_at(source: &str, needle: &str) -> Position {
        let offset = source.find(needle).expect("needle not found");
        let (mut line, mut col) = (0u32, 0u32);
        for ch in source[..offset].chars() {
            if ch == '\n' {
                line += 1;
                col = 0;
            } else {
                col += 1;
            }
        }
        Position { line, character: col }
    }

    fn hover_value(source: &str, uri: &str, needle: &str) -> String {
        let pos = pos_at(source, needle);
        match Backend::hover_at(source, uri, pos) {
            Some(h) => match h.contents {
                HoverContents::Markup(m) => m.value,
                _ => String::new(),
            },
            None => String::new(),
        }
    }

    #[test]
    fn generic_example_has_no_diagnostics() {
        let (source, uri) = generic_example();
        let diags = Backend::compute_diagnostics(&uri, &source);
        assert!(
            diags.is_empty(),
            "expected no diagnostics, got: {:?}",
            diags.iter().map(|d| &d.message).collect::<Vec<_>>()
        );
    }

    #[test]
    fn hover_shows_function_signature_with_generics() {
        let (source, uri) = generic_example();
        let value = hover_value(&source, &uri, "swap[A, B](");
        assert!(
            value.contains("func swap[A, B](p: Pair[A, B]) -> Pair[B, A]"),
            "got: {}",
            value
        );
    }

    #[test]
    fn hover_shows_generic_local_variable_type() {
        let (source, uri) = generic_example();
        // `t` = swap(make_pair(10, false)) → Pair[Bool, Int], pretty-printed
        // (not the mangled Pair__bool__int).
        let value = hover_value(&source, &uri, "t.b, 10");
        assert!(
            value.contains("t: Pair[Bool, Int]") && value.contains("local variable"),
            "got: {}",
            value
        );
    }

    #[test]
    fn hover_shows_match_pattern_binding_type() {
        let (source, uri) = generic_example();
        // `inner` bound by `case .some(inner)` of an Option[Pair[Int, Bool]].
        let value = hover_value(&source, &uri, "inner.a, 1");
        assert!(
            value.contains("inner: Pair[Int, Bool]"),
            "got: {}",
            value
        );
    }

    #[test]
    fn hover_shows_parameter_in_signature() {
        let (source, uri) = generic_example();
        let value = hover_value(&source, &uri, "x: T) -> T");
        assert!(
            value.contains("x: T") && value.contains("parameter of `id`"),
            "got: {}",
            value
        );
    }

    #[test]
    fn hover_shows_enum_case() {
        let (source, uri) = generic_example();
        // Hover the `some` in `.some(Pair { ... })`.
        let value = hover_value(&source, &uri, "some(Pair");
        assert!(
            value.contains("case some(T) of enum Option"),
            "got: {}",
            value
        );
    }

    #[test]
    fn hover_shows_struct_declaration() {
        let (source, uri) = generic_example();
        let value = hover_value(&source, &uri, "Pair[A, B] {");
        assert!(
            value.contains("struct Pair[A, B]") && value.contains("a: A"),
            "got: {}",
            value
        );
    }

    #[test]
    fn hover_shows_trait_typed_local() {
        let (source, uri) = matrix_example();
        // `a` comes from make_matrix(0) → Matrix (trait type).
        let value = hover_value(&source, &uri, "a.size(), 5");
        assert!(
            value.contains("a: Matrix"),
            "got: {}",
            value
        );
    }

    #[test]
    fn hover_returns_none_for_non_identifier() {
        let (source, uri) = generic_example();
        // Cursor on an integer literal — not an identifier.
        let pos = pos_at(&source, "0\n}");
        assert!(Backend::hover_at(&source, &uri, pos).is_none());
    }

    #[test]
    fn dot_completion_on_struct_receiver() {
        let (source, uri) = matrix_example();
        // Cursor right after `d.` in `d.debug();`
        let offset = source.find("d.debug").unwrap() + 2;
        let labels = completion_labels(&source, &uri, offset);

        assert!(labels.contains(&"sz".into()), "field sz missing: {:?}", labels);
        assert!(labels.contains(&"size".into()), "impl method size missing: {:?}", labels);
        assert!(labels.contains(&"debug".into()), "macro-generated debug missing: {:?}", labels);
        // UFCS through the Matrix trait Dense implements
        assert!(labels.contains(&"dot".into()), "trait-param func dot missing: {:?}", labels);
        assert_eq!(labels.iter().filter(|l| *l == "inner").count(), 2, "both inner overloads: {:?}", labels);
        // First param doesn't accept Dense
        assert!(!labels.contains(&"make_matrix".into()), "make_matrix leaked: {:?}", labels);
        assert!(!labels.contains(&"assert_eq".into()), "assert_eq leaked: {:?}", labels);
        // Trait method deduped against the identical impl signature
        assert_eq!(labels.iter().filter(|l| *l == "size").count(), 1, "size duplicated: {:?}", labels);
    }

    #[test]
    fn dot_completion_on_trait_receiver() {
        let (source, uri) = matrix_example();
        // `a` is a Matrix (trait type) from make_matrix
        let offset = source.find("a.debug").unwrap() + 2;
        let labels = completion_labels(&source, &uri, offset);

        assert!(labels.contains(&"size".into()), "trait method size missing: {:?}", labels);
        assert!(labels.contains(&"debug".into()), "debug(Matrix) missing: {:?}", labels);
        assert!(!labels.contains(&"sz".into()), "no fields on a trait type: {:?}", labels);
    }

    #[test]
    fn dot_completion_on_generic_struct_literal_receiver() {
        let (source, uri) = generic_example();
        // `p` is inferred as Pair[Int, Bool]; simulate typing `p.`.
        let broken = source.replace("assert_eq(\"pair.a int\", p.a, 1);", "p.");
        let offset = broken.find("p.\n").unwrap() + 2;
        let labels = completion_labels(&broken, &uri, offset);

        assert!(labels.contains(&"a".into()), "field a missing: {:?}", labels);
        assert!(labels.contains(&"b".into()), "field b missing: {:?}", labels);
    }

    #[test]
    fn dot_completion_on_swap_receiver() {
        let (source, uri) = generic_example();
        // `t` comes from swap(make_pair(10, false)) → Pair[Bool, Int];
        // the LSP must see through the generic calls and offer fields.
        let broken = source.replace("    assert_true(\"swap moved a\", t.a == false);", "    t.");
        let anchor = broken.find("let t = swap(make_pair(10, false));").unwrap();
        let offset = broken[anchor..].find("t.").unwrap() + anchor + 2;
        let labels = completion_labels(&broken, &uri, offset);

        assert!(labels.contains(&"a".into()), "field a missing: {:?}", labels);
        assert!(labels.contains(&"b".into()), "field b missing: {:?}", labels);
    }

    #[test]
    fn dot_completion_while_typing_incomplete_member() {
        let (source, uri) = matrix_example();
        // Simulate mid-typing: `d.debug();` replaced by a bare `d.` (won't parse)
        let broken = source.replace("d.debug();  // static: calls debug(Dense) ✓", "d.");
        let offset = broken.find("d.\n").unwrap() + 2;
        let labels = completion_labels(&broken, &uri, offset);

        assert!(labels.contains(&"sz".into()), "field sz missing: {:?}", labels);
        assert!(labels.contains(&"size".into()), "method size missing: {:?}", labels);
    }

    #[test]
    fn dot_receiver_detection() {
        assert_eq!(Backend::dot_receiver("d.", 2), Some("d".into()));
        assert_eq!(Backend::dot_receiver("foo.par", 7), Some("foo".into()));
        assert_eq!(Backend::dot_receiver("1.5", 2), None); // float literal
        assert_eq!(Backend::dot_receiver(".Case", 1), None); // enum case
        assert_eq!(Backend::dot_receiver("plain", 5), None); // no dot
    }
}

// ─── Entry point ──────────────────────────────────────────────────────────────

/// Start the Nova LSP server on stdin/stdout.
pub async fn run() {
    let stdin = tokio::io::stdin();
    let stdout = tokio::io::stdout();

    let (service, socket) = LspService::new(Backend::new);
    Server::new(stdin, stdout, socket)
        .serve(service)
        .await;
}
