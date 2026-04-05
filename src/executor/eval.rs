use crate::{
    binder::{BoundExpr, BoundExprKind, BoundLiteral, FunctionKind},
    databox::Value,
    parser::ast::{BinaryOp, UnaryOp},
};

pub fn eval_expr(expr: &BoundExpr, row: &[Value]) -> Value {
    match &expr.kind {
        BoundExprKind::Literal(lit) => lit_to_value(lit),
        BoundExprKind::ColumnRef(cr) => row.get(cr.scope_index).cloned().unwrap_or(Value::Null),

        BoundExprKind::BinaryOp { left, op, right } => {
            if *op == BinaryOp::And {
                let l = eval_expr(left, row);
                if l == Value::Bool(false) {
                    return Value::Bool(false);
                }
                return eval_binary(l, op, eval_expr(right, row));
            }
            if *op == BinaryOp::Or {
                let l = eval_expr(left, row);
                if l == Value::Bool(true) {
                    return Value::Bool(true);
                }
                return eval_binary(l, op, eval_expr(right, row));
            }
            eval_binary(eval_expr(left, row), op, eval_expr(right, row))
        }

        BoundExprKind::UnaryOp { op, expr: e } => eval_unary(op, eval_expr(e, row)),
        BoundExprKind::IsNull(e) => Value::Bool(eval_expr(e, row).is_null()),
        BoundExprKind::IsNotNull(e) => Value::Bool(!eval_expr(e, row).is_null()),

        BoundExprKind::Like {
            expr: e,
            pattern,
            negated,
        } => {
            let text = eval_expr(e, row);
            let pat = eval_expr(pattern, row);
            if text.is_null() || pat.is_null() {
                return Value::Null;
            }
            let matched = like_match(&pat.to_string(), &text.to_string());
            Value::Bool(if *negated { !matched } else { matched })
        }

        BoundExprKind::Between {
            expr: e,
            low,
            high,
            negated,
        } => {
            let v = eval_expr(e, row);
            let lo = eval_expr(low, row);
            let hi = eval_expr(high, row);
            if v.is_null() || lo.is_null() || hi.is_null() {
                return Value::Null;
            }
            let in_range = v >= lo && v <= hi;
            Value::Bool(if *negated { !in_range } else { in_range })
        }

        BoundExprKind::In {
            expr: e,
            list,
            negated,
        } => {
            let v = eval_expr(e, row);
            if v.is_null() {
                return Value::Null;
            }
            let found = list.iter().any(|le| eval_expr(le, row) == v);
            Value::Bool(if *negated { !found } else { found })
        }

        BoundExprKind::Function(f) => eval_scalar_fn(&f.kind, &f.args, row),

        BoundExprKind::Cast {
            expr: e,
            target_type,
        } => eval_expr(e, row).cast(*target_type).unwrap_or(Value::Null),

        BoundExprKind::Case {
            operand,
            when_then,
            else_result,
        } => {
            if let Some(op) = operand {
                let op_val = eval_expr(op, row);
                for (w, t) in when_then {
                    if eval_expr(w, row) == op_val {
                        return eval_expr(t, row);
                    }
                }
            } else {
                for (w, t) in when_then {
                    if eval_expr(w, row) == Value::Bool(true) {
                        return eval_expr(t, row);
                    }
                }
            }
            else_result
                .as_ref()
                .map(|e| eval_expr(e, row))
                .unwrap_or(Value::Null)
        }

        // Subquery expressions are not supported in simple in-memory executor
        BoundExprKind::InSubquery { .. }
        | BoundExprKind::Exists { .. }
        | BoundExprKind::Subquery(_) => Value::Null,
    }
}

pub fn eval_to_bool(expr: &BoundExpr, row: &[Value]) -> bool {
    matches!(eval_expr(expr, row), Value::Bool(true))
}

fn lit_to_value(lit: &BoundLiteral) -> Value {
    match lit {
        BoundLiteral::Integer(n) => Value::I64(*n),
        BoundLiteral::Float(f) => Value::F64(*f),
        BoundLiteral::String(s) => Value::String(s.clone()),
        BoundLiteral::Bool(b) => Value::Bool(*b),
        BoundLiteral::Null => Value::Null,
    }
}

fn eval_binary(left: Value, op: &BinaryOp, right: Value) -> Value {
    if left.is_null() || right.is_null() {
        return match op {
            BinaryOp::And if left == Value::Bool(false) || right == Value::Bool(false) => {
                Value::Bool(false)
            }
            BinaryOp::Or if left == Value::Bool(true) || right == Value::Bool(true) => {
                Value::Bool(true)
            }
            _ => Value::Null,
        };
    }

    match op {
        BinaryOp::Add | BinaryOp::Sub | BinaryOp::Mul | BinaryOp::Div | BinaryOp::Mod => {
            eval_arithmetic(&left, op, &right)
        }
        BinaryOp::Eq => Value::Bool(left == right),
        BinaryOp::NotEq => Value::Bool(left != right),
        BinaryOp::Lt => Value::Bool(left.partial_cmp(&right) == Some(std::cmp::Ordering::Less)),
        BinaryOp::LtEq => Value::Bool(matches!(
            left.partial_cmp(&right),
            Some(std::cmp::Ordering::Less | std::cmp::Ordering::Equal)
        )),
        BinaryOp::Gt => Value::Bool(left.partial_cmp(&right) == Some(std::cmp::Ordering::Greater)),
        BinaryOp::GtEq => Value::Bool(matches!(
            left.partial_cmp(&right),
            Some(std::cmp::Ordering::Greater | std::cmp::Ordering::Equal)
        )),
        BinaryOp::And => match (as_bool(&left), as_bool(&right)) {
            (Some(a), Some(b)) => Value::Bool(a && b),
            _ => Value::Null,
        },
        BinaryOp::Or => match (as_bool(&left), as_bool(&right)) {
            (Some(a), Some(b)) => Value::Bool(a || b),
            _ => Value::Null,
        },
        BinaryOp::Concat => Value::String(format!("{left}{right}")),
    }
}

