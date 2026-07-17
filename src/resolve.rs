use crate::ast::*;
use std::collections::HashMap;

/// How a DotAccess node was resolved
#[derive(Debug, Clone)]
pub enum ResolvedAs {
    Field,
    Call,
}

/// Maps span.start of DotAccess nodes to their resolution
pub type ResolutionMap = HashMap<usize, ResolvedAs>;

/// Walk the AST and replace DotAccess with Field or Call based on resolutions
pub fn apply_resolutions(module: &mut Module, resolutions: &ResolutionMap) {
    for item in &mut module.items {
        if let Item::Function(ref mut func) = item {
            apply_resolutions_block(&mut func.body, resolutions);
        }
    }
}

fn apply_resolutions_block(block: &mut Block, resolutions: &ResolutionMap) {
    for stmt in &mut block.stmts {
        apply_resolutions_expr(stmt, resolutions);
    }
}

fn apply_resolutions_expr(expr: &mut Expr, resolutions: &ResolutionMap) {
    // Replace DotAccess if resolved
    if let ExprKind::DotAccess { object, field } = &expr.kind {
        if let Some(resolved) = resolutions.get(&expr.span.start) {
            let span = expr.span;
            match resolved {
                ResolvedAs::Field => {
                    expr.kind = ExprKind::Field { object: object.clone(), field: field.clone() };
                }
                ResolvedAs::Call => {
                    expr.kind = ExprKind::Call {
                        func: Box::new(Expr::ident(field, span)),
                        args: vec![*object.clone()],
                    };
                }
            }
        }
    }
    // Recurse
    match &mut expr.kind {
        ExprKind::Block(ref mut b) => apply_resolutions_block(b, resolutions),
        ExprKind::If { ref mut then_branch, ref mut else_branch, .. } => {
            apply_resolutions_block(then_branch, resolutions);
            if let Some(ref mut eb) = else_branch { apply_resolutions_block(eb, resolutions); }
        }
        ExprKind::While { ref mut body, .. } => apply_resolutions_block(body, resolutions),
        ExprKind::For { ref mut body, .. } => apply_resolutions_block(body, resolutions),
        ExprKind::Let { ref mut value, .. } => apply_resolutions_expr(value, resolutions),
        ExprKind::Return(Some(ref mut e)) => apply_resolutions_expr(e, resolutions),
        ExprKind::Binary { ref mut left, ref mut right, .. } => {
            apply_resolutions_expr(left, resolutions);
            apply_resolutions_expr(right, resolutions);
        }
        ExprKind::Call { ref mut func, ref mut args } => {
            apply_resolutions_expr(func, resolutions);
            for arg in args { apply_resolutions_expr(arg, resolutions); }
        }
        ExprKind::Field { ref mut object, .. } => apply_resolutions_expr(object, resolutions),
        ExprKind::DotAccess { ref mut object, .. } => apply_resolutions_expr(object, resolutions),
        ExprKind::Assign { ref mut target, ref mut value } => {
            apply_resolutions_expr(target, resolutions);
            apply_resolutions_expr(value, resolutions);
        }
        _ => {}
    }
}
