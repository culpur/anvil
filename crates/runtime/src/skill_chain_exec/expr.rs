//! Small expression evaluator for `when:` conditions in chain manifests.
//!
//! v0.1 grammar — deliberately minimal; sub-track C will extend once typed
//! outputs land:
//!
//! ```text
//! expr        := compare | unary
//! compare     := unary op unary
//! op          := == | != | > | < | >= | <=
//! unary       := length | atom
//! length      := "len(" path ")"
//! atom        := path | number | string
//! path        := ident ( "." ident )*
//! string      := '"' chars '"'
//! number      := /-?\d+(\.\d+)?/
//! ```
//!
//! Identifiers reference the [`Scope`] map. The canonical path for output
//! binding is `<node-id>.outputs.body` (matching the executor's v0.1
//! string-only output model).
//!
//! Sub-track C will replace this with a typed expression engine that walks
//! richer output schemas. The crate boundary is `evaluate(expr, &scope) ->
//! Result<bool, ExprError>` — that signature is stable.

use std::collections::HashMap;

/// A single value in the evaluator's scope.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    /// String value (default for chain outputs in v0.1).
    String(String),
    /// Number value (rare in v0.1 — produced by `len()` over a string).
    Number(f64),
    /// Boolean value.
    Bool(bool),
    /// Null — produced when an upstream node failed or was skipped.
    Null,
}

impl Value {
    /// Borrow the inner string when the value is a `String`.  Returns `None`
    /// for other kinds — sub-track C will exercise this when typed outputs
    /// land.
    #[must_use]
    pub fn as_string(&self) -> Option<&str> {
        if let Value::String(s) = self { Some(s) } else { None }
    }
    /// Borrow the inner number when the value is a `Number`.
    #[must_use]
    pub fn as_number(&self) -> Option<f64> {
        match self {
            Value::Number(n) => Some(*n),
            _ => None,
        }
    }
    fn truthy(&self) -> bool {
        match self {
            Value::Bool(b) => *b,
            Value::Number(n) => *n != 0.0,
            Value::String(s) => !s.is_empty(),
            Value::Null => false,
        }
    }
}

/// Lookup scope passed to [`evaluate`].
///
/// Keys are dotted paths, e.g. `fetch.outputs.body`.
pub type Scope = HashMap<String, Value>;

/// Evaluation errors.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExprError {
    /// Reached end of input mid-parse.
    UnexpectedEnd,
    /// Token did not match expected grammar.
    Unexpected(String),
    /// Variable path missing from scope and no default.
    UndefinedPath(String),
    /// Type mismatch in operator.
    TypeMismatch(String),
}

impl std::fmt::Display for ExprError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnexpectedEnd => write!(f, "expr: unexpected end of input"),
            Self::Unexpected(t) => write!(f, "expr: unexpected token `{t}`"),
            Self::UndefinedPath(p) => write!(f, "expr: undefined path `{p}`"),
            Self::TypeMismatch(msg) => write!(f, "expr: type mismatch: {msg}"),
        }
    }
}

impl std::error::Error for ExprError {}

/// Evaluate an expression string to a boolean.
///
/// Bare paths/atoms are converted via [`Value::truthy`].
pub fn evaluate(expr: &str, scope: &Scope) -> Result<bool, ExprError> {
    let mut parser = Parser::new(expr);
    let value = parser.parse_expr(scope)?;
    parser.expect_end()?;
    Ok(value.truthy())
}

/// Evaluate to a [`Value`] (mostly for testing).
pub fn evaluate_value(expr: &str, scope: &Scope) -> Result<Value, ExprError> {
    let mut parser = Parser::new(expr);
    let value = parser.parse_expr(scope)?;
    parser.expect_end()?;
    Ok(value)
}

// ---------------------------------------------------------------------------
// Parser
// ---------------------------------------------------------------------------

struct Parser<'a> {
    src: &'a str,
    pos: usize,
}

impl<'a> Parser<'a> {
    fn new(src: &'a str) -> Self {
        Self { src, pos: 0 }
    }

    fn parse_expr(&mut self, scope: &Scope) -> Result<Value, ExprError> {
        let lhs = self.parse_unary(scope)?;
        self.skip_whitespace();
        let Some(op) = self.peek_operator() else {
            return Ok(lhs);
        };
        self.pos += op.len();
        let rhs = self.parse_unary(scope)?;
        compare(&lhs, op, &rhs)
    }

    fn parse_unary(&mut self, scope: &Scope) -> Result<Value, ExprError> {
        self.skip_whitespace();
        if self.src[self.pos..].starts_with("len(") {
            self.pos += 4;
            let inner = self.parse_unary(scope)?;
            self.skip_whitespace();
            if !self.src[self.pos..].starts_with(')') {
                return Err(ExprError::Unexpected("expected `)`".to_string()));
            }
            self.pos += 1;
            return Ok(match inner {
                Value::String(s) => Value::Number(s.chars().count() as f64),
                Value::Null => Value::Number(0.0),
                Value::Number(_) | Value::Bool(_) => {
                    return Err(ExprError::TypeMismatch("len() requires string".to_string()));
                }
            });
        }
        self.parse_atom(scope)
    }

