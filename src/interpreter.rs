use crate::ast::*;
use crate::error::CompileError;
use crate::token::Span;
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

#[derive(Debug, Clone)]
pub enum Value {
    Int(i64),
    Float(f64),
    String(String),
    Bool(bool),
    Expr(Expr),
    Items(Vec<Item>),
    Item(Item),
    List(Vec<Value>),
    Module(Module),
    /// Runtime struct instance: type name, field_name → value.
    /// The type name is what runtime overload dispatch keys on.
    Struct(String, HashMap<String, Value>),
    /// Enum variant: case name, optional payload
    Enum(String, Option<Box<Value>>),
    Nil,
}

pub struct Interpreter {
    search_paths: Vec<PathBuf>,
    known_structs: HashMap<String, Struct>,
    /// User-defined functions available at runtime; a name maps to its
    /// overload set (Nova allows overloading by parameter type, like the C++
    /// output does)
    functions: HashMap<String, Vec<Function>>,
    /// type name → traits it implements, from `impl Trait for Type` blocks
    impls: HashMap<String, Vec<String>>,
    /// Runtime mode: enables print/println/exit builtins
    runtime_mode: bool,
}

impl Interpreter {
    pub fn new(search_paths: Vec<PathBuf>) -> Self {
        Self { search_paths, known_structs: HashMap::new(), functions: HashMap::new(), impls: HashMap::new(), runtime_mode: false }
    }

    /// Enable runtime mode (print, println, etc.)
    pub fn enable_runtime_mode(&mut self) {
        self.runtime_mode = true;
    }

    /// Register structs AND functions from a module for runtime evaluation
    pub fn register_module(&mut self, module: &Module) {
        self.register_structs(module);
        for item in &module.items {
            match item {
                Item::Function(f) => {
                    if !f.body.stmts.is_empty() {
                        self.functions.entry(f.name.clone()).or_default().push(f.clone());
                    }
                }
                Item::Impl(impl_block) => {
                    if let Some(target) = Self::type_name(&impl_block.target_type) {
                        self.impls.entry(target.to_string()).or_default().push(impl_block.trait_name.clone());
                    }
                    for method in &impl_block.methods {
                        if !method.body.stmts.is_empty() {
                            self.functions.entry(method.name.clone()).or_default().push(method.clone());
                        }
                    }
                }
                _ => {}
            }
        }
    }

    /// The bare name of a type, seen through @T references (e.g. `@Point` → "Point")
    fn type_name(ty: &Type) -> Option<&str> {
        match ty {
            Type::Path(p) => p.segments.last().map(|s| s.name.as_str()),
            Type::GcRef(inner) => Self::type_name(inner),
            _ => None,
        }
    }

    /// The runtime type of a value, where it has a nameable one. Macro-time
    /// values (Expr, Item, List, …) return None and stay neutral in dispatch.
    fn value_type_name(value: &Value) -> Option<&str> {
        match value {
            Value::Int(_) => Some("Int"),
            Value::Float(_) => Some("Float"),
            Value::String(_) => Some("String"),
            Value::Bool(_) => Some("Bool"),
            Value::Struct(name, _) => Some(name),
            _ => None,
        }
    }

    pub fn register_structs(&mut self, module: &Module) {
        for item in &module.items {
            if let Item::Struct(s) = item {
                self.known_structs.insert(s.name.clone(), s.clone());
            }
            if let Item::Trait(t) = item {
                // Store traits too — lookup_type works for both
                self.known_structs.insert(t.name.clone(), Struct {
                    name: t.name.clone(), generics: vec![], fields: vec![], is_pub: true, span: Span::zero()
                });
            }
        }
    }

