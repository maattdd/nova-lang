//! Monomorphization pass.
//!
//! Nova's codegen cannot emit generic functions/structs/enums itself, so this
//! pass lowers every first-order generic item into concrete, non-generic items.
//! It:
//!   1. infers the concrete type arguments at each generic use site,
//!   2. specializes the generic item under that substitution,
//!   3. rewrites the whole module so codegen never sees a type parameter.
//!
//! Types are kept "unmangled" (with their arguments, e.g. `Option[Int]`)
//! during inference so that unification is structural, then `emit_type`
//! rewrites them to their mangled C++ name (`Option__int`) when a node is
//! written back into the AST.

use crate::ast::*;
use crate::error::CompileError;
use crate::token::Span;
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};

/// Replace single-segment, argument-less paths named by `map` with their
/// concrete target type, recursing into composite types.
pub fn substitute_type(ty: &Type, map: &HashMap<String, Type>) -> Type {
    match ty {
        Type::Path(p) => {
            if p.segments.len() == 1 && p.segments[0].args.is_empty() {
                if let Some(t) = map.get(&p.segments[0].name) {
                    return t.clone();
                }
            }
            Type::Path(Path {
                segments: p
                    .segments
                    .iter()
                    .map(|s| PathSegment {
                        name: s.name.clone(),
                        args: s.args.iter().map(|a| substitute_type(a, map)).collect(),
                    })
                    .collect(),
            })
        }
        Type::GcRef(inner) => Type::GcRef(Box::new(substitute_type(inner, map))),
        Type::Function(ft) => Type::Function(FunctionType {
            params: ft.params.iter().map(|p| substitute_type(p, map)).collect(),
            ret: Box::new(substitute_type(&ft.ret, map)),
        }),
        Type::Tuple(ts) => Type::Tuple(ts.iter().map(|t| substitute_type(t, map)).collect()),
        Type::Unit => Type::Unit,
        Type::Never => Type::Never,
    }
}

fn subst_map(generics: &[GenericParam], args: &[Type]) -> HashMap<String, Type> {
    generics
        .iter()
        .zip(args)
        .map(|(g, a)| (g.name.clone(), a.clone()))
        .collect()
}

fn mangle_ident(name: &str) -> String {
    match name {
        "Int" => "int".into(),
        "Float" => "double".into(),
        "Bool" => "bool".into(),
        "String" => "string".into(),
        "Char" => "char".into(),
        _ => name.to_string(),
    }
}

/// A stable C++-safe identifier for a type (used only in mangled names).
fn type_mangle(ty: &Type) -> String {
    match ty {
        Type::Path(p) => {
            let mut out = String::new();
            if let Some(last) = p.segments.last() {
                out.push_str(&mangle_ident(&last.name));
                for a in &last.args {
                    out.push_str("__");
                    out.push_str(&type_mangle(a));
                }
            }
            out
        }
        Type::GcRef(inner) => format!("gc__{}", type_mangle(inner)),
        Type::Function(_) => "fn".into(),
        Type::Tuple(_) => "tuple".into(),
        Type::Unit => "unit".into(),
        Type::Never => "never".into(),
    }
}

fn mangle(name: &str, args: &[Type]) -> String {
    if args.is_empty() {
        name.to_string()
    } else {
        format!(
            "{}__{}",
            name,
            args.iter().map(type_mangle).collect::<Vec<_>>().join("__")
        )
    }
}

fn var_name(ty: &Type, vars: &HashSet<String>) -> Option<String> {
    if let Type::Path(p) = ty {
        if p.segments.len() == 1 && p.segments[0].args.is_empty() {
            let n = &p.segments[0].name;
            if vars.contains(n) {
                return Some(n.clone());
            }
        }
    }
    None
}

fn path_key(path: &Path) -> String {
    path.segments
        .iter()
        .map(|s| s.name.as_str())
        .collect::<Vec<_>>()
        .join("::")
}

pub struct Mono {
    structs: HashMap<String, Struct>,
    enums: HashMap<String, Enum>,
    // Declaration order of enums (deterministic case-name resolution).
    enum_order: Vec<String>,
    functions: HashMap<String, Vec<Function>>,
    aliases: HashMap<String, TypeAlias>,

    // (name, concrete args) -> mangled name, per kind
    struct_names: HashMap<(String, Vec<Type>), String>,
    enum_names: HashMap<(String, Vec<Type>), String>,
    fn_names: HashMap<(String, Vec<Type>), String>,

    // Concrete (non-generic) definitions to emit.
    out_structs: BTreeMap<String, Struct>,
    out_enums: BTreeMap<String, Enum>,
    out_functions: Vec<Function>,
    out_traits: Vec<TraitDef>,
    out_impls: Vec<ImplBlock>,
    out_vars: Vec<VarDecl>,
    out_misc: Vec<Item>,

    // Function bodies pending specialization: (name, args, overload index).
    fn_queue: VecDeque<(String, Vec<Type>, usize)>,

    // Return type of the function currently being lowered (for `return`).
    cur_ret: Option<Type>,
}

impl Mono {
    fn new() -> Self {
        Self {
            structs: HashMap::new(),
            enums: HashMap::new(),
            enum_order: Vec::new(),
            functions: HashMap::new(),
            aliases: HashMap::new(),
            struct_names: HashMap::new(),
            enum_names: HashMap::new(),
            fn_names: HashMap::new(),
            out_structs: BTreeMap::new(),
            out_enums: BTreeMap::new(),
            out_functions: Vec::new(),
            out_traits: Vec::new(),
            out_impls: Vec::new(),
            out_vars: Vec::new(),
            out_misc: Vec::new(),
            fn_queue: VecDeque::new(),
            cur_ret: None,
        }
    }

    /// Lower a module to its monomorphic equivalent.
    pub fn lower(module: &Module) -> Result<Module, CompileError> {
        let mut m = Mono::new();
        m.collect_tables(module);
        m.seed(module)?;
        m.process_queue()?;
        Ok(m.build_module(module))
    }

    fn collect_tables(&mut self, module: &Module) {
        for item in &module.items {
            match item {
                Item::Struct(s) => {
                    self.structs.insert(s.name.clone(), s.clone());
                }
                Item::Enum(e) => {
                    self.enums.insert(e.name.clone(), e.clone());
                    self.enum_order.push(e.name.clone());
                }
                Item::Function(f) => {
                    self.functions.entry(f.name.clone()).or_default().push(f.clone());
                }
                Item::TypeAlias(a) => {
                    self.aliases.insert(a.name.clone(), a.clone());
                }
                _ => {}
            }
        }
    }

