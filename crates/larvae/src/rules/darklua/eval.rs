/*!
A very small constant evaluator

The evaluator answers one question: does this expression have a value that
larvae can write back into the source without a change in program
behavior? Thus it folds only values that it can print exactly. A number
must be a whole f64 inside the range that doubles represent exactly. A
string must be a literal with no escapes. Then the output bytes equal the
input bytes.
*/

use super::support;
use crate::rules::engine::RuleCtx;
use crate::syntax::ast::*;

/// The largest whole number that an f64 represents exactly.
const SAFE_INT: f64 = 9_007_199_254_740_992.0;

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Nil,
    Bool(bool),
    Num(f64),
    Str(String),
}

impl Value {
    /// Lua truthiness. Only nil and false are false. Zero is not false.
    pub fn truthy(&self) -> bool {
        !matches!(self, Value::Nil | Value::Bool(false))
    }
}

/// The value of an expression when it is a compile time constant.
pub fn eval(ctx: &RuleCtx, e: &Expr) -> Option<Value> {
    match e {
        Expr::Nil(_) => Some(Value::Nil),

        Expr::True(_) => Some(Value::Bool(true)),

        Expr::False(_) => Some(Value::Bool(false)),

        Expr::Number(span) => parse_number(ctx.text(*span)).map(Value::Num),

        Expr::String(span) => {
            support::plain_string_value(ctx, *span).map(|s| Value::Str(s.to_string()))
        }

        // A define is a constant, so folding and branch pruning see through it.
        Expr::Name(span) => match ctx.define_at(*span)? {
            crate::rules::defines::Value::Bool(b) => Some(Value::Bool(*b)),

            crate::rules::defines::Value::Number(n) => parse_number(n).map(Value::Num),

            crate::rules::defines::Value::Str(s) => Some(Value::Str(s.clone())),

            crate::rules::defines::Value::Nil => Some(Value::Nil),
        },

        Expr::Paren { inner, .. } => eval(ctx, inner),

        Expr::Unary { op, operand, .. } => {
            let v = eval(ctx, operand)?;

            match ctx.text(*op) {
                "-" => match v {
                    Value::Num(n) => Some(Value::Num(-n)),

                    _ => None,
                },

                "not" => Some(Value::Bool(!v.truthy())),
                // `#` would need the length of a real value.
                _ => None,
            }
        }

        Expr::Binary { op, lhs, rhs, .. } => {
            let name = ctx.text(*op);
            // The short circuit operators need only the left side to decide.
            if name == "and" || name == "or" {
                let l = eval(ctx, lhs)?;
                let r = eval(ctx, rhs)?;

                let take_left = if name == "and" {
                    !l.truthy()
                } else {
                    l.truthy()
                };

                return Some(if take_left { l } else { r });
            }

            binary(name, eval(ctx, lhs)?, eval(ctx, rhs)?)
        }

        _ => None,
    }
}

fn binary(op: &str, l: Value, r: Value) -> Option<Value> {
    use Value::*;

    match op {
        "+" | "-" | "*" | "/" | "%" | "^" => {
            let (Num(a), Num(b)) = (l, r) else {
                return None;
            };

            Some(Num(match op {
                "+" => a + b,

                "-" => a - b,

                "*" => a * b,

                "/" => a / b,
                // Lua's modulo follows the sign of the divisor. Rust's modulo does not.
                "%" => a - (a / b).floor() * b,

                _ => a.powf(b),
            }))
        }

        ".." => {
            let (Str(a), Str(b)) = (l, r) else {
                return None;
            };

            Some(Str(format!("{a}{b}")))
        }

        "==" => Some(Bool(equal(&l, &r))),

        "~=" => Some(Bool(!equal(&l, &r))),

        "<" | "<=" | ">" | ">=" => {
            let ord = match (&l, &r) {
                (Num(a), Num(b)) => a.partial_cmp(b)?,

                (Str(a), Str(b)) => a.cmp(b),

                _ => return None,
            };

            Some(Bool(match op {
                "<" => ord.is_lt(),

                "<=" => ord.is_le(),

                ">" => ord.is_gt(),

                _ => ord.is_ge(),
            }))
        }

        _ => None,
    }
}

/// Lua equality never coerces across types.
fn equal(l: &Value, r: &Value) -> bool {
    match (l, r) {
        (Value::Num(a), Value::Num(b)) => a == b,

        (Value::Str(a), Value::Str(b)) => a == b,

        (Value::Bool(a), Value::Bool(b)) => a == b,

        (Value::Nil, Value::Nil) => true,

        _ => false,
    }
}