    pub fn eval(&mut self, expr: &Expr, env: &mut HashMap<String, Value>) -> Result<Value, CompileError> {
        match &expr.kind {
            ExprKind::IntLiteral(n) => Ok(Value::Int(*n)),
            ExprKind::FloatLiteral(n) => Ok(Value::Float(*n)),
            ExprKind::StringLiteral(s) => Ok(Value::String(s.clone())),
            ExprKind::BoolLiteral(b) => Ok(Value::Bool(*b)),
            ExprKind::NilLiteral => Ok(Value::Nil),

            ExprKind::Unary { op, expr: inner } => {
                let val = self.eval(inner, env)?;
                match op {
                    UnaryOp::Neg => match val {
                        Value::Int(n) => Ok(Value::Int(-n)),
                        Value::Float(n) => Ok(Value::Float(-n)),
                        _ => Err(CompileError::macro_err("Cannot negate this type", expr.span)),
                    },
                    UnaryOp::Not => match val {
                        Value::Bool(b) => Ok(Value::Bool(!b)),
                        _ => Err(CompileError::macro_err("Cannot ! this type", expr.span)),
                    },
                }
            }

            ExprKind::Ident(name) => env.get(name).cloned().ok_or_else(|| {
                CompileError::macro_err(format!("Undefined: '{}'", name), expr.span)
            }),

            ExprKind::Let { name, value, .. } => {
                let val = self.eval(value, env)?;
                env.insert(name.clone(), val.clone());
                Ok(val)
            }

            ExprKind::Block(block) => {
                let mut last = Value::Nil;
                for stmt in &block.stmts {
                    last = self.eval(stmt, env)?;
                }
                Ok(last)
            }

            ExprKind::Binary { op, left, right } => {
                let l = self.eval(left, env)?;
                let r = self.eval(right, env)?;
                match op {
                    BinOp::Add => match (&l, &r) {
                        (Value::Int(a), Value::Int(b)) => Ok(Value::Int(a + b)),
                        (Value::Float(a), Value::Float(b)) => Ok(Value::Float(a + b)),
                        (Value::String(a), Value::String(b)) => Ok(Value::String(format!("{}{}", a, b))),
                        _ => Err(CompileError::macro_err("Cannot add these types", expr.span)),
                    },
                    BinOp::Sub => match (&l, &r) {
                        (Value::Int(a), Value::Int(b)) => Ok(Value::Int(a - b)),
                        (Value::Float(a), Value::Float(b)) => Ok(Value::Float(a - b)),
                        _ => Err(CompileError::macro_err("Cannot subtract", expr.span)),
                    },
                    BinOp::Mul => match (&l, &r) {
                        (Value::Int(a), Value::Int(b)) => Ok(Value::Int(a * b)),
                        (Value::Float(a), Value::Float(b)) => Ok(Value::Float(a * b)),
                        _ => Err(CompileError::macro_err("Cannot multiply", expr.span)),
                    },
                    BinOp::Div => match (&l, &r) {
                        (Value::Int(a), Value::Int(b)) => Ok(Value::Int(a / b)),
                        (Value::Float(a), Value::Float(b)) => Ok(Value::Float(a / b)),
                        _ => Err(CompileError::macro_err("Cannot divide", expr.span)),
                    },
                    BinOp::Mod => match (&l, &r) {
                        (Value::Int(a), Value::Int(b)) => Ok(Value::Int(a % b)),
                        _ => Err(CompileError::macro_err("Cannot modulo", expr.span)),
                    },
                    BinOp::Eq => Ok(Value::Bool(self.equal(&l, &r))),
                    BinOp::NotEq => Ok(Value::Bool(!self.equal(&l, &r))),
                    BinOp::Lt => match (&l, &r) {
                        (Value::Int(a), Value::Int(b)) => Ok(Value::Bool(a < b)),
                        (Value::Float(a), Value::Float(b)) => Ok(Value::Bool(a < b)),
                        _ => Err(CompileError::macro_err("Cannot compare", expr.span)),
                    },
                    BinOp::Gt => match (&l, &r) {
                        (Value::Int(a), Value::Int(b)) => Ok(Value::Bool(a > b)),
                        (Value::Float(a), Value::Float(b)) => Ok(Value::Bool(a > b)),
                        _ => Err(CompileError::macro_err("Cannot compare", expr.span)),
                    },
                    BinOp::LtEq => match (&l, &r) {
                        (Value::Int(a), Value::Int(b)) => Ok(Value::Bool(a <= b)),
                        (Value::Float(a), Value::Float(b)) => Ok(Value::Bool(a <= b)),
                        _ => Err(CompileError::macro_err("Cannot compare", expr.span)),
                    },
                    BinOp::GtEq => match (&l, &r) {
                        (Value::Int(a), Value::Int(b)) => Ok(Value::Bool(a >= b)),
                        (Value::Float(a), Value::Float(b)) => Ok(Value::Bool(a >= b)),
                        _ => Err(CompileError::macro_err("Cannot compare", expr.span)),
                    },
                    BinOp::And => match (&l, &r) {
                        (Value::Bool(a), Value::Bool(b)) => Ok(Value::Bool(*a && *b)),
                        _ => Err(CompileError::macro_err("And requires bool", expr.span)),
                    },
                    BinOp::Or => match (&l, &r) {
                        (Value::Bool(a), Value::Bool(b)) => Ok(Value::Bool(*a || *b)),
                        _ => Err(CompileError::macro_err("Or requires bool", expr.span)),
                    },
                }
            }

            ExprKind::Call { func, args } => {
                let name = match &func.kind {
                    ExprKind::Ident(n) => n.clone(),
                    _ => return Err(CompileError::macro_err("Only named calls in macros", expr.span)),
                };
                let evaled: Vec<Value> = args.iter()
                    .map(|a| self.eval(a, env))
                    .collect::<Result<_, _>>()?;
                self.call_function(&name, &evaled, expr.span)
            }

            ExprKind::CompileTimeResult(items) => Ok(Value::Items(items.clone())),

            ExprKind::DotAccess { object, field } => {
                let obj = self.eval(object, env)?;
                self.access_field(&obj, field, expr.span)
            }

            ExprKind::Field { object, field } => {
                let obj = self.eval(object, env)?;
                self.access_field(&obj, field, expr.span)
            }

            ExprKind::Return(Some(inner)) => self.eval(inner, env),
            ExprKind::Return(None) => Ok(Value::Nil),

            // Index: list[idx]
            ExprKind::Index { object, index } => {
                let obj = self.eval(object, env)?;
                let idx = self.eval(index, env)?;
                match (&obj, &idx) {
                    (Value::List(items), Value::Int(i)) => {
                        items.get(*i as usize).cloned()
                            .ok_or_else(|| CompileError::macro_err("Index out of bounds", expr.span))
                    }
                    _ => Err(CompileError::macro_err("Can only index lists with integers", expr.span)),
                }
            }

            // Assignment: name = value
            ExprKind::Assign { target, value } => {
                let val = self.eval(value, env)?;
                match &target.kind {
                    ExprKind::Ident(name) => {
                        env.insert(name.clone(), val.clone());
                        Ok(val)
                    }
                    _ => Err(CompileError::macro_err("Can only assign to variables", expr.span)),
                }
            }

            // If expression
            ExprKind::If { cond, then_branch, else_branch } => {
                let cond_val = self.eval(cond, env)?;
                let take_then = match cond_val {
                    Value::Bool(b) => b,
                    _ => return Err(CompileError::macro_err("If condition must be bool", expr.span)),
                };
                if take_then {
                    let mut last = Value::Nil;
                    for stmt in &then_branch.stmts { last = self.eval(stmt, env)?; }
                    Ok(last)
                } else if let Some(ref else_b) = else_branch {
                    let mut last = Value::Nil;
                    for stmt in &else_b.stmts { last = self.eval(stmt, env)?; }
                    Ok(last)
                } else {
                    Ok(Value::Nil)
                }
            }

            // For loop: for <var> in <list> { <body> }
            ExprKind::For { var, iter, body } => {
                let list = self.eval(iter, env)?;
                let items: Vec<Value> = match &list {
                    Value::List(l) => l.clone(),
                    Value::Items(i) => i.iter().map(|item| Value::Item(item.clone())).collect(),
                    _ => return Err(CompileError::macro_err("Can only iterate over lists", expr.span)),
                };
                let mut last = Value::Nil;
                for item in &items {
                    env.insert(var.clone(), item.clone());
                    for stmt in &body.stmts {
                        last = self.eval(stmt, env)?;
                    }
                }
                Ok(last)
            }

            ExprKind::NamedArg { value, .. } => self.eval(value, env),

            // @cpp { ... } — only std::exit is handled in interpreter mode
            ExprKind::CppBlock(code) => {
                if self.runtime_mode {
                    if code.contains("std::exit") {
                        std::process::exit(1);
                    }
                    return Err(CompileError::macro_err(
                        format!("@cpp blocks are not supported in interpreter mode: {}", code.trim()), expr.span,
                    ));
                }
                Ok(Value::Nil)
            }

            // While loop
            ExprKind::While { cond, body } => {
                let mut last = Value::Nil;
                loop {
                    let cond_val = self.eval(cond, env)?;
                    let keep_going = match cond_val {
                        Value::Bool(b) => b,
                        _ => return Err(CompileError::macro_err("While condition must be bool", expr.span)),
                    };
                    if !keep_going { break; }
                    for stmt in &body.stmts {
                        last = self.eval(stmt, env)?;
                    }
                }
                Ok(last)
            }

            // Struct literal: TypeName { field: val, ... }
            ExprKind::StructLit { path, fields } => {
                let type_name = path.last().cloned().unwrap_or_default();
                let mut map = HashMap::new();
                for (name, val_expr) in fields {
                    let val = self.eval(val_expr, env)?;
                    map.insert(name.clone(), val);
                }
                Ok(Value::Struct(type_name, map))
            }

            // GC allocation: @Type { fields } — treat like struct in interpreter
            ExprKind::GcNew { ty, fields } => {
                let type_name = Self::type_name(ty).unwrap_or_default().to_string();
                let mut map = HashMap::new();
                for (name, val_expr) in fields {
                    let val = self.eval(val_expr, env)?;
                    map.insert(name.clone(), val);
                }
                Ok(Value::Struct(type_name, map))
            }

            // Enum constructor: .CaseName or .CaseName(arg)
            ExprKind::EnumCtor { case, arg, .. } => {
                let payload = match arg {
                    Some(a) => Some(Box::new(self.eval(a, env)?)),
                    None => None,
                };
                Ok(Value::Enum(case.clone(), payload))
            }

            // Pattern matching
            ExprKind::Match { expr: matched, arms } => {
                let val = self.eval(matched, env)?;
                for arm in arms {
                    if let Some(bindings) = self.match_pattern(&val, &arm.pattern)? {
                        // Check guard if present
                        if let Some(ref guard) = arm.guard {
                            let mut guard_env = env.clone();
                            for (name, v) in &bindings {
                                guard_env.insert(name.clone(), v.clone());
                            }
                            let guard_val = self.eval(guard, &mut guard_env)?;
                            if let Value::Bool(false) = guard_val {
                                continue;
                            }
                        }
                        // Pattern matched — evaluate body with bindings
                        let mut body_env = env.clone();
                        for (name, v) in &bindings {
                            body_env.insert(name.clone(), v.clone());
                        }
                        return self.eval(&arm.body, &mut body_env);
                    }
                }
                Err(CompileError::macro_err("No matching pattern", expr.span))
            }

            // Type path used as an expression (e.g. returning a Matrix type)
            ExprKind::Path(segments) => {
                // Return the type name as a string for now
                let name = segments.join("::");
                Ok(Value::String(name))
            }

            // Lambda / closure — not supported yet
            ExprKind::Lambda { .. } => {
                Err(CompileError::macro_err("Lambdas not supported in interpreter", expr.span))
            }

            // AssignOp: x += 1
            ExprKind::AssignOp { target, op, value } => {
                let target_name = match &target.kind {
                    ExprKind::Ident(n) => n.clone(),
                    _ => return Err(CompileError::macro_err("Can only assign to variables", expr.span)),
                };
                let current = env.get(&target_name).cloned().unwrap_or(Value::Nil);
                let rhs = self.eval(value, env)?;
                let result = match op {
                    BinOp::Add => match (&current, &rhs) {
                        (Value::Int(a), Value::Int(b)) => Value::Int(a + b),
                        _ => return Err(CompileError::macro_err("Cannot += these types", expr.span)),
                    },
                    BinOp::Sub => match (&current, &rhs) {
                        (Value::Int(a), Value::Int(b)) => Value::Int(a - b),
                        _ => return Err(CompileError::macro_err("Cannot -= these types", expr.span)),
                    },
                    _ => return Err(CompileError::macro_err("Unsupported assign-op", expr.span)),
                };
                env.insert(target_name, result.clone());
                Ok(result)
            }

            _ => Err(CompileError::macro_err(
                format!("Unsupported: {:?}", expr.kind), expr.span,
            )),
        }
    }

