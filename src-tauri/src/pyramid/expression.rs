use anyhow::{anyhow, bail, Result};
use serde_json::{Number, Value};

pub trait ExpressionEnv {
    fn resolve_symbol(&self, symbol: &str) -> Result<Value>;
}

pub struct ValueEnv<'a> {
    root: &'a Value,
}

impl<'a> ValueEnv<'a> {
    pub fn new(root: &'a Value) -> Self {
        Self { root }
    }
}

impl ExpressionEnv for ValueEnv<'_> {
    fn resolve_symbol(&self, symbol: &str) -> Result<Value> {
        match self.root {
            Value::Object(map) => map
                .get(symbol)
                .cloned()
                .ok_or_else(|| anyhow!("unknown symbol '{}'", symbol)),
            _ if symbol == "input" => Ok(self.root.clone()),
            _ => Err(anyhow!("unknown symbol '{}'", symbol)),
        }
    }
}

pub fn looks_like_expression(input: &str) -> bool {
    Parser::new(input).parse().is_ok()
}

pub fn evaluate_expression<E: ExpressionEnv>(input: &str, env: &E) -> Result<Value> {
    let expr = Parser::new(input).parse()?;
    expr.evaluate(env)
}

pub fn evaluate_path_against_value(root: &Value, path: &str) -> Result<Value> {
    let expression = if path.trim_start().starts_with('$') {
        path.trim().to_string()
    } else {
        format!("${}", path.trim())
    };
    evaluate_expression(&expression, &ValueEnv::new(root))
}

#[derive(Debug, Clone)]
enum Expr {
    Number(f64),
    Boolean(bool),
    String(String),
    Reference(ReferenceExpr),
    FunctionCall {
        name: String,
        args: Vec<Expr>,
    },
    Binary {
        left: Box<Expr>,
        op: BinaryOp,
        right: Box<Expr>,
    },
}

