use crate::{
    binder::{BoundExpr, BoundExprKind, BoundLiteral, FunctionKind},
    databox::Value,
    error::{DbError, Result},
    parser::ast::{BinaryOp, UnaryOp},
};

use super::Tuple;

/// Evaluate a bound expression against a tuple, producing a Value.
pub fn evaluate_expr(expr: &BoundExpr, tuple: &Tuple) -> Result<Value> {
    match &expr.kind {
        BoundExprKind::Literal(lit) => Ok(eval_literal(lit)),

        BoundExprKind::ColumnRef(cr) => tuple
            .get(cr.scope_index)
            .cloned()
            .ok_or_else(|| DbError::ColumnIndexOutOfBounds(cr.scope_index)),

        BoundExprKind::BinaryOp { left, op, right } => {
            let lval = evaluate_expr(left, tuple)?;
            let rval = evaluate_expr(right, tuple)?;
            eval_binary_op(op, lval, rval)
        }

        BoundExprKind::UnaryOp { op, expr } => {
            let val = evaluate_expr(expr, tuple)?;
            eval_unary_op(op, val)
        }

        BoundExprKind::IsNull(inner) => {
            let val = evaluate_expr(inner, tuple)?;
            Ok(Value::Bool(val.is_null()))
        }

        BoundExprKind::IsNotNull(inner) => {
            let val = evaluate_expr(inner, tuple)?;
            Ok(Value::Bool(!val.is_null()))
        }

        BoundExprKind::Cast { expr, target_type } => {
            let val = evaluate_expr(expr, tuple)?;
            val.cast(*target_type)
                .ok_or_else(|| DbError::CastError(format!("{:?} -> {:?}", val, target_type)))
        }

        BoundExprKind::Between {
            expr,
            low,
            high,
            negated,
        } => {
            let val = evaluate_expr(expr, tuple)?;
            let lo = evaluate_expr(low, tuple)?;
            let hi = evaluate_expr(high, tuple)?;
            let in_range = val >= lo && val <= hi;
            Ok(Value::Bool(if *negated { !in_range } else { in_range }))
        }

        BoundExprKind::In {
            expr,
            list,
            negated,
        } => {
            let val = evaluate_expr(expr, tuple)?;
            let mut found = false;
            for item in list {
                let item_val = evaluate_expr(item, tuple)?;
                if val == item_val {
                    found = true;
                    break;
                }
            }
            Ok(Value::Bool(if *negated { !found } else { found }))
        }

        BoundExprKind::Like {
            expr,
            pattern,
            negated,
        } => {
            let val = evaluate_expr(expr, tuple)?;
            let pat = evaluate_expr(pattern, tuple)?;
            let matches = match (&val, &pat) {
                (Value::String(s), Value::String(p)) => like_match(s, p),
                _ => false,
            };
            Ok(Value::Bool(if *negated { !matches } else { matches }))
        }

        BoundExprKind::Case {
            operand,
            when_then,
            else_result,
        } => {
            let op_val = operand
                .as_ref()
                .map(|e| evaluate_expr(e, tuple))
                .transpose()?;

            for (when_expr, then_expr) in when_then {
                let when_val = evaluate_expr(when_expr, tuple)?;
                let matches = match &op_val {
                    Some(ov) => *ov == when_val,
                    None => when_val == Value::Bool(true),
                };
                if matches {
                    return evaluate_expr(then_expr, tuple);
                }
            }

            if let Some(else_expr) = else_result {
                evaluate_expr(else_expr, tuple)
            } else {
                Ok(Value::Null)
            }
        }

        BoundExprKind::Function(func) => eval_function(&func.kind, &func.args, tuple),

        // Subqueries are not supported in expression evaluation
        BoundExprKind::InSubquery { .. }
        | BoundExprKind::Exists { .. }
        | BoundExprKind::Subquery(_) => Err(DbError::Unsupported(
            "Subquery in expression evaluation".to_string(),
        )),
    }
}

fn eval_literal(lit: &BoundLiteral) -> Value {
    match lit {
        BoundLiteral::Integer(n) => Value::I64(*n),
        BoundLiteral::Float(f) => Value::F64(*f),
        BoundLiteral::String(s) => Value::String(s.clone()),
        BoundLiteral::Bool(b) => Value::Bool(*b),
        BoundLiteral::Null => Value::Null,
    }
}

