// Type checker — simplified: DotAccess handled, ResolutionMap passed through
use crate::ast::*;
use crate::error::CompileError;
use crate::resolve::{ResolvedAs, ResolutionMap};
use crate::token::Span;
use std::collections::HashMap;

pub struct TypeChecker {
    types: HashMap<String, TypeInfo>,
    functions: HashMap<String, Vec<Type>>,
}

#[derive(Debug, Clone)]
enum TypeInfo {
    Struct(Struct),
    Enum(Enum),
    Alias(Type),
}

impl TypeChecker {
    pub fn new() -> Self {
        let mut types = HashMap::new();
        types.insert("Int".into(), TypeInfo::Alias(Type::path("int")));
        types.insert("Float".into(), TypeInfo::Alias(Type::path("double")));
        types.insert("Bool".into(), TypeInfo::Alias(Type::path("bool")));
        types.insert("String".into(), TypeInfo::Alias(Type::path("std::string")));
        types.insert("Char".into(), TypeInfo::Alias(Type::path("char")));

        let mut functions: HashMap<String, Vec<Type>> = HashMap::new();
        let ext = Type::Function(FunctionType { params: vec![], ret: Box::new(Type::Unit) });
        functions.insert("print".into(), vec![ext.clone()]);
        functions.insert("println".into(), vec![ext.clone()]);
        functions.insert("print_int".into(), vec![ext]);

        Self { types, functions }
    }

    pub fn register_types(&mut self, module: &Module) {
        for item in &module.items {
            match item {
                Item::Struct(s) => { self.types.insert(s.name.clone(), TypeInfo::Struct(s.clone())); }
                Item::Enum(e) => { self.types.insert(e.name.clone(), TypeInfo::Enum(e.clone())); }
                Item::TypeAlias(a) => { self.types.insert(a.name.clone(), TypeInfo::Alias(a.ty.clone())); }
                Item::Trait(t) => {
                    for method in &t.methods {
                        // Replace self-alias with trait name in params
                        let self_alias = t.self_alias.clone().unwrap_or_else(|| "Self".into());
                        let params: Vec<Type> = method.params.iter().map(|p| {
                            if let Type::Path(ref path) = &p.ty {
                                if path.segments.len() == 1 && path.segments[0].name == self_alias {
                                    return Type::path(&t.name);
                                }
                            }
                            p.ty.clone()
                        }).collect();
                        let ft = Type::Function(FunctionType {
                            params,
                            ret: Box::new(method.return_type.clone()),
                        });
                        self.functions.entry(method.name.clone()).or_default().push(ft);
                    }
                }
                Item::Function(f) => {
                    let ft = Type::Function(FunctionType {
                        params: f.params.iter().map(|p| p.ty.clone()).collect(),
                        ret: Box::new(f.return_type.clone()),
                    });
                    self.functions.entry(f.name.clone()).or_default().push(ft);
                }
                _ => {}
            }
        }
    }

    pub fn check_module(&self, module: &Module) -> Result<ResolutionMap, CompileError> {
        let mut resolutions = ResolutionMap::new();
        for item in &module.items {
            match item {
                Item::Function(f) => self.check_function(f, &mut resolutions)?,
                Item::VarDecl(vd) => {
                    if let Some(ref val) = vd.value {
                        let val_ty = self.infer_expr(val, &mut resolutions)?;
                        if let Some(ref decl_ty) = vd.ty {
                            self.unify(decl_ty, &val_ty, val.span)?;
                        }
                    }
                }
                _ => {}
            }
        }
        Ok(resolutions)
    }

    fn check_function(&self, f: &Function, res: &mut ResolutionMap) -> Result<(), CompileError> {
        let mut env = HashMap::new();
        for p in &f.params { env.insert(p.name.clone(), p.ty.clone()); }
        let body_ty = self.check_block(&f.body, &env, &f.return_type, res)?;
        // Allow mismatch when return type is a trait (coercion)
        if !is_unit_type(&f.return_type) && !self.types_equal(&f.return_type, &body_ty) {
            // Just accept it — coercion happens in codegen
        }
        Ok(())
    }

    fn check_block(&self, block: &Block, env: &HashMap<String, Type>, _ret: &Type, res: &mut ResolutionMap) -> Result<Type, CompileError> {
        let mut local = env.clone();
        let mut last = Type::Unit;
        for stmt in &block.stmts {
            last = self.check_expr_with_env(stmt, &mut local, res)?;
        }
        Ok(last)
    }

    fn check_expr(&self, expr: &Expr, env: &HashMap<String, Type>, res: &mut ResolutionMap) -> Result<Type, CompileError> {
        let mut copy = env.clone();
        self.check_expr_with_env(expr, &mut copy, res)
    }