/// Check if a JSON Value is truthy (used by when-clause evaluation and boolean comparison).
pub fn value_is_truthy(val: &Value) -> bool {
    match val {
        Value::Bool(b) => *b,
        Value::Null => false,
        Value::Number(n) => n.as_f64().map(|f| f != 0.0).unwrap_or(false),
        Value::String(s) => !s.is_empty() && s != "false",
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}

impl Expr {
    fn evaluate<E: ExpressionEnv>(&self, env: &E) -> Result<Value> {
        match self {
            Expr::Number(value) => number_value(*value),
            Expr::Boolean(value) => Ok(Value::Bool(*value)),
            Expr::String(s) => Ok(Value::String(s.clone())),
            Expr::Reference(reference) => reference.evaluate(env),
            Expr::FunctionCall { name, args } => evaluate_function(name, args, env),
            Expr::Binary { left, op, right } => {
                let lhs = left.evaluate(env)?;
                let rhs = right.evaluate(env)?;
                if op.is_comparison() {
                    // String comparison: either side is a string
                    if lhs.is_string() || rhs.is_string() {
                        let ls = lhs.as_str().unwrap_or_default();
                        let rs = rhs.as_str().unwrap_or_default();
                        let result = match op {
                            BinaryOp::Eq => ls == rs,
                            BinaryOp::Neq => ls != rs,
                            _ => bail!("ordered comparison (>, <, >=, <=) not supported for strings"),
                        };
                        return Ok(Value::Bool(result));
                    }
                    // Boolean comparison: either side is a bool
                    if lhs.is_boolean() || rhs.is_boolean() {
                        let lb = value_is_truthy(&lhs);
                        let rb = value_is_truthy(&rhs);
                        let result = match op {
                            BinaryOp::Eq => lb == rb,
                            BinaryOp::Neq => lb != rb,
                            _ => bail!("ordered comparison (>, <, >=, <=) not supported for booleans"),
                        };
                        return Ok(Value::Bool(result));
                    }
                    // Numeric comparison (existing behavior)
                    let lhs = coerce_number(&lhs)?;
                    let rhs = coerce_number(&rhs)?;
                    let result = match op {
                        BinaryOp::Gt => lhs > rhs,
                        BinaryOp::Gte => lhs >= rhs,
                        BinaryOp::Lt => lhs < rhs,
                        BinaryOp::Lte => lhs <= rhs,
                        BinaryOp::Eq => (lhs - rhs).abs() < f64::EPSILON,
                        BinaryOp::Neq => (lhs - rhs).abs() >= f64::EPSILON,
                        _ => unreachable!(),
                    };
                    Ok(Value::Bool(result))
                } else {
                    let _lhs = coerce_number(&lhs)?;
                    let _rhs = coerce_number(&rhs)?;
                    let lhs = coerce_number(&left.evaluate(env)?)?;
                    let rhs = coerce_number(&right.evaluate(env)?)?;
                    let result = match op {
                        BinaryOp::Add => lhs + rhs,
                        BinaryOp::Sub => lhs - rhs,
                        BinaryOp::Mul => lhs * rhs,
                        BinaryOp::Div => {
                            if rhs == 0.0 {
                                bail!("division by zero");
                            }
                            lhs / rhs
                        }
                        _ => unreachable!(),
                    };
                    number_value(result)
                }
            }
        }
    }
}

fn evaluate_function<E: ExpressionEnv>(name: &str, args: &[Expr], env: &E) -> Result<Value> {
    match name {
        "count" => {
            if args.len() != 1 {
                bail!("count() requires exactly 1 argument");
            }
            let val = args[0].evaluate(env)?;
            let len = match &val {
                Value::Array(items) => items.len() as u64,
                Value::Object(map) => map.len() as u64,
                Value::Null => 0u64,
                _ => 1u64,
            };
            Ok(Value::Number(len.into()))
        }
        "len" => {
            // Alias for count
            evaluate_function("count", args, env)
        }
        other => bail!("unknown function '{}'", other),
    }
}

#[derive(Debug, Clone, Copy)]
enum BinaryOp {
    Add,
    Sub,
    Mul,
    Div,
    Gt,
    Gte,
    Lt,
    Lte,
    Eq,
    Neq,
}

impl BinaryOp {
    fn is_comparison(self) -> bool {
        matches!(
            self,
            Self::Gt | Self::Gte | Self::Lt | Self::Lte | Self::Eq | Self::Neq
        )
    }
}

#[derive(Debug, Clone)]
struct ReferenceExpr {
    root: String,
    segments: Vec<PathSegment>,
}

impl ReferenceExpr {
    fn evaluate<E: ExpressionEnv>(&self, env: &E) -> Result<Value> {
        let mut current = vec![env.resolve_symbol(&self.root)?];
        let mut used_wildcard = false;
        for segment in &self.segments {
            if matches!(segment, PathSegment::Wildcard) {
                used_wildcard = true;
            }
            current = apply_segment(segment, current, env)?;
        }
        if current.is_empty() {
            // Wildcards/projections on empty arrays produce empty arrays
            Ok(Value::Array(vec![]))
        } else if current.len() == 1 && !used_wildcard {
            Ok(current.into_iter().next().unwrap())
        } else {
            Ok(Value::Array(current))
        }
    }
}

#[derive(Debug, Clone)]
enum PathSegment {
    Field(String),
    Index(IndexExpr),
    Wildcard,
}

#[derive(Debug, Clone)]
enum IndexExpr {
    Literal(usize),
    Env(String),
    PairI,
    PairIPlusOne,
}

fn apply_segment<E: ExpressionEnv>(
    segment: &PathSegment,
    values: Vec<Value>,
    env: &E,
) -> Result<Vec<Value>> {
    let mut output = Vec::new();
    for value in values {
        match segment {
            PathSegment::Field(field) => {
                if field == "length" {
                    output.push(length_value(&value)?);
                    continue;
                }
                match value {
                    Value::Object(map) => {
                        let next = map
                            .get(field)
                            .cloned()
                            .ok_or_else(|| anyhow!("field '{}' not found", field))?;
                        output.push(next);
                    }
                    Value::Array(items) => {
                        for item in items {
                            match item {
                                Value::Object(map) => {
                                    let next = map
                                        .get(field)
                                        .cloned()
                                        .ok_or_else(|| anyhow!("field '{}' not found", field))?;
                                    output.push(next);
                                }
                                other => {
                                    return Err(anyhow!(
                                        "cannot project field '{}' from non-object array item {}",
                                        field,
                                        other
                                    ));
                                }
                            }
                        }
                    }
                    other => {
                        return Err(anyhow!("cannot project field '{}' from {}", field, other));
                    }
                }
            }
            PathSegment::Wildcard => match value {
                Value::Array(items) => output.extend(items),
                other => return Err(anyhow!("cannot wildcard-project over {}", other)),
            },
            PathSegment::Index(index_expr) => match value {
                Value::Array(items) => {
                    let index = resolve_index(index_expr, env)?;
                    let item = items.get(index).cloned().ok_or_else(|| {
                        anyhow!("index {} out of bounds (len {})", index, items.len())
                    })?;
                    output.push(item);
                }
                other => return Err(anyhow!("cannot index into {}", other)),
            },
        }
    }

    Ok(output)
}

fn resolve_index<E: ExpressionEnv>(index: &IndexExpr, env: &E) -> Result<usize> {
    match index {
        IndexExpr::Literal(value) => Ok(*value),
        IndexExpr::Env(name) => {
            let value = env.resolve_symbol(name)?;
            value
                .as_u64()
                .map(|v| v as usize)
                .ok_or_else(|| anyhow!("index symbol '{}' did not resolve to an integer", name))
        }
        IndexExpr::PairI => env
            .resolve_symbol("pair_index")
            .or_else(|_| env.resolve_symbol("index"))
            .and_then(|value| {
                value
                    .as_u64()
                    .map(|v| (v as usize) * 2)
                    .ok_or_else(|| anyhow!("pair index did not resolve to an integer"))
            }),
        IndexExpr::PairIPlusOne => env
            .resolve_symbol("pair_index")
            .or_else(|_| env.resolve_symbol("index"))
            .and_then(|value| {
                value
                    .as_u64()
                    .map(|v| (v as usize) * 2 + 1)
                    .ok_or_else(|| anyhow!("pair index did not resolve to an integer"))
            }),
    }
}

fn number_value(value: f64) -> Result<Value> {
    let Some(number) = Number::from_f64(value) else {
        bail!("cannot represent non-finite number {}", value);
    };
    Ok(Value::Number(number))
}

fn length_value(value: &Value) -> Result<Value> {
    let length = match value {
        Value::Array(items) => items.len() as u64,
        Value::Object(map) => map.len() as u64,
        Value::String(text) => text.chars().count() as u64,
        other => return Err(anyhow!("cannot compute length of {}", other)),
    };
    Ok(Value::Number(Number::from(length)))
}

fn coerce_number(value: &Value) -> Result<f64> {
    match value {
        Value::Number(number) => number
            .as_f64()
            .ok_or_else(|| anyhow!("number {} cannot be represented as f64", number)),
        Value::String(text) => text
            .parse::<f64>()
            .map_err(|_| anyhow!("string '{}' is not numeric", text)),
        other => Err(anyhow!("value {} is not numeric", other)),
    }
}

struct Parser<'a> {
    input: &'a str,
    cursor: usize,
}

