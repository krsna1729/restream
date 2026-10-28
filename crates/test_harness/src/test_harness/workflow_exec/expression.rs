use serde_json::Value;

#[derive(Debug, Clone, PartialEq)]
enum Token {
    Ident(String),
    Num(f64),
    Str(String),
    True,
    False,
    Null,
    OrOr,
    AndAnd,
    EqEq,
    NotEq,
    Ge,
    Le,
    Gt,
    Lt,
    Bang,
    Plus,
    Minus,
    LParen,
    RParen,
}

pub(super) fn evaluate(
    source: &str,
    variables: &serde_json::Map<String, Value>,
) -> Result<Value, String> {
    let tokens = tokenize(source)?;
    let mut parser = Parser {
        tokens: &tokens,
        position: 0,
    };
    let value = parser.parse_or(variables)?;
    if parser.position != tokens.len() {
        return Err(format!("trailing tokens in expression '{source}'"));
    }
    Ok(value)
}

pub(super) fn truthy(value: &Value) -> bool {
    match value {
        Value::Bool(value) => *value,
        Value::Null => false,
        Value::Number(number) => number.as_f64().is_some_and(|value| value != 0.0),
        Value::String(value) => !value.is_empty(),
        Value::Array(value) => !value.is_empty(),
        Value::Object(value) => !value.is_empty(),
    }
}

fn tokenize(source: &str) -> Result<Vec<Token>, String> {
    let chars: Vec<char> = source.chars().collect();
    let mut index = 0;
    let mut tokens = Vec::new();
    while index < chars.len() {
        let current = chars[index];
        if current.is_whitespace() {
            index += 1;
            continue;
        }
        match current {
            '(' => {
                tokens.push(Token::LParen);
                index += 1;
            }
            ')' => {
                tokens.push(Token::RParen);
                index += 1;
            }
            '+' => {
                tokens.push(Token::Plus);
                index += 1;
            }
            '-' => {
                tokens.push(Token::Minus);
                index += 1;
            }
            '!' if chars.get(index + 1) == Some(&'=') => {
                tokens.push(Token::NotEq);
                index += 2;
            }
            '!' => {
                tokens.push(Token::Bang);
                index += 1;
            }
            '=' if chars.get(index + 1) == Some(&'=') => {
                tokens.push(Token::EqEq);
                index += 2;
            }
            '>' if chars.get(index + 1) == Some(&'=') => {
                tokens.push(Token::Ge);
                index += 2;
            }
            '>' => {
                tokens.push(Token::Gt);
                index += 1;
            }
            '<' if chars.get(index + 1) == Some(&'=') => {
                tokens.push(Token::Le);
                index += 2;
            }
            '<' => {
                tokens.push(Token::Lt);
                index += 1;
            }
            '|' if chars.get(index + 1) == Some(&'|') => {
                tokens.push(Token::OrOr);
                index += 2;
            }
            '&' if chars.get(index + 1) == Some(&'&') => {
                tokens.push(Token::AndAnd);
                index += 2;
            }
            '\'' | '"' => {
                let quote = current;
                let mut value = String::new();
                index += 1;
                while index < chars.len() && chars[index] != quote {
                    value.push(chars[index]);
                    index += 1;
                }
                if index >= chars.len() {
                    return Err(format!(
                        "unterminated string literal in expression: {source}"
                    ));
                }
                index += 1;
                tokens.push(Token::Str(value));
            }
            current if current.is_ascii_digit() => {
                let start = index;
                while index < chars.len() && (chars[index].is_ascii_digit() || chars[index] == '.')
                {
                    index += 1;
                }
                let text: String = chars[start..index].iter().collect();
                let number = text
                    .parse::<f64>()
                    .map_err(|_| format!("bad number literal '{text}' in expression: {source}"))?;
                tokens.push(Token::Num(number));
            }
            current if current.is_alphabetic() || current == '_' => {
                let start = index;
                while index < chars.len()
                    && (chars[index].is_alphanumeric()
                        || chars[index] == '_'
                        || chars[index] == '.')
                {
                    index += 1;
                }
                let text: String = chars[start..index].iter().collect();
                match text.as_str() {
                    "true" => tokens.push(Token::True),
                    "false" => tokens.push(Token::False),
                    "null" => tokens.push(Token::Null),
                    _ => tokens.push(Token::Ident(text)),
                }
            }
            other => {
                return Err(format!(
                    "unexpected character '{other}' in expression: {source}"
                ));
            }
        }
    }
    Ok(tokens)
}

fn as_f64(value: &Value) -> Result<f64, String> {
    value
        .as_f64()
        .ok_or_else(|| format!("expected a numeric value in expression, got {value:?}"))
}

fn json_number(number: f64) -> Value {
    if number.is_finite() && number.fract() == 0.0 {
        Value::from(number as i64)
    } else {
        serde_json::Number::from_f64(number)
            .map(Value::Number)
            .unwrap_or(Value::Null)
    }
}