fn eval_binary_op(op: &BinaryOp, left: Value, right: Value) -> Result<Value> {
    // Handle NULL propagation for most ops
    if left.is_null() || right.is_null() {
        return match op {
            // Null comparisons return null (except for IS NULL handled elsewhere)
            BinaryOp::Eq
            | BinaryOp::NotEq
            | BinaryOp::Lt
            | BinaryOp::LtEq
            | BinaryOp::Gt
            | BinaryOp::GtEq => Ok(Value::Null),
            // AND/OR have special null semantics
            BinaryOp::And => {
                // false AND null = false, null AND null = null, true AND null = null
                if left == Value::Bool(false) || right == Value::Bool(false) {
                    Ok(Value::Bool(false))
                } else {
                    Ok(Value::Null)
                }
            }
            BinaryOp::Or => {
                // true OR null = true, null OR null = null, false OR null = null
                if left == Value::Bool(true) || right == Value::Bool(true) {
                    Ok(Value::Bool(true))
                } else {
                    Ok(Value::Null)
                }
            }
            _ => Ok(Value::Null),
        };
    }

    match op {
        BinaryOp::Eq => Ok(Value::Bool(left == right)),
        BinaryOp::NotEq => Ok(Value::Bool(left != right)),
        BinaryOp::Lt => Ok(Value::Bool(left < right)),
        BinaryOp::LtEq => Ok(Value::Bool(left <= right)),
        BinaryOp::Gt => Ok(Value::Bool(left > right)),
        BinaryOp::GtEq => Ok(Value::Bool(left >= right)),

        BinaryOp::And => match (&left, &right) {
            (Value::Bool(a), Value::Bool(b)) => Ok(Value::Bool(*a && *b)),
            _ => Err(DbError::TypeMismatch("AND requires booleans".to_string())),
        },
        BinaryOp::Or => match (&left, &right) {
            (Value::Bool(a), Value::Bool(b)) => Ok(Value::Bool(*a || *b)),
            _ => Err(DbError::TypeMismatch("OR requires booleans".to_string())),
        },

        BinaryOp::Add => numeric_op(left, right, |a, b| a + b, |a, b| a + b),
        BinaryOp::Sub => numeric_op(left, right, |a, b| a - b, |a, b| a - b),
        BinaryOp::Mul => numeric_op(left, right, |a, b| a * b, |a, b| a * b),
        BinaryOp::Div => {
            // Check for division by zero
            match &right {
                Value::I64(0) | Value::I32(0) | Value::I16(0) | Value::I8(0) => {
                    return Err(DbError::DivisionByZero);
                }
                Value::F64(f) if *f == 0.0 => return Err(DbError::DivisionByZero),
                Value::F32(f) if *f == 0.0 => return Err(DbError::DivisionByZero),
                _ => {}
            }
            numeric_op(left, right, |a, b| a / b, |a, b| a / b)
        }
        BinaryOp::Mod => {
            match &right {
                Value::I64(0) | Value::I32(0) | Value::I16(0) | Value::I8(0) => {
                    return Err(DbError::DivisionByZero);
                }
                _ => {}
            }
            numeric_op(left, right, |a, b| a % b, |a, b| a % b)
        }

        BinaryOp::Concat => match (left, right) {
            (Value::String(a), Value::String(b)) => Ok(Value::String(a + &b)),
            (Value::String(a), b) => Ok(Value::String(a + &b.to_string())),
            (a, Value::String(b)) => Ok(Value::String(a.to_string() + &b)),
            (a, b) => Ok(Value::String(a.to_string() + &b.to_string())),
        },
    }
}

fn numeric_op<F, G>(left: Value, right: Value, int_op: F, float_op: G) -> Result<Value>
where
    F: Fn(i64, i64) -> i64,
    G: Fn(f64, f64) -> f64,
{
    match (left, right) {
        (Value::I64(a), Value::I64(b)) => Ok(Value::I64(int_op(a, b))),
        (Value::I32(a), Value::I32(b)) => Ok(Value::I32(int_op(a as i64, b as i64) as i32)),
        (Value::F64(a), Value::F64(b)) => Ok(Value::F64(float_op(a, b))),
        (Value::F32(a), Value::F32(b)) => Ok(Value::F32(float_op(a as f64, b as f64) as f32)),
        // Cross-type: promote to f64
        (a, b) => match (a.to_f64(), b.to_f64()) {
            (Some(af), Some(bf)) => Ok(Value::F64(float_op(af, bf))),
            _ => Err(DbError::TypeMismatch(
                "Cannot perform arithmetic on non-numeric types".to_string(),
            )),
        },
    }
}

fn eval_unary_op(op: &UnaryOp, val: Value) -> Result<Value> {
    if val.is_null() {
        return Ok(Value::Null);
    }

    match op {
        UnaryOp::Not => match val {
            Value::Bool(b) => Ok(Value::Bool(!b)),
            _ => Err(DbError::TypeMismatch("NOT requires boolean".to_string())),
        },
        UnaryOp::Neg => match val {
            Value::I64(n) => Ok(Value::I64(-n)),
            Value::I32(n) => Ok(Value::I32(-n)),
            Value::I16(n) => Ok(Value::I16(-n)),
            Value::I8(n) => Ok(Value::I8(-n)),
            Value::F64(f) => Ok(Value::F64(-f)),
            Value::F32(f) => Ok(Value::F32(-f)),
            _ => Err(DbError::TypeMismatch("NEG requires numeric".to_string())),
        },
    }
}