impl<'a> Parser<'a> {
    fn new(input: &'a str) -> Self {
        Self { input, cursor: 0 }
    }

    fn parse(mut self) -> Result<Expr> {
        self.skip_whitespace();
        let expr = self.parse_comparison()?;
        self.skip_whitespace();
        if !self.is_eof() {
            bail!("unexpected trailing input: {}", &self.input[self.cursor..]);
        }
        Ok(expr)
    }

    fn parse_comparison(&mut self) -> Result<Expr> {
        let expr = self.parse_additive()?;
        self.skip_whitespace();
        let op = if self.consume_str(">=") {
            Some(BinaryOp::Gte)
        } else if self.consume_str("<=") {
            Some(BinaryOp::Lte)
        } else if self.consume_str("!=") {
            Some(BinaryOp::Neq)
        } else if self.consume_str("==") {
            Some(BinaryOp::Eq)
        } else if self.consume('>') {
            Some(BinaryOp::Gt)
        } else if self.consume('<') {
            Some(BinaryOp::Lt)
        } else {
            None
        };
        let Some(op) = op else { return Ok(expr) };
        self.skip_whitespace();
        let rhs = self.parse_additive()?;
        Ok(Expr::Binary {
            left: Box::new(expr),
            op,
            right: Box::new(rhs),
        })
    }

