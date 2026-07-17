use crate::ast::*;
use crate::error::CompileError;
use crate::lexer::Lexer;
use crate::parser::Parser;
use crate::token::Span;
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

/// Built-in macro that handles @import("module.path", ...)
///
/// Usage:
///   @import("std.io")                  — import everything public
///   @import("std.io", "print")         — import just print
///   @import("std.io", "print" => "display") — import print as display
pub struct ImportMacro {
    search_paths: Vec<PathBuf>,
    /// Cache of loaded modules: path -> Module
    cache: HashMap<String, Module>,
    /// Public items from loaded modules: (module_key, name) -> Item
    public_items: HashMap<String, HashMap<String, Item>>,
}

impl ImportMacro {
    pub fn new(search_paths: Vec<PathBuf>) -> Self {
        Self {
            search_paths,
            cache: HashMap::new(),
            public_items: HashMap::new(),
        }
    }

    /// Try to read a file from search paths (for bootstrapping)
    pub fn try_read_file(&self, path: &str) -> Option<String> {
        for sp in &self.search_paths {
            let full = sp.join(path);
            if full.exists() {
                return fs::read_to_string(&full).ok();
            }
        }
        None
    }

    /// Evaluate @import("path", ...) and return the items to splice
    pub fn eval(
        &mut self,
        args: &[Expr],
        call_span: Span,
    ) -> Result<Vec<Item>, CompileError> {
        if args.is_empty() {
            return Err(CompileError::macro_err(
                "@import requires at least a module path",
                call_span,
            ));
        }

        // First argument: module path as a string
        let module_path = self.expect_string(&args[0], "module path")?;
        let path_segments: Vec<String> = module_path.split('.').map(|s| s.to_string()).collect();

        // Load the module if not cached
        let module_key = path_segments.join(".");
        if !self.cache.contains_key(&module_key) {
            self.load_module(&path_segments)?;
        }

        let items_map = self.public_items.get(&module_key).ok_or_else(|| {
            CompileError::macro_err(
                format!("Module '{}' has no public items", module_key),
                call_span,
            )
        })?;

        // If no further args, import everything
        if args.len() == 1 {
            return Ok(items_map.values().cloned().collect());
        }

        // Selective import: remaining args specify what to import
        let mut result = Vec::new();
        for arg in &args[1..] {
            match &arg.kind {
                ExprKind::StringLiteral(name) => {
                    // @import("std.io", "print")
                    if let Some(item) = items_map.get(name) {
                        result.push(item.clone());
                    } else {
                        return Err(CompileError::macro_err(
                            format!("'{}' not found in module '{}'", name, module_key),
                            arg.span,
                        ));
                    }
                }
                ExprKind::Ident(name) => {
                    // @import("std.io", print) — bare identifier
                    if let Some(item) = items_map.get(name) {
                        result.push(item.clone());
                    } else {
                        return Err(CompileError::macro_err(
                            format!("'{}' not found in module '{}'", name, module_key),
                            arg.span,
                        ));
                    }
                }
                ExprKind::Assign { target, value } => {
                    // @import("std.io", name = alias) — rename import
                    if let (ExprKind::Ident(name), ExprKind::Ident(alias)) = (&target.kind, &value.kind) {
                        if let Some(item) = items_map.get(name) {
                            let mut renamed = item.clone();
                            match &mut renamed {
                                Item::Function(ref mut f) => f.name = alias.clone(),
                                Item::Struct(ref mut s) => s.name = alias.clone(),
                                Item::Enum(ref mut e) => e.name = alias.clone(),
                                Item::TypeAlias(ref mut t) => t.name = alias.clone(),
                                _ => {}
                            }
                            result.push(renamed);
                        } else {
                            return Err(CompileError::macro_err(
                                format!("'{}' not found in module '{}'", name, module_key),
                                arg.span,
                            ));
                        }
                    } else {
                        return Err(CompileError::macro_err(
                            "Expected name = alias for import rename",
                            arg.span,
                        ));
                    }
                }
                _ => {
                    return Err(CompileError::macro_err(
                        "Expected string literal (import name) in @import",
                        arg.span,
                    ));
                }
            }
        }

        Ok(result)
    }

    fn expect_string(&self, expr: &Expr, ctx: &str) -> Result<String, CompileError> {
        match &expr.kind {
            ExprKind::StringLiteral(s) => Ok(s.clone()),
            _ => Err(CompileError::macro_err(
                format!("Expected string literal for {}", ctx),
                expr.span,
            )),
        }
    }

    fn load_module(&mut self, path: &[String]) -> Result<(), CompileError> {
        let module_key = path.join(".");
        let file_path = self.find_module_file(path)?;

        let source = fs::read_to_string(&file_path).map_err(|e| {
            CompileError::Generic(format!("Cannot read '{}': {}", module_key, e))
        })?;

        let mut lex = Lexer::new(&source);
        let tokens = lex.tokenize()?;
        let mut parser = Parser::new(tokens, &source);
        let module = parser.parse_module(path.last().cloned().unwrap_or_default())?;

        // Extract public items
        let mut items: HashMap<String, Item> = HashMap::new();
        for item in &module.items {
            match item {
                Item::Function(f) if f.is_pub => {
                    items.insert(f.name.clone(), item.clone());
                }
                Item::Struct(s) if s.is_pub => {
                    items.insert(s.name.clone(), item.clone());
                }
                Item::Enum(e) if e.is_pub => {
                    items.insert(e.name.clone(), item.clone());
                }
                Item::TypeAlias(t) => {
                    items.insert(t.name.clone(), item.clone());
                }
                Item::Macro(m) => {
                    items.insert(m.name.clone(), item.clone());
                }
                Item::VarDecl(v) => {
                    items.insert(v.name.clone(), item.clone());
                }
                _ => {}
            }
        }

        self.public_items.insert(module_key.clone(), items);
        self.cache.insert(module_key, module);

        Ok(())
    }

    fn find_module_file(&self, path: &[String]) -> Result<PathBuf, CompileError> {
        let relative = if path.len() == 1 {
            format!("{}.nv", path[0])
        } else {
            let mut p = path[..path.len() - 1].join("/");
            p.push_str(&format!("/{}.nv", path.last().unwrap()));
            p
        };

        for sp in &self.search_paths {
            let full = sp.join(&relative);
            if full.exists() {
                return Ok(full);
            }
        }

        // Try mod.nv inside a directory
        for sp in &self.search_paths {
            let mod_file = sp.join(path.join("/")).join("mod.nv");
            if mod_file.exists() {
                return Ok(mod_file);
            }
        }

        Err(CompileError::Generic(format!(
            "Module not found: '{}'. Searched in: {:?}",
            path.join("."),
            self.search_paths,
        )))
    }
}
