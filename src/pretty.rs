//! Render an expression back to source text, for error messages.

use crate::ast::*;

pub fn show_expr(e: &Expr) -> String {
    match &e.kind {
        ExprKind::Int(n) => n.to_string(),
        ExprKind::Bool(b) => b.to_string(),
        ExprKind::Absent => "absent".into(),
        ExprKind::Dur(_, s) => s.clone(),
        ExprKind::Str(segs) => {
            let mut s = String::from("\"");
            for seg in segs {
                match seg {
                    StrSeg::Lit(l) => s.push_str(&l.replace('\\', "\\\\").replace('"', "\\\"").replace('\n', "\\n")),
                    StrSeg::Expr(x) => s.push_str(&format!("${{{}}}", show_expr(x))),
                }
            }
            s.push('"');
            s
        }
        ExprKind::Ident(n) => n.clone(),
        ExprKind::List(items) => format!("[{}]", items.iter().map(show_arg).collect::<Vec<_>>().join(", ")),
        ExprKind::Record(f) => format!(
            "{{{}}}",
            f.iter().map(|(n, x)| format!("{n}: {}", show_expr(x))).collect::<Vec<_>>().join(", ")
        ),
        ExprKind::Call { name, args } => format!("{name}({})", args.iter().map(show_arg).collect::<Vec<_>>().join(", ")),
        ExprKind::Stage { recv, name, args } => {
            format!("{}.{name}({})", show_expr(recv), args.iter().map(show_arg).collect::<Vec<_>>().join(", "))
        }
        ExprKind::Member { recv, name } => format!("{}.{name}", show_expr(recv)),
        ExprKind::Unary(UnOp::Not, x) => format!("not {}", show_expr(x)),
        ExprKind::Unary(UnOp::Neg, x) => format!("-{}", show_expr(x)),
        ExprKind::Binary(op, l, r) => {
            let sym = match op {
                BinOp::Add => "+",
                BinOp::Sub => "-",
                BinOp::Mul => "*",
                BinOp::Div => "/",
                BinOp::Eq => "==",
                BinOp::Ne => "!=",
                BinOp::Lt => "<",
                BinOp::Le => "<=",
                BinOp::Gt => ">",
                BinOp::Ge => ">=",
                BinOp::And => "and",
                BinOp::Or => "or",
            };
            format!("{} {sym} {}", show_expr(l), show_expr(r))
        }
        ExprKind::Try(x) => format!("try {}", show_expr(x)),
        ExprKind::RunFlow { name, args } => format!("run {name}({})", args.iter().map(show_arg).collect::<Vec<_>>().join(", ")),
        ExprKind::If { cond, .. } => format!("if {} {{ ... }} else {{ ... }}", show_expr(cond)),
        ExprKind::Within { .. } => "within ... { ... }".to_string(),
    }
}

fn show_arg(a: &Arg) -> String {
    if a.spread {
        return format!("...{}", show_expr(&a.expr));
    }
    match &a.name {
        Some(n) => format!("{n}: {}", show_expr(&a.expr)),
        None => show_expr(&a.expr),
    }
}