    /// Seed instantiation of every non-generic item and lower items that are
    /// not functions (var decls, impls, traits).
    fn seed(&mut self, module: &Module) -> Result<(), CompileError> {
        for item in &module.items {
            match item {
                Item::Struct(s) if s.generics.is_empty() => {
                    self.instantiate_struct(&s.name, &[])?;
                }
                Item::Enum(e) if e.generics.is_empty() => {
                    self.instantiate_enum(&e.name, &[])?;
                }
                Item::Function(f) if f.generics.is_empty() => {
                    self.instantiate_function(&f.name, &[])?;
                }
                Item::VarDecl(vd) => {
                    let ann = match &vd.ty {
                        Some(t) => Some(self.infer_type(t, &HashMap::new())?),
                        None => None,
                    };
                    let mut env = HashMap::new();
                    let value = match &vd.value {
                        Some(v) => {
                            let (v2, _) = self.lower_expr(v, &mut env, &HashMap::new(), ann.as_ref())?;
                            Some(v2)
                        }
                        None => None,
                    };
                    let ann_emitted = ann.as_ref().map(|t| self.emit_type(t));
                    self.out_vars.push(VarDecl {
                        name: vd.name.clone(),
                        ty: ann_emitted,
                        value,
                        is_mut: vd.is_mut,
                        span: vd.span,
                    });
                }
                Item::Trait(t) => {
                    let methods = t
                        .methods
                        .iter()
                        .map(|m| self.lower_signature(m))
                        .collect::<Result<Vec<_>, _>>()?;
                    self.out_traits.push(TraitDef {
                        name: t.name.clone(),
                        generics: vec![],
                        self_alias: t.self_alias.clone(),
                        methods,
                        span: t.span,
                    });
                }
                Item::Impl(imp) => {
                    let target = self.infer_type(&imp.target_type, &HashMap::new())?;
                    let target_emitted = self.emit_type(&target);
                    let methods = imp
                        .methods
                        .iter()
                        .map(|m| self.lower_function(m, &[], &m.name))
                        .collect::<Result<Vec<_>, _>>()?;
                    self.out_impls.push(ImplBlock {
                        trait_name: imp.trait_name.clone(),
                        generics: vec![],
                        target_type: target_emitted,
                        methods,
                        span: imp.span,
                    });
                }
                Item::Macro(_) | Item::MacroCall(_) => {
                    self.out_misc.push(item.clone());
                }
                _ => {}
            }
        }
        Ok(())
    }

    /// Lower only the signature (params + return type) of a trait method.
    fn lower_signature(&mut self, f: &Function) -> Result<Function, CompileError> {
        let params = f
            .params
            .iter()
            .map(|p| {
                let inferred = self.infer_type(&p.ty, &HashMap::new())?;
                Ok(Param {
                    name: p.name.clone(),
                    ty: self.emit_type(&inferred),
                    named: p.named,
                    default: None,
                    span: p.span,
                })
            })
            .collect::<Result<Vec<_>, CompileError>>()?;
        let ret = self.infer_type(&f.return_type, &HashMap::new())?;
        Ok(Function {
            name: f.name.clone(),
            generics: vec![],
            params,
            return_type: self.emit_type(&ret),
            body: Block { stmts: vec![], span: f.span },
            is_pub: f.is_pub,
            span: f.span,
        })
    }

    fn process_queue(&mut self) -> Result<(), CompileError> {
        while let Some((name, args, idx)) = self.fn_queue.pop_front() {
            let f = self
                .functions
                .get(&name)
                .and_then(|ovs| ovs.get(idx).cloned())
                .ok_or_else(|| CompileError::Generic(format!("unknown function '{}'", name)))?;
            let mangled = self
                .fn_names
                .get(&(name.clone(), args.clone()))
                .cloned()
                .unwrap_or_else(|| mangle(&name, &args));
            let lowered = self.lower_function(&f, &args, &mangled)?;
            self.out_functions.push(lowered);
        }
        Ok(())
    }

    fn build_module(&self, module: &Module) -> Module {
        let mut items = Vec::new();
        for s in self.out_structs.values() {
            items.push(Item::Struct(s.clone()));
        }
        for e in self.out_enums.values() {
            items.push(Item::Enum(e.clone()));
        }
        for t in &self.out_traits {
            items.push(Item::Trait(t.clone()));
        }
        for i in &self.out_impls {
            items.push(Item::Impl(i.clone()));
        }
        for f in &self.out_functions {
            items.push(Item::Function(f.clone()));
        }
        for v in &self.out_vars {
            items.push(Item::VarDecl(v.clone()));
        }
        items.extend(self.out_misc.iter().cloned());
        Module {
            name: module.name.clone(),
            imports: module.imports.clone(),
            items,
        }
    }

    // ─── Instantiation ────────────────────────────────────────────────────────

    fn instantiate_struct(&mut self, name: &str, args: &[Type]) -> Result<String, CompileError> {
        let key = (name.to_string(), args.to_vec());
        if let Some(m) = self.struct_names.get(&key) {
            return Ok(m.clone());
        }
        let s = self
            .structs
            .get(name)
            .cloned()
            .ok_or_else(|| CompileError::Generic(format!("unknown struct '{}'", name)))?;
        let mangled = mangle(name, args);
        self.struct_names.insert(key.clone(), mangled.clone());

        let s_subst = subst_map(&s.generics, args);
        let mut fields = Vec::new();
        for f in &s.fields {
            let inferred = self.infer_type(&substitute_type(&f.ty, &s_subst), &HashMap::new())?;
            fields.push(StructField {
                name: f.name.clone(),
                ty: self.emit_type(&inferred),
                span: f.span,
            });
        }
        self.out_structs.insert(
            mangled.clone(),
            Struct {
                name: mangled.clone(),
                generics: vec![],
                fields,
                is_pub: s.is_pub,
                span: s.span,
            },
        );
        Ok(mangled)
    }

    fn instantiate_enum(&mut self, name: &str, args: &[Type]) -> Result<String, CompileError> {
        let key = (name.to_string(), args.to_vec());
        if let Some(m) = self.enum_names.get(&key) {
            return Ok(m.clone());
        }
        let e = self
            .enums
            .get(name)
            .cloned()
            .ok_or_else(|| CompileError::Generic(format!("unknown enum '{}'", name)))?;
        let mangled = mangle(name, args);
        self.enum_names.insert(key.clone(), mangled.clone());

        let e_subst = subst_map(&e.generics, args);
        let mut cases = Vec::new();
        for c in &e.cases {
            let payload = match &c.payload {
                Some(pt) => {
                    let inferred = self.infer_type(&substitute_type(pt, &e_subst), &HashMap::new())?;
                    Some(self.emit_type(&inferred))
                }
                None => None,
            };
            cases.push(EnumCase {
                name: c.name.clone(),
                payload,
                span: c.span,
            });
        }
        self.out_enums.insert(
            mangled.clone(),
            Enum {
                name: mangled.clone(),
                generics: vec![],
                cases,
                is_pub: e.is_pub,
                span: e.span,
            },
        );
        Ok(mangled)
    }