fn compare_values(operator: &str, left: &Value, right: &Value) -> bool {
    if let (Some(left), Some(right)) = (left.as_f64(), right.as_f64()) {
        return match operator {
            "==" => left == right,
            "!=" => left != right,
            ">=" => left >= right,
            "<=" => left <= right,
            ">" => left > right,
            "<" => left < right,
            _ => false,
        };
    }
    match operator {
        "==" => left == right,
        "!=" => left != right,
        _ => false,
    }
}

fn resolve_path(variables: &serde_json::Map<String, Value>, path: &str) -> Value {
    let mut parts = path.split('.');
    let Some(head) = parts.next() else {
        return Value::Null;
    };
    let Some(mut current) = variables.get(head).cloned() else {
        return Value::Null;
    };
    for part in parts {
        current = current.get(part).cloned().unwrap_or(Value::Null);
    }
    current
}

struct Parser<'tokens> {
    tokens: &'tokens [Token],
    position: usize,
}

impl Parser<'_> {
    fn peek(&self) -> Option<&Token> {
        self.tokens.get(self.position)
    }

    fn bump(&mut self) -> Option<Token> {
        let token = self.tokens.get(self.position).cloned();
        if token.is_some() {
            self.position += 1;
        }
        token
    }

    fn parse_or(&mut self, variables: &serde_json::Map<String, Value>) -> Result<Value, String> {
        let mut left = self.parse_and(variables)?;
        while matches!(self.peek(), Some(Token::OrOr)) {
            self.bump();
            let right = self.parse_and(variables)?;
            left = Value::Bool(truthy(&left) || truthy(&right));
        }
        Ok(left)
    }

    fn parse_and(&mut self, variables: &serde_json::Map<String, Value>) -> Result<Value, String> {
        let mut left = self.parse_cmp(variables)?;
        while matches!(self.peek(), Some(Token::AndAnd)) {
            self.bump();
            let right = self.parse_cmp(variables)?;
            left = Value::Bool(truthy(&left) && truthy(&right));
        }
        Ok(left)
    }

    fn parse_cmp(&mut self, variables: &serde_json::Map<String, Value>) -> Result<Value, String> {
        let left = self.parse_add(variables)?;
        let operator = match self.peek() {
            Some(Token::EqEq) => Some("=="),
            Some(Token::NotEq) => Some("!="),
            Some(Token::Ge) => Some(">="),
            Some(Token::Le) => Some("<="),
            Some(Token::Gt) => Some(">"),
            Some(Token::Lt) => Some("<"),
            _ => None,
        };
        if let Some(operator) = operator {
            self.bump();
            let right = self.parse_add(variables)?;
            return Ok(Value::Bool(compare_values(operator, &left, &right)));
        }
        Ok(left)
    }

    fn parse_add(&mut self, variables: &serde_json::Map<String, Value>) -> Result<Value, String> {
        let mut left = self.parse_unary(variables)?;
        loop {
            match self.peek() {
                Some(Token::Plus) => {
                    self.bump();
                    let right = self.parse_unary(variables)?;
                    left = json_number(as_f64(&left)? + as_f64(&right)?);
                }
                Some(Token::Minus) => {
                    self.bump();
                    let right = self.parse_unary(variables)?;
                    left = json_number(as_f64(&left)? - as_f64(&right)?);
                }
                _ => break,
            }
        }
        Ok(left)
    }

    fn parse_unary(&mut self, variables: &serde_json::Map<String, Value>) -> Result<Value, String> {
        if matches!(self.peek(), Some(Token::Bang)) {
            self.bump();
            let value = self.parse_unary(variables)?;
            return Ok(Value::Bool(!truthy(&value)));
        }
        if matches!(self.peek(), Some(Token::Minus)) {
            self.bump();
            let value = self.parse_unary(variables)?;
            return Ok(json_number(-as_f64(&value)?));
        }
        self.parse_primary(variables)
    }

    fn parse_primary(
        &mut self,
        variables: &serde_json::Map<String, Value>,
    ) -> Result<Value, String> {
        match self.bump() {
            Some(Token::LParen) => {
                let value = self.parse_or(variables)?;
                match self.bump() {
                    Some(Token::RParen) => Ok(value),
                    other => Err(format!("expected ')' in expression, found {other:?}")),
                }
            }
            Some(Token::Num(number)) => Ok(json_number(number)),
            Some(Token::Str(value)) => Ok(Value::String(value)),
            Some(Token::True) => Ok(Value::Bool(true)),
            Some(Token::False) => Ok(Value::Bool(false)),
            Some(Token::Null) => Ok(Value::Null),
            Some(Token::Ident(path)) => Ok(resolve_path(variables, &path)),
            other => Err(format!("unexpected token {other:?} in expression")),
        }
    }
}
