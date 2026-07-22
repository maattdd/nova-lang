use crate::ast::*;
use std::collections::{BTreeMap, HashMap, HashSet};

/// One overload of a function (free function or impl method).
#[derive(Debug, Clone)]
struct Overload {
    params: Vec<Param>,
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
    // trait name → concrete impl targets, in declaration order
    trait_impls: BTreeMap<String, Vec<String>>,
    type_ids: HashMap<String, u32>,
    // every overload set, for named-argument reordering
    fn_sigs: HashMap<String, Vec<Overload>>,
    // enum case name → (enum name, tag index, has payload, enum is generic)
    enum_cases: HashMap<String, (String, usize, bool, bool)>,
    // structs allocated via @T — they get a nova::GCObject base
    gc_structs: HashSet<String>,
}

impl CppGenerator {
    pub fn new() -> Self {
        Self {
            output: String::new(), indent: 0, dispatch: BTreeMap::new(),
            return_trait: None, trait_names: HashSet::new(),
            trait_impls: BTreeMap::new(), type_ids: HashMap::new(),
            fn_sigs: HashMap::new(), enum_cases: HashMap::new(), gc_structs: HashSet::new(),
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
                params: f.params.clone(),
                ret: f.return_type.clone(),
            });
        };
        for item in &module.items {
            match item {
                Item::Function(f) => {
                    add(f, &mut names, &mut sets);
                    self.scan_gc_function(f);
                }
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
                    for m in &imp.methods {
                        add(m, &mut names, &mut sets);
                        self.scan_gc_function(m);
                    }
                }
                Item::Enum(e) => {
                    for (i, case) in e.cases.iter().enumerate() {
                        self.enum_cases.insert(case.name.clone(),
                            (e.name.clone(), i, case.payload.is_some(), !e.generics.is_empty()));
                    }
                }
                Item::Struct(s) => {
                    for f in &s.fields { self.note_gc_type(&f.ty); }
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
                    .filter_map(|o| Self::last_name(&o.params[i].ty))
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
                ovs.iter().all(|o| self.gen_type(&o.params[i].ty) == self.gen_type(&ovs[0].params[i].ty)));
            if !fixed_ok { continue; }
            let has_generic_fallback = ovs.iter().any(|o| positions.iter().enumerate().all(|(i, pos)| {
                match pos {
                    Some(t) => Self::last_name(&o.params[i].ty) == Some(t.as_str()),
                    None => true,
                }
            }));
            self.dispatch.insert(name, DispatchInfo {
                positions,
                param_types: ovs[0].params.iter().map(|p| p.ty.clone()).collect(),
                ret: ovs[0].ret.clone(),
                overloads: ovs.clone(),
                has_generic_fallback,
            });
        }
        self.fn_sigs = sets;
    }

    /// Record GC usage in a function: @T types in the signature and @T{...}
    /// allocations in the body.
    fn scan_gc_function(&mut self, f: &Function) {
        for p in &f.params { self.note_gc_type(&p.ty); }
        self.note_gc_type(&f.return_type);
        for stmt in &f.body.stmts { self.scan_gc_expr(stmt); }
    }

    fn note_gc_type(&mut self, ty: &Type) {
        if let Type::GcRef(inner) = ty {
            if let Some(n) = Self::last_name(inner) { self.gc_structs.insert(n.to_string()); }
        }
    }

    fn scan_gc_expr(&mut self, expr: &Expr) {
        match &expr.kind {
            ExprKind::GcNew { ty, fields } => {
                if let Some(n) = Self::last_name(ty) { self.gc_structs.insert(n.to_string()); }
                for (_, e) in fields { self.scan_gc_expr(e); }
            }
            ExprKind::Let { value, .. } | ExprKind::Return(Some(value))
            | ExprKind::Unary { expr: value, .. } | ExprKind::NamedArg { value, .. } => self.scan_gc_expr(value),
            ExprKind::Binary { left, right, .. } | ExprKind::Assign { target: left, value: right }
            | ExprKind::AssignOp { target: left, value: right, .. }
            | ExprKind::Index { object: left, index: right } => {
                self.scan_gc_expr(left); self.scan_gc_expr(right);
            }
            ExprKind::Call { func, args } => {
                self.scan_gc_expr(func);
                for a in args { self.scan_gc_expr(a); }
            }
            ExprKind::Field { object, .. } | ExprKind::DotAccess { object, .. } => self.scan_gc_expr(object),
            ExprKind::Block(b) => for s in &b.stmts { self.scan_gc_expr(s); },
            ExprKind::If { cond, then_branch, else_branch } => {
                self.scan_gc_expr(cond);
                for s in &then_branch.stmts { self.scan_gc_expr(s); }
                if let Some(eb) = else_branch { for s in &eb.stmts { self.scan_gc_expr(s); } }
            }
            ExprKind::While { cond, body } => {
                self.scan_gc_expr(cond);
                for s in &body.stmts { self.scan_gc_expr(s); }
            }
            ExprKind::For { iter, body, .. } => {
                self.scan_gc_expr(iter);
                for s in &body.stmts { self.scan_gc_expr(s); }
            }
            ExprKind::StructLit { fields, .. } => for (_, e) in fields { self.scan_gc_expr(e); },
            ExprKind::Match { expr: m, arms } => {
                self.scan_gc_expr(m);
                for arm in arms { self.scan_gc_expr(&arm.body); }
            }
            _ => {}
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
                    match Self::last_name(&ov.params[i].ty) {
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
        self.emitln("inline void nova_println(const std::string& s) { std::cout << s << std::endl; }");
        self.emitln("");
    }

    fn emit_module(&mut self, module: &Module) {
        // Structs, enums, and traits first: everything else references them.
        for item in &module.items {
            match item {
                Item::Struct(s) => self.emit_struct(s),
                Item::Enum(e) => self.emit_enum(e),
                Item::Trait(t) => self.emit_trait(t),
                _ => {}
            }
        }
        // Forward declarations before impls/functions, so any body can call
        // any function (or _d_* dispatcher) regardless of emission order.
        self.emit_fn_decls();
        self.emit_dispatch_decls();
        for item in &module.items {
            if let Item::Impl(imp) = item { self.emit_impl(imp); }
        }
        for item in &module.items {
            if let Item::Function(f) = item { self.emit_function(f); }
        }
    }

    /// Forward declarations for every user function and impl-method wrapper.
    fn emit_fn_decls(&mut self) {
        if self.fn_sigs.is_empty() { return; }
        self.emitln("// ─── function declarations ───");
        let mut names: Vec<&String> = self.fn_sigs.keys().collect();
        names.sort();
        let mut decls = Vec::new();
        for name in names {
            if name == "main" { continue; }
            for ov in &self.fn_sigs[name] {
                let ps: Vec<String> = ov.params.iter().map(|p| self.gen_type(&p.ty)).collect();
                decls.push(format!("{} {}({});", self.gen_type(&ov.ret), Self::safe_ident(name), ps.join(", ")));
            }
        }
        for d in decls { self.emitln(&d); }
        self.emitln("");
    }

    /// Declarations for dispatched functions: a function-pointer alias, the
    /// dynamic entry point, and a variadic template that forwards calls with
    /// statically-concrete arguments to plain C++ overload resolution.
    fn emit_dispatch_decls(&mut self) {
        if self.dispatch.is_empty() { return; }
        self.emitln("// ─── dynamic dispatch declarations ───");
        for (name, info) in self.dispatch.clone() {
            let ret = self.gen_type(&info.ret);
            let ptypes = self.dispatch_param_types(&info);
            self.emitln(&format!("using _fn_{} = {}(*)({});", name, ret, ptypes.join(", ")));
            self.emitln(&format!("{} _d_{}({});", ret, name, ptypes.join(", ")));
            self.emitln(&format!(
                "template<typename... Ts> auto _d_{}(Ts&&... xs) {{ return {}(std::forward<Ts>(xs)...); }}",
                name, Self::safe_ident(&name)
            ));
        }
        self.emitln("");
    }

    fn emit_struct(&mut self, s: &Struct) {
        let is_gc = self.gc_structs.contains(&s.name);
        let base = if is_gc { " : public nova::GCObject" } else { "" };
        self.emitln(&format!("struct {}{} {{", s.name, base));
        for field in &s.fields {
            self.emitln(&format!("  {} {};", self.gen_type(&field.ty), Self::safe_ident(&field.name)));
        }
        let args: Vec<String> = s.fields.iter().map(|f| format!("{} {}", self.gen_type(&f.ty), Self::safe_ident(&f.name))).collect();
        let inits: Vec<String> = s.fields.iter().map(|f| format!("{0}({0})", Self::safe_ident(&f.name))).collect();
        if !args.is_empty() {
            self.emitln(&format!("  {}({}) : {} {{}}", s.name, args.join(", "), inits.join(", ")));
        }
        if is_gc {
            self.emitln("  void gc_mark(nova::GC& gc) override {");
            for field in &s.fields {
                if matches!(field.ty, Type::GcRef(_)) {
                    let fname = Self::safe_ident(&field.name);
                    self.emitln(&format!("    if ({}) gc.mark_object({}.get());", fname, fname));
                }
            }
            self.emitln("  }");
        }
        self.emitln("};");
        self.emitln("");
    }

    /// Enums compile to a tagged struct: one member per payload case and
    /// static constructor functions per case.
    fn emit_enum(&mut self, e: &Enum) {
        self.emitln(&format!("// ─── enum {} ───", e.name));
        if !e.generics.is_empty() {
            let tps: Vec<String> = e.generics.iter().map(|g| format!("typename {}", g.name)).collect();
            self.emitln(&format!("template<{}>", tps.join(", ")));
        }
        self.emitln(&format!("struct {} {{", e.name));
        self.emitln("  int _tag = 0;");
        for case in &e.cases {
            if let Some(ref payload) = case.payload {
                self.emitln(&format!("  {} _{}{{}};", self.gen_type(payload), case.name));
            }
        }
        for (tag, case) in e.cases.iter().enumerate() {
            match &case.payload {
                Some(payload) => self.emitln(&format!(
                    "  static {0} {1}({2} v) {{ {0} e; e._tag = {3}; e._{4} = v; return e; }}",
                    e.name, Self::safe_ident(&case.name), self.gen_type(payload), tag, case.name)),
                None => self.emitln(&format!(
                    "  static {0} {1}() {{ {0} e; e._tag = {2}; return e; }}",
                    e.name, Self::safe_ident(&case.name), tag)),
            }
        }
        self.emitln("};");
        // Constructor helpers for generic enums: static members of a template
        // can't deduce the instantiation, so payload cases get a deducing free
        // function and payload-less cases get a converting tag (like nullopt).
        if !e.generics.is_empty() {
            let tps: Vec<String> = e.generics.iter().map(|g| format!("typename {}", g.name)).collect();
            let targs: Vec<String> = e.generics.iter().map(|g| g.name.clone()).collect();
            let (tps, targs) = (tps.join(", "), targs.join(", "));
            for case in &e.cases {
                match &case.payload {
                    Some(payload) => self.emitln(&format!(
                        "template<{tps}> {en}<{ta}> {en}__{c}({pt} v) {{ return {en}<{ta}>::{sc}(v); }}",
                        tps = tps, ta = targs, en = e.name, c = case.name,
                        pt = self.gen_type(payload), sc = Self::safe_ident(&case.name))),
                    None => self.emitln(&format!(
                        "struct {en}__{c}_t {{ template<{tps}> operator {en}<{ta}>() const {{ return {en}<{ta}>::{sc}(); }} }};",
                        tps = tps, ta = targs, en = e.name, c = case.name,
                        sc = Self::safe_ident(&case.name))),
                }
            }
        }
        self.emitln("");
    }

    fn emit_trait(&mut self, t: &TraitDef) {
        self.emitln(&format!("// ─── trait {} ───", t.name));
        self.emitln(&format!("struct {}_vtable {{", t.name));
        self.emitln("  uint32_t type_id;");
        for m in &t.methods {
            let ret = self.gen_type(&m.return_type);
            let params: Vec<String> = m.params.iter().skip(1).map(|p| format!("{} {}", self.gen_type(&p.ty), Self::safe_ident(&p.name))).collect();
            self.emitln(&format!("  {} (*{})(void*{});", ret, m.name, if params.is_empty() { "".into() } else { format!(", {}", params.join(", ")) }));
        }
        self.emitln("};");
        self.emitln(&format!("struct {} {{", t.name));
        self.emitln("  void* _ptr;");
        self.emitln(&format!("  {}_vtable* _vt;", t.name));
        for m in &t.methods {
            let ret = self.gen_type(&m.return_type);
            let pn: Vec<String> = m.params.iter().skip(1).map(|p| Self::safe_ident(&p.name)).collect();
            let pd: Vec<String> = m.params.iter().skip(1).map(|p| format!("{} {}", self.gen_type(&p.ty), Self::safe_ident(&p.name))).collect();
            self.emitln(&format!("  {} {}({}) {{ return _vt->{}(_ptr{}); }}", ret, m.name, pd.join(", "), m.name, if pn.is_empty() { "".into() } else { format!(", {}", pn.join(", ")) }));
        }
        self.emitln("};");
        // Free-function overload on the fat pointer, so UFCS calls always
        // compile to plain overloaded calls regardless of receiver type.
        for m in &t.methods {
            let ret = self.gen_type(&m.return_type);
            let pn: Vec<String> = m.params.iter().skip(1).map(|p| Self::safe_ident(&p.name)).collect();
            let pd: Vec<String> = m.params.iter().skip(1).map(|p| format!("{} {}", self.gen_type(&p.ty), Self::safe_ident(&p.name))).collect();
            self.emitln(&format!("inline {} {}({} self{}) {{ return self.{}({}); }}",
                ret, Self::safe_ident(&m.name), t.name,
                if pd.is_empty() { "".into() } else { format!(", {}", pd.join(", ")) },
                m.name, pn.join(", ")));
        }
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
                .map(|p| format!("{} {}", self.gen_type(&p.ty), Self::safe_ident(&p.name))).collect();
            self.emit(&format!("inline {} {}__{}({}) {{", ret, target, method.name, impl_params.join(", ")));
            self.indent += 1;
            self.emit_block(&method.body, !matches!(method.return_type, Type::Unit) && self.gen_type(&method.return_type) != "void");
            self.indent -= 1;
            self.emitln("}");
            // Vtable trampoline: void* → concrete
            self.emit(&format!("inline {} {}__{}_vt(void* self", ret, target, method.name));
            for p in method.params.iter().skip(1) {
                self.emit(&format!(", {} {}", self.gen_type(&p.ty), Self::safe_ident(&p.name)));
            }
            self.emit(&format!(") {{ return {}__{}(*({}*)self", target, method.name, target));
            for p in method.params.iter().skip(1) {
                self.emit(&format!(", {}", Self::safe_ident(&p.name)));
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
            let params: Vec<String> = method.params.iter().map(|p| format!("{} {}", self.gen_type(&p.ty), Self::safe_ident(&p.name))).collect();
            self.emit(&format!("inline {} {}({}) {{", ret, Self::safe_ident(&method.name), params.join(", ")));
            self.emit(&format!(" return {}__{}(", target, method.name));
            let args: Vec<String> = method.params.iter().map(|p| Self::safe_ident(&p.name)).collect();
            self.emit(&format!("{}); }}", args.join(", ")));
        }
        self.emitln("");
    }

    fn emit_function(&mut self, func: &Function) {
        self.return_trait = self.trait_name(&func.return_type)
            .filter(|n| self.trait_names.contains(n));
        let ret = self.gen_type(&func.return_type);
        let params: Vec<String> = func.params.iter().map(|p| format!("{} {}", self.gen_type(&p.ty), Self::safe_ident(&p.name))).collect();
        self.emitln(&format!("{} {}({}) {{", ret, Self::safe_ident(&func.name), params.join(", ")));
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
            ExprKind::Let { name, ty, value, .. } => {
                let decl = ty.as_ref().map(|t| self.gen_type(t)).unwrap_or_else(|| "auto".into());
                self.emitln(&format!("{} {} = {};", decl, Self::safe_ident(name), self.gen_expr(value)));
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
            ExprKind::While { cond, body } => {
                self.emitln(&format!("while ({}) {{", self.gen_expr(cond)));
                self.indent += 1; self.emit_block(body, false); self.indent -= 1;
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
        format!("{}({})", mapped, args.join(", "))
    }

    fn gen_expr(&self, expr: &Expr) -> String {
        match &expr.kind {
            ExprKind::IntLiteral(n) => n.to_string(),
            ExprKind::FloatLiteral(n) => format!("{}", n),
            ExprKind::StringLiteral(s) => format!("std::string(\"{}\")", Self::escape_cpp_string(s)),
            ExprKind::BoolLiteral(b) => if *b { "true".into() } else { "false".into() },
            ExprKind::Ident(name) => Self::safe_ident(name),
            ExprKind::Field { object, field } => format!("{}.{}", self.gen_expr(object), Self::safe_ident(field)),
            ExprKind::DotAccess { object, field } => format!("{}.{}", self.gen_expr(object), Self::safe_ident(field)),
            ExprKind::Binary { op, left, right } =>
                format!("({} {} {})", self.gen_expr(left), op.cpp_op(), self.gen_expr(right)),
            ExprKind::Call { func, args } => {
                let ordered = self.order_call_args(func, args);
                let a: Vec<String> = ordered.iter().map(|a| self.gen_expr(a)).collect();
                self.call_name(func, &a)
            }
            ExprKind::NamedArg { value, .. } => self.gen_expr(value),
            ExprKind::Assign { target, value } => format!("{} = {}", self.gen_expr(target), self.gen_expr(value)),
            ExprKind::CppBlock(src) => src.trim().to_string(),
            ExprKind::StructLit { path, fields } => {
                let tn = path.join("::");
                let vals: Vec<String> = fields.iter().map(|(_, v)| self.gen_expr(v)).collect();
                format!("{} {{ {} }}", tn, vals.join(", "))
            }
            ExprKind::GcNew { ty, fields } => {
                let vals: Vec<String> = fields.iter().map(|(_, v)| self.gen_expr(v)).collect();
                format!("nova::gc_alloc<{}>({})", self.gen_type(ty), vals.join(", "))
            }
            ExprKind::EnumCtor { path, case, arg } => {
                let a = arg.as_ref().map(|e| self.gen_expr(e)).unwrap_or_default();
                match self.enum_cases.get(case) {
                    // Generic enums go through deduction helpers: payload cases
                    // deduce from the argument, payload-less cases produce a tag
                    // that converts to whatever instantiation the context needs.
                    Some((ename, _, has_payload, true)) => {
                        if *has_payload { format!("{}__{}({})", ename, case, a) }
                        else { format!("{}__{}_t{{}}", ename, case) }
                    }
                    Some((ename, _, _, false)) => format!("{}::{}({})", ename, Self::safe_ident(case), a),
                    None => format!("{}::{}({})", path.join("::"), Self::safe_ident(case), a),
                }
            }
            ExprKind::Match { expr: subject, arms } => self.gen_match(subject, arms),
            _ => format!("/* {:?} */", expr.kind),
        }
    }

    /// Match compiles to an immediately-invoked lambda so it stays an expression.
    fn gen_match(&self, subject: &Expr, arms: &[MatchArm]) -> String {
        let mut s = String::from("([&]() {\n");
        s.push_str(&format!("    auto&& _m = {};\n", self.gen_expr(subject)));
        for arm in arms {
            let body = self.gen_expr(&arm.body);
            let guard = arm.guard.as_ref().map(|g| self.gen_expr(g));
            match &arm.pattern.kind {
                PatternKind::EnumCtor { case, inner, .. } => {
                    if let Some((_, tag, has_payload, _)) = self.enum_cases.get(case) {
                        s.push_str(&format!("    if (_m._tag == {}) {{\n", tag));
                        if *has_payload {
                            if let Some(p) = inner {
                                if let PatternKind::Variable { name, .. } = &p.kind {
                                    s.push_str(&format!("      auto {} = _m._{};\n", Self::safe_ident(name), case));
                                }
                            }
                        }
                        match &guard {
                            Some(g) => s.push_str(&format!("      if ({}) return {};\n    }}\n", g, body)),
                            None => s.push_str(&format!("      return {};\n    }}\n", body)),
                        }
                    } else {
                        s.push_str(&format!("    /* unknown enum case: {} */\n", case));
                    }
                }
                PatternKind::Literal(lit) => {
                    let l = match lit {
                        LiteralPat::Int(n) => n.to_string(),
                        LiteralPat::Float(f) => f.to_string(),
                        LiteralPat::String(v) => format!("std::string(\"{}\")", Self::escape_cpp_string(v)),
                        LiteralPat::Char(c) => format!("'{}'", c),
                        LiteralPat::Bool(b) => b.to_string(),
                        LiteralPat::Nil => "nullptr".into(),
                    };
                    match &guard {
                        Some(g) => s.push_str(&format!("    if (_m == {} && ({})) return {};\n", l, g, body)),
                        None => s.push_str(&format!("    if (_m == {}) return {};\n", l, body)),
                    }
                }
                PatternKind::Variable { name, .. } => {
                    s.push_str(&format!("    {{ auto {} = _m;\n", Self::safe_ident(name)));
                    match &guard {
                        Some(g) => s.push_str(&format!("      if ({}) return {};\n    }}\n", g, body)),
                        None => s.push_str(&format!("      return {};\n    }}\n", body)),
                    }
                }
                PatternKind::Wildcard => {
                    match &guard {
                        Some(g) => s.push_str(&format!("    if ({}) return {};\n", g, body)),
                        None => s.push_str(&format!("    return {};\n", body)),
                    }
                }
                other => s.push_str(&format!("    /* unsupported pattern: {:?} */\n", other)),
            }
        }
        s.push_str("    std::abort();\n  }())");
        s
    }

    /// Resolve ~name: value arguments into declaration order using the target's
    /// signature; positional calls pass through untouched.
    fn order_call_args<'a>(&self, func: &Expr, args: &'a [Expr]) -> Vec<&'a Expr> {
        let has_named = args.iter().any(|a| matches!(a.kind, ExprKind::NamedArg { .. }));
        if has_named {
            if let ExprKind::Ident(name) = &func.kind {
                if let Some(overloads) = self.fn_sigs.get(name) {
                    for ov in overloads {
                        if let Some(slots) = crate::typeck::assign_arg_slots(&ov.params, args) {
                            return slots.into_iter().map(|ai| &args[ai]).collect();
                        }
                    }
                }
            }
        }
        args.iter().collect()
    }

    fn gen_type(&self, ty: &Type) -> String {
        match ty {
            Type::Path(p) => p.segments.iter().map(|s| {
                if s.args.is_empty() { self.map_primitive(&s.name).to_string() }
                else { format!("{}<{}>", s.name, s.args.iter().map(|a| self.gen_type(a)).collect::<Vec<_>>().join(", ")) }
            }).collect::<Vec<_>>().join("::"),
            Type::GcRef(inner) => format!("nova::gc_ptr<{}>", self.gen_type(inner)),
            Type::Unit => "void".to_string(),
            _ => "auto".to_string(),
        }
    }

    fn map_primitive<'a>(&self, name: &'a str) -> &'a str {
        match name { "Int" => "int", "Float" => "double", "Bool" => "bool", "Char" => "char", "String" => "std::string", _ => name }
    }

    fn map_runtime(&self, name: &str) -> String {
        match name {
            "print" => "nova_print".into(),
            "println" => "nova_println".into(),
            "print_int" => "nova_print_int".into(),
            _ => Self::safe_ident(name),
        }
    }

    /// Nova identifiers that collide with C++ keywords get a trailing underscore.
    fn safe_ident(name: &str) -> String {
        const CPP_KEYWORDS: &[&str] = &[
            "default", "new", "delete", "class", "template", "typename", "this",
            "operator", "private", "public", "protected", "namespace", "union",
            "virtual", "friend", "inline", "mutable", "typedef", "using", "volatile",
            "extern", "register", "signed", "unsigned", "short", "long", "int",
            "float", "double", "char", "bool", "void", "auto", "const", "static",
            "switch", "do", "goto", "try", "catch", "throw", "sizeof", "export",
        ];
        if CPP_KEYWORDS.contains(&name) { format!("{}_", name) } else { name.to_string() }
    }

    fn escape_cpp_string(s: &str) -> String {
        let mut out = String::with_capacity(s.len());
        for c in s.chars() {
            match c {
                '\\' => out.push_str("\\\\"),
                '"' => out.push_str("\\\""),
                '\n' => out.push_str("\\n"),
                '\t' => out.push_str("\\t"),
                '\r' => out.push_str("\\r"),
                _ => out.push(c),
            }
        }
        out
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
                        if Self::last_name(&ov.params[i].ty) == Some(combo[j].as_str()) {
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
                    name, key, pdecls.join(", "), ret, Self::safe_ident(&name), cargs.join(", ")));
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
                self.emitln(&format!("  return {}({});", Self::safe_ident(&name), anames.join(", ")));
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