    fn instantiate_function(&mut self, name: &str, args: &[Type]) -> Result<String, CompileError> {
        let key = (name.to_string(), args.to_vec());
        if let Some(m) = self.fn_names.get(&key) {
            return Ok(m.clone());
        }
        let ovs = self
            .functions
            .get(name)
            .ok_or_else(|| CompileError::Generic(format!("unknown function '{}'", name)))?;
        let mangled = mangle(name, args);
        self.fn_names.insert(key.clone(), mangled.clone());

        if args.is_empty() {
            // Non-generic overloads share their C++ name (C++ overload resolution).
            for (i, f) in ovs.iter().enumerate() {
                if f.generics.is_empty() {
                    self.fn_queue.push_back((name.to_string(), args.to_vec(), i));
                }
            }
        } else if let Some(i) = ovs.iter().position(|f| f.generics.len() == args.len()) {
            self.fn_queue.push_back((name.to_string(), args.to_vec(), i));
        }
        Ok(mangled)
    }

    // ─── Types ────────────────────────────────────────────────────────────────

    /// Infer a concrete type with arguments preserved (`Option[Int]` stays
    /// `Option[Int]`). Also instantiates any generic struct/enum mentioned.
    fn infer_type(
        &mut self,
        ty: &Type,
        subst: &HashMap<String, Type>,
    ) -> Result<Type, CompileError> {
        let ty = substitute_type(ty, subst);
        self.infer_type_concrete(&ty)
    }

