use crate::token::Span;
use std::fmt;

#[allow(dead_code)]
// ─── Module ───────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct Module {
    pub name: String,
    pub imports: Vec<Import>,
    pub items: Vec<Item>,
}

#[derive(Debug, Clone)]
pub struct Import {
    pub path: Vec<String>,
    pub items: Vec<ImportItem>,
    pub span: Span,
}

/// What to import from a module
#[derive(Debug, Clone)]
pub enum ImportItem {
    /// import foo.bar — imports everything public
    All,
    /// import foo.{bar, baz} or foo.{bar as qux}
    Single { name: String, alias: Option<String> },
}

/// A top-level declaration item
#[derive(Debug, Clone)]
pub enum Item {
    Function(Function),
    Struct(Struct),
    Enum(Enum),
    Macro(MacroDef),
    TypeAlias(TypeAlias),
    VarDecl(VarDecl), // module-level variables
    MacroCall(MacroCallItem), // @macro_name(args) at module level
    Trait(TraitDef),
    Impl(ImplBlock),
}

// ─── Declarations ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct Function {
    pub name: String,
    pub generics: Vec<GenericParam>,
    pub params: Vec<Param>,
    pub return_type: Type,
    pub body: Block,
    pub is_pub: bool,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct Param {
    pub name: String,
    pub ty: Type,
    pub named: bool,  // ~name
    pub default: Option<Expr>,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct Struct {
    pub name: String,
    pub generics: Vec<GenericParam>,
    pub fields: Vec<StructField>,
    pub is_pub: bool,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct StructField {
    pub name: String,
    pub ty: Type,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct Enum {
    pub name: String,
    pub generics: Vec<GenericParam>,
    pub cases: Vec<EnumCase>,
    pub is_pub: bool,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct EnumCase {
    pub name: String,
    pub payload: Option<Type>,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct TypeAlias {
    pub name: String,
    pub generics: Vec<GenericParam>,
    pub ty: Type,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct VarDecl {
    pub name: String,
    pub ty: Option<Type>,
    pub value: Option<Expr>,
    pub is_mut: bool,
    pub span: Span,
}

// ─── Generics ─────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct GenericParam {
    pub name: String,
    pub span: Span,
}

// ─── Types ────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub enum Type {
    Path(Path),
    GcRef(Box<Type>),         // @T — GC-managed heap reference
    Function(FunctionType),
    Tuple(Vec<Type>),
    Unit,
    Never,
}

#[derive(Debug, Clone)]
pub struct FunctionType {
    pub params: Vec<Type>,
    pub ret: Box<Type>,
}

#[derive(Debug, Clone)]
pub struct Path {
    pub segments: Vec<PathSegment>,
}

#[derive(Debug, Clone)]
pub struct PathSegment {
    pub name: String,
    pub args: Vec<Type>, // generic type arguments
}

impl Path {
    pub fn simple(name: &str) -> Self {
        Path {
            segments: vec![PathSegment {
                name: name.to_string(),
                args: vec![],
            }],
        }
    }
}

impl Type {
    pub fn path(name: &str) -> Self {
        Type::Path(Path::simple(name))
    }

    pub fn gc_ref(inner: Type) -> Self {
        Type::GcRef(Box::new(inner))
    }
}

impl fmt::Display for Type {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Type::Path(path) => write!(f, "{}", path),
            Type::GcRef(inner) => write!(f, "@{}", inner),
            Type::Function(ft) => write!(f, "{}", ft),
            Type::Tuple(types) => {
                write!(f, "(")?;
                for (i, t) in types.iter().enumerate() {
                    if i > 0 { write!(f, ", ")?; }
                    write!(f, "{}", t)?;
                }
                write!(f, ")")
            }
            Type::Unit => write!(f, "()"),
            Type::Never => write!(f, "!"),
        }
    }
}

impl fmt::Display for Path {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (i, seg) in self.segments.iter().enumerate() {
            if i > 0 { write!(f, "::")?; }
            write!(f, "{}", seg.name)?;
            if !seg.args.is_empty() {
                write!(f, "[")?;
                for (j, arg) in seg.args.iter().enumerate() {
                    if j > 0 { write!(f, ", ")?; }
                    write!(f, "{}", arg)?;
                }
                write!(f, "]")?;
            }
        }
        Ok(())
    }
}

impl fmt::Display for FunctionType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "(")?;
        for (i, p) in self.params.iter().enumerate() {
            if i > 0 { write!(f, ", ")?; }
            write!(f, "{}", p)?;
        }
        write!(f, ") -> {}", self.ret)
    }
}