    fn parse_additive(&mut self) -> Result<Expr> {
        let mut expr = self.parse_multiplicative()?;
        loop {
            self.skip_whitespace();
            let op = if self.consume('+') {
                Some(BinaryOp::Add)
            } else if self.consume('-') {
                Some(BinaryOp::Sub)
            } else {
                None
            };

            let Some(op) = op else { break };
            let rhs = self.parse_multiplicative()?;
            expr = Expr::Binary {
                left: Box::new(expr),
                op,
                right: Box::new(rhs),
            };
        }
        Ok(expr)
    }

    fn parse_multiplicative(&mut self) -> Result<Expr> {
        let mut expr = self.parse_primary()?;
        loop {
            self.skip_whitespace();
            let op = if self.consume('*') {
                Some(BinaryOp::Mul)
            } else if self.consume('/') {
                Some(BinaryOp::Div)
            } else {
                None
            };

            let Some(op) = op else { break };
            let rhs = self.parse_primary()?;
            expr = Expr::Binary {
                left: Box::new(expr),
                op,
                right: Box::new(rhs),
            };
        }
        Ok(expr)
    }

    fn parse_primary(&mut self) -> Result<Expr> {
        self.skip_whitespace();
        if self.consume('(') {
            let expr = self.parse_comparison()?;
            self.skip_whitespace();
            self.expect(')')?;
            return Ok(expr);
        }
        if self.peek() == Some('$') {
            return self.parse_reference();
        }
        // String literals: "..." or '...'
        if matches!(self.peek(), Some('"') | Some('\'')) {
            return self.parse_string_literal();
        }
        // Check for identifier (function call or boolean literal)
        if matches!(self.peek(), Some(ch) if ch.is_ascii_alphabetic() || ch == '_') {
            let saved = self.cursor;
            let ident = self.parse_identifier()?;
            self.skip_whitespace();
            if self.consume('(') {
                // Function call: name(arg1, arg2, ...)
                let mut args = Vec::new();
                self.skip_whitespace();
                if !matches!(self.peek(), Some(')')) {
                    args.push(self.parse_comparison()?);
                    loop {
                        self.skip_whitespace();
                        if !self.consume(',') {
                            break;
                        }
                        self.skip_whitespace();
                        args.push(self.parse_comparison()?);
                    }
                }
                self.skip_whitespace();
                self.expect(')')?;
                return Ok(Expr::FunctionCall { name: ident, args });
            }
            // Boolean literals
            match ident.as_str() {
                "true" => return Ok(Expr::Boolean(true)),
                "false" => return Ok(Expr::Boolean(false)),
                _ => {
                    // Not a known identifier — backtrack and try number
                    self.cursor = saved;
                }
            }
        }
        self.parse_number()
    }

    fn parse_string_literal(&mut self) -> Result<Expr> {
        let quote = self.peek().unwrap();
        self.cursor += 1; // consume opening quote
        let start = self.cursor;
        while self.cursor < self.input.len() && self.input.as_bytes()[self.cursor] as char != quote {
            self.cursor += 1;
        }
        let s = self.input[start..self.cursor].to_string();
        if !self.consume(quote) {
            bail!("unterminated string literal");
        }
        Ok(Expr::String(s))
    }

