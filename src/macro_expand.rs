use crate::ast::*;
use crate::error::CompileError;
use crate::import_macro::ImportMacro;
use crate::interpreter::{Interpreter, Value};
use crate::token::Span;
use std::collections::HashMap;

/// Macro expander — walks the AST and expands macro invocations.
/// Macros operate on AST fragments via quote/unquote.
pub struct MacroExpander {
    macros: HashMap<String, MacroDef>,
    import_macro: ImportMacro,
    interpreter: Interpreter,
}

impl MacroExpander {
    pub fn new(import_macro: ImportMacro, interpreter: Interpreter) -> Self {
        let mut expander = Self { macros: HashMap::new(), import_macro, interpreter };
        expander.bootstrap_import_macro();
        expander
    }

    /// Register structs from a module for type lookup in macros
    pub fn register_structs(&mut self, module: &Module) {
        self.interpreter.register_structs(module);
    }

    /// Load the userland @import macro from std/compile/import.nv
    fn bootstrap_import_macro(&mut self) {
        // Try to find and load std/compile/import.nv
        let import_source = self.import_macro.try_read_file("std/compile/import.nv");
        if let Some(source) = import_source {
            let mut lex = crate::lexer::Lexer::new(&source);
            if let Ok(tokens) = lex.tokenize() {
                let mut parser = crate::parser::Parser::new(tokens, &source);
                if let Ok(module) = parser.parse_module("import".to_string()) {
                    for item in &module.items {
                        if let Item::Macro(mdef) = item {
                            if mdef.name == "import" {
                                self.macros.insert("import".to_string(), mdef.clone());
                                return;
                            }
                        }
                    }
                }
            }
        }
        // If bootstrap fails, the built-in import_macro handles @import
    }

    /// Expand all macro invocations in a module.
    /// This walks all items and expands macros within expressions.
    pub fn expand_module(&mut self, module: &mut Module) -> Result<(), CompileError> {
        // First pass: register macros
        let macros: Vec<MacroDef> = module.items.iter().filter_map(|item| {
            if let Item::Macro(m) = item { Some(m.clone()) } else { None }
        }).collect();

        for m in &macros {
            self.macros.insert(m.name.clone(), m.clone());
        }

        // Second pass: expand macro calls in all items
        let mut expanded_items = Vec::new();
        let mut spliced_sigs: std::collections::HashSet<String> = std::collections::HashSet::new();
        for item in module.items.drain(..) {
            match item {
                Item::Macro(_) => {
                    // Don't emit macro definitions in the output
                    // (they're compile-time only for now)
                }
                Item::MacroCall(call) => {
                    let expanded = self.expand_module_macro(&call)?;
                    // Register any macro definitions in the result
                    for item in &expanded {
                        if let Item::Macro(mdef) = item {
                            self.macros.insert(mdef.name.clone(), mdef.clone());
                        }
                    }
                    // Skip exact re-splices (e.g. an item imported both by a
                    // whole-module @import and a later selective @import)
                    for item in expanded {
                        let sig = Self::item_signature(&item);
                        if let Some(sig) = sig {
                            if !spliced_sigs.insert(sig) { continue; }
                        }
                        expanded_items.push(item);
                    }
                }
                Item::Function(mut func) => {
                    self.expand_in_function(&mut func)?;
                    expanded_items.push(Item::Function(func));
                }
                Item::VarDecl(mut vd) => {
                    if let Some(ref mut val) = vd.value {
                        *val = self.expand_expr(val.clone())?.clone();
                    }
                    expanded_items.push(Item::VarDecl(vd));
                }
                other => {
                    expanded_items.push(other);
                }
            }
        }

        module.items = expanded_items;
        Ok(())
    }

    /// Identity of a spliced item for dedup: kind + name + (for functions) param types.
    fn item_signature(item: &Item) -> Option<String> {
        match item {
            Item::Function(f) => {
                let params: Vec<String> = f.params.iter().map(|p| p.ty.to_string()).collect();
                Some(format!("fn {}({})", f.name, params.join(",")))
            }
            Item::Struct(s) => Some(format!("struct {}", s.name)),
            Item::Enum(e) => Some(format!("enum {}", e.name)),
            Item::TypeAlias(t) => Some(format!("type {}", t.name)),
            Item::VarDecl(v) => Some(format!("var {}", v.name)),
            _ => None,
        }
    }

