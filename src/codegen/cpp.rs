use crate::ast::*;
use std::collections::{BTreeMap, HashMap, HashSet};

/// One overload of a dispatched function (free function or impl method).
#[derive(Debug, Clone)]
struct Overload {
    params: Vec<Type>,
    ret: Type,
}

/// Dispatch metadata for one overloaded function name.
/// Unified N-ary dynamic dispatch: every parameter position where some
/// overload is trait-typed becomes a "dispatch position"; the runtime key
/// packs the type_id of each dispatch position into 16 bits.
#[derive(Debug, Clone)]
struct DispatchInfo {
    /// Per parameter: Some(trait_name) if this is a dispatch position.
    positions: Vec<Option<String>>,
    /// Representative parameter types (from the first overload) for fixed positions.
    param_types: Vec<Type>,
    ret: Type,
    /// All overloads, in declaration order (first wins specificity ties).
    overloads: Vec<Overload>,
    /// True if an overload with all-trait dispatch positions exists (runtime fallback).
    has_generic_fallback: bool,
}

pub struct CppGenerator {
    output: String,
    indent: usize,
    dispatch: BTreeMap<String, DispatchInfo>,
    return_trait: Option<String>,
    trait_names: HashSet<String>,
    trait_methods: HashSet<String>,
    // trait name → concrete impl targets, in declaration order
    trait_impls: BTreeMap<String, Vec<String>>,
    type_ids: HashMap<String, u32>,
}

impl CppGenerator {
    pub fn new() -> Self {
        Self {
            output: String::new(), indent: 0, dispatch: BTreeMap::new(),
            return_trait: None, trait_names: HashSet::new(), trait_methods: HashSet::new(),
            trait_impls: BTreeMap::new(), type_ids: HashMap::new(),
        }
    }

    pub fn generate(mut self, module: &Module) -> String {
        self.collect_info(module);
        self.emit_header();
        self.emit_module(module);
        self.emit_dispatch_wrappers();
        self.output
    }

    fn type_id_of(name: &str) -> u32 {
        name.bytes().fold(1u32, |h, b| h.wrapping_mul(31).wrapping_add(b as u32)) % 65535 + 1
    }

    fn last_name(ty: &Type) -> Option<&str> {
        if let Type::Path(p) = ty { p.segments.last().map(|s| s.name.as_str()) } else { None }
    }