    fn parse_reference(&mut self) -> Result<Expr> {
        self.expect('$')?;
        let root = self.parse_identifier()?;
        let mut segments = Vec::new();
        loop {
            match self.peek() {
                Some('.') => {
                    self.cursor += 1;
                    let field = self.parse_identifier()?;
                    segments.push(PathSegment::Field(field));
                }
                Some('[') => {
                    self.cursor += 1;
                    let raw = self.take_until(']')?;
                    self.expect(']')?;
                    let trimmed = raw.trim();
                    if trimmed == "*" {
                        segments.push(PathSegment::Wildcard);
                    } else if trimmed == "i" {
                        segments.push(PathSegment::Index(IndexExpr::PairI));
                    } else if trimmed == "i+1" {
                        segments.push(PathSegment::Index(IndexExpr::PairIPlusOne));
                    } else if let Some(name) = trimmed.strip_prefix('$') {
                        segments.push(PathSegment::Index(IndexExpr::Env(name.to_string())));
                    } else {
                        let index = trimmed
                            .parse::<usize>()
                            .map_err(|_| anyhow!("invalid index expression '{}'", trimmed))?;
                        segments.push(PathSegment::Index(IndexExpr::Literal(index)));
                    }
                }
                _ => break,
            }
        }
        Ok(Expr::Reference(ReferenceExpr { root, segments }))
    }

    fn parse_number(&mut self) -> Result<Expr> {
        let start = self.cursor;
        if self.consume('-') {}
        while matches!(self.peek(), Some(ch) if ch.is_ascii_digit()) {
            self.cursor += 1;
        }
        if self.consume('.') {
            while matches!(self.peek(), Some(ch) if ch.is_ascii_digit()) {
                self.cursor += 1;
            }
        }
        if self.cursor == start
            || (self.cursor == start + 1 && &self.input[start..self.cursor] == "-")
        {
            bail!("expected number or reference at '{}'", &self.input[start..]);
        }
        let text = &self.input[start..self.cursor];
        let value = text
            .parse::<f64>()
            .map_err(|_| anyhow!("invalid numeric literal '{}'", text))?;
        Ok(Expr::Number(value))
    }

    fn parse_identifier(&mut self) -> Result<String> {
        let start = self.cursor;
        match self.peek() {
            Some(ch) if ch.is_ascii_alphabetic() || ch == '_' => {
                self.cursor += 1;
            }
            _ => bail!("expected identifier at '{}'", &self.input[self.cursor..]),
        }
        while matches!(self.peek(), Some(ch) if ch.is_ascii_alphanumeric() || ch == '_') {
            self.cursor += 1;
        }
        Ok(self.input[start..self.cursor].to_string())
    }