fn eval_function(kind: &FunctionKind, args: &[BoundExpr], tuple: &Tuple) -> Result<Value> {
    match kind {
        FunctionKind::Coalesce => {
            for arg in args {
                let val = evaluate_expr(arg, tuple)?;
                if !val.is_null() {
                    return Ok(val);
                }
            }
            Ok(Value::Null)
        }
        FunctionKind::Nullif => {
            if args.len() != 2 {
                return Err(DbError::InvalidArguments("NULLIF requires 2 arguments".into()));
            }
            let a = evaluate_expr(&args[0], tuple)?;
            let b = evaluate_expr(&args[1], tuple)?;
            if a == b {
                Ok(Value::Null)
            } else {
                Ok(a)
            }
        }
        FunctionKind::Upper => {
            if args.is_empty() {
                return Ok(Value::Null);
            }
            let val = evaluate_expr(&args[0], tuple)?;
            match val {
                Value::String(s) => Ok(Value::String(s.to_uppercase())),
                Value::Null => Ok(Value::Null),
                _ => Err(DbError::TypeMismatch("UPPER requires string".into())),
            }
        }
        FunctionKind::Lower => {
            if args.is_empty() {
                return Ok(Value::Null);
            }
            let val = evaluate_expr(&args[0], tuple)?;
            match val {
                Value::String(s) => Ok(Value::String(s.to_lowercase())),
                Value::Null => Ok(Value::Null),
                _ => Err(DbError::TypeMismatch("LOWER requires string".into())),
            }
        }
        FunctionKind::Length => {
            if args.is_empty() {
                return Ok(Value::Null);
            }
            let val = evaluate_expr(&args[0], tuple)?;
            match val {
                Value::String(s) => Ok(Value::I64(s.len() as i64)),
                Value::Null => Ok(Value::Null),
                _ => Err(DbError::TypeMismatch("LENGTH requires string".into())),
            }
        }
        FunctionKind::Abs => {
            if args.is_empty() {
                return Ok(Value::Null);
            }
            let val = evaluate_expr(&args[0], tuple)?;
            match val {
                Value::I64(n) => Ok(Value::I64(n.abs())),
                Value::I32(n) => Ok(Value::I32(n.abs())),
                Value::F64(f) => Ok(Value::F64(f.abs())),
                Value::F32(f) => Ok(Value::F32(f.abs())),
                Value::Null => Ok(Value::Null),
                _ => Err(DbError::TypeMismatch("ABS requires numeric".into())),
            }
        }
        // Aggregate functions should not be evaluated row-by-row
        FunctionKind::Count | FunctionKind::Sum | FunctionKind::Avg | FunctionKind::Min | FunctionKind::Max => {
            Err(DbError::Unsupported(
                "Aggregate functions cannot be evaluated per-row".into(),
            ))
        }
        FunctionKind::Unknown => Err(DbError::Unsupported("Unknown function".into())),
    }
}

/// Simple SQL LIKE pattern matching.
/// Supports % (any sequence) and _ (single char).
fn like_match(s: &str, pattern: &str) -> bool {
    let s_chars: Vec<char> = s.chars().collect();
    let p_chars: Vec<char> = pattern.chars().collect();
    like_match_recursive(&s_chars, &p_chars)
}

fn like_match_recursive(s: &[char], p: &[char]) -> bool {
    if p.is_empty() {
        return s.is_empty();
    }

    match p[0] {
        '%' => {
            // % matches zero or more characters
            // Try matching 0 chars, 1 char, 2 chars, etc.
            for i in 0..=s.len() {
                if like_match_recursive(&s[i..], &p[1..]) {
                    return true;
                }
            }
            false
        }
        '_' => {
            // _ matches exactly one character
            if s.is_empty() {
                false
            } else {
                like_match_recursive(&s[1..], &p[1..])
            }
        }
        c => {
            // Literal character match (case-insensitive for simplicity)
            if s.is_empty() {
                false
            } else if s[0].to_ascii_lowercase() == c.to_ascii_lowercase() {
                like_match_recursive(&s[1..], &p[1..])
            } else {
                false
            }
        }
    }
}

/// Evaluate an expression to a boolean for filter predicates.
pub fn evaluate_predicate(expr: &BoundExpr, tuple: &Tuple) -> Result<bool> {
    let val = evaluate_expr(expr, tuple)?;
    match val {
        Value::Bool(b) => Ok(b),
        Value::Null => Ok(false), // NULL in predicate context is falsy
        // Non-zero numeric = true (C-style)
        Value::I64(n) => Ok(n != 0),
        Value::I32(n) => Ok(n != 0),
        _ => Err(DbError::TypeMismatch(
            "Predicate must evaluate to boolean".to_string(),
        )),
    }
}