    fn equal(&self, a: &Value, b: &Value) -> bool {
        match (a, b) {
            (Value::Int(a), Value::Int(b)) => a == b,
            (Value::Float(a), Value::Float(b)) => a == b,
            (Value::String(a), Value::String(b)) => a == b,
            (Value::Bool(a), Value::Bool(b)) => a == b,
            (Value::Nil, Value::Nil) => true,
            (Value::Enum(a_case, a_payload), Value::Enum(b_case, b_payload)) => {
                if a_case != b_case { return false; }
                match (a_payload, b_payload) {
                    (None, None) => true,
                    (Some(a), Some(b)) => self.equal(a, b),
                    _ => false,
                }
            }
            _ => false,
        }
    }

    fn access_field(&self, obj: &Value, field: &str, span: Span) -> Result<Value, CompileError> {
        match (obj, field) {
            // Struct field access
            (Value::Struct(type_name, fields), _) => {
                fields.get(field).cloned().ok_or_else(|| {
                    CompileError::macro_err(
                        format!("Struct '{}' has no field '{}'", type_name, field), span,
                    )
                })
            }
            // Module introspection
            (Value::Module(m), "name") => Ok(Value::String(m.name.clone())),
            (Value::Module(m), "items") => Ok(Value::Items(m.items.clone())),
            // Item introspection
            (Value::Item(item), "name") => Ok(Value::String(match item {
                Item::Function(f) => f.name.clone(),
                Item::Struct(s) => s.name.clone(),
                Item::Enum(e) => e.name.clone(),
                Item::TypeAlias(t) => t.name.clone(),
                Item::VarDecl(v) => v.name.clone(),
                _ => "<unnamed>".into(),
            })),
            (Value::Item(item), "kind") => Ok(Value::String(match item {
                Item::Function(_) => "Function",
                Item::Struct(_) => "Struct",
                Item::Enum(_) => "Enum",
                Item::TypeAlias(_) => "TypeAlias",
                Item::VarDecl(_) => "VarDecl",
                Item::Macro(_) => "Macro",
                Item::MacroCall(_) => "MacroCall",
                Item::Trait(_) => "Trait",
                Item::Impl(_) => "Impl",
            }.into())),
            _ => Err(CompileError::macro_err(format!("Unknown field '{}'", field), span)),
        }
    }