    fn expand_in_function(&mut self, func: &mut Function) -> Result<(), CompileError> {
        self.expand_in_block(&mut func.body)?;
        Ok(())
    }

    fn expand_in_block(&mut self, block: &mut Block) -> Result<(), CompileError> {
        let mut new_stmts = Vec::new();
        for stmt in block.stmts.drain(..) {
            let expanded = self.expand_expr(stmt)?;
            // If expansion yields a block, flatten its statements
            if let ExprKind::Block(b) = expanded.kind {
                new_stmts.extend(b.stmts);
            } else {
                new_stmts.push(expanded);
            }
        }
        block.stmts = new_stmts;
        Ok(())
    }

    /// Expand a module-level macro call and return the generated items.
    fn expand_module_macro(&mut self, call: &MacroCallItem) -> Result<Vec<Item>, CompileError> {
        // Built-in: @import("module.path", ...) — kept for bootstrap
        if call.name == "import" {
            // Use the userland macro when its signature fits (plain imports);
            // selective/renaming imports are handled by the built-in.
            if let Some(macro_def) = self.macros.get("import").cloned() {
                if macro_def.params.len() == call.args.len() {
                    let result = self.eval_macro(&macro_def, &call.args, call.span)?;
                    return self.extract_result_items(result);
                }
            }
            // Fallback to built-in import
            return self.import_macro.eval(&call.args, call.span);
        }

        let macro_def = self.macros.get(&call.name).cloned().ok_or_else(|| {
            CompileError::macro_err(
                format!("Undefined macro: '{}'", call.name),
                call.span,
            )
        })?;

        let result = self.eval_macro(&macro_def, &call.args, call.span)?;
        self.extract_result_items(result)
    }

    fn extract_result_items(&self, result: Expr) -> Result<Vec<Item>, CompileError> {
        match result.kind {
            ExprKind::Block(ref block) => {
                Ok(self.extract_items_from_stmts(&block.stmts))
            }
            ExprKind::FuncDef(func) => {
                Ok(vec![Item::Function(func)])
            }
            ExprKind::Quote(ref stmts) => {
                Ok(self.extract_items_from_stmts(stmts))
            }
            ExprKind::CompileTimeResult(items) => {
                Ok(items)
            }
            _ => Ok(vec![])
        }
    }

    fn extract_items_from_stmts(&self, stmts: &[Expr]) -> Vec<Item> {
        let mut items = Vec::new();
        for stmt in stmts {
            match &stmt.kind {
                ExprKind::FuncDef(func) => {
                    items.push(Item::Function(func.clone()));
                }
                ExprKind::Let { name, ty, value, is_mut } => {
                    items.push(Item::VarDecl(VarDecl {
                        name: name.clone(),
                        ty: ty.clone(),
                        value: Some((**value).clone()),
                        is_mut: *is_mut,
                        span: stmt.span,
                    }));
                }
                ExprKind::Quote(ref inner) => {
                    items.extend(self.extract_items_from_stmts(inner));
                }
                ExprKind::Block(ref block) => {
                    items.extend(self.extract_items_from_stmts(&block.stmts));
                }
                _ => {}
            }
        }
        items
    }