fn eval_arithmetic(left: &Value, op: &BinaryOp, right: &Value) -> Value {
    // Try integer first
    if let (Some(l), Some(r)) = (left.to_i64(), right.to_i64()) {
        return match op {
            BinaryOp::Add => Value::I64(l.wrapping_add(r)),
            BinaryOp::Sub => Value::I64(l.wrapping_sub(r)),
            BinaryOp::Mul => Value::I64(l.wrapping_mul(r)),
            BinaryOp::Div => {
                if r == 0 {
                    Value::Null
                } else {
                    Value::I64(l / r)
                }
            }
            BinaryOp::Mod => {
                if r == 0 {
                    Value::Null
                } else {
                    Value::I64(l % r)
                }
            }
            _ => Value::Null,
        };
    }
    // Fall back to float
    if let (Some(l), Some(r)) = (left.to_f64(), right.to_f64()) {
        return match op {
            BinaryOp::Add => Value::F64(l + r),
            BinaryOp::Sub => Value::F64(l - r),
            BinaryOp::Mul => Value::F64(l * r),
            BinaryOp::Div => {
                if r == 0.0 {
                    Value::Null
                } else {
                    Value::F64(l / r)
                }
            }
            BinaryOp::Mod => {
                if r == 0.0 {
                    Value::Null
                } else {
                    Value::F64(l % r)
                }
            }
            _ => Value::Null,
        };
    }
    Value::Null
}

fn eval_unary(op: &UnaryOp, val: Value) -> Value {
    if val.is_null() {
        return Value::Null;
    }
    match op {
        UnaryOp::Neg => {
            if let Some(n) = val.to_i64() {
                Value::I64(-n)
            } else if let Some(f) = val.to_f64() {
                Value::F64(-f)
            } else {
                Value::Null
            }
        }
        UnaryOp::Not => match val {
            Value::Bool(b) => Value::Bool(!b),
            _ => Value::Null,
        },
    }
}

fn eval_scalar_fn(kind: &FunctionKind, args: &[BoundExpr], row: &[Value]) -> Value {
    let vals: Vec<Value> = args.iter().map(|a| eval_expr(a, row)).collect();
    match kind {
        FunctionKind::Coalesce => vals
            .into_iter()
            .find(|v| !v.is_null())
            .unwrap_or(Value::Null),
        FunctionKind::Nullif => {
            if vals.len() >= 2 && vals[0] == vals[1] {
                Value::Null
            } else {
                vals.into_iter().next().unwrap_or(Value::Null)
            }
        }
        FunctionKind::Upper => match vals.first() {
            Some(Value::String(s)) => Value::String(s.to_uppercase()),
            _ => Value::Null,
        },
        FunctionKind::Lower => match vals.first() {
            Some(Value::String(s)) => Value::String(s.to_lowercase()),
            _ => Value::Null,
        },
        FunctionKind::Length => match vals.first() {
            Some(Value::String(s)) => Value::I64(s.len() as i64),
            _ => Value::Null,
        },
        FunctionKind::Abs => match vals.first() {
            Some(v) if !v.is_null() => {
                if let Some(n) = v.to_i64() {
                    Value::I64(n.abs())
                } else if let Some(f) = v.to_f64() {
                    Value::F64(f.abs())
                } else {
                    Value::Null
                }
            }
            _ => Value::Null,
        },
        // Aggregates are handled by the aggregate operator, not here
        FunctionKind::Count
        | FunctionKind::Sum
        | FunctionKind::Avg
        | FunctionKind::Min
        | FunctionKind::Max => Value::Null,
        FunctionKind::Unknown => Value::Null,
    }
}

fn as_bool(v: &Value) -> Option<bool> {
    match v {
        Value::Bool(b) => Some(*b),
        _ => None,
    }
}

/// SQL LIKE pattern matching. `%` = any sequence, `_` = any single char.
fn like_match(pattern: &str, text: &str) -> bool {
    let p: Vec<char> = pattern.chars().collect();
    let t: Vec<char> = text.chars().collect();
    like_impl(&p, &t, 0, 0)
}

fn like_impl(p: &[char], t: &[char], mut pi: usize, mut ti: usize) -> bool {
    let (mut star_pi, mut star_ti) = (usize::MAX, 0usize);
    while ti < t.len() {
        if pi < p.len() && (p[pi] == '_' || p[pi] == t[ti]) {
            pi += 1;
            ti += 1;
        } else if pi < p.len() && p[pi] == '%' {
            star_pi = pi;
            star_ti = ti;
            pi += 1;
        } else if star_pi != usize::MAX {
            pi = star_pi + 1;
            star_ti += 1;
            ti = star_ti;
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == '%' {
        pi += 1;
    }
    pi == p.len()
}