    /// Try to match a pattern against a value. Returns Some(bindings) on success, None on failure.
    fn match_pattern(
        &self,
        value: &Value,
        pattern: &Pattern,
    ) -> Result<Option<HashMap<String, Value>>, CompileError> {
        match &pattern.kind {
            PatternKind::Wildcard => Ok(Some(HashMap::new())),
            PatternKind::Variable { name, .. } => {
                let mut bindings = HashMap::new();
                bindings.insert(name.clone(), value.clone());
                Ok(Some(bindings))
            }
            PatternKind::EnumCtor { case, inner, .. } => {
                match value {
                    Value::Enum(val_case, val_payload) if val_case == case => {
                        match (inner, val_payload) {
                            (None, None) => Ok(Some(HashMap::new())),
                            (Some(inner_pat), Some(payload)) => {
                                self.match_pattern(payload, inner_pat)
                            }
                            (None, Some(_)) => Ok(None), // pattern expects no payload but value has one
                            (Some(_), None) => Ok(None), // pattern expects payload but value has none
                        }
                    }
                    _ => Ok(None),
                }
            }
            PatternKind::Literal(lit) => match (lit, value) {
                (LiteralPat::Int(a), Value::Int(b)) => Ok(if a == b { Some(HashMap::new()) } else { None }),
                (LiteralPat::Bool(a), Value::Bool(b)) => Ok(if a == b { Some(HashMap::new()) } else { None }),
                (LiteralPat::String(a), Value::String(b)) => Ok(if a == b { Some(HashMap::new()) } else { None }),
                (LiteralPat::Nil, Value::Nil) => Ok(Some(HashMap::new())),
                _ => Ok(None),
            },
            _ => Err(CompileError::macro_err(
                format!("Unsupported pattern: {:?}", pattern.kind), pattern.span,
            )),
        }
    }