    /// Recursively expand macro calls in an expression
    pub fn expand_expr(&mut self, expr: Expr) -> Result<Expr, CompileError> {
        let span = expr.span;

        match expr.kind {
            ExprKind::MacroCall { name, args } => {
                // Expand arguments first
                let expanded_args: Vec<Expr> = args
                    .into_iter()
                    .map(|a| self.expand_expr(a))
                    .collect::<Result<_, _>>()?;

                // Look up the macro
                let macro_def = self.macros.get(&name).cloned().ok_or_else(|| {
                    CompileError::macro_err(
                        format!("Undefined macro: '{}'", name),
                        span,
                    )
                })?;

                // Evaluate the macro: this runs the macro body with args bound as AST
                self.eval_macro(&macro_def, &expanded_args, span)
            }

            ExprKind::Quote(stmts) => {
                // Don't expand inside quote — that's the whole point!
                // But we DO process unquotes within quote.
                // For now, leave quotes as-is; they're handled during macro evaluation.
                Ok(Expr::new(ExprKind::Quote(stmts), span))
            }

            // Recursive cases
            ExprKind::Block(mut block) => {
                self.expand_in_block(&mut block)?;
                Ok(Expr::new(ExprKind::Block(block), span))
            }

            ExprKind::Binary { op, left, right } => {
                Ok(Expr::new(ExprKind::Binary {
                    op,
                    left: Box::new(self.expand_expr(*left)?),
                    right: Box::new(self.expand_expr(*right)?),
                }, span))
            }

            ExprKind::Unary { op, expr: inner } => {
                Ok(Expr::new(ExprKind::Unary {
                    op,
                    expr: Box::new(self.expand_expr(*inner)?),
                }, span))
            }

            ExprKind::Call { func, args } => {
                Ok(Expr::new(ExprKind::Call {
                    func: Box::new(self.expand_expr(*func)?),
                    args: args.into_iter().map(|a| self.expand_expr(a)).collect::<Result<_, _>>()?,
                }, span))
            }

            ExprKind::If { cond, then_branch, else_branch } => {
                let mut then_b = then_branch;
                self.expand_in_block(&mut then_b)?;
                let mut else_b = else_branch;
                if let Some(ref mut eb) = else_b {
                    self.expand_in_block(eb)?;
                }
                Ok(Expr::new(ExprKind::If {
                    cond: Box::new(self.expand_expr(*cond)?),
                    then_branch: then_b,
                    else_branch: else_b,
                }, span))
            }

            ExprKind::Match { expr: matched, arms } => {
                Ok(Expr::new(ExprKind::Match {
                    expr: Box::new(self.expand_expr(*matched)?),
                    arms: arms.into_iter().map(|arm| {
                        Ok(MatchArm {
                            pattern: arm.pattern,
                            guard: arm.guard.map(|g| Box::new(self.expand_expr(*g).unwrap())),
                            body: Box::new(self.expand_expr(*arm.body)?),
                        })
                    }).collect::<Result<_, CompileError>>()?,
                }, span))
            }

            ExprKind::While { cond, mut body } => {
                self.expand_in_block(&mut body)?;
                Ok(Expr::new(ExprKind::While {
                    cond: Box::new(self.expand_expr(*cond)?),
                    body,
                }, span))
            }

            ExprKind::Let { name, ty, value, is_mut } => {
                Ok(Expr::new(ExprKind::Let {
                    name,
                    ty,
                    value: Box::new(self.expand_expr(*value)?),
                    is_mut,
                }, span))
            }

            ExprKind::Return(opt) => {
                Ok(Expr::new(ExprKind::Return(
                    opt.map(|e| Box::new(self.expand_expr(*e).unwrap())),
                ), span))
            }

            ExprKind::Assign { target, value } => {
                Ok(Expr::new(ExprKind::Assign {
                    target: Box::new(self.expand_expr(*target)?),
                    value: Box::new(self.expand_expr(*value)?),
                }, span))
            }

            ExprKind::GcNew { ty, fields } => {
                Ok(Expr::new(ExprKind::GcNew {
                    ty,
                    fields: fields.into_iter().map(|(n, e)| Ok((n, self.expand_expr(e)?))).collect::<Result<_, CompileError>>()?,
                }, span))
            }

            ExprKind::StructLit { path, fields } => {
                Ok(Expr::new(ExprKind::StructLit {
                    path,
                    fields: fields.into_iter().map(|(n, e)| Ok((n, self.expand_expr(e)?))).collect::<Result<_, CompileError>>()?,
                }, span))
            }

            // Leaves — no expansion needed
            ExprKind::IntLiteral(_)
            | ExprKind::FloatLiteral(_)
            | ExprKind::StringLiteral(_)
            | ExprKind::CharLiteral(_)
            | ExprKind::BoolLiteral(_)
            | ExprKind::NilLiteral
            | ExprKind::Ident(_)
            | ExprKind::Path(_)
            | ExprKind::FuncDef(_)
            | ExprKind::CppBlock(_)
            | ExprKind::CompileTimeResult(_)
            | ExprKind::DotAccess { .. }
            | ExprKind::NamedArg { .. } => Ok(expr),

            // These shouldn't appear at expansion time, but pass through
            ExprKind::Field { object, field } => {
                Ok(Expr::new(ExprKind::Field {
                    object: Box::new(self.expand_expr(*object)?),
                    field,
                }, span))
            }
            ExprKind::Index { object, index } => {
                Ok(Expr::new(ExprKind::Index {
                    object: Box::new(self.expand_expr(*object)?),
                    index: Box::new(self.expand_expr(*index)?),
                }, span))
            }
            ExprKind::For { var, iter, mut body } => {
                self.expand_in_block(&mut body)?;
                Ok(Expr::new(ExprKind::For {
                    var,
                    iter: Box::new(self.expand_expr(*iter)?),
                    body,
                }, span))
            }
            ExprKind::Lambda { params, return_type, body } => {
                Ok(Expr::new(ExprKind::Lambda {
                    params,
                    return_type,
                    body: Box::new(self.expand_expr(*body)?),
                }, span))
            }
            ExprKind::EnumCtor { path, case, arg } => {
                Ok(Expr::new(ExprKind::EnumCtor {
                    path,
                    case,
                    arg: arg.map(|a| Box::new(self.expand_expr(*a).unwrap())),
                }, span))
            }
            ExprKind::AssignOp { target, op, value } => {
                Ok(Expr::new(ExprKind::AssignOp {
                    target: Box::new(self.expand_expr(*target)?),
                    op,
                    value: Box::new(self.expand_expr(*value)?),
                }, span))
            }
            ExprKind::Unquote(_) | ExprKind::UnquoteIdent(_) => {
                // Unquote outside of a quote context is an error
                Err(CompileError::macro_err(
                    "$unquote outside of quote context",
                    span,
                ))
            }
        }
    }