/// Parse each numeric literal form that Luau writes, with separators included.
pub fn parse_number(text: &str) -> Option<f64> {
    let cleaned: String = text.chars().filter(|&c| c != '_').collect();
    let lower = cleaned.to_ascii_lowercase();

    if let Some(hex) = lower.strip_prefix("0x") {
        // A hex float carries a binary exponent. The rounding risk is too large.
        if hex.is_empty() || hex.contains('p') || hex.contains('.') {
            return None;
        }

        return u64::from_str_radix(hex, 16).ok().map(|v| v as f64);
    }

    if let Some(bits) = lower.strip_prefix("0b") {
        if bits.is_empty() || !bits.chars().all(|c| c == '0' || c == '1') {
            return None;
        }

        return u64::from_str_radix(bits, 2).ok().map(|v| v as f64);
    }

    cleaned.parse::<f64>().ok()
}

/*
Print a value back as Luau source. Return None when the result would not
round trip. This check is the full safety condition for
compute_expression.
*/
pub fn print(v: &Value, quote: char) -> Option<String> {
    match v {
        Value::Nil => Some("nil".to_string()),

        Value::Bool(true) => Some("true".to_string()),

        Value::Bool(false) => Some("false".to_string()),

        Value::Num(n) => {
            // A fraction would need a rounding policy. A whole number does not.
            if !n.is_finite() || n.fract() != 0.0 || n.abs() > SAFE_INT {
                return None;
            }

            Some(format!("{}", *n as i64))
        }

        Value::Str(s) => {
            if s.contains(quote) || s.contains('\\') || s.contains('\n') || s.contains('\r') {
                return None;
            }

            Some(format!("{quote}{s}{quote}"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::syntax::{lexer, parser};

    fn value(src: &str) -> Option<Value> {
        let full = format!("local _ = {src}");
        let lexed = lexer::lex(&full).unwrap();
        let chunk = parser::parse(&full, &lexed.toks).unwrap();

        let ctx = RuleCtx {
            src: &full,
            toks: &lexed.toks,
            chunk: &chunk,
            comments: &lexed.comments,
            require_forms: &[],
            dm_path: None,
            quote: '"',
            defines: &Default::default(),
            globals: &Default::default(),
        };

        let Stmt::Local(l) = &chunk.block.stmts[0] else {
            panic!()
        };

        eval(&ctx, &l.values[0])
    }

    fn shown(src: &str) -> Option<String> {
        print(&value(src)?, '"')
    }

    #[test]
    fn arithmetic_folds_when_it_stays_whole() {
        assert_eq!(shown("1 + 2").as_deref(), Some("3"));
        assert_eq!(shown("10 / 2").as_deref(), Some("5"));
        assert_eq!(shown("2 ^ 10").as_deref(), Some("1024"));
        assert_eq!(shown("7 % 3").as_deref(), Some("1"));
        assert_eq!(shown("2 * 3 + 4").as_deref(), Some("10"));
        assert_eq!(shown("-(3 - 5)").as_deref(), Some("2"));
    }

    #[test]
    fn fractions_and_nonsense_do_not_fold() {
        // A fraction would need a rounding policy, and larvae does not guess one.
        assert_eq!(shown("10 / 4"), None);
        assert_eq!(shown("1 / 0"), None);
        assert_eq!(shown("0 / 0"), None);
        // Above the exact double range, the printed form would not equal the value.
        assert_eq!(shown("9007199254740992 * 2"), None);
    }

    #[test]
    fn lua_modulo_follows_the_divisor() {
        assert_eq!(shown("-1 % 3").as_deref(), Some("2"));
    }

    #[test]
    fn comparisons_and_logic_fold() {
        assert_eq!(shown("1 < 2").as_deref(), Some("true"));
        assert_eq!(shown("\"a\" == \"a\"").as_deref(), Some("true"));
        assert_eq!(shown("1 == \"1\"").as_deref(), Some("false"));
        assert_eq!(shown("not nil").as_deref(), Some("true"));
        // Zero is truthy in Lua.
        assert_eq!(shown("not 0").as_deref(), Some("false"));
        assert_eq!(shown("true and 2").as_deref(), Some("2"));
        assert_eq!(shown("false and 2").as_deref(), Some("false"));
        assert_eq!(shown("nil or 3").as_deref(), Some("3"));
    }

    #[test]
    fn string_concat_folds_only_for_plain_literals() {
        assert_eq!(shown("\"a\" .. \"b\"").as_deref(), Some("\"ab\""));
        // With an escape, the source bytes do not equal the string bytes.
        assert_eq!(shown("\"a\\n\" .. \"b\""), None);
        assert_eq!(shown("1 .. 2"), None);
    }

    #[test]
    fn anything_with_a_name_in_it_is_not_constant() {
        assert_eq!(value("x + 1"), None);
        assert_eq!(value("f()"), None);
        assert_eq!(value("#t"), None);
    }

    #[test]
    fn number_literals_parse_the_way_luau_writes_them() {
        assert_eq!(parse_number("0b1010"), Some(10.0));
        assert_eq!(parse_number("0xFF"), Some(255.0));
        assert_eq!(parse_number("1_000"), Some(1000.0));
        assert_eq!(parse_number("1.5e3"), Some(1500.0));
    }
}