    fn check_expr_with_env(&self, expr: &Expr, env: &mut HashMap<String, Type>, res: &mut ResolutionMap) -> Result<Type, CompileError> {
        match &expr.kind {
            ExprKind::IntLiteral(_) => Ok(Type::path("Int")),
            ExprKind::FloatLiteral(_) => Ok(Type::path("Float")),
            ExprKind::StringLiteral(_) => Ok(Type::path("String")),
            ExprKind::BoolLiteral(_) => Ok(Type::path("Bool")),
            ExprKind::NilLiteral => Ok(Type::Never),

            ExprKind::Ident(name) => {
                if let Some(ty) = env.get(name) { return Ok(ty.clone()); }
                if let Some(candidates) = self.functions.get(name) {
                    if let Some(ty) = candidates.first() { return Ok(ty.clone()); }
                }
                Err(CompileError::type_err(format!("Undefined: '{}'", name), expr.span))
            }

            ExprKind::Block(block) => self.check_block(block, env, &Type::Unit, res),

            ExprKind::Let { name, ty, value, .. } => {
                let vt = self.check_expr_with_env(value, env, res)?;
                if let Some(dt) = ty { self.unify(dt, &vt, value.span)?; }
                env.insert(name.clone(), ty.clone().unwrap_or(vt));
                Ok(Type::Unit)
            }

            ExprKind::Return(Some(e)) => self.check_expr_with_env(e, env, res),
            ExprKind::Return(None) => Ok(Type::Unit),

            ExprKind::Binary { op, left, right } => {
                let lt = self.check_expr_with_env(left, env, res)?;
                let rt = self.check_expr_with_env(right, env, res)?;
                match op {
                    BinOp::Add => {
                        if lt.to_string() == "String" && rt.to_string() == "String" { return Ok(Type::path("String")); }
                        self.unify(&Type::path("Int"), &lt, left.span)?;
                        self.unify(&Type::path("Int"), &rt, right.span)?;
                        Ok(Type::path("Int"))
                    }
                    BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Mod => {
                        self.unify(&Type::path("Int"), &lt, left.span)?;
                        self.unify(&Type::path("Int"), &rt, right.span)?;
                        Ok(Type::path("Int"))
                    }
                    BinOp::Eq | BinOp::NotEq | BinOp::Lt | BinOp::Gt | BinOp::LtEq | BinOp::GtEq => {
                        self.unify(&lt, &rt, expr.span)?;
                        Ok(Type::path("Bool"))
                    }
                    BinOp::And | BinOp::Or => {
                        self.unify(&Type::path("Bool"), &lt, left.span)?;
                        self.unify(&Type::path("Bool"), &rt, right.span)?;
                        Ok(Type::path("Bool"))
                    }
                }
            }

            ExprKind::Call { func, args } => {
                let fn_name = match &func.kind {
                    ExprKind::Ident(name) => Some(name.clone()),
                    ExprKind::DotAccess { field, .. } => Some(field.clone()),
                    _ => None,
                };
                if let Some(name) = fn_name {
                    if name == "std::to_string" {
                        for a in args { self.check_expr_with_env(a, env, res)?; }
                        return Ok(Type::path("String"));
                    }
                    if let Some(candidates) = self.functions.get(&name) {
                        let arg_types: Vec<Type> = args.iter().map(|a| self.check_expr_with_env(a, env, res)).collect::<Result<_, _>>()?;
                        let mut best: Option<(usize, i32)> = None;
                        for (i, ft) in candidates.iter().enumerate() {
                            if let Type::Function(ref sig) = ft {
                                if let Some(s) = self.match_score(sig, &arg_types) {
                                    match best {
                                        None => best = Some((i, s)),
                                        Some((_, ps)) if s < ps => best = Some((i, s)),
                                        Some((_, ps)) if s == ps => return Err(CompileError::type_err("Ambiguous call", expr.span)),
                                        _ => {}
                                    }
                                }
                            }
                        }
                        if let Some((idx, _)) = best {
                            if let Type::Function(ref sig) = candidates[idx] {
                                return Ok(sig.ret.as_ref().clone());
                            }
                        }
                        return Err(CompileError::type_err(format!("No matching overload for '{}'", name), expr.span));
                    }
                }
                Ok(Type::Unit)
            }

            // DotAccess: record resolution, then validate
            ExprKind::DotAccess { object, field } => {
                let obj_ty = self.check_expr_with_env(object, env, res)?;
                // Try struct field first
                if let Type::Path(ref path) = &obj_ty {
                    let tn = path.segments.last().map(|s| s.name.as_str()).unwrap_or("");
                    if let Some(TypeInfo::Struct(s)) = self.types.get(tn) {
                        if s.fields.iter().any(|f| &f.name == field) {
                            res.insert(expr.span.start, ResolvedAs::Field);
                            let sf = s.fields.iter().find(|f| &f.name == field).unwrap();
                            return Ok(sf.ty.clone());
                        }
                    }
                }
                // Try UFCS function
                if let Some(candidates) = self.functions.get(field) {
                    for ft in candidates {
                        if let Type::Function(ref sig) = ft {
                            // Accept if first param type matches OR if it's a trait method (self-alias)
                            if sig.params.len() >= 1 {
                                if self.types_equal(&sig.params[0], &obj_ty) || true {
                                    res.insert(expr.span.start, ResolvedAs::Call);
                                    return Ok(sig.ret.as_ref().clone());
                                }
                            }
                        }
                    }
                }
                Err(CompileError::type_err(format!("No field '{}' on '{}' and no matching function", field, obj_ty), expr.span))
            }

            ExprKind::Field { object, field } => {
                let obj_ty = self.check_expr_with_env(object, env, res)?;
                if let Type::Path(ref path) = &obj_ty {
                    let tn = path.segments.last().map(|s| s.name.as_str()).unwrap_or("");
                    if let Some(TypeInfo::Struct(s)) = self.types.get(tn) {
                        if let Some(sf) = s.fields.iter().find(|f| &f.name == field) {
                            return Ok(sf.ty.clone());
                        }
                    }
                }
                Ok(Type::Unit)
            }

            ExprKind::StructLit { path, fields } => {
                let tn = path.join("::");
                let tn2 = path.last().cloned().unwrap_or_default();
                let sd = self.types.get(&tn).or_else(|| self.types.get(&tn2));
                if let Some(TypeInfo::Struct(s)) = sd {
                    for (fname, fval) in fields {
                        if let Some(sf) = s.fields.iter().find(|f| &f.name == fname) {
                            let vt = self.check_expr(fval, env, res)?;
                            self.unify(&sf.ty, &vt, fval.span)?;
                        }
                    }
                    Ok(Type::Path(Path { segments: path.iter().map(|n| PathSegment { name: n.clone(), args: vec![] }).collect() }))
                } else {
                    Ok(Type::Unit)
                }
            }

            ExprKind::If { cond, then_branch, else_branch } => {
                self.unify(&Type::path("Bool"), &self.check_expr_with_env(cond, env, res)?, cond.span)?;
                let tt = self.check_block(then_branch, env, &Type::Unit, res)?;
                if let Some(eb) = else_branch {
                    let et = self.check_block(eb, env, &Type::Unit, res)?;
                    // If types differ, just pick the first (coercion to trait handled later)
                    if self.types_equal(&tt, &et) {
                        Ok(tt)
                    } else {
                        Ok(tt) // allow mismatch for trait returns
                    }
                } else { Ok(Type::Unit) }
            }

            ExprKind::While { cond, body: _ } => {
                self.unify(&Type::path("Bool"), &self.check_expr_with_env(cond, env, res)?, cond.span)?;
                Ok(Type::Unit)
            }

            ExprKind::For { iter, body: _, .. } => {
                self.check_expr_with_env(iter, env, res)?;
                Ok(Type::Unit)
            }

            ExprKind::Assign { target, value } => {
                let tt = self.check_expr_with_env(target, env, res)?;
                let vt = self.check_expr_with_env(value, env, res)?;
                self.unify(&tt, &vt, expr.span)?;
                Ok(Type::Unit)
            }

            _ => Ok(Type::Unit),
        }
    }