    fn parse_atom(&mut self, scope: &Scope) -> Result<Value, ExprError> {
        self.skip_whitespace();
        if self.pos >= self.src.len() {
            return Err(ExprError::UnexpectedEnd);
        }
        let rest = &self.src[self.pos..];
        let first = rest.chars().next().unwrap();
        if first == '"' {
            // String literal.
            let close = rest[1..]
                .find('"')
                .ok_or_else(|| ExprError::Unexpected("unterminated string".to_string()))?;
            let lit = &rest[1..=close];
            self.pos += close + 2;
            return Ok(Value::String(lit.to_string()));
        }
        if first == '-' || first.is_ascii_digit() {
            // Number literal.
            let end = rest
                .char_indices()
                .find(|(i, c)| {
                    if *i == 0 && *c == '-' {
                        return false;
                    }
                    !(c.is_ascii_digit() || *c == '.')
                })
                .map_or(rest.len(), |(i, _)| i);
            let num: f64 = rest[..end]
                .parse()
                .map_err(|_| ExprError::Unexpected(rest[..end].to_string()))?;
            self.pos += end;
            return Ok(Value::Number(num));
        }
        if first.is_ascii_alphabetic() || first == '_' {
            // Path. Parse `ident("." ident)*`.
            let end = rest
                .char_indices()
                .find(|(_, c)| !(c.is_ascii_alphanumeric() || *c == '_' || *c == '-' || *c == '.'))
                .map_or(rest.len(), |(i, _)| i);
            let path = &rest[..end];
            self.pos += end;
            // Bool literals.
            if path == "true" {
                return Ok(Value::Bool(true));
            }
            if path == "false" {
                return Ok(Value::Bool(false));
            }
            if path == "null" {
                return Ok(Value::Null);
            }
            return scope
                .get(path)
                .cloned()
                .ok_or_else(|| ExprError::UndefinedPath(path.to_string()));
        }
        Err(ExprError::Unexpected(rest.chars().next().unwrap().to_string()))
    }

    fn peek_operator(&self) -> Option<&'static str> {
        let rest = &self.src[self.pos..];
        for op in ["==", "!=", ">=", "<=", ">", "<"] {
            if rest.starts_with(op) {
                return Some(op);
            }
        }
        None
    }

    fn skip_whitespace(&mut self) {
        while let Some(c) = self.src[self.pos..].chars().next() {
            if c.is_whitespace() {
                self.pos += c.len_utf8();
            } else {
                break;
            }
        }
    }

    fn expect_end(&mut self) -> Result<(), ExprError> {
        self.skip_whitespace();
        if self.pos < self.src.len() {
            return Err(ExprError::Unexpected(
                self.src[self.pos..].to_string(),
            ));
        }
        Ok(())
    }
}

fn compare(lhs: &Value, op: &str, rhs: &Value) -> Result<Value, ExprError> {
    use std::cmp::Ordering;
    let ord = match (lhs, rhs) {
        (Value::Number(a), Value::Number(b)) => a
            .partial_cmp(b)
            .ok_or_else(|| ExprError::TypeMismatch(format!("compare NaN: {a} {op} {b}")))?,
        (Value::String(a), Value::String(b)) => a.cmp(b),
        (Value::Bool(a), Value::Bool(b)) => a.cmp(b),
        (Value::Null, Value::Null) => Ordering::Equal,
        (a, b) => {
            return Err(ExprError::TypeMismatch(format!(
                "cannot compare {a:?} {op} {b:?}"
            )));
        }
    };
    let result = match op {
        "==" => ord == Ordering::Equal,
        "!=" => ord != Ordering::Equal,
        ">" => ord == Ordering::Greater,
        "<" => ord == Ordering::Less,
        ">=" => ord != Ordering::Less,
        "<=" => ord != Ordering::Greater,
        _ => return Err(ExprError::Unexpected(op.to_string())),
    };
    Ok(Value::Bool(result))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scope() -> Scope {
        let mut s = Scope::new();
        s.insert(
            "fetch.outputs.body".to_string(),
            Value::String("hello world this is the body".to_string()),
        );
        s.insert("empty.outputs.body".to_string(), Value::String(String::new()));
        s.insert("dead.outputs.body".to_string(), Value::Null);
        s
    }

    #[test]
    fn len_greater_than() {
        assert!(evaluate("len(fetch.outputs.body) > 5", &scope()).unwrap());
    }

    #[test]
    fn len_less_than_false() {
        assert!(!evaluate("len(fetch.outputs.body) < 5", &scope()).unwrap());
    }

    #[test]
    fn equality_strings() {
        let mut s = scope();
        s.insert("greeting".to_string(), Value::String("hi".to_string()));
        assert!(evaluate("greeting == \"hi\"", &s).unwrap());
        assert!(!evaluate("greeting == \"bye\"", &s).unwrap());
    }

    #[test]
    fn null_truthiness() {
        // bare null is falsy
        assert!(!evaluate("null", &scope()).unwrap());
    }

    #[test]
    fn len_of_null_is_zero() {
        assert!(!evaluate("len(dead.outputs.body) > 0", &scope()).unwrap());
    }

    #[test]
    fn bool_literals() {
        assert!(evaluate("true", &scope()).unwrap());
        assert!(!evaluate("false", &scope()).unwrap());
    }

    #[test]
    fn undefined_path_errors() {
        let err = evaluate("ghost.outputs.body == \"\"", &scope()).unwrap_err();
        assert!(matches!(err, ExprError::UndefinedPath(ref p) if p == "ghost.outputs.body"));
    }

    #[test]
    fn unterminated_string_errors() {
        let err = evaluate("\"unterminated", &scope()).unwrap_err();
        assert!(matches!(err, ExprError::Unexpected(_)));
    }

    #[test]
    fn missing_close_paren_errors() {
        let err = evaluate("len(fetch.outputs.body > 0", &scope()).unwrap_err();
        assert!(matches!(err, ExprError::Unexpected(_)));
    }

    #[test]
    fn truthy_non_empty_string() {
        let mut s = Scope::new();
        s.insert("x".to_string(), Value::String("yes".to_string()));
        assert!(evaluate("x", &s).unwrap());
    }
}
