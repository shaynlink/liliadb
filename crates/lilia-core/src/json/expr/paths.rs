//! The document paths an expression reads.

use super::ast::Expr;

/// Collects every document path referenced by `expr`.
#[must_use]
pub fn referenced_paths(expr: &Expr) -> Vec<&str> {
    let mut paths = Vec::new();
    collect_paths(expr, &mut paths);
    paths
}

fn collect_paths<'a>(expr: &'a Expr, out: &mut Vec<&'a str>) {
    match expr {
        Expr::Path(path) => out.push(path),
        Expr::Literal(_) => {}
        Expr::FunctionCall { args, .. } => args.iter().for_each(|arg| collect_paths(arg, out)),
        Expr::Binary { left, right, .. } => {
            collect_paths(left, out);
            collect_paths(right, out);
        }
        Expr::Unary { expr, .. } | Expr::IsNull(expr) | Expr::IsNotNull(expr) => {
            collect_paths(expr, out);
        }
    }
}