    /// Evaluate a macro: bind args to parameter names, then evaluate the body.
    /// The body should produce an AST via quote/unquote or interpreter execution.
    fn eval_macro(
        &mut self,
        macro_def: &MacroDef,
        args: &[Expr],
        call_span: Span,
    ) -> Result<Expr, CompileError> {
        if macro_def.params.len() != args.len() {
            return Err(CompileError::macro_err(
                format!(
                    "Macro '{}' expects {} arguments, got {}",
                    macro_def.name,
                    macro_def.params.len(),
                    args.len()
                ),
                call_span,
            ));
        }

        // Check if this macro body uses compile-time features (#include, etc.)
        let uses_interpreter = self.body_uses_interpreter(&macro_def.body);

        if uses_interpreter {
            // Use the interpreter for full compile-time evaluation
            let mut env: HashMap<String, Value> = HashMap::new();
            for (param, arg) in macro_def.params.iter().zip(args.iter()) {
                let val = self.expr_to_value(arg);
                env.insert(param.clone(), val);
            }
            let result = self.interpreter.eval(&macro_def.body, &mut env)?;
            self.value_to_expr(&result, call_span)
        } else {
            // Simple substitution (original behavior for quote/unquote macros)
            let mut bindings: HashMap<String, Expr> = HashMap::new();
            for (param, arg) in macro_def.params.iter().zip(args.iter()) {
                bindings.insert(param.clone(), arg.clone());
            }
            self.eval_macro_body(&macro_def.body, &bindings)
        }
    }

    /// Check if a macro body uses interpreter-requiring features
    fn body_uses_interpreter(&self, expr: &Expr) -> bool {
        match &expr.kind {
            ExprKind::Block(block) => block.stmts.iter().any(|s| self.body_uses_interpreter(s)),
            ExprKind::Let { value, .. } => self.body_uses_interpreter(value),
            ExprKind::Call { .. } => true, // function calls need interpreter
            ExprKind::Binary { .. } => true,
            ExprKind::Field { .. } => true,
            ExprKind::For { .. } => true,
            ExprKind::If { .. } => true,
            ExprKind::Assign { .. } => true,
            _ => false,
        }
    }

    fn expr_to_value(&self, expr: &Expr) -> Value {
        match &expr.kind {
            ExprKind::StringLiteral(s) => Value::String(s.clone()),
            ExprKind::IntLiteral(n) => Value::Int(*n),
            ExprKind::BoolLiteral(b) => Value::Bool(*b),
            ExprKind::Ident(name) => Value::String(name.clone()),
            _ => Value::Nil,
        }
    }

    fn value_to_expr(&self, val: &Value, span: Span) -> Result<Expr, CompileError> {
        match val {
            Value::String(s) => Ok(Expr::string_literal(s, span)),
            Value::Int(n) => Ok(Expr::int_literal(*n, span)),
            Value::Bool(b) => Ok(Expr::new(ExprKind::BoolLiteral(*b), span)),
            Value::Items(items) => Ok(Expr::new(ExprKind::CompileTimeResult(items.clone()), span)),
            Value::Nil => Ok(Expr::new(ExprKind::NilLiteral, span)),
            _ => Err(CompileError::macro_err(
                "Cannot convert this value back to an expression",
                span,
            )),
        }
    }