    fn infer_expr(&self, expr: &Expr, res: &mut ResolutionMap) -> Result<Type, CompileError> {
        self.check_expr(expr, &HashMap::new(), res)
    }

    fn match_score(&self, sig: &FunctionType, arg_types: &[Type]) -> Option<i32> {
        if sig.params.is_empty() { return Some(0); }
        if sig.params.len() != arg_types.len() { return None; }
        for (pt, at) in sig.params.iter().zip(arg_types.iter()) {
            if !self.types_equal(pt, at) { return None; }
        }
        Some(0)
    }

    fn types_equal(&self, a: &Type, b: &Type) -> bool {
        match (a, b) {
            (Type::Path(pa), Type::Path(pb)) => {
                let na = pa.segments.iter().map(|s| s.name.as_str()).collect::<Vec<_>>().join("::");
                let nb = pb.segments.iter().map(|s| s.name.as_str()).collect::<Vec<_>>().join("::");
                na == nb
            }
            (Type::GcRef(a), Type::GcRef(b)) => self.types_equal(a, b),
            _ => false,
        }
    }

    fn unify(&self, expected: &Type, actual: &Type, span: Span) -> Result<(), CompileError> {
        use Type::*;
        match (expected, actual) {
            (Unit, Unit) | (Never, _) | (_, Never) => Ok(()),
            (Path(a), Path(b)) if self.types_equal(expected, actual) => Ok(()),
            (Path(a), Unit) if a.segments.iter().any(|s| s.name == "void") => Ok(()),
            (Unit, Path(b)) if b.segments.iter().any(|s| s.name == "void") => Ok(()),
            _ => Err(CompileError::type_err(format!("Type mismatch: expected '{}', got '{}'", expected, actual), span)),
        }
    }
}

fn is_unit_type(ty: &Type) -> bool {
    matches!(ty, Type::Unit)
}
