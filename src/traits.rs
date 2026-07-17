// traits.rs — Multiple dispatch trait system
// Fat pointers by default. @sealed for compile-time monomorphization.
// Per-call-site cache for open, N-arg dispatch.

use crate::ast::*;
use crate::token::Span;
use std::collections::HashMap;

#[derive(Debug, Clone)]
pub struct TraitInfo {
    pub name: String,
    pub generics: Vec<String>,
    pub methods: Vec<Function>,
}

pub struct TraitRegistry {
    pub traits: HashMap<String, TraitInfo>,
    // (trait_name, impl_type) → methods
    pub impls: HashMap<(String, String), Vec<Function>>,
    // Per-method: set of (impl_type1, impl_type2, ...) tuples that have specializations
    pub specializations: HashMap<String, Vec<Vec<String>>>,
}

impl TraitRegistry {
    pub fn new() -> Self {
        Self { traits: HashMap::new(), impls: HashMap::new(), specializations: HashMap::new() }
    }

    pub fn register_trait(&mut self, t: TraitInfo) {
        self.traits.insert(t.name.clone(), t);
    }

    pub fn register_impl(&mut self, trait_name: &str, target: &str, methods: Vec<Function>) {
        self.impls.insert((trait_name.to_string(), target.to_string()), methods);
    }

    pub fn is_concrete(&self, ty: &Type) -> bool {
        match ty {
            Type::Path(ref p) => {
                let name = p.segments.last().map(|s| s.name.as_str()).unwrap_or("");
                !self.traits.contains_key(name)
            }
            _ => true,
        }
    }

    /// Generate C++ for a trait: vtable + fat pointer struct
    pub fn codegen_trait_decl(&self, name: &str) -> String {
        let info = match self.traits.get(name) {
            Some(t) => t,
            None => return String::new(),
        };
        let mut out = String::new();
        out.push_str(&format!("// ─── trait {} ───\n", name));
        out.push_str(&format!("struct {}_vtable {{\n", name));
        out.push_str("  uint32_t type_id;\n");
        for m in &info.methods {
            let ret = self.cpp_type(&m.return_type);
            let params: Vec<String> = m.params.iter()
                .skip(1).map(|p| format!("{} {}", self.cpp_type(&p.ty), p.name)).collect();
            out.push_str(&format!("  {} (*{})(void*{}{});\n", ret, m.name,
                if params.is_empty() { "".into() } else { ", ".to_string() },
                params.join(", ")
            ));
        }
        out.push_str("};\n\n");

        // Fat pointer
        out.push_str(&format!("struct {} {{\n", name));
        out.push_str("  void* _ptr;\n");
        out.push_str(&format!("  {}_vtable* _vt;\n", name));
        for m in &info.methods {
            let ret = self.cpp_type(&m.return_type);
            let param_names: Vec<String> = m.params.iter().skip(1).map(|p| p.name.clone()).collect();
            let param_decls: Vec<String> = m.params.iter().skip(1)
                .map(|p| format!("{} {}", self.cpp_type(&p.ty), p.name)).collect();
            out.push_str(&format!("  {} {}({}) {{\n    return _vt->{}(_ptr{});\n  }}\n",
                ret, m.name,
                param_decls.join(", "),
                m.name,
                if param_names.is_empty() { "".into() } else {
                    format!(", {}", param_names.join(", "))
                }
            ));
        }
        out.push_str("};\n\n");
        out
    }

    /// Generate C++ for an impl block: vtable instantiation + constructor
    pub fn codegen_impl(&self, trait_name: &str, impl_type: &str) -> String {
        let mut out = String::new();
        let info = match self.traits.get(trait_name) {
            Some(t) => t,
            None => return out,
        };
        out.push_str(&format!("// impl {} for {}\n", trait_name, impl_type));

        // Vtable instance
        out.push_str(&format!("inline {}_vtable __{}_vt_{} = {{\n", trait_name, trait_name, impl_type));
        out.push_str(&format!("  {}_type_id,\n", impl_type));
        for m in &info.methods {
            out.push_str(&format!("  ({} (*)(void*{}))&{}__{},\n",
                self.cpp_type(&m.return_type),
                m.params.iter().skip(1).map(|p| format!(", {}", self.cpp_type(&p.ty))).collect::<Vec<_>>().join(""),
                impl_type, m.name
            ));
        }
        out.push_str("};\n\n");

        // Constructor: wrap in fat pointer
        out.push_str(&format!("inline {} as_{}({}* p) {{\n", trait_name, trait_name, impl_type));
        out.push_str(&format!("  return {{ p, &__{}_vt_{} }};\n", trait_name, impl_type));
        out.push_str("}\n\n");
        out
    }

    /// Generate a multiple-dispatch call site for a function
    pub fn codegen_dispatch_call(&self, fn_name: &str, trait_args: &[(String, String)]) -> String {
        let mut out = String::new();
        let n = trait_args.len();
        out.push_str(&format!("// Multi-dispatch: {}(\n", fn_name));
        for (name, trait_) in trait_args {
            out.push_str(&format!("//   {}: {}\n", name, trait_));
        }
        out.push_str("// )\n");

        // Cache struct
        out.push_str("static struct {\n");
        out.push_str("  uint64_t key = 0;\n");
        out.push_str("  auto* fn = (decltype(&dispatch_impl_0))nullptr;\n");
        out.push_str("} _cache;\n\n");

        // Dispatch function
        out.push_str(&format!("auto {}(", fn_name));
        for (i, (name, trait_)) in trait_args.iter().enumerate() {
            if i > 0 { out.push_str(", "); }
            out.push_str(&format!("{}& {}", trait_, name));
        }
        out.push_str(") {\n");

        // Build key from type_ids
        out.push_str("  uint64_t k = 0;\n");
        for (i, (name, _)) in trait_args.iter().enumerate() {
            out.push_str(&format!("  k |= (uint64_t){}._vt->type_id << ({} * 16);\n", name, i));
        }

        // Fast path
        out.push_str("  if (k == _cache.key) [[likely]] return _cache.fn(");
        for (i, (name, _)) in trait_args.iter().enumerate() {
            if i > 0 { out.push_str(", "); }
            out.push_str(&format!("{}._ptr", name));
        }
        out.push_str(");\n");

        // Slow path: hash table lookup
        out.push_str("  auto it = _dispatch_table.find(k);\n");
        out.push_str("  if (it != _dispatch_table.end()) {\n");
        out.push_str("    _cache.key = k;\n");
        out.push_str("    _cache.fn = it->second;\n");
        out.push_str("    return _cache.fn(");
        for (i, (name, _)) in trait_args.iter().enumerate() {
            if i > 0 { out.push_str(", "); }
            out.push_str(&format!("{}._ptr", name));
        }
        out.push_str(");\n");
        out.push_str("  }\n");
        out.push_str("  throw std::runtime_error(\"No matching dispatch\");\n");
        out.push_str("}\n\n");
        out
    }

    fn cpp_type(&self, ty: &Type) -> String {
        match ty {
            Type::Path(p) => {
                let name = p.segments.iter().map(|s| s.name.as_str()).collect::<Vec<_>>().join("::");
                match name.as_str() {
                    "Int" | "Int32" => "int32_t".into(),
                    "Float" => "double".into(),
                    "String" => "std::string".into(),
                    "Bool" => "bool".into(),
                    "Unit" | "()" => "void".into(),
                    _ => name,
                }
            }
            _ => "auto".into(),
        }
    }
}