    fn take_until(&mut self, needle: char) -> Result<&'a str> {
        let start = self.cursor;
        while let Some(ch) = self.peek() {
            if ch == needle {
                return Ok(&self.input[start..self.cursor]);
            }
            self.cursor += 1;
        }
        bail!("unterminated bracket expression");
    }

    fn skip_whitespace(&mut self) {
        while matches!(self.peek(), Some(ch) if ch.is_whitespace()) {
            self.cursor += 1;
        }
    }

    fn expect(&mut self, ch: char) -> Result<()> {
        if self.consume(ch) {
            Ok(())
        } else {
            bail!("expected '{}'", ch);
        }
    }

    fn consume(&mut self, ch: char) -> bool {
        if self.peek() == Some(ch) {
            self.cursor += 1;
            true
        } else {
            false
        }
    }

    fn consume_str(&mut self, s: &str) -> bool {
        if self.input[self.cursor..].starts_with(s) {
            self.cursor += s.len();
            true
        } else {
            false
        }
    }

    fn peek(&self) -> Option<char> {
        self.input[self.cursor..].chars().next()
    }

    fn is_eof(&self) -> bool {
        self.cursor >= self.input.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn evaluates_wildcard_projection() {
        let root = json!({
            "step": [
                { "name": "alpha" },
                { "name": "beta" }
            ]
        });
        let value = evaluate_expression("$step[*].name", &ValueEnv::new(&root)).unwrap();
        assert_eq!(value, json!(["alpha", "beta"]));
    }

    #[test]
    fn evaluates_nested_projection_and_flattening() {
        let root = json!({
            "step": [
                { "topics": [{ "name": "a" }, { "name": "b" }] },
                { "topics": [{ "name": "c" }] }
            ]
        });
        let value = evaluate_expression("$step[*].topics[*].name", &ValueEnv::new(&root)).unwrap();
        assert_eq!(value, json!(["a", "b", "c"]));
    }

    #[test]
    fn evaluates_indexed_access_and_length() {
        let root = json!({
            "step": [
                { "name": "alpha" },
                { "name": "beta" }
            ]
        });
        assert_eq!(
            evaluate_expression("$step[1].name", &ValueEnv::new(&root)).unwrap(),
            json!("beta")
        );
        assert_eq!(
            evaluate_expression("$step.length", &ValueEnv::new(&root)).unwrap(),
            json!(2)
        );
    }

    #[test]
    fn evaluates_arithmetic() {
        let root = json!({ "input": { "value": 4 } });
        assert_eq!(
            evaluate_expression("$input.value + 1", &ValueEnv::new(&root)).unwrap(),
            json!(5.0)
        );
    }

    #[test]
    fn evaluates_arithmetic_subtraction_and_multiplication() {
        let root = json!({ "a": 10, "b": 3 });
        assert_eq!(
            evaluate_expression("$a - $b", &ValueEnv::new(&root)).unwrap(),
            json!(7.0)
        );
        assert_eq!(
            evaluate_expression("$a * $b", &ValueEnv::new(&root)).unwrap(),
            json!(30.0)
        );
    }

    #[test]
    fn evaluates_literal_arithmetic() {
        let root = json!({});
        assert_eq!(
            evaluate_expression("3 + 4", &ValueEnv::new(&root)).unwrap(),
            json!(7.0)
        );
    }

    #[test]
    fn division_by_zero_errors() {
        let root = json!({ "val": 10 });
        let result = evaluate_expression("$val / 0", &ValueEnv::new(&root));
        assert!(result.is_err());
    }

    #[test]
    fn empty_array_wildcard_returns_empty_array() {
        let root = json!({ "items": [] });
        let result = evaluate_expression("$items[*].name", &ValueEnv::new(&root)).unwrap();
        assert_eq!(result, json!([]));
    }

    #[test]
    fn empty_array_nested_wildcard_returns_empty_array() {
        let root = json!({ "items": [] });
        let result = evaluate_expression("$items[*].sub[*].name", &ValueEnv::new(&root)).unwrap();
        assert_eq!(result, json!([]));
    }

    #[test]
    fn type_error_arithmetic_on_string() {
        let root = json!({ "text": "hello" });
        let result = evaluate_expression("$text + 1", &ValueEnv::new(&root));
        assert!(result.is_err());
    }

    #[test]
    fn type_error_wildcard_on_non_array() {
        let root = json!({ "val": 42 });
        let result = evaluate_expression("$val[*].name", &ValueEnv::new(&root));
        assert!(result.is_err());
    }

    #[test]
    fn index_out_of_bounds_errors() {
        let root = json!({ "items": [1, 2] });
        let result = evaluate_expression("$items[5]", &ValueEnv::new(&root));
        assert!(result.is_err());
    }

    #[test]
    fn length_on_string() {
        let root = json!({ "text": "hello" });
        assert_eq!(
            evaluate_expression("$text.length", &ValueEnv::new(&root)).unwrap(),
            json!(5)
        );
    }

    #[test]
    fn length_on_object() {
        let root = json!({ "obj": { "a": 1, "b": 2, "c": 3 } });
        assert_eq!(
            evaluate_expression("$obj.length", &ValueEnv::new(&root)).unwrap(),
            json!(3)
        );
    }

    #[test]
    fn precedence_mul_before_add() {
        let root = json!({});
        // 2 + 3 * 4 = 14 (not 20)
        assert_eq!(
            evaluate_expression("2 + 3 * 4", &ValueEnv::new(&root)).unwrap(),
            json!(14.0)
        );
    }

    #[test]
    fn parenthesized_expression() {
        let root = json!({});
        // (2 + 3) * 4 = 20
        assert_eq!(
            evaluate_expression("(2 + 3) * 4", &ValueEnv::new(&root)).unwrap(),
            json!(20.0)
        );
    }

    #[test]
    fn invalid_expression_errors() {
        assert!(Parser::new("").parse().is_err());
        assert!(Parser::new("$").parse().is_err());
        assert!(Parser::new("$a +").parse().is_err());
    }

    #[test]
    fn evaluate_path_against_value_adds_dollar() {
        let root = json!({ "items": [1, 2, 3] });
        let result = evaluate_path_against_value(&root, "items.length").unwrap();
        assert_eq!(result, json!(3));
    }

    #[test]
    fn looks_like_expression_detects_valid() {
        assert!(looks_like_expression("$step[*].name"));
        assert!(looks_like_expression("$input + 1"));
        assert!(looks_like_expression("$foo.bar.baz"));
        assert!(looks_like_expression("count($items) > 4"));
        assert!(!looks_like_expression("just a string"));
        assert!(!looks_like_expression(""));
    }

    // ── Function calls ──

    #[test]
    fn count_function_on_array() {
        let root = json!({ "items": [1, 2, 3, 4, 5] });
        assert_eq!(
            evaluate_expression("count($items)", &ValueEnv::new(&root)).unwrap(),
            json!(5)
        );
    }

    #[test]
    fn count_function_on_empty_array() {
        let root = json!({ "items": [] });
        assert_eq!(
            evaluate_expression("count($items)", &ValueEnv::new(&root)).unwrap(),
            json!(0)
        );
    }

    #[test]
    fn count_function_on_object() {
        let root = json!({ "map": { "a": 1, "b": 2 } });
        assert_eq!(
            evaluate_expression("count($map)", &ValueEnv::new(&root)).unwrap(),
            json!(2)
        );
    }

    // ── Comparison operators ──

    #[test]
    fn comparison_greater_than() {
        let root = json!({ "items": [1, 2, 3, 4, 5] });
        assert_eq!(
            evaluate_expression("count($items) > 4", &ValueEnv::new(&root)).unwrap(),
            json!(true)
        );
        assert_eq!(
            evaluate_expression("count($items) > 5", &ValueEnv::new(&root)).unwrap(),
            json!(false)
        );
    }

    #[test]
    fn comparison_less_than_or_equal() {
        let root = json!({ "items": [1, 2, 3] });
        assert_eq!(
            evaluate_expression("count($items) <= 4", &ValueEnv::new(&root)).unwrap(),
            json!(true)
        );
        assert_eq!(
            evaluate_expression("count($items) <= 3", &ValueEnv::new(&root)).unwrap(),
            json!(true)
        );
        assert_eq!(
            evaluate_expression("count($items) <= 2", &ValueEnv::new(&root)).unwrap(),
            json!(false)
        );
    }

    #[test]
    fn comparison_equality() {
        let root = json!({});
        assert_eq!(
            evaluate_expression("3 == 3", &ValueEnv::new(&root)).unwrap(),
            json!(true)
        );
        assert_eq!(
            evaluate_expression("3 != 4", &ValueEnv::new(&root)).unwrap(),
            json!(true)
        );
    }

    #[test]
    fn converge_when_guard_expression() {
        // This is the exact pattern emitted by converge_expand.rs
        let root = json!({ "thread_syntheses": [1, 2, 3] });
        assert_eq!(
            evaluate_expression("count($thread_syntheses) <= 4", &ValueEnv::new(&root)).unwrap(),
            json!(true)
        );
        let root = json!({ "thread_syntheses": [1, 2, 3, 4, 5, 6] });
        assert_eq!(
            evaluate_expression("count($thread_syntheses) > 4", &ValueEnv::new(&root)).unwrap(),
            json!(true)
        );
    }

    #[test]
    fn boolean_literals() {
        let root = json!({});
        assert_eq!(
            evaluate_expression("true", &ValueEnv::new(&root)).unwrap(),
            json!(true)
        );
        assert_eq!(
            evaluate_expression("false", &ValueEnv::new(&root)).unwrap(),
            json!(false)
        );
    }

    #[test]
    fn unknown_function_errors() {
        let root = json!({ "items": [1] });
        let result = evaluate_expression("unknown_fn($items)", &ValueEnv::new(&root));
        assert!(result.is_err());
    }
}