    fn collect_info(&mut self, module: &Module) {
        for item in &module.items {
            if let Item::Trait(t) = item { self.trait_names.insert(t.name.clone()); }
        }
        // Gather overload sets (free functions + impl methods) in declaration order,
        // plus trait → concrete impl types and their type ids.
        let mut names: Vec<String> = Vec::new();
        let mut sets: HashMap<String, Vec<Overload>> = HashMap::new();
        let add = |f: &Function, names: &mut Vec<String>, sets: &mut HashMap<String, Vec<Overload>>| {
            if !sets.contains_key(&f.name) { names.push(f.name.clone()); }
            sets.entry(f.name.clone()).or_default().push(Overload {
                params: f.params.iter().map(|p| p.ty.clone()).collect(),
                ret: f.return_type.clone(),
            });
        };
        for item in &module.items {
            match item {
                Item::Function(f) => add(f, &mut names, &mut sets),
                Item::Impl(imp) => {
                    let target = match &imp.target_type {
                        Type::Path(p) => p.segments.last().map(|s| s.name.as_str()).unwrap_or("unknown"),
                        _ => "unknown",
                    };
                    if !self.trait_names.contains(target) {
                        self.type_ids.insert(target.to_string(), Self::type_id_of(target));
                        let v = self.trait_impls.entry(imp.trait_name.clone()).or_default();
                        if !v.iter().any(|t| t == target) { v.push(target.to_string()); }
                    }
                    for m in &imp.methods { add(m, &mut names, &mut sets); }
                }
                Item::Trait(t) => {
                    if t.name != "Debug" {
                        for m in &t.methods { self.trait_methods.insert(m.name.clone()); }
                    }
                }
                _ => {}
            }
        }
        // Decide which overload sets need dynamic dispatch.
        for name in names {
            let ovs = &sets[&name];
            if ovs.len() < 2 { continue; }
            let arity = ovs[0].params.len();
            if ovs.iter().any(|o| o.params.len() != arity) { continue; }
            // Return types must agree.
            if ovs.iter().any(|o| self.gen_type(&o.ret) != self.gen_type(&ovs[0].ret)) { continue; }
            // A position is dynamic when some overload is trait-typed there;
            // exactly one trait may appear per position.
            let mut positions: Vec<Option<String>> = vec![None; arity];
            let mut ok = true;
            for i in 0..arity {
                let mut traits: Vec<&str> = ovs.iter()
                    .filter_map(|o| Self::last_name(&o.params[i]))
                    .filter(|n| self.trait_names.contains(*n))
                    .collect();
                traits.dedup();
                match traits.len() {
                    0 => {}
                    1 => positions[i] = Some(traits[0].to_string()),
                    _ => { ok = false; break; }
                }
            }
            if !ok { continue; }
            let dispatch_positions: Vec<&String> = positions.iter().flatten().collect();
            if dispatch_positions.is_empty() || dispatch_positions.len() > 4 { continue; }
            // Every dispatch trait needs at least one concrete impl to enumerate.
            if dispatch_positions.iter().any(|t| self.trait_impls.get(*t).map_or(true, |v| v.is_empty())) { continue; }
            // Fixed positions must have identical types across overloads.
            let fixed_ok = (0..arity).all(|i| positions[i].is_some() ||
                ovs.iter().all(|o| self.gen_type(&o.params[i]) == self.gen_type(&ovs[0].params[i])));
            if !fixed_ok { continue; }
            let has_generic_fallback = ovs.iter().any(|o| positions.iter().enumerate().all(|(i, pos)| {
                match pos {
                    Some(t) => Self::last_name(&o.params[i]) == Some(t.as_str()),
                    None => true,
                }
            }));
            self.dispatch.insert(name, DispatchInfo {
                positions,
                param_types: ovs[0].params.clone(),
                ret: ovs[0].ret.clone(),
                overloads: ovs.clone(),
                has_generic_fallback,
            });
        }
    }

    /// C++ parameter type for each position of a dispatched function:
    /// trait fat pointer (by value) at dispatch positions, concrete type elsewhere.
    fn dispatch_param_types(&self, info: &DispatchInfo) -> Vec<String> {
        info.positions.iter().enumerate().map(|(i, pos)| match pos {
            Some(t) => t.clone(),
            None => self.gen_type(&info.param_types[i]),
        }).collect()
    }