    /// Call a function by name (user-defined or builtin)
    pub fn call_function(
        &mut self,
        name: &str,
        args: &[Value],
        span: Span,
    ) -> Result<Value, CompileError> {
        // Check for user-defined function first
        if let Some(overloads) = self.functions.get(name) {
            let mut candidates: Vec<Function> =
                overloads.iter().filter(|f| f.params.len() == args.len()).cloned().collect();
            if candidates.is_empty() {
                // No overload with this arity — report against the first one
                let func = overloads[0].clone();
                return self.call_user_function(&func, args, span);
            }
            if candidates.len() == 1 {
                let func = candidates.remove(0);
                return self.call_user_function(&func, args, span);
            }
            // Several overloads: dispatch on the runtime types of the arguments.
            // An exact type match outranks a trait match, so e.g.
            // inner(Matrix, HotVector) beats inner(Matrix, Matrix) for a HotVector.
            let mut best: Option<(u32, Function)> = None;
            for func in candidates {
                if let Some(score) = self.overload_score(&func, args) {
                    let beaten = best.as_ref().is_some_and(|(b, _)| *b >= score);
                    if !beaten {
                        best = Some((score, func));
                    }
                }
            }
            return match best {
                Some((_, func)) => self.call_user_function(&func, args, span),
                None => Err(CompileError::macro_err(
                    format!("No overload of '{}' matches the argument types", name), span,
                )),
            };
        }
        // Fall back to builtins
        self.call_builtin(name, args, span)
    }

    /// Rank an overload against runtime argument types: 2 per exact type
    /// match, 1 per trait match, 0 where either side has no nameable type.
    /// None means a definite mismatch — the overload is not viable.
    fn overload_score(&self, func: &Function, args: &[Value]) -> Option<u32> {
        let mut total = 0;
        for (param, arg) in func.params.iter().zip(args) {
            total += match (Self::type_name(&param.ty), Self::value_type_name(arg)) {
                (Some(p), Some(a)) if p == a => 2,
                (Some(p), Some(a))
                    if self.impls.get(a).is_some_and(|traits| traits.iter().any(|t| t == p)) => 1,
                (Some(_), Some(_)) => return None,
                _ => 0,
            };
        }
        Some(total)
    }

    /// Evaluate a user-defined function body with bound arguments
    fn call_user_function(
        &mut self,
        func: &Function,
        args: &[Value],
        span: Span,
    ) -> Result<Value, CompileError> {
        if func.params.len() != args.len() {
            return Err(CompileError::macro_err(
                format!("Function '{}' expects {} arguments, got {}", func.name, func.params.len(), args.len()),
                span,
            ));
        }
        let mut local_env: HashMap<String, Value> = HashMap::new();
        for (param, arg) in func.params.iter().zip(args.iter()) {
            local_env.insert(param.name.clone(), arg.clone());
        }
        let mut last = Value::Nil;
        for stmt in &func.body.stmts {
            match &stmt.kind {
                ExprKind::Return(Some(inner)) => {
                    return self.eval(inner, &mut local_env);
                }
                ExprKind::Return(None) => {
                    return Ok(Value::Nil);
                }
                _ => {
                    last = self.eval(stmt, &mut local_env)?;
                }
            }
        }
        Ok(last)
    }