    /// Evaluate a macro body expression, substituting variables from bindings
    /// and processing quote/unquote.
    fn eval_macro_body(
        &mut self,
        expr: &Expr,
        bindings: &HashMap<String, Expr>,
    ) -> Result<Expr, CompileError> {
        let span = expr.span;

        match &expr.kind {
            ExprKind::Ident(name) => {
                // Variable reference — substitute if bound
                // Also handle $name (which comes from parsing inside quote)
                if let Some(stripped) = name.strip_prefix('$') {
                    if let Some(bound) = bindings.get(stripped) {
                        return Ok(bound.clone());
                    }
                }
                if let Some(bound) = bindings.get(name) {
                    Ok(bound.clone())
                } else {
                    Ok(expr.clone())
                }
            }

            ExprKind::Quote(stmts) => {
                // Process the quoted statements, handling unquotes.
                // Unquotes ($ident and $(expr)) are evaluated and spliced.
                let mut processed = Vec::new();
                for stmt in stmts {
                    let result = self.process_quote_stmt(stmt, bindings)?;
                    // If the result is a block, flatten it (splicing)
                    if let ExprKind::Block(b) = &result.kind {
                        processed.extend(b.stmts.clone());
                    } else {
                        processed.push(result);
                    }
                }
                Ok(Expr::new(ExprKind::Quote(processed), span))
            }

            ExprKind::UnquoteIdent(ref name) => {
                // $name — look up in bindings
                bindings.get(name).cloned().ok_or_else(|| {
                    CompileError::macro_err(
                        format!("Unbound variable in unquote: '{}'", name),
                        span,
                    )
                })
            }

            ExprKind::Unquote(ref inner) => {
                // $(expr) — evaluate the expression in the macro context
                // The expression may reference bound variables
                self.eval_macro_body(inner, bindings)
            }

            ExprKind::Block(b) => {
                let mut new_stmts = Vec::new();
                for stmt in &b.stmts {
                    new_stmts.push(self.eval_macro_body(stmt, bindings)?);
                }
                Ok(Expr::block(new_stmts, span))
            }

            ExprKind::Call { func, args } => {
                let func = self.eval_macro_body(func, bindings)?;
                let args: Vec<Expr> = args.iter()
                    .map(|a| self.eval_macro_body(a, bindings))
                    .collect::<Result<_, _>>()?;
                if let ExprKind::Ident(name) = &func.kind {
                    if name == "esc" && args.len() == 1 {
                        // esc() — escape from hygiene, return the argument as-is
                        // In a more complete implementation, this would mark the AST
                        // as unhygienic. For now, just return the arg.
                        return Ok(args.into_iter().next().unwrap());
                    }
                }
                Ok(Expr::new(ExprKind::Call { func: Box::new(func), args }, span))
            }

            ExprKind::Binary { op, left, right } => {
                Ok(Expr::new(ExprKind::Binary {
                    op: *op,
                    left: Box::new(self.eval_macro_body(left, bindings)?),
                    right: Box::new(self.eval_macro_body(right, bindings)?),
                }, span))
            }

            ExprKind::MacroCall { name, args } => {
                let expanded_args: Vec<Expr> = args.iter()
                    .map(|a| self.eval_macro_body(a, bindings))
                    .collect::<Result<_, _>>()?;

                let macro_def = self.macros.get(name).cloned().ok_or_else(|| {
                    CompileError::macro_err(
                        format!("Undefined macro in macro body: '{}'", name),
                        span,
                    )
                })?;

                self.eval_macro(&macro_def, &expanded_args, span)
            }

            // Literals and other leaves
            _ => Ok(expr.clone()),
        }
    }