// ─── Expressions ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct Expr {
    pub kind: ExprKind,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub enum ExprKind {
    // Literals
    IntLiteral(i64),
    FloatLiteral(f64),
    StringLiteral(String),
    CharLiteral(char),
    BoolLiteral(bool),
    NilLiteral,

    // Identifiers and paths
    Ident(String),
    Path(Vec<String>), // foo::bar::baz

    // Blocks
    Block(Block),

    // Operations
    Binary { op: BinOp, left: Box<Expr>, right: Box<Expr> },
    Unary { op: UnaryOp, expr: Box<Expr> },
    Call { func: Box<Expr>, args: Vec<Expr> },
    Field { object: Box<Expr>, field: String },
    Index { object: Box<Expr>, index: Box<Expr> },
    
    // Dot access — resolved by type checker to either field or UFCS call
    DotAccess { object: Box<Expr>, field: String },

    // Variable binding
    Let { name: String, ty: Option<Type>, value: Box<Expr>, is_mut: bool },
    Assign { target: Box<Expr>, value: Box<Expr> },
    AssignOp { target: Box<Expr>, op: BinOp, value: Box<Expr> },

    // Control flow
    If { cond: Box<Expr>, then_branch: Block, else_branch: Option<Block> },
    While { cond: Box<Expr>, body: Block },
    For { var: String, iter: Box<Expr>, body: Block },
    Return(Option<Box<Expr>>),

    // Pattern matching
    Match { expr: Box<Expr>, arms: Vec<MatchArm> },

    // GC allocation
    GcNew { ty: Type, fields: Vec<(String, Expr)> },

    // Closure
    Lambda { params: Vec<Param>, return_type: Type, body: Box<Expr> },

    // Struct literal
    StructLit { path: Vec<String>, fields: Vec<(String, Expr)> },

    // Enum constructor call
    EnumCtor { path: Vec<String>, case: String, arg: Option<Box<Expr>> },

    // Macro
    Quote(Vec<Expr>),
    Unquote(Box<Expr>),        // $expr
    UnquoteIdent(String),      // $ident
    MacroCall { name: String, args: Vec<Expr> },

    // For quote-internal items
    FuncDef(Function),

    // Compile-time intrinsics
    CppBlock(String),                                       // @cpp { raw C++ }
    CompileTimeResult(Vec<Item>),                            // result of a compile-time call (splice)
    NamedArg { name: String, value: Box<Expr> },             // ~name: value
}

#[derive(Debug, Clone)]
pub struct Block {
    pub stmts: Vec<Expr>,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct MatchArm {
    pub pattern: Pattern,
    pub guard: Option<Box<Expr>>,
    pub body: Box<Expr>,
}

// ─── Patterns ─────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct Pattern {
    pub kind: PatternKind,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub enum PatternKind {
    Wildcard,
    Literal(LiteralPat),
    Variable { name: String, is_mut: bool },
    EnumCtor { path: Vec<String>, case: String, inner: Option<Box<Pattern>> },
    Struct { path: Vec<String>, fields: Vec<(String, Pattern)> },
    Or(Vec<Pattern>),
}

#[derive(Debug, Clone)]
pub enum LiteralPat {
    Int(i64),
    Float(f64),
    String(String),
    Char(char),
    Bool(bool),
    Nil,
}

// ─── Operators ────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinOp {
    Add, Sub, Mul, Div, Mod,
    Eq, NotEq, Lt, Gt, LtEq, GtEq,
    And, Or,
}