    fn call_builtin(&mut self, name: &str, args: &[Value], span: Span) -> Result<Value, CompileError> {
        match name {
            // std/io
            "read_file" => {
                let path = self.arg_string(args, 0, span)?;
                Ok(Value::String(self.read_file(&path)?))
            }
            // std/compile/parse
            "parse" => {
                let source = self.arg_string(args, 0, span)?;
                Ok(Value::Module(self.parse_source(&source)?))
            }
            "splice" => match args.first() {
                Some(Value::Items(items)) => Ok(Value::Items(items.clone())),
                Some(Value::Item(item)) => Ok(Value::Items(vec![item.clone()])),
                // splice(list(item1, item2, ...)) — splice several items at once
                Some(Value::List(vals)) => {
                    let mut items = Vec::new();
                    for v in vals {
                        match v {
                            Value::Item(item) => items.push(item.clone()),
                            Value::Items(more) => items.extend(more.iter().cloned()),
                            _ => return Err(CompileError::macro_err("splice list may only contain items", span)),
                        }
                    }
                    Ok(Value::Items(items))
                }
                _ => Err(CompileError::macro_err("splice expects items or an item", span)),
            },
            "error" => {
                let msg = self.arg_string(args, 0, span).unwrap_or_else(|_| "error".into());
                Err(CompileError::macro_err(msg, span))
            },
            "filter_pub" => match args.first() {
                Some(Value::Items(items)) => {
                    let pub_items: Vec<Item> = items.iter().filter(|item| match item {
                        Item::Function(f) => f.is_pub,
                        Item::Struct(s) => s.is_pub,
                        Item::Enum(e) => e.is_pub,
                        Item::Macro(_) => true,
                        Item::Trait(_) => true,  // macros are always importable
                        Item::TypeAlias(_) => true,
                        Item::VarDecl(_) => true,
                        _ => false,
                    }).cloned().collect();
                    Ok(Value::Items(pub_items))
                }
                _ => Err(CompileError::macro_err("filter_pub expects items", span)),
            },
            // AST mutation
            "set_name" => {
                if args.len() != 2 {
                    return Err(CompileError::macro_err("set_name(item, name) takes 2 args", span));
                }
                let new_name = self.arg_string(args, 1, span)?;
                match &args[0] {
                    Value::Item(item) => {
                        let mut item = item.clone();
                        match &mut item {
                            Item::Function(ref mut f) => f.name = new_name,
                            Item::Struct(ref mut s) => s.name = new_name,
                            Item::Enum(ref mut e) => e.name = new_name,
                            Item::TypeAlias(ref mut t) => t.name = new_name,
                            Item::VarDecl(ref mut v) => v.name = new_name,
                            _ => {}
                        }
                        Ok(Value::Item(item))
                    }
                    _ => Err(CompileError::macro_err("set_name: first arg must be an item", span)),
                }
            },
            // Type introspection
            "lookup_type" => {
                let name = self.arg_string(args, 0, span)?;
                match self.known_structs.get(&name) {
                    Some(s) => Ok(Value::Item(Item::Struct(s.clone()))),
                    None => Err(CompileError::macro_err(format!("Type not found: '{}'", name), span)),
                }
            },
            "struct_fields" => match &args[0] {
                Value::Item(Item::Struct(s)) => {
                    let fields: Vec<Value> = s.fields.iter().map(|f| {
                        Value::List(vec![
                            Value::String(f.name.clone()),
                            Value::String(f.ty.to_string()),
                        ])
                    }).collect();
                    Ok(Value::List(fields))
                }
                _ => Err(CompileError::macro_err("struct_fields expects a struct", span)),
            },
            "type_exists" => {
                let name = self.arg_string(args, 0, span)?;
                Ok(Value::Bool(self.known_structs.contains_key(&name)))
            },

            // ─── AST construction ───
            "make_function" => {
                // make_function(name, params_list, return_type, body_block)
                let name = self.arg_string(args, 0, span)?;
                let ret_ty = self.arg_string(args, 2, span)?;
                let params = match args.get(1) {
                    Some(Value::List(list)) => list.iter().map(|v| {
                        if let Value::List(ref p) = v {
                            if let (Value::String(pname), Value::String(pty)) = (&p[0], &p[1]) {
                                Ok(Param { name: pname.clone(), ty: Type::path(pty), named: false, default: None, span: Span::zero() })
                            } else { Err(CompileError::macro_err("bad param", span)) }
                        } else { Err(CompileError::macro_err("bad param", span)) }
                    }).collect::<Result<Vec<_>, _>>()?,
                    _ => vec![],
                };
                let body = match args.get(3) {
                    Some(Value::Expr(e)) => {
                        if let ExprKind::Block(b) = &e.kind { Block { stmts: b.stmts.clone(), span: Span::zero() } }
                        else { Block { stmts: vec![e.clone()], span: Span::zero() } }
                    }
                    _ => Block { stmts: vec![], span: Span::zero() },
                };
                let func = Function { name, generics: vec![], params, return_type: Type::path(&ret_ty), body, is_pub: true, span: Span::zero() };
                Ok(Value::Item(Item::Function(func)))
            },
            "make_return" => {
                match args.first() {
                    Some(Value::Expr(e)) => Ok(Value::Expr(Expr::return_expr(Some(e.clone()), span))),
                    _ => Ok(Value::Expr(Expr::return_expr(None, span))),
                }
            },
            "make_binary" => {
                // make_binary("+", left_expr, right_expr)
                let op_str = self.arg_string(args, 0, span)?;
                let op = match op_str.as_str() { "+" => BinOp::Add, "-" => BinOp::Sub, "*" => BinOp::Mul, _ => BinOp::Add };
                match (&args.get(1), &args.get(2)) {
                    (Some(Value::Expr(l)), Some(Value::Expr(r))) => {
                        Ok(Value::Expr(Expr::binary(l.clone(), op, r.clone(), span)))
                    }
                    _ => Err(CompileError::macro_err("make_binary needs two exprs", span)),
                }
            },
            "make_string" => Ok(Value::Expr(Expr::string_literal(&self.arg_string(args, 0, span)?, span))),
            "make_field" => {
                // make_field(object_expr, "field_name")
                match (args.first(), self.arg_string(args, 1, span)) {
                    (Some(Value::Expr(obj)), Ok(field)) => {
                        Ok(Value::Expr(Expr::new(ExprKind::Field { object: Box::new(obj.clone()), field }, span)))
                    }
                    _ => Err(CompileError::macro_err("make_field(obj, 'name')", span)),
                }
            },
            "make_ident" => Ok(Value::Expr(Expr::ident(&self.arg_string(args, 0, span)?, span))),
            "list" => Ok(Value::List(args.to_vec())),
            "list_push" => match &args[0] {
                Value::List(items) => {
                    let mut new_list = items.clone();
                    new_list.push(args.get(1).cloned().unwrap_or(Value::Nil));
                    Ok(Value::List(new_list))
                }
                _ => Err(CompileError::macro_err("list_push needs a list", span)),
            },

            // ─── Literals ───
            "make_int" => match args.first() {
                Some(Value::Int(n)) => Ok(Value::Expr(Expr::int_literal(*n, span))),
                _ => Err(CompileError::macro_err("make_int(n) expects an integer", span)),
            },
            "make_float" => match args.first() {
                Some(Value::Float(n)) => Ok(Value::Expr(Expr::new(ExprKind::FloatLiteral(*n), span))),
                _ => Err(CompileError::macro_err("make_float(n)", span)),
            },
            "make_bool" => match args.first() {
                Some(Value::Bool(b)) => Ok(Value::Expr(Expr::new(ExprKind::BoolLiteral(*b), span))),
                _ => Err(CompileError::macro_err("make_bool(b)", span)),
            },

            // ─── Statements ───
            "make_block" => {
                let stmts: Vec<Expr> = match args.first() {
                    Some(Value::List(list)) => list.iter().filter_map(|v| match v {
                        Value::Expr(e) => Some(e.clone()),
                        _ => None,
                    }).collect(),
                    _ => args.iter().filter_map(|v| match v {
                        Value::Expr(e) => Some(e.clone()),
                        _ => None,
                    }).collect(),
                };
                Ok(Value::Expr(Expr::block(stmts, span)))
            },
            "make_let" => {
                let name = self.arg_string(args, 0, span)?;
                match args.get(1) {
                    Some(Value::Expr(val)) => Ok(Value::Expr(Expr::let_binding(&name, None, val.clone(), false, span))),
                    _ => Err(CompileError::macro_err("make_let(name, expr)", span)),
                }
            },

            // ─── Calls ───
            "make_call" => {
                let fname = self.arg_string(args, 0, span)?;
                let call_args: Vec<Expr> = match args.get(1) {
                    Some(Value::List(list)) => list.iter().filter_map(|v| match v {
                        Value::Expr(e) => Some(e.clone()),
                        _ => None,
                    }).collect(),
                    _ => args.iter().skip(1).filter_map(|v| match v {
                        Value::Expr(e) => Some(e.clone()),
                        _ => None,
                    }).collect(),
                };
                Ok(Value::Expr(Expr::call(Expr::ident(&fname, span), call_args, span)))
            },

            // ─── Type definitions ───
            "make_struct" => {
                let name = self.arg_string(args, 0, span)?;
                let fields = match args.get(1) {
                    Some(Value::List(list)) => list.iter().map(|v| {
                        if let Value::List(ref pair) = v {
                            if let (Value::String(fname), Value::String(ftype)) = (&pair[0], &pair[1]) {
                                Ok(StructField { name: fname.clone(), ty: Type::path(ftype), span: Span::zero() })
                            } else { Err(CompileError::macro_err("bad field", span)) }
                        } else { Err(CompileError::macro_err("bad field", span)) }
                    }).collect::<Result<Vec<_>, _>>()?,
                    _ => vec![],
                };
                Ok(Value::Item(Item::Struct(Struct { name, generics: vec![], fields, is_pub: true, span: Span::zero() })))
            },

            "make_impl" => {
                // make_impl(trait_name, target_type, [method1, method2, ...])
                let trait_name = self.arg_string(args, 0, span)?;
                let target = self.arg_string(args, 1, span)?;
                let methods: Vec<Function> = match args.get(2) {
                    Some(Value::List(list)) => list.iter().filter_map(|v| {
                        if let Value::Item(Item::Function(ref f)) = v { Some(f.clone()) } else { None }
                    }).collect(),
                    _ => vec![],
                };
                Ok(Value::Item(Item::Impl(ImplBlock {
                    trait_name,
                    generics: vec![],
                    target_type: Type::path(&target),
                    methods,
                    span: Span::zero(),
                })))
            },

            "to_string" => {
                // Primitive to_string for generated code
                let inner = match args.first() {
                    Some(Value::Expr(e)) => e.clone(),
                    _ => return Err(CompileError::macro_err("to_string needs an expr", span)),
                };
                // Generate a call to std::to_string in the output C++
                // We represent this as a function call that the codegen maps
                Ok(Value::Expr(Expr::new(
                    ExprKind::Call { func: Box::new(Expr::ident("std::to_string", span)), args: vec![inner] },
                    span
                )))
            },

            // ─── Runtime builtins (available in interpreter mode) ───
            "print" if self.runtime_mode => {
                match args.first() {
                    Some(Value::String(s)) => print!("{}", s),
                    Some(Value::Int(n)) => print!("{}", n),
                    Some(Value::Float(n)) => print!("{}", n),
                    Some(Value::Bool(b)) => print!("{}", b),
                    Some(v) => print!("{:?}", v),
                    None => {}
                }
                Ok(Value::Nil)
            }
            "println" if self.runtime_mode => {
                match args.first() {
                    Some(Value::String(s)) => println!("{}", s),
                    Some(Value::Int(n)) => println!("{}", n),
                    Some(Value::Float(n)) => println!("{}", n),
                    Some(Value::Bool(b)) => println!("{}", b),
                    Some(v) => println!("{:?}", v),
                    None => println!(),
                }
                Ok(Value::Nil)
            }
            "print_int" if self.runtime_mode => {
                match args.first() {
                    Some(Value::Int(n)) => { print!("{}", n); Ok(Value::Nil) }
                    _ => Err(CompileError::macro_err("print_int expects an integer", span)),
                }
            }
            "std::to_string" if self.runtime_mode => {
                // Used by compile-time macros for codegen; in interpreter just render the value
                match args.first() {
                    Some(Value::Int(n)) => Ok(Value::String(n.to_string())),
                    // Match C++ std::to_string(double): fixed six decimal places
                    Some(Value::Float(n)) => Ok(Value::String(format!("{:.6}", n))),
                    Some(Value::String(s)) => Ok(Value::String(s.clone())),
                    Some(Value::Bool(b)) => Ok(Value::String(b.to_string())),
                    _ => Ok(Value::String("<unknown>".into())),
                }
            }

            _ => Err(CompileError::macro_err(format!("Unknown: '{}'", name), span)),
        }
    }

