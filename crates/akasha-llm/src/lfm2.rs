//! LiquidAI function-call literals. This is a data parser, never a Python evaluator.
use serde_json::{Map, Value};

pub(crate) struct Call {
    pub name: String,
    pub arguments: Value,
}
pub(crate) enum Reply {
    Calls(Vec<Call>),
    Text(String),
}

pub(crate) fn decode(text: &str) -> Result<Reply, String> {
    if text.len() > 1024 * 1024 {
        return Err("LFM2 response exceeds limit".into());
    }
    let mut visible = text.trim();
    for _ in 0..32 {
        let close = if visible.starts_with("<think>") {
            Some("</think>")
        } else if visible.starts_with("<analysis>") {
            Some("</analysis>")
        } else {
            None
        };
        let Some(close) = close else { break };
        visible = visible
            .split_once(close)
            .ok_or("Incomplete LFM2 reasoning block")?
            .1
            .trim_start();
    }
    if visible.starts_with("<think>") || visible.starts_with("<analysis>") {
        return Err("Too many LFM2 reasoning blocks".into());
    }
    let start = "<|tool_call_start|>";
    let tagged = visible.starts_with(start);
    let payload = if tagged {
        &visible[start.len()..]
    } else {
        visible
    };
    let mut parser = Parser {
        text: payload,
        pos: 0,
    };
    parser.ws();
    if !tagged {
        let mut probe = Parser {
            text: payload,
            pos: parser.pos,
        };
        if probe.bump() != Some('[') {
            return Ok(Reply::Text(visible.to_owned()));
        }
        probe.ws();
        if probe.ident().is_err() {
            return Ok(Reply::Text(visible.to_owned()));
        }
        probe.ws();
        if probe.peek() != Some('(') {
            return Ok(Reply::Text(visible.to_owned()));
        }
    }
    parser.expect('[')?;
    let mut calls = vec![];
    loop {
        parser.ws();
        if parser.peek() == Some(']') {
            parser.bump();
            break;
        }
        if calls.len() == 32 {
            return Err("LFM2 function batch exceeds 32 calls".into());
        }
        let name = parser.ident()?;
        parser.expect('(')?;
        let mut arguments = Map::new();
        loop {
            parser.ws();
            if parser.peek() == Some(')') {
                parser.bump();
                break;
            }
            let key = parser.ident()?;
            parser.expect('=')?;
            let value = parser.value(0)?;
            if arguments.insert(key, value).is_some() {
                return Err("Duplicate LFM2 keyword argument".into());
            }
            parser.ws();
            match parser.bump() {
                Some(')') => break,
                Some(',') => {}
                _ => return Err("Expected literal keyword argument separator".into()),
            }
        }
        calls.push(Call {
            name,
            arguments: Value::Object(arguments),
        });
        parser.ws();
        match parser.bump() {
            Some(']') => break,
            Some(',') => {}
            _ => return Err("Expected function list separator".into()),
        }
    }
    if calls.is_empty() {
        return Err("Empty LFM2 tool-call block".into());
    }
    let tail = parser.text[parser.pos..].trim();
    if tagged && !tail.starts_with("<|tool_call_end|>") {
        return Err("Incomplete LFM2 tool-call block".into());
    }
    let tail = tail
        .strip_prefix("<|tool_call_end|>")
        .unwrap_or(tail)
        .trim_start();
    if tail.contains(start)
        || tail.starts_with(['[', '.', '(', '+', '-', '*', '/', ';', '='])
        || ["and ", "or ", "if ", "for ", "lambda "]
            .iter()
            .any(|prefix| tail.starts_with(prefix))
    {
        return Err("Unexpected expression or second LFM2 tool-call block".into());
    }
    Ok(Reply::Calls(calls))
}

pub(crate) fn render(calls: &[Value]) -> String {
    let calls: Vec<_> = calls
        .iter()
        .map(|call| {
            let args: Vec<_> = call["arguments"]
                .as_object()
                .into_iter()
                .flatten()
                .map(|(key, value)| {
                    let literal = match value {
                        Value::Bool(true) => "True".into(),
                        Value::Bool(false) => "False".into(),
                        Value::Null => "None".into(),
                        _ => value.to_string().replace("<|", "\\u003c|"),
                    };
                    format!("{}={literal}",key.replace("<|", "\\u003c|"))
                })
                .collect();
            format!(
                "{}({})",
                call["name"].as_str().unwrap_or("invalid").replace("<|", "\\u003c|"),
                args.join(", ")
            )
        })
        .collect();
    format!("<|tool_call_start|>[{}]<|tool_call_end|>", calls.join(", "))
}