    /// Process a single statement within a quote block.
    /// Handles $ident interpolation and leaves everything else untouched.
    fn process_quote_stmt(
        &mut self,
        stmt: &Expr,
        bindings: &HashMap<String, Expr>,
    ) -> Result<Expr, CompileError> {
        let span = stmt.span;

        match &stmt.kind {
            ExprKind::UnquoteIdent(ref name) => {
                // $name inside quote — substitute the bound AST
                bindings.get(name).cloned().ok_or_else(|| {
                    CompileError::macro_err(
                        format!("Unbound variable in quote: '{}'", name),
                        span,
                    )
                })
            }

            ExprKind::Unquote(ref inner) => {
                // $(expr) inside quote — evaluate and splice
                self.eval_macro_body(inner, bindings)
            }

            ExprKind::Quote(ref inner_stmts) => {
                // Nested quote — process recursively
                let mut processed = Vec::new();
                for s in inner_stmts {
                    processed.push(self.process_quote_stmt(s, bindings)?);
                }
                Ok(Expr::new(ExprKind::Quote(processed), span))
            }

            ExprKind::Block(ref b) => {
                let mut new_stmts = Vec::new();
                for s in &b.stmts {
                    new_stmts.push(self.process_quote_stmt(s, bindings)?);
                }
                Ok(Expr::block(new_stmts, span))
            }

            ExprKind::Call { func, args } => {
                let func = Box::new(self.process_quote_stmt(func, bindings)?);
                let args: Vec<Expr> = args.iter()
                    .map(|a| self.process_quote_stmt(a, bindings))
                    .collect::<Result<_, _>>()?;
                Ok(Expr::new(ExprKind::Call { func, args }, span))
            }

            ExprKind::Binary { op, left, right } => {
                Ok(Expr::new(ExprKind::Binary {
                    op: *op,
                    left: Box::new(self.process_quote_stmt(left, bindings)?),
                    right: Box::new(self.process_quote_stmt(right, bindings)?),
                }, span))
            }

            ExprKind::Return(opt) => {
                Ok(Expr::new(ExprKind::Return(
                    opt.as_ref().map(|e| Box::new(self.process_quote_stmt(e, bindings).unwrap())),
                ), span))
            }

            ExprKind::Let { name, ty, value, is_mut } => {
                Ok(Expr::new(ExprKind::Let {
                    name: name.clone(),
                    ty: ty.clone(),
                    value: Box::new(self.process_quote_stmt(value, bindings)?),
                    is_mut: *is_mut,
                }, span))
            }

            ExprKind::If { cond, then_branch, else_branch } => {
                let mut then_b = then_branch.clone();
                let new_stmts: Result<Vec<_>, _> = then_b.stmts.iter()
                    .map(|s| self.process_quote_stmt(s, bindings))
                    .collect();
                then_b.stmts = new_stmts?;

                let else_b = else_branch.as_ref().and_then(|eb| {
                    let mut eb = eb.clone();
                    let new_stmts: Result<Vec<_>, _> = eb.stmts.iter()
                        .map(|s| self.process_quote_stmt(s, bindings))
                        .collect();
                    if let Ok(stmts) = new_stmts {
                        eb.stmts = stmts;
                        Some(eb)
                    } else {
                        None
                    }
                });

                Ok(Expr::new(ExprKind::If {
                    cond: Box::new(self.process_quote_stmt(cond, bindings)?),
                    then_branch: then_b,
                    else_branch: else_b,
                }, span))
            }

            ExprKind::MacroCall { name, args } => {
                let expanded_args: Vec<Expr> = args.iter()
                    .map(|a| self.process_quote_stmt(a, bindings))
                    .collect::<Result<_, _>>()?;

                let macro_def = self.macros.get(name).cloned().ok_or_else(|| {
                    CompileError::macro_err(
                        format!("Undefined macro in quote: '{}'", name),
                        span,
                    )
                })?;

                self.eval_macro(&macro_def, &expanded_args, span)
            }

            ExprKind::FuncDef(ref func) => {
                let mut new_func = func.clone();
                // Substitute $name in function name
                if let Some(stripped) = new_func.name.strip_prefix('$') {
                    if let Some(bound) = bindings.get(stripped) {
                        if let ExprKind::Ident(ref name) = bound.kind {
                            new_func.name = name.clone();
                        }
                    }
                }
                // Process the function body recursively
                let mut new_body_stmts = Vec::new();
                for stmt in &new_func.body.stmts {
                    new_body_stmts.push(self.process_quote_stmt(stmt, bindings)?);
                }
                new_func.body.stmts = new_body_stmts;
                Ok(Expr::new(ExprKind::FuncDef(new_func), span))
            }

            // Leaves — return as-is, but check for $name identifiers
            ExprKind::Ident(ref name) => {
                if let Some(stripped) = name.strip_prefix('$') {
                    // $name inside quote — substitute from bindings
                    bindings.get(stripped).cloned().map_or_else(|| Ok(stmt.clone()), |e| Ok(e))
                } else {
                    Ok(stmt.clone())
                }
            }
            _ => Ok(stmt.clone()),
        }
    }
}