impl BinOp {
    pub fn from_token(kind: &crate::token::TokenKind) -> Option<Self> {
        use crate::token::TokenKind;
        Some(match kind {
            TokenKind::Plus => BinOp::Add,
            TokenKind::Minus => BinOp::Sub,
            TokenKind::Star => BinOp::Mul,
            TokenKind::Slash => BinOp::Div,
            TokenKind::Percent => BinOp::Mod,
            TokenKind::EqEq => BinOp::Eq,
            TokenKind::NotEq => BinOp::NotEq,
            TokenKind::Lt => BinOp::Lt,
            TokenKind::Gt => BinOp::Gt,
            TokenKind::LtEq => BinOp::LtEq,
            TokenKind::GtEq => BinOp::GtEq,
            TokenKind::AndAnd => BinOp::And,
            TokenKind::OrOr => BinOp::Or,
            _ => return None,
        })
    }

    pub fn cpp_op(&self) -> &str {
        match self {
            BinOp::Add => "+",
            BinOp::Sub => "-",
            BinOp::Mul => "*",
            BinOp::Div => "/",
            BinOp::Mod => "%",
            BinOp::Eq => "==",
            BinOp::NotEq => "!=",
            BinOp::Lt => "<",
            BinOp::Gt => ">",
            BinOp::LtEq => "<=",
            BinOp::GtEq => ">=",
            BinOp::And => "&&",
            BinOp::Or => "||",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnaryOp {
    Neg,  // -
    Not,  // !
}

// ─── Macro Definition ─────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct MacroDef {
    pub name: String,
    pub params: Vec<String>,
    pub body: Box<Expr>, // should be a block containing quote expressions
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct MacroCallItem {
    pub name: String,
    pub args: Vec<Expr>,
    pub span: Span,
}

// ─── Helpers for building AST nodes ───────────────────────────────────────────

impl Expr {
    pub fn new(kind: ExprKind, span: Span) -> Self {
        Self { kind, span }
    }

    pub fn ident(name: &str, span: Span) -> Self {
        Self::new(ExprKind::Ident(name.to_string()), span)
    }

    pub fn int_literal(n: i64, span: Span) -> Self {
        Self::new(ExprKind::IntLiteral(n), span)
    }

    pub fn string_literal(s: &str, span: Span) -> Self {
        Self::new(ExprKind::StringLiteral(s.to_string()), span)
    }

    pub fn call(func: Expr, args: Vec<Expr>, span: Span) -> Self {
        Self::new(ExprKind::Call { func: Box::new(func), args }, span)
    }

    pub fn binary(left: Expr, op: BinOp, right: Expr, span: Span) -> Self {
        Self::new(ExprKind::Binary {
            op,
            left: Box::new(left),
            right: Box::new(right),
        }, span)
    }

    pub fn block(stmts: Vec<Expr>, span: Span) -> Self {
        Self::new(ExprKind::Block(Block { stmts, span }), span)
    }

    pub fn return_expr(expr: Option<Expr>, span: Span) -> Self {
        Self::new(ExprKind::Return(expr.map(Box::new)), span)
    }

    pub fn let_binding(name: &str, ty: Option<Type>, value: Expr, is_mut: bool, span: Span) -> Self {
        Self::new(ExprKind::Let {
            name: name.to_string(),
            ty,
            value: Box::new(value),
            is_mut,
        }, span)
    }

    pub fn assign(target: Expr, value: Expr, span: Span) -> Self {
        Self::new(ExprKind::Assign { target: Box::new(target), value: Box::new(value) }, span)
    }

    pub fn r#if(cond: Expr, then_branch: Block, else_branch: Option<Block>, span: Span) -> Self {
        Self::new(ExprKind::If {
            cond: Box::new(cond),
            then_branch,
            else_branch,
        }, span)
    }

    pub fn r#match(expr: Expr, arms: Vec<MatchArm>, span: Span) -> Self {
        Self::new(ExprKind::Match { expr: Box::new(expr), arms }, span)
    }

    pub fn gc_new(ty: Type, fields: Vec<(String, Expr)>, span: Span) -> Self {
        Self::new(ExprKind::GcNew { ty, fields }, span)
    }
}

#[derive(Debug, Clone)]
pub struct TraitDef {
    pub name: String,
    pub generics: Vec<GenericParam>,
    pub self_alias: Option<String>,
    pub methods: Vec<Function>,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct ImplBlock {
    pub trait_name: String,
    pub generics: Vec<GenericParam>,
    pub target_type: Type,
    pub methods: Vec<Function>,
    pub span: Span,
}