    fn infer_type_concrete(&mut self, ty: &Type) -> Result<Type, CompileError> {
        match ty {
            Type::Path(p) => {
                let key = path_key(p);
                let args = p.segments.last().map(|s| s.args.clone()).unwrap_or_default();

                if let Some(a) = self.aliases.get(&key) {
                    let a_subst = subst_map(&a.generics, &args);
                    let expanded = substitute_type(&a.ty, &a_subst);
                    return self.infer_type_concrete(&expanded);
                }

                let lowered_args = args
                    .iter()
                    .map(|a| self.infer_type_concrete(a))
                    .collect::<Result<Vec<_>, _>>()?;

                if let Some(s) = self.structs.get(&key).cloned() {
                    let (sname, ngen) = (s.name.clone(), s.generics.len());
                    if ngen == 0 {
                        if !lowered_args.is_empty() {
                            return Err(CompileError::type_err(
                                format!("type '{}' takes no type arguments", key),
                                Span::zero(),
                            ));
                        }
                        self.instantiate_struct(&sname, &[])?;
                        return Ok(Type::path(&sname));
                    } else {
                        if lowered_args.len() != ngen {
                            return Err(CompileError::type_err(
                                format!(
                                    "type '{}' expects {} type arguments, got {}",
                                    key,
                                    ngen,
                                    lowered_args.len()
                                ),
                                Span::zero(),
                            ));
                        }
                        self.instantiate_struct(&sname, &lowered_args)?;
                        return Ok(Type::Path(Path {
                            segments: vec![PathSegment { name: sname, args: lowered_args }],
                        }));
                    }
                }

                if let Some(e) = self.enums.get(&key).cloned() {
                    let (ename, ngen) = (e.name.clone(), e.generics.len());
                    if ngen == 0 {
                        if !lowered_args.is_empty() {
                            return Err(CompileError::type_err(
                                format!("type '{}' takes no type arguments", key),
                                Span::zero(),
                            ));
                        }
                        self.instantiate_enum(&ename, &[])?;
                        return Ok(Type::path(&ename));
                    } else {
                        if lowered_args.len() != ngen {
                            return Err(CompileError::type_err(
                                format!(
                                    "type '{}' expects {} type arguments, got {}",
                                    key,
                                    ngen,
                                    lowered_args.len()
                                ),
                                Span::zero(),
                            ));
                        }
                        self.instantiate_enum(&ename, &lowered_args)?;
                        return Ok(Type::Path(Path {
                            segments: vec![PathSegment { name: ename, args: lowered_args }],
                        }));
                    }
                }

                // Unknown type: keep the name, but lower nested args.
                let segments = p
                    .segments
                    .iter()
                    .map(|s| -> Result<PathSegment, CompileError> {
                        let args = s
                            .args
                            .iter()
                            .map(|a| self.infer_type_concrete(a))
                            .collect::<Result<Vec<_>, _>>()?;
                        Ok(PathSegment { name: s.name.clone(), args })
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(Type::Path(Path { segments }))
            }
            Type::GcRef(inner) => Ok(Type::GcRef(Box::new(self.infer_type_concrete(inner)?))),
            Type::Function(ft) => Ok(Type::Function(FunctionType {
                params: ft
                    .params
                    .iter()
                    .map(|p| self.infer_type_concrete(p))
                    .collect::<Result<Vec<_>, _>>()?,
                ret: Box::new(self.infer_type_concrete(&ft.ret)?),
            })),
            Type::Tuple(ts) => Ok(Type::Tuple(
                ts.iter()
                    .map(|t| self.infer_type_concrete(t))
                    .collect::<Result<Vec<_>, _>>()?,
            )),
            Type::Unit => Ok(Type::Unit),
            Type::Never => Ok(Type::Never),
        }
    }

    /// Rewrite an inferred concrete type to its emitted (mangled) form.
    fn emit_type(&self, ty: &Type) -> Type {
        match ty {
            Type::Path(p) => {
                let key = path_key(p);
                let args = p.segments.last().map(|s| s.args.clone()).unwrap_or_default();
                if !args.is_empty() {
                    if let Some(m) = self.struct_names.get(&(key.clone(), args.clone())) {
                        return Type::path(m);
                    }
                    if let Some(m) = self.enum_names.get(&(key.clone(), args.clone())) {
                        return Type::path(m);
                    }
                }
                Type::Path(Path {
                    segments: p
                        .segments
                        .iter()
                        .map(|s| PathSegment {
                            name: s.name.clone(),
                            args: s.args.iter().map(|a| self.emit_type(a)).collect(),
                        })
                        .collect(),
                })
            }
            Type::GcRef(inner) => Type::GcRef(Box::new(self.emit_type(inner))),
            Type::Function(ft) => Type::Function(FunctionType {
                params: ft.params.iter().map(|p| self.emit_type(p)).collect(),
                ret: Box::new(self.emit_type(&ft.ret)),
            }),
            Type::Tuple(ts) => Type::Tuple(ts.iter().map(|t| self.emit_type(t)).collect()),
            Type::Unit => Type::Unit,
            Type::Never => Type::Never,
        }
    }

    // ─── Unification ──────────────────────────────────────────────────────────

    fn resolve(&self, ty: &Type, vars: &HashSet<String>, subst: &HashMap<String, Type>) -> Type {
        let mut t = ty.clone();
        loop {
            match var_name(&t, vars) {
                Some(n) => match subst.get(&n) {
                    Some(next) => t = next.clone(),
                    None => break,
                },
                None => break,
            }
        }
        t
    }

    fn occurs(&self, name: &str, ty: &Type) -> bool {
        match ty {
            Type::Path(p) => {
                p.segments.iter().any(|s| {
                    (s.name == name && s.args.is_empty())
                        || s.args.iter().any(|a| self.occurs(name, a))
                })
            }
            Type::GcRef(i) => self.occurs(name, i),
            Type::Function(ft) => {
                ft.params.iter().any(|p| self.occurs(name, p)) || self.occurs(name, &ft.ret)
            }
            Type::Tuple(ts) => ts.iter().any(|t| self.occurs(name, t)),
            _ => false,
        }
    }

    fn unify(
        &self,
        a: &Type,
        b: &Type,
        vars: &HashSet<String>,
        subst: &mut HashMap<String, Type>,
    ) -> Result<(), String> {
        let a = self.resolve(a, vars, subst);
        let b = self.resolve(b, vars, subst);

        if let Some(na) = var_name(&a, vars) {
            // unify(T, T): already identical, nothing to bind.
            if var_name(&b, vars).as_ref() == Some(&na) {
                return Ok(());
            }
            if self.occurs(&na, &b) {
                return Err(format!("occurs check: {} in {}", na, b));
            }
            subst.insert(na, b);
            return Ok(());
        }
        if let Some(nb) = var_name(&b, vars) {
            if self.occurs(&nb, &a) {
                return Err(format!("occurs check: {} in {}", nb, a));
            }
            subst.insert(nb, a);
            return Ok(());
        }

        match (&a, &b) {
            (Type::Path(pa), Type::Path(pb)) => {
                if pa.segments.len() != pb.segments.len() {
                    return Err(format!("{} vs {}", a, b));
                }
                for (sa, sb) in pa.segments.iter().zip(pb.segments.iter()) {
                    if sa.name != sb.name || sa.args.len() != sb.args.len() {
                        return Err(format!("{} vs {}", a, b));
                    }
                    for (aa, bb) in sa.args.iter().zip(sb.args.iter()) {
                        self.unify(aa, bb, vars, subst)?;
                    }
                }
                Ok(())
            }
            (Type::GcRef(x), Type::GcRef(y)) => self.unify(x, y, vars, subst),
            (Type::Function(fa), Type::Function(fb)) => {
                if fa.params.len() != fb.params.len() {
                    return Err(format!("{} vs {}", a, b));
                }
                for (x, y) in fa.params.iter().zip(fb.params.iter()) {
                    self.unify(x, y, vars, subst)?;
                }
                self.unify(&fa.ret, &fb.ret, vars, subst)
            }
            (Type::Tuple(ta), Type::Tuple(tb)) => {
                if ta.len() != tb.len() {
                    return Err(format!("{} vs {}", a, b));
                }
                for (x, y) in ta.iter().zip(tb.iter()) {
                    self.unify(x, y, vars, subst)?;
                }
                Ok(())
            }
            (Type::Unit, Type::Unit) => Ok(()),
            (Type::Never, _) | (_, Type::Never) => Ok(()),
            _ => Err(format!("cannot unify {} and {}", a, b)),
        }
    }

    fn unify_params(
        &self,
        params: &[Param],
        args: &[Type],
        vars: &HashSet<String>,
        subst: &mut HashMap<String, Type>,
    ) -> Result<(), String> {
        if params.len() != args.len() {
            return Err(format!("arity mismatch"));
        }
        for (p, a) in params.iter().zip(args.iter()) {
            self.unify(&p.ty, a, vars, subst)?;
        }
        Ok(())
    }

    // ─── Functions & blocks ───────────────────────────────────────────────────

    fn lower_function(
        &mut self,
        f: &Function,
        args: &[Type],
        name: &str,
    ) -> Result<Function, CompileError> {
        let f_subst = subst_map(&f.generics, args);

        let mut params = Vec::new();
        for p in &f.params {
            let inferred = self.infer_type(&p.ty, &f_subst)?;
            let default = match &p.default {
                Some(d) => {
                    let mut env = HashMap::new();
                    let (d2, _) = self.lower_expr(d, &mut env, &f_subst, None)?;
                    Some(d2)
                }
                None => None,
            };
            params.push(Param {
                name: p.name.clone(),
                ty: self.emit_type(&inferred),
                named: p.named,
                default,
                span: p.span,
            });
        }

        let ret = self.infer_type(&f.return_type, &f_subst)?;
        let ret_emitted = self.emit_type(&ret);

        let mut env = HashMap::new();
        for p in &params {
            // Recover inferred (unmangled) types for the body environment.
            if let Some(orig) = f.params.iter().find(|op| op.name == p.name) {
                let inferred = self.infer_type(&orig.ty, &f_subst)?;
                env.insert(p.name.clone(), inferred);
            }
        }

        let saved = self.cur_ret.take();
        self.cur_ret = Some(ret.clone());
        let (body, _) = self.lower_block(&f.body, &env, &f_subst, Some(&ret))?;
        self.cur_ret = saved;

        Ok(Function {
            name: name.to_string(),
            generics: vec![],
            params,
            return_type: ret_emitted,
            body,
            is_pub: f.is_pub,
            span: f.span,
        })
    }

    fn lower_block(
        &mut self,
        block: &Block,
        env: &HashMap<String, Type>,
        subst: &HashMap<String, Type>,
        expected: Option<&Type>,
    ) -> Result<(Block, Type), CompileError> {
        let mut local = env.clone();
        let mut stmts = Vec::new();
        let mut last = Type::Unit;
        let n = block.stmts.len();
        for (i, s) in block.stmts.iter().enumerate() {
            let exp = if i == n - 1 { expected } else { None };
            let (s2, t) = self.lower_expr(s, &mut local, subst, exp)?;
            last = t;
            stmts.push(s2);
        }
        Ok((Block { stmts, span: block.span }, last))
    }

    fn field_type(&self, obj_ty: &Type, field: &str) -> Option<Type> {
        if let Type::Path(p) = obj_ty {
            let key = path_key(p);
            if let Some(s) = self.structs.get(&key) {
                let args = p.segments.last().map(|s| s.args.clone()).unwrap_or_default();
                if args.len() != s.generics.len() {
                    return None;
                }
                let sub = subst_map(&s.generics, &args);
                return s
                    .fields
                    .iter()
                    .find(|f| f.name == field)
                    .map(|f| substitute_type(&f.ty, &sub));
            }
            if let Some(s) = self.out_structs.get(&key) {
                return s.fields.iter().find(|f| f.name == field).map(|f| f.ty.clone());
            }
        }
        None
    }

    fn lower_expr(
        &mut self,
        expr: &Expr,
        env: &mut HashMap<String, Type>,
        subst: &HashMap<String, Type>,
        expected: Option<&Type>,
    ) -> Result<(Expr, Type), CompileError> {
        let span = expr.span;
        let mk = |kind| Expr::new(kind, span);

        match &expr.kind {
            ExprKind::IntLiteral(_) => Ok((expr.clone(), Type::path("Int"))),
            ExprKind::FloatLiteral(_) => Ok((expr.clone(), Type::path("Float"))),
            ExprKind::StringLiteral(_) => Ok((expr.clone(), Type::path("String"))),
            ExprKind::CharLiteral(_) => Ok((expr.clone(), Type::path("Char"))),
            ExprKind::BoolLiteral(_) => Ok((expr.clone(), Type::path("Bool"))),
            ExprKind::NilLiteral => Ok((expr.clone(), Type::Never)),

            ExprKind::Ident(name) => {
                let ty = env.get(name).cloned().unwrap_or(Type::Never);
                Ok((expr.clone(), ty))
            }

            ExprKind::Block(b) => {
                let (b2, t) = self.lower_block(b, env, subst, expected)?;
                Ok((mk(ExprKind::Block(b2)), t))
            }

            ExprKind::Let { name, ty, value, is_mut } => {
                let ann = match ty {
                    Some(t) => Some(self.infer_type(t, subst)?),
                    None => None,
                };
                let (v2, vt) = self.lower_expr(value, env, subst, ann.as_ref())?;
                let cty = ann.clone().unwrap_or(vt);
                env.insert(name.clone(), cty.clone());
                let ann_emitted = ann.as_ref().map(|t| self.emit_type(t));
                Ok((
                    mk(ExprKind::Let {
                        name: name.clone(),
                        ty: ann_emitted,
                        value: Box::new(v2),
                        is_mut: *is_mut,
                    }),
                    Type::Unit,
                ))
            }

            ExprKind::Return(Some(e)) => {
                let ret = self.cur_ret.clone();
                let (e2, _) = self.lower_expr(e, env, subst, ret.as_ref())?;
                Ok((mk(ExprKind::Return(Some(Box::new(e2)))), Type::Never))
            }
            ExprKind::Return(None) => Ok((expr.clone(), Type::Unit)),

            ExprKind::Binary { op, left, right } => {
                let (l2, lt) = self.lower_expr(left, env, subst, None)?;
                let (r2, rt) = self.lower_expr(right, env, subst, None)?;
                let t = match op {
                    BinOp::Add => {
                        if lt == Type::path("String") && rt == Type::path("String") {
                            Type::path("String")
                        } else {
                            Type::path("Int")
                        }
                    }
                    BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Mod => Type::path("Int"),
                    BinOp::Eq | BinOp::NotEq | BinOp::Lt | BinOp::Gt | BinOp::LtEq | BinOp::GtEq => {
                        Type::path("Bool")
                    }
                    BinOp::And | BinOp::Or => Type::path("Bool"),
                };
                Ok((mk(ExprKind::Binary { op: *op, left: Box::new(l2), right: Box::new(r2) }), t))
            }

            ExprKind::Unary { op, expr: inner } => {
                let (i2, _) = self.lower_expr(inner, env, subst, None)?;
                let t = match op {
                    crate::ast::UnaryOp::Neg => Type::path("Int"),
                    crate::ast::UnaryOp::Not => Type::path("Bool"),
                };
                Ok((mk(ExprKind::Unary { op: *op, expr: Box::new(i2) }), t))
            }

            ExprKind::Call { func, args } => {
                // Lower the callee expression (UFCS receivers live here).
                let new_func = match &func.kind {
                    ExprKind::DotAccess { object, field } => {
                        let (o2, _) = self.lower_expr(object, env, subst, None)?;
                        mk(ExprKind::DotAccess { object: Box::new(o2), field: field.clone() })
                    }
                    ExprKind::Field { object, field } => {
                        let (o2, _) = self.lower_expr(object, env, subst, None)?;
                        mk(ExprKind::Field { object: Box::new(o2), field: field.clone() })
                    }
                    _ => (**func).clone(),
                };

                let mut largs = Vec::new();
                let mut atypes = Vec::new();
                let mut has_named = false;
                for a in args {
                    if matches!(a.kind, ExprKind::NamedArg { .. }) {
                        has_named = true;
                    }
                    let (a2, t) = self.lower_expr(a, env, subst, None)?;
                    largs.push(a2);
                    atypes.push(t);
                }

                if let ExprKind::Ident(name) = &func.kind {
                    if name == "std::to_string" {
                        return Ok((mk(ExprKind::Call { func: Box::new(new_func), args: largs }), Type::path("String")));
                    }
                    if name == "print" || name == "println" || name == "print_int" {
                        return Ok((mk(ExprKind::Call { func: Box::new(new_func), args: largs }), Type::Unit));
                    }

                    if let Some(ovs) = self.functions.get(name).cloned() {
                        for f in &ovs {
                            let ordered = if has_named {
                                match crate::typeck::assign_arg_slots(&f.params, &largs) {
                                    Some(slots) => slots.iter().map(|&i| atypes[i].clone()).collect(),
                                    None => continue,
                                }
                            } else {
                                if f.params.len() != atypes.len() {
                                    continue;
                                }
                                atypes.clone()
                            };

                            let vars: HashSet<String> =
                                f.generics.iter().map(|g| g.name.clone()).collect();
                            let mut s = HashMap::new();
                            if self.unify_params(&f.params, &ordered, &vars, &mut s).is_ok() {
                                let cargs: Vec<Type> = f
                                    .generics
                                    .iter()
                                    .map(|g| self.resolve(&Type::path(&g.name), &vars, &s))
                                    .collect();
                                if cargs.iter().any(|t| var_name(t, &vars).is_some()) {
                                    return Err(CompileError::type_err(
                                        format!("cannot infer type arguments for '{}'", name),
                                        span,
                                    ));
                                }
                                let fname = if f.generics.is_empty() {
                                    name.clone()
                                } else {
                                    self.instantiate_function(name, &cargs)?
                                };
                                let f_subst = subst_map(&f.generics, &cargs);
                                let ret = self.infer_type(&f.return_type, &f_subst)?;
                                let callee = Expr::ident(&fname, func.span);
                                return Ok((mk(ExprKind::Call { func: Box::new(callee), args: largs }), ret));
                            }
                        }
                    }
                    // Unknown/overload-mismatch: keep the call intact and be permissive.
                    return Ok((mk(ExprKind::Call { func: Box::new(new_func), args: largs }), Type::Unit));
                }

                // UFCS / non-identifier callee: pass through, permissive type.
                Ok((mk(ExprKind::Call { func: Box::new(new_func), args: largs }), Type::Unit))
            }

            ExprKind::DotAccess { object, field } => {
                let (o2, ot) = self.lower_expr(object, env, subst, None)?;
                let t = self.field_type(&ot, field).unwrap_or(Type::Unit);
                Ok((mk(ExprKind::DotAccess { object: Box::new(o2), field: field.clone() }), t))
            }

            ExprKind::Field { object, field } => {
                let (o2, ot) = self.lower_expr(object, env, subst, None)?;
                let t = self.field_type(&ot, field).unwrap_or(Type::Unit);
                Ok((mk(ExprKind::Field { object: Box::new(o2), field: field.clone() }), t))
            }

            ExprKind::StructLit { path, fields } => {
                let key = path.join("::");
                if let Some(s) = self.structs.get(&key).cloned() {
                    let mut lfields = Vec::new();
                    let mut ftypes = Vec::new();
                    for (fname, fval) in fields {
                        let (v2, vt) = self.lower_expr(fval, env, subst, None)?;
                        lfields.push((fname.clone(), v2));
                        ftypes.push((fname.clone(), vt));
                    }

                    let cargs = if s.generics.is_empty() {
                        vec![]
                    } else {
                        let vars: HashSet<String> =
                            s.generics.iter().map(|g| g.name.clone()).collect();
                        let mut ss = HashMap::new();
                        // Expected-type constraints first (pins params that
                        // never appear in the field list).
                        if let Some(exp) = expected {
                            if let Type::Path(ep) = exp {
                                if ep.segments.last().map(|s| s.name.as_str())
                                    == Some(s.name.as_str())
                                {
                                    let eargs = ep.segments.last().unwrap().args.clone();
                                    if eargs.len() == s.generics.len() {
                                        for (g, ea) in s.generics.iter().zip(eargs.iter()) {
                                            let _ =
                                                self.unify(&Type::path(&g.name), ea, &vars, &mut ss);
                                        }
                                    }
                                }
                            }
                        }
                        for (fname, vt) in &ftypes {
                            if let Some(sf) = s.fields.iter().find(|f| &f.name == fname) {
                                self.unify(&sf.ty, vt, &vars, &mut ss).map_err(|e| {
                                    CompileError::type_err(
                                        format!("cannot infer type arguments for '{}': {}", key, e),
                                        span,
                                    )
                                })?;
                            }
                        }
                        let cargs: Vec<Type> = s
                            .generics
                            .iter()
                            .map(|g| self.resolve(&Type::path(&g.name), &vars, &ss))
                            .collect();
                        if cargs.iter().any(|t| var_name(t, &vars).is_some()) {
                            return Err(CompileError::type_err(
                                format!("cannot infer type arguments for '{}'", key),
                                span,
                            ));
                        }
                        cargs
                    };

                    let mangled = if s.generics.is_empty() {
                        self.instantiate_struct(&s.name, &[])?
                    } else {
                        self.instantiate_struct(&s.name, &cargs)?
                    };
                    // Reorder fields to declaration order (constructor
                    // parameter order is the declaration order).
                    let mut ordered = Vec::new();
                    for sf in &s.fields {
                        if let Some((_, v)) = lfields.iter().find(|(n, _)| n == &sf.name) {
                            ordered.push((sf.name.clone(), v.clone()));
                        }
                    }
                    let ret = if cargs.is_empty() {
                        Type::path(&s.name)
                    } else {
                        Type::Path(Path {
                            segments: vec![PathSegment { name: s.name.clone(), args: cargs }],
                        })
                    };
                    return Ok((mk(ExprKind::StructLit { path: vec![mangled], fields: ordered }), ret));
                }

                let mut lfields = Vec::new();
                for (fname, fval) in fields {
                    let (v2, _) = self.lower_expr(fval, env, subst, None)?;
                    lfields.push((fname.clone(), v2));
                }
                Ok((mk(ExprKind::StructLit { path: path.clone(), fields: lfields }), Type::Unit))
            }

            ExprKind::EnumCtor { path, case, arg } => {
                let (a2, at) = match arg {
                    Some(a) => {
                        let (x, t) = self.lower_expr(a, env, subst, None)?;
                        (Some(x), t)
                    }
                    None => (None, Type::Never),
                };

                let ename = if !path.is_empty() {
                    Some(path.join("::"))
                } else {
                    // Declaration order, not hash order.
                    self.enum_order
                        .iter()
                        .find(|n| {
                            self.enums
                                .get(*n)
                                .map(|e| e.cases.iter().any(|c| &c.name == case))
                                .unwrap_or(false)
                        })
                        .cloned()
                };
                let ename = ename.ok_or_else(|| {
                    CompileError::type_err(format!("unknown enum case '.{}'", case), span)
                })?;

                if let Some(e) = self.enums.get(&ename).cloned() {
                    if e.generics.is_empty() {
                        let mangled = self.instantiate_enum(&e.name, &[])?;
                        return Ok((
                            mk(ExprKind::EnumCtor {
                                path: vec![mangled],
                                case: case.clone(),
                                arg: a2.map(Box::new),
                            }),
                            Type::path(&e.name),
                        ));
                    }

                    let ec = e.cases.iter().find(|c| &c.name == case).cloned();
                    let vars: HashSet<String> =
                        e.generics.iter().map(|g| g.name.clone()).collect();
                    let mut ss = HashMap::new();

                    // Expected-type constraints first: they may pin down
                    // params that the payload does not mention (e.g.
                    // `Either[Int, String] = .left(5)` pins B).
                    if let Some(exp) = expected {
                        if let Type::Path(ep) = exp {
                            if ep.segments.last().map(|s| s.name.as_str())
                                == Some(e.name.as_str())
                            {
                                let eargs = ep.segments.last().unwrap().args.clone();
                                if eargs.len() == e.generics.len() {
                                    for (g, ea) in e.generics.iter().zip(eargs.iter()) {
                                        let _ =
                                            self.unify(&Type::path(&g.name), ea, &vars, &mut ss);
                                    }
                                }
                            }
                        }
                    }

                    // Payload constraint.
                    if let Some(payload) = ec.as_ref().and_then(|c| c.payload.as_ref()) {
                        if !matches!(at, Type::Never) {
                            self.unify(payload, &at, &vars, &mut ss).map_err(|err| {
                                CompileError::type_err(
                                    format!(
                                        "cannot infer type arguments for '.{}': {}",
                                        case, err
                                    ),
                                    span,
                                )
                            })?;
                        }
                    }

                    let cargs: Vec<Type> = e
                        .generics
                        .iter()
                        .map(|g| self.resolve(&Type::path(&g.name), &vars, &ss))
                        .collect();
                    let unresolved = cargs.iter().any(|t| var_name(t, &vars).is_some());
                    if unresolved && expected.is_none() && arg.is_none() {
                        // Payload-less constructor with no context to pin the
                        // enum: leave it unresolved; codegen emits a conversion
                        // tag that picks the instantiation from the context.
                        return Ok((expr.clone(), Type::Never));
                    }
                    if unresolved {
                        return Err(CompileError::type_err(
                            format!(
                                "cannot infer type arguments for '.{}' (add a type annotation)",
                                case
                            ),
                            span,
                        ));
                    }

                    let mangled = self.instantiate_enum(&e.name, &cargs)?;
                    let ret = if cargs.is_empty() {
                        Type::path(&e.name)
                    } else {
                        Type::Path(Path {
                            segments: vec![PathSegment { name: e.name.clone(), args: cargs }],
                        })
                    };
                    return Ok((
                        mk(ExprKind::EnumCtor {
                            path: vec![mangled],
                            case: case.clone(),
                            arg: a2.map(Box::new),
                        }),
                        ret,
                    ));
                }

                Ok((expr.clone(), Type::Unit))
            }

            ExprKind::Match { expr: subject, arms } => {
                let (s2, st) = self.lower_expr(subject, env, subst, None)?;
                let mut new_arms = Vec::new();
                let mut result = Type::Never;
                for arm in arms {
                    let (pat2, bindings) = self.lower_pattern(&arm.pattern, &st)?;
                    let mut penv = env.clone();
                    for (name, ty) in bindings {
                        penv.insert(name, ty);
                    }
                    let guard = match &arm.guard {
                        Some(g) => {
                            let (g2, _) = self.lower_expr(g, &mut penv, subst, None)?;
                            Some(Box::new(g2))
                        }
                        None => None,
                    };
                    let (body2, bt) = self.lower_expr(&arm.body, &mut penv, subst, expected)?;
                    if result == Type::Never {
                        result = bt;
                    }
                    new_arms.push(MatchArm {
                        pattern: pat2,
                        guard,
                        body: Box::new(body2),
                    });
                }
                if result == Type::Never {
                    result = Type::Unit;
                }
                Ok((mk(ExprKind::Match { expr: Box::new(s2), arms: new_arms }), result))
            }

            ExprKind::GcNew { ty, fields } => {
                let inferred = self.infer_type(ty, subst)?;
                let emitted = self.emit_type(&inferred);
                let mut lfields = Vec::new();
                for (fname, fval) in fields {
                    let (v2, _) = self.lower_expr(fval, env, subst, None)?;
                    lfields.push((fname.clone(), v2));
                }
                let ret = Type::GcRef(Box::new(inferred));
                Ok((mk(ExprKind::GcNew { ty: emitted, fields: lfields }), ret))
            }

            ExprKind::Assign { target, value } => {
                let (t2, tt) = self.lower_expr(target, env, subst, None)?;
                let (v2, _) = self.lower_expr(value, env, subst, Some(&tt))?;
                Ok((mk(ExprKind::Assign { target: Box::new(t2), value: Box::new(v2) }), Type::Unit))
            }

            ExprKind::AssignOp { target, op, value } => {
                let (t2, _) = self.lower_expr(target, env, subst, None)?;
                let (v2, _) = self.lower_expr(value, env, subst, None)?;
                Ok((mk(ExprKind::AssignOp { target: Box::new(t2), op: *op, value: Box::new(v2) }), Type::Unit))
            }

            ExprKind::If { cond, then_branch, else_branch } => {
                let (c2, _) = self.lower_expr(cond, env, subst, None)?;
                let (then2, tt) = self.lower_block(then_branch, env, subst, expected)?;
                let (else2, et) = match else_branch {
                    Some(eb) => {
                        let (e2, et) = self.lower_block(eb, env, subst, expected)?;
                        (Some(e2), et)
                    }
                    None => (None, Type::Unit),
                };
                let ret = if tt != Type::Never { tt } else { et };
                Ok((mk(ExprKind::If { cond: Box::new(c2), then_branch: then2, else_branch: else2 }), ret))
            }

            ExprKind::While { cond, body } => {
                let (c2, _) = self.lower_expr(cond, env, subst, None)?;
                let (b2, _) = self.lower_block(body, env, subst, None)?;
                Ok((mk(ExprKind::While { cond: Box::new(c2), body: b2 }), Type::Unit))
            }

            ExprKind::For { var, iter, body } => {
                let (i2, _) = self.lower_expr(iter, env, subst, None)?;
                let mut benv = env.clone();
                benv.insert(var.clone(), Type::Unit);
                let (b2, _) = self.lower_block(body, &benv, subst, None)?;
                Ok((mk(ExprKind::For { var: var.clone(), iter: Box::new(i2), body: b2 }), Type::Unit))
            }

            ExprKind::Index { object, index } => {
                let (o2, _) = self.lower_expr(object, env, subst, None)?;
                let (i2, _) = self.lower_expr(index, env, subst, None)?;
                Ok((mk(ExprKind::Index { object: Box::new(o2), index: Box::new(i2) }), Type::Unit))
            }

            ExprKind::NamedArg { name, value } => {
                let (v2, t) = self.lower_expr(value, env, subst, None)?;
                Ok((mk(ExprKind::NamedArg { name: name.clone(), value: Box::new(v2) }), t))
            }

            // Compile-time / macro leftovers and unsupported forms: pass through.
            _ => Ok((expr.clone(), Type::Unit)),
        }
    }

    fn lower_pattern(
        &mut self,
        pat: &Pattern,
        subject: &Type,
    ) -> Result<(Pattern, Vec<(String, Type)>), CompileError> {
        let mut bindings = Vec::new();
        let kind = match &pat.kind {
            PatternKind::Wildcard => PatternKind::Wildcard,
            PatternKind::Literal(l) => PatternKind::Literal(l.clone()),
            PatternKind::Variable { name, is_mut } => {
                bindings.push((name.clone(), subject.clone()));
                PatternKind::Variable { name: name.clone(), is_mut: *is_mut }
            }
            PatternKind::EnumCtor { case, inner, .. } => {
                let ekey = match subject {
                    Type::Path(p) => path_key(p),
                    _ => String::new(),
                };
                let e = self.enums.get(&ekey).cloned();
                let subject_args = match subject {
                    Type::Path(p) => p.segments.last().map(|s| s.args.clone()).unwrap_or_default(),
                    _ => vec![],
                };
                let ssub = e.as_ref().map(|e| subst_map(&e.generics, &subject_args)).unwrap_or_default();
                let payload = e
                    .as_ref()
                    .and_then(|e| e.cases.iter().find(|c| &c.name == case).and_then(|c| c.payload.clone()));

                let payload_ty = match payload {
                    Some(pt) => Some(self.infer_type(&pt, &ssub)?),
                    None => None,
                };

                let inner2 = match inner {
                    Some(ip) => {
                        let (ip2, ib) = self.lower_pattern(ip, &payload_ty.clone().unwrap_or(Type::Unit))?;
                        (Some(Box::new(ip2)), ib)
                    }
                    None => (None, vec![]),
                };
                bindings.extend(inner2.1);

                let emitted = self.emit_type(subject);
                let mangled = match &emitted {
                    Type::Path(p) => path_key(p),
                    _ => String::new(),
                };
                PatternKind::EnumCtor {
                    path: if mangled.is_empty() { vec![] } else { vec![mangled] },
                    case: case.clone(),
                    inner: inner2.0,
                }
            }
            PatternKind::Struct { path, fields } => {
                let fields = fields
                    .iter()
                    .map(|(f, p)| {
                        let (p2, _) = self.lower_pattern(p, &Type::Unit)?;
                        Ok((f.clone(), p2))
                    })
                    .collect::<Result<Vec<_>, CompileError>>()?;
                PatternKind::Struct { path: path.clone(), fields }
            }
            PatternKind::Or(ps) => PatternKind::Or(
                ps.iter()
                    .map(|p| self.lower_pattern(p, subject).map(|(p2, _)| p2))
                    .collect::<Result<Vec<_>, _>>()?,
            ),
        };
        Ok((Pattern { kind, span: pat.span }, bindings))
    }
}

// ─── Editor support (hover type inference) ───────────────────────────────────

impl Mono {
    /// Infer the type of a variable visible at `offset` inside `f`, from the
    /// *original* (generic) AST: types keep their source shape
    /// (`Pair[Bool, Int]`, never the mangled `Pair__bool__int`) so hover can
    /// display them nicely. Best effort: returns `None` on any hiccup.
    pub fn infer_var_type_at(
        module: &Module,
        f: &Function,
        name: &str,
        offset: usize,
    ) -> Option<Type> {
        let mut mono = Self::new();
        mono.collect_tables(module);
        let mut env: HashMap<String, Type> = HashMap::new();
        for p in &f.params {
            env.insert(p.name.clone(), p.ty.clone());
        }
        mono.collect_bindings_before(&f.body, offset, &mut env);
        env.get(name).cloned()
    }

    /// Record bindings from statements textually before `offset`, descending
    /// into nested blocks/control flow. Parser spans of compound statements
    /// cover only their first token, so scoping is approximated by source
    /// order: a binding is visible when its code precedes the cursor.
    fn collect_bindings_before(
        &mut self,
        block: &Block,
        offset: usize,
        env: &mut HashMap<String, Type>,
    ) {
        for stmt in &block.stmts {
            if stmt.span.start >= offset {
                break;
            }
            self.collect_stmt_bindings(stmt, offset, env);
        }
    }

    fn collect_stmt_bindings(
        &mut self,
        stmt: &Expr,
        offset: usize,
        env: &mut HashMap<String, Type>,
    ) {
        match &stmt.kind {
            ExprKind::Let { name, ty, value, .. } => {
                let vt = ty.as_ref().cloned().or_else(|| {
                    self.lower_expr(value, env, &HashMap::new(), None)
                        .ok()
                        .map(|(_, t)| t)
                });
                if let Some(vt) = vt {
                    env.insert(name.clone(), vt);
                }
            }
            ExprKind::Block(b) => self.collect_bindings_before(b, offset, env),
            ExprKind::If { then_branch, else_branch, .. } => {
                self.collect_bindings_before(then_branch, offset, env);
                if let Some(eb) = else_branch {
                    self.collect_bindings_before(eb, offset, env);
                }
            }
            ExprKind::While { body, .. } | ExprKind::For { body, .. } => {
                self.collect_bindings_before(body, offset, env);
            }
            ExprKind::Match { expr, arms } => {
                // Bind the pattern variables of the arm that contains the
                // cursor: the last arm whose code starts before the offset.
                let subject_ty = self
                    .lower_expr(expr, env, &HashMap::new(), None)
                    .ok()
                    .map(|(_, t)| t);
                let chosen = arms.iter().take_while(|arm| {
                    let guard = arm
                        .guard
                        .as_ref()
                        .map(|g| g.span.start)
                        .unwrap_or(usize::MAX);
                    arm.pattern.span.start.min(arm.body.span.start).min(guard) < offset
                });
                let chosen = chosen.last();
                if let (Some(arm), Some(st)) = (chosen, &subject_ty) {
                    let mut arm_env = env.clone();
                    if let Ok((_, bindings)) = self.lower_pattern(&arm.pattern, st) {
                        for (n, t) in bindings {
                            arm_env.insert(n, t);
                        }
                    }
                    if let ExprKind::Block(b) = &arm.body.kind {
                        self.collect_bindings_before(b, offset, &mut arm_env);
                    }
                    *env = arm_env;
                }
            }
            _ => {}
        }
    }
}