    /// Most specific overload for a concrete type combination (one entry per
    /// dispatch position). Concrete match beats trait match; first declared wins ties.
    fn select_overload<'a>(info: &'a DispatchInfo, combo: &[String]) -> Option<&'a Overload> {
        let mut best: Option<(&Overload, usize)> = None;
        for ov in &info.overloads {
            let mut score = 0usize;
            let mut applicable = true;
            let mut j = 0;
            for (i, pos) in info.positions.iter().enumerate() {
                if let Some(trait_name) = pos {
                    match Self::last_name(&ov.params[i]) {
                        Some(n) if n == combo[j] => score += 1,
                        Some(n) if n == trait_name => {}
                        _ => { applicable = false; break; }
                    }
                    j += 1;
                }
            }
            if applicable && best.map_or(true, |(_, s)| score > s) {
                best = Some((ov, score));
            }
        }
        best.map(|(ov, _)| ov)
    }

    fn cartesian(pools: &[Vec<String>]) -> Vec<Vec<String>> {
        let mut out: Vec<Vec<String>> = vec![vec![]];
        for pool in pools {
            let mut next = Vec::new();
            for combo in &out {
                for item in pool {
                    let mut c = combo.clone();
                    c.push(item.clone());
                    next.push(c);
                }
            }
            out = next;
        }
        out
    }

    fn is_primitive(path: &Path) -> bool {
        let n = path.segments.last().map(|s| s.name.as_str()).unwrap_or("");
        matches!(n, "Int" | "Float" | "Bool" | "String" | "Char" | "void")
    }

    fn emit_header(&mut self) {
        self.emitln("#pragma once\n// ─── Generated by Nova compiler ───");
        self.emitln("#include <string>");
        self.emitln("#include <vector>");
        self.emitln("#include <memory>");
        self.emitln("#include <unordered_map>");
        self.emitln("#include <iostream>");
        self.emitln("#include <cstdint>");
        self.emitln("#include <cstdlib>");
        self.emitln("#include <utility>");
        self.emitln("inline void nova_print_int(int n) { std::cout << n << std::endl; }");
        self.emitln("inline void nova_print(const std::string& s) { std::cout << s; }");
        self.emitln("template<typename T> void nova_print(T v) { std::cout << v; }");
        self.emitln("");
    }

    fn emit_module(&mut self, module: &Module) {
        // Structs and traits first: everything else references them.
        for item in &module.items {
            match item {
                Item::Struct(s) => self.emit_struct(s),
                Item::Trait(t) => self.emit_trait(t),
                _ => {}
            }
        }
        // Dispatch declarations before impls/functions so any body can call _d_*.
        self.emit_dispatch_decls();
        for item in &module.items {
            if let Item::Impl(imp) = item { self.emit_impl(imp); }
        }
        for item in &module.items {
            if let Item::Function(f) = item { self.emit_function(f); }
        }
    }

    /// Forward declarations for dispatched functions: all overloads (so the
    /// static-fallback template can resolve them), a function-pointer alias,
    /// the dynamic entry point, and a variadic template that forwards calls
    /// with statically-concrete arguments to plain C++ overload resolution.
    fn emit_dispatch_decls(&mut self) {
        if self.dispatch.is_empty() { return; }
        self.emitln("// ─── dynamic dispatch declarations ───");
        for (name, info) in self.dispatch.clone() {
            let ret = self.gen_type(&info.ret);
            let ptypes = self.dispatch_param_types(&info);
            for ov in &info.overloads {
                let ps: Vec<String> = ov.params.iter().map(|t| self.gen_type(t)).collect();
                self.emitln(&format!("{} {}({});", self.gen_type(&ov.ret), name, ps.join(", ")));
            }
            self.emitln(&format!("using _fn_{} = {}(*)({});", name, ret, ptypes.join(", ")));
            self.emitln(&format!("{} _d_{}({});", ret, name, ptypes.join(", ")));
            self.emitln(&format!(
                "template<typename... Ts> auto _d_{}(Ts&&... xs) {{ return {}(std::forward<Ts>(xs)...); }}",
                name, name
            ));
        }
        self.emitln("");
    }

    fn emit_struct(&mut self, s: &Struct) {
        self.emitln(&format!("struct {} {{", s.name));
        for field in &s.fields {
            self.emitln(&format!("  {} {};", self.gen_type(&field.ty), field.name));
        }
        let args: Vec<String> = s.fields.iter().map(|f| format!("{} {}", self.gen_type(&f.ty), f.name)).collect();
        let inits: Vec<String> = s.fields.iter().map(|f| format!("{}({})", f.name, f.name)).collect();
        if !args.is_empty() {
            self.emitln(&format!("  {}({}) : {} {{}}", s.name, args.join(", "), inits.join(", ")));
        }
        self.emitln("};");
        self.emitln("");
    }

    fn emit_trait(&mut self, t: &TraitDef) {
        self.emitln(&format!("// ─── trait {} ───", t.name));
        self.emitln(&format!("struct {}_vtable {{", t.name));
        self.emitln("  uint32_t type_id;");
        for m in &t.methods {
            let ret = self.gen_type(&m.return_type);
            let params: Vec<String> = m.params.iter().skip(1).map(|p| format!("{} {}", self.gen_type(&p.ty), p.name)).collect();
            self.emitln(&format!("  {} (*{})(void*{});", ret, m.name, if params.is_empty() { "".into() } else { format!(", {}", params.join(", ")) }));
        }
        self.emitln("};");
        self.emitln(&format!("struct {} {{", t.name));
        self.emitln("  void* _ptr;");
        self.emitln(&format!("  {}_vtable* _vt;", t.name));
        for m in &t.methods {
            let ret = self.gen_type(&m.return_type);
            let pn: Vec<String> = m.params.iter().skip(1).map(|p| p.name.clone()).collect();
            let pd: Vec<String> = m.params.iter().skip(1).map(|p| format!("{} {}", self.gen_type(&p.ty), p.name)).collect();
            self.emitln(&format!("  {} {}({}) {{ return _vt->{}(_ptr{}); }}", ret, m.name, pd.join(", "), m.name, if pn.is_empty() { "".into() } else { format!(", {}", pn.join(", ")) }));
        }
        self.emitln("};");
        self.emitln("");
    }

    fn emit_impl(&mut self, imp: &ImplBlock) {
        let target = match &imp.target_type {
            Type::Path(p) => p.segments.last().map(|s| s.name.as_str()).unwrap_or("unknown"),
            _ => "unknown",
        };
        let trait_name = &imp.trait_name;
        let type_id = self.type_ids.get(target).copied().unwrap_or_else(|| Self::type_id_of(target));
        self.emitln(&format!("// impl {} for {} (id={})", trait_name, target, type_id));
        for method in &imp.methods {
            let ret = self.gen_type(&method.return_type);
            // Generate impl function with concrete type (not void*)
            let impl_params: Vec<String> = method.params.iter()
                .map(|p| format!("{} {}", self.gen_type(&p.ty), p.name)).collect();
            self.emit(&format!("inline {} {}__{}({}) {{", ret, target, method.name, impl_params.join(", ")));
            self.indent += 1;
            self.emit_block(&method.body, !matches!(method.return_type, Type::Unit) && self.gen_type(&method.return_type) != "void");
            self.indent -= 1;
            self.emitln("}");
            // Vtable trampoline: void* → concrete
            self.emit(&format!("inline {} {}__{}_vt(void* self", ret, target, method.name));
            for p in method.params.iter().skip(1) {
                self.emit(&format!(", {} {}", self.gen_type(&p.ty), p.name));
            }
            self.emit(&format!(") {{ return {}__{}(*({}*)self", target, method.name, target));
            for p in method.params.iter().skip(1) {
                self.emit(&format!(", {}", p.name));
            }
            self.emitln("); }");
        }
        self.emitln(&format!("inline {}_vtable __vt_{}_{} = {{", trait_name, trait_name, target));
        self.emitln(&format!("  {},", type_id));
        for method in &imp.methods {
            self.emitln(&format!("  (decltype({}_vtable::{}))&{}__{}_vt,", trait_name, method.name, target, method.name));
        }
        self.emitln("};");
        self.emitln(&format!("inline {} as_{}({}* p) {{ return {{ p, &__vt_{}_{} }}; }}", trait_name, trait_name, target, trait_name, target));
        // Generate free function wrappers for UFCS
        for method in &imp.methods {
            let ret = self.gen_type(&method.return_type);
            let params: Vec<String> = method.params.iter().map(|p| format!("{} {}", self.gen_type(&p.ty), p.name)).collect();
            self.emit(&format!("inline {} {}({}) {{", ret, method.name, params.join(", ")));
            self.emit(&format!(" return {}__{}(", target, method.name));
            let args: Vec<String> = method.params.iter().map(|p| p.name.clone()).collect();
            self.emit(&format!("{}); }}", args.join(", ")));
        }
        self.emitln("");
    }

    fn emit_function(&mut self, func: &Function) {
        self.return_trait = self.trait_name(&func.return_type);
        let ret = self.gen_type(&func.return_type);
        let params: Vec<String> = func.params.iter().map(|p| format!("{} {}", self.gen_type(&p.ty), p.name)).collect();
        self.emitln(&format!("{} {}({}) {{", ret, func.name, params.join(", ")));
        self.indent += 1;
        self.emit_block(&func.body, !matches!(func.return_type, Type::Unit) && self.gen_type(&func.return_type) != "void");
        self.indent -= 1;
        self.emitln("}");
        self.emitln("");
        self.return_trait = None;
    }

    fn emit_block(&mut self, block: &Block, in_expr_pos: bool) {
        for (i, stmt) in block.stmts.iter().enumerate() {
            self.emit_stmt(stmt, i == block.stmts.len() - 1 && in_expr_pos);
        }
    }

    fn emit_stmt(&mut self, expr: &Expr, is_tail: bool) {
        match &expr.kind {
            ExprKind::Return(Some(e)) => {
                let v = self.coerce(&self.gen_expr(e));
                self.emitln(&format!("return {};", v));
            }
            ExprKind::Return(None) => self.emitln("return;"),
            ExprKind::Let { name, value, .. } => {
                self.emitln(&format!("auto {} = {};", name, self.gen_expr(value)));
            }
            ExprKind::If { cond, then_branch, else_branch } => {
                self.emitln(&format!("if ({}) {{", self.gen_expr(cond)));
                self.indent += 1; self.emit_block(then_branch, is_tail); self.indent -= 1;
                if let Some(eb) = else_branch {
                    self.emitln("} else {");
                    self.indent += 1; self.emit_block(eb, is_tail); self.indent -= 1;
                }
                self.emitln("}");
            }
            ExprKind::Binary { op, left, right } => {
                let s = format!("({} {} {})", self.gen_expr(left), op.cpp_op(), self.gen_expr(right));
                if is_tail { self.emitln(&format!("return {};", s)); } else { self.emitln(&format!("{};", s)); }
            }
            _ => {
                let v = self.gen_expr(expr);
                if is_tail { self.emitln(&format!("return {};", v)); } else { self.emitln(&format!("{};", v)); }
            }
        }
    }

    fn call_name(&self, func: &Expr, args: &[String]) -> String {
        let f = self.gen_expr(func);
        let name = if let ExprKind::Ident(n) = &func.kind { n.clone() } else { f.clone() };
        let mapped = self.map_runtime(&name);
        // Overloaded on traits → runtime multiple dispatch (any arity).
        // Statically-concrete call sites fall through _d_'s template overload
        // back to plain C++ overload resolution.
        if self.dispatch.contains_key(&name) {
            return format!("_d_{}({})", name, args.join(", "));
        }
        // Single-impl trait methods → fat pointer vtable call
        if args.len() == 1 && self.trait_methods.contains(&name) {
            return format!("{}.{}()", args[0], name);
        }
        format!("{}({})", mapped, args.join(", "))
    }

    fn gen_expr(&self, expr: &Expr) -> String {
        match &expr.kind {
            ExprKind::IntLiteral(n) => n.to_string(),
            ExprKind::FloatLiteral(n) => format!("{}", n),
            ExprKind::StringLiteral(s) => format!("\"{}\"", s),
            ExprKind::BoolLiteral(b) => if *b { "true".into() } else { "false".into() },
            ExprKind::Ident(name) => name.clone(),
            ExprKind::Field { object, field } => format!("{}.{}", self.gen_expr(object), field),
            ExprKind::DotAccess { object, field } => format!("{}.{}", self.gen_expr(object), field),
            ExprKind::Binary { op, left, right } =>
                format!("({} {} {})", self.gen_expr(left), op.cpp_op(), self.gen_expr(right)),
            ExprKind::Call { func, args } => {
                let a: Vec<String> = args.iter().map(|a| self.gen_expr(a)).collect();
                self.call_name(func, &a)
            }
            ExprKind::StructLit { path, fields } => {
                let tn = path.join("::");
                let vals: Vec<String> = fields.iter().map(|(_, v)| self.gen_expr(v)).collect();
                format!("{} {{ {} }}", tn, vals.join(", "))
            }
            _ => format!("/* {:?} */", expr.kind),
        }
    }

    fn gen_type(&self, ty: &Type) -> String {
        match ty {
            Type::Path(p) => p.segments.iter().map(|s| {
                if s.args.is_empty() { self.map_primitive(&s.name).to_string() }
                else { format!("{}<{}>", s.name, s.args.iter().map(|a| self.gen_type(a)).collect::<Vec<_>>().join(", ")) }
            }).collect::<Vec<_>>().join("::"),
            Type::Unit => "void".to_string(),
            _ => "auto".to_string(),
        }
    }

    fn map_primitive<'a>(&self, name: &'a str) -> &'a str {
        match name { "Int" => "int", "Float" => "double", "Bool" => "bool", "Char" => "char", "String" => "std::string", _ => name }
    }

    fn map_runtime(&self, name: &str) -> String {
        match name { "print" => "nova_print".into(), "print_int" => "nova_print_int".into(), _ => name.to_string() }
    }

    fn trait_name(&self, ty: &Type) -> Option<String> {
        match ty { Type::Path(p) if !Self::is_primitive(p) => Some(p.segments.last().unwrap().name.clone()), _ => None }
    }

    fn coerce(&self, expr: &str) -> String {
        match &self.return_trait { Some(t) => format!("as_{}(new {})", t, expr), None => expr.to_string() }
    }

    /// Table + inline cache + dynamic entry point for every dispatched name.
    /// The table maps packed type_id keys to the most specific overload for
    /// that concrete type combination; unpacking casts trait-position args to
    /// the concrete type the chosen overload expects.
    fn emit_dispatch_wrappers(&mut self) {
        for (name, info) in self.dispatch.clone() {
            let ret = self.gen_type(&info.ret);
            let arity = info.positions.len();
            let ptypes = self.dispatch_param_types(&info);
            let pdecls: Vec<String> = ptypes.iter().enumerate().map(|(i, t)| format!("{} a{}", t, i)).collect();
            let anames: Vec<String> = (0..arity).map(|i| format!("a{}", i)).collect();
            let dpos: Vec<(usize, String)> = info.positions.iter().enumerate()
                .filter_map(|(i, p)| p.as_ref().map(|t| (i, t.clone()))).collect();

            self.emitln(&format!("// ─── dispatch: {} ({}-ary over {}) ───",
                name, dpos.len(),
                dpos.iter().map(|(_, t)| t.as_str()).collect::<Vec<_>>().join(", ")));
            self.emitln(&format!("static std::unordered_map<uint64_t, _fn_{0}> _t_{0};", name));
            self.emitln(&format!("static struct {{ uint64_t k = UINT64_MAX; _fn_{0} fn = nullptr; }} _c_{0};", name));
            self.emitln(&format!("static struct _Reg_{0} {{ _Reg_{0}() {{", name));
            let pools: Vec<Vec<String>> = dpos.iter()
                .map(|(_, t)| self.trait_impls.get(t).cloned().unwrap_or_default()).collect();
            for combo in Self::cartesian(&pools) {
                let ov = match Self::select_overload(&info, &combo) { Some(o) => o, None => continue };
                let mut key: u64 = 0;
                for (j, ty) in combo.iter().enumerate() {
                    key |= (self.type_ids[ty] as u64) << (16 * j);
                }
                let mut cargs = Vec::new();
                let mut j = 0;
                for (i, pos) in info.positions.iter().enumerate() {
                    if pos.is_some() {
                        if Self::last_name(&ov.params[i]) == Some(combo[j].as_str()) {
                            cargs.push(format!("*({}*)a{}._ptr", combo[j], i));
                        } else {
                            cargs.push(format!("a{}", i));
                        }
                        j += 1;
                    } else {
                        cargs.push(format!("a{}", i));
                    }
                }
                self.emitln(&format!("  _t_{}[{}ULL] = []({}) -> {} {{ return {}({}); }};",
                    name, key, pdecls.join(", "), ret, name, cargs.join(", ")));
            }
            self.emitln(&format!("}} }} _reg_{};", name));

            self.emitln(&format!("{} _d_{}({}) {{", ret, name, pdecls.join(", ")));
            let keyexpr: Vec<String> = dpos.iter().enumerate()
                .map(|(j, (i, _))| format!("((uint64_t)a{}._vt->type_id << {})", i, 16 * j)).collect();
            self.emitln(&format!("  uint64_t k = {};", keyexpr.join(" | ")));
            self.emitln(&format!("  if (k == _c_{0}.k) [[likely]] return _c_{0}.fn({1});", name, anames.join(", ")));
            self.emitln(&format!("  auto it = _t_{0}.find(k);", name));
            self.emitln(&format!("  if (it != _t_{0}.end()) {{ _c_{0} = {{k, it->second}}; return it->second({1}); }}", name, anames.join(", ")));
            if info.has_generic_fallback {
                self.emitln(&format!("  return {}({});", name, anames.join(", ")));
            } else {
                self.emitln(&format!("  std::cerr << \"nova: no matching method for '{}'\" << std::endl; std::abort();", name));
            }
            self.emitln("}");
            self.emitln("");
        }
    }

    fn emit(&mut self, s: &str) { self.output.push_str(s); }
    fn emitln(&mut self, s: &str) {
        for _ in 0..self.indent { self.output.push_str("  "); }
        self.output.push_str(s); self.output.push('\n');
    }
}
