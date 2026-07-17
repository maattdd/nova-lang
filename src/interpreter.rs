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
    Nil,
}

pub struct Interpreter {
    search_paths: Vec<PathBuf>,
    known_structs: HashMap<String, Struct>,
}

impl Interpreter {
    pub fn new(search_paths: Vec<PathBuf>) -> Self {
        Self { search_paths, known_structs: HashMap::new() }
    }

    pub fn register_structs(&mut self, module: &Module) {
        for item in &module.items {
            if let Item::Struct(s) = item {
                self.known_structs.insert(s.name.clone(), s.clone());
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
                        (Value::String(a), Value::String(b)) => Ok(Value::String(format!("{}{}", a, b))),
                        _ => Err(CompileError::macro_err("Cannot add these types", expr.span)),
                    },
                    BinOp::Sub => match (&l, &r) {
                        (Value::Int(a), Value::Int(b)) => Ok(Value::Int(a - b)),
                        _ => Err(CompileError::macro_err("Cannot subtract", expr.span)),
                    },
                    BinOp::Mul => match (&l, &r) {
                        (Value::Int(a), Value::Int(b)) => Ok(Value::Int(a * b)),
                        _ => Err(CompileError::macro_err("Cannot multiply", expr.span)),
                    },
                    BinOp::Eq => Ok(Value::Bool(self.equal(&l, &r))),
                    BinOp::NotEq => Ok(Value::Bool(!self.equal(&l, &r))),
                    _ => Err(CompileError::macro_err("Unsupported operator", expr.span)),
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
                self.call_builtin(&name, &evaled, expr.span)
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

            _ => Err(CompileError::macro_err(
                format!("Unsupported: {:?}", expr.kind), expr.span,
            )),
        }
    }

    fn equal(&self, a: &Value, b: &Value) -> bool {
        match (a, b) {
            (Value::Int(a), Value::Int(b)) => a == b,
            (Value::String(a), Value::String(b)) => a == b,
            (Value::Bool(a), Value::Bool(b)) => a == b,
            (Value::Nil, Value::Nil) => true,
            _ => false,
        }
    }

    fn access_field(&self, obj: &Value, field: &str, span: Span) -> Result<Value, CompileError> {
        match (obj, field) {
            (Value::Module(m), "name") => Ok(Value::String(m.name.clone())),
            (Value::Module(m), "items") => Ok(Value::Items(m.items.clone())),
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
                        Item::Macro(_) => true,  // macros are always importable
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
        for sp in &self.search_paths {
            let full = sp.join(path);
            if full.exists() {
                return fs::read_to_string(&full).map_err(|e| {
                    CompileError::Generic(format!("Cannot read '{}': {}", path, e))
                });
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