    fn arg_string(&self, args: &[Value], idx: usize, span: Span) -> Result<String, CompileError> {
        match args.get(idx) {
            Some(Value::String(s)) => Ok(s.clone()),
            _ => Err(CompileError::macro_err("Expected string argument", span)),
        }
    }

    fn read_file(&self, path: &str) -> Result<String, CompileError> {
        // "std.math.nv" and "std/math.nv" both refer to std/math.nv
        let dotted = path.strip_suffix(".nv")
            .map(|stem| format!("{}.nv", stem.replace('.', "/")));
        let candidates: Vec<&str> = std::iter::once(path)
            .chain(dotted.as_deref())
            .collect();
        for sp in &self.search_paths {
            for cand in &candidates {
                let full = sp.join(cand);
                if full.exists() {
                    return fs::read_to_string(&full).map_err(|e| {
                        CompileError::Generic(format!("Cannot read '{}': {}", cand, e))
                    });
                }
            }
        }
        Err(CompileError::macro_err(format!("File not found: '{}'", path), Span::zero()))
    }

    fn parse_source(&self, source: &str) -> Result<Module, CompileError> {
        let mut lex = crate::lexer::Lexer::new(source);
        let tokens = lex.tokenize()?;
        let mut parser = crate::parser::Parser::new(tokens, source);
        parser.parse_module("imported".to_string())
    }
}