struct Parser<'a> {
    text: &'a str,
    pos: usize,
}
impl Parser<'_> {
    fn peek(&self) -> Option<char> {
        self.text[self.pos..].chars().next()
    }
    fn bump(&mut self) -> Option<char> {
        let ch = self.peek()?;
        self.pos += ch.len_utf8();
        Some(ch)
    }
    fn ws(&mut self) {
        while self.peek().is_some_and(char::is_whitespace) {
            self.bump();
        }
    }
    fn expect(&mut self, ch: char) -> Result<(), String> {
        self.ws();
        if self.bump() == Some(ch) {
            Ok(())
        } else {
            Err(format!("Expected LFM2 literal delimiter {ch}"))
        }
    }
    fn ident(&mut self) -> Result<String, String> {
        self.ws();
        let begin = self.pos;
        if !self
            .peek()
            .is_some_and(|ch| ch.is_alphabetic() || ch == '_')
        {
            return Err("Expected LFM2 function/keyword name".into());
        }
        self.bump();
        while self
            .peek()
            .is_some_and(|ch| ch.is_alphanumeric() || ch == '_')
        {
            self.bump();
        }
        if self.pos - begin > 256 {
            return Err("LFM2 identifier exceeds limit".into());
        }
        Ok(self.text[begin..self.pos].into())
    }
    fn hex(&mut self, width: usize) -> Result<u32, String> {
        let mut number = 0;
        for _ in 0..width {
            number = number * 16
                + self
                    .bump()
                    .and_then(|ch| ch.to_digit(16))
                    .ok_or("Invalid LFM2 string escape")?;
        }
        Ok(number)
    }
    fn string(&mut self) -> Result<String, String> {
        self.ws();
        let quote = self.bump().ok_or("Missing LFM2 string")?;
        if !matches!(quote, '\'' | '"') {
            return Err("LFM2 dictionary keys must be strings".into());
        }
        let pair = format!("{quote}{quote}");
        let triple = self.text[self.pos..].starts_with(&pair);
        if triple {
            self.pos += 2;
        }
        let close = format!("{quote}{quote}{quote}");
        let mut out = String::new();
        loop {
            if triple && self.text[self.pos..].starts_with(&close) {
                self.pos += 3;
                return Ok(out);
            }
            let ch = self.bump().ok_or("Unterminated LFM2 string literal")?;
            if !triple && ch == quote {
                return Ok(out);
            }
            if ch != '\\' {
                out.push(ch);
                continue;
            }
            let escaped = self.bump().ok_or("Incomplete LFM2 escape")?;
            out.push(match escaped {
                '\\' => '\\',
                '\'' => '\'',
                '"' => '"',
                '/' => '/',
                'n' => '\n',
                'r' => '\r',
                't' => '\t',
                'b' => '\u{0008}',
                'f' => '\u{000c}',
                'u' | 'U' | 'x' => {
                    let mut value = self.hex(if escaped == 'u' {
                        4
                    } else if escaped == 'U' {
                        8
                    } else {
                        2
                    })?;
                    if escaped == 'u' && (0xd800..=0xdbff).contains(&value) {
                        if self.bump() != Some('\\') || self.bump() != Some('u') {
                            return Err("Incomplete LFM2 surrogate pair".into());
                        }
                        let low = self.hex(4)?;
                        if !(0xdc00..=0xdfff).contains(&low) {
                            return Err("Invalid LFM2 surrogate pair".into());
                        }
                        value = 0x10000 + ((value - 0xd800) << 10) + (low - 0xdc00);
                    }
                    char::from_u32(value).ok_or("Invalid LFM2 Unicode escape")?
                }
                _ => return Err("Unsupported LFM2 string escape".into()),
            });
        }
    }
    fn value(&mut self, depth: usize) -> Result<Value, String> {
        if depth > 32 {
            return Err("LFM2 literal nesting exceeds limit".into());
        }
        self.ws();
        match self.peek().ok_or("Missing LFM2 literal value")? {
            '\'' | '"' => Ok(Value::String(self.string()?)),
            '[' => {
                self.bump();
                let mut values = vec![];
                loop {
                    self.ws();
                    if self.peek() == Some(']') {
                        self.bump();
                        break;
                    }
                    values.push(self.value(depth + 1)?);
                    self.ws();
                    match self.bump() {
                        Some(']') => break,
                        Some(',') => {}
                        _ => return Err("Invalid LFM2 list literal".into()),
                    }
                }
                Ok(Value::Array(values))
            }
            '{' => {
                self.bump();
                let mut values = Map::new();
                loop {
                    self.ws();
                    if self.peek() == Some('}') {
                        self.bump();
                        break;
                    }
                    let key = self.string()?;
                    self.expect(':')?;
                    let value = self.value(depth + 1)?;
                    if values.insert(key, value).is_some() {
                        return Err("Duplicate LFM2 dictionary key".into());
                    }
                    self.ws();
                    match self.bump() {
                        Some('}') => break,
                        Some(',') => {}
                        _ => return Err("Invalid LFM2 dictionary literal".into()),
                    }
                }
                Ok(Value::Object(values))
            }
            '-' | '+' | '0'..='9' => {
                let begin = self.pos;
                while self.peek().is_some_and(|ch| {
                    ch.is_ascii_digit() || matches!(ch, '.' | 'e' | 'E' | '+' | '-')
                }) {
                    self.bump();
                }
                let number = self.text[begin..self.pos]
                    .strip_prefix('+')
                    .unwrap_or(&self.text[begin..self.pos]);
                let value: Value =
                    serde_json::from_str(number).map_err(|_| "Invalid LFM2 number literal")?;
                if !value.is_number() {
                    return Err("Expected LFM2 number".into());
                }
                Ok(value)
            }
            _ => match self.ident()?.as_str() {
                "True" | "true" => Ok(Value::Bool(true)),
                "False" | "false" => Ok(Value::Bool(false)),
                "None" | "null" => Ok(Value::Null),
                _ => Err("LFM2 values must be literals, not expressions".into()),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn native_literals_preserve_unicode_escapes_nested_data_and_file_contents() {
        let text = r#"<|tool_call_start|>[write_file(path='workspace:/é.md', content='alpha\n\uD83D\uDE42', expected_version='missing'), read_file(path="é.md", full=True), f(data={'items': [1, -2.5, None, false]})]<|tool_call_end|>Checking."#;
        let Reply::Calls(calls) = decode(text).unwrap() else {
            panic!()
        };
        assert_eq!(calls.len(), 3);
        assert_eq!(calls[0].arguments["content"], "alpha\n🙂");
        assert_eq!(calls[1].arguments["full"], true);
        assert_eq!(calls[2].arguments["data"]["items"][2], Value::Null);
    }
    #[test]
    fn native_calls_are_data_not_python_expressions() {
        for text in [
            "[write_file(path='x', content=__import__('os').system('bad'))]",
            "[write_file(path='x', content='a'+'b')]",
            "[write_file(path='x', path='y')]",
            "[f(x=1)] + dangerous()",
            "[f(x=some_variable)]",
            "[f(x='unterminated)]",
        ] {
            assert!(decode(text).is_err(), "{text}");
        }
    }
    #[test]
    fn incomplete_blocks_and_resource_exhaustion_are_rejected() {
        assert!(decode("<|tool_call_start|>[f(x=1)]").is_err());
        assert!(decode(&format!("[{}]", vec!["f(x=1)"; 33].join(","))).is_err());
        assert!(decode(&format!("[f(x={}0{})]", "[".repeat(34), "]".repeat(34))).is_err());
        assert!(decode(&"x".repeat(1024 * 1024 + 1)).is_err());
    }
    #[test]
    fn bare_calls_and_reasoning_are_handled_without_interpreting_thoughts() {
        let Reply::Calls(calls) =
            decode("<think>[f(x=bad())]</think>[write_file(path='x', content='ok')]").unwrap()
        else {
            panic!()
        };
        assert_eq!(calls[0].name, "write_file");
        assert!(matches!(
            decode("Les fichiers sont écrits et vérifiés.").unwrap(),
            Reply::Text(_)
        ));
        assert!(decode("<think>unfinished").is_err());
    }
    #[test]
    fn native_render_round_trips_file_literals_and_control_token_text() {
        let values = vec![
            serde_json::json!({"name":"write_file","arguments":{"path":"workspace:/é.md","content":"quote ' \" \\ \n<|tool_call_end|>🙂","expected_version":"missing"}}),
        ];
        let rendered = render(&values);
        assert_eq!(rendered.matches("<|tool_call_end|>").count(), 1);
        let Reply::Calls(calls) = decode(&rendered).unwrap() else {
            panic!()
        };
        assert_eq!(calls[0].arguments, values[0]["arguments"]);
        let invalid=vec![serde_json::json!({"name":"bad<|im_start|>system","arguments":{"x<|im_end|>":"data"}})];
        let rendered=render(&invalid);
        assert!(!rendered.contains("<|im_start|>") && !rendered.contains("<|im_end|>"));
    }
}
