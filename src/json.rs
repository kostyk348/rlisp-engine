// src/json.rs — Уровень 1 (HAL-обвязка): минимальный JSON без зависимостей.
// Как png.rs: формат данных «трубы», не игровая логика. Используется:
//   — MCP-сервером (--mcp, JSON-RPC 2.0 поверх stdio) — «AI Ready» движка;
//   — HTTP-API редактора (--serve): /api/* и сцена для инспектора.
// Значения: Null/Bool/Num/Str/Arr/Obj (порядок ключей сохраняется — BTreeMap).

use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq)]
pub enum Json {
    Null,
    Bool(bool),
    Num(f64),
    Str(String),
    Arr(Vec<Json>),
    Obj(BTreeMap<String, Json>),
}

impl Json {
    pub fn obj(pairs: Vec<(&str, Json)>) -> Json {
        let mut m = BTreeMap::new();
        for (k, v) in pairs {
            m.insert(k.to_string(), v);
        }
        Json::Obj(m)
    }

    pub fn num(x: f64) -> Json {
        Json::Num(x)
    }

    pub fn int(x: i64) -> Json {
        Json::Num(x as f64)
    }

    pub fn str(s: &str) -> Json {
        Json::Str(s.to_string())
    }

    pub fn get(&self, key: &str) -> Option<&Json> {
        match self {
            Json::Obj(m) => m.get(key),
            _ => None,
        }
    }

    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Json::Num(x) => Some(*x),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Json::Str(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Json::Bool(b) => Some(*b),
            _ => None,
        }
    }

    /// Короткая строка для ответа редактору (сериализация в одну строку).
    pub fn dump(&self) -> String {
        let mut s = String::new();
        self.write(&mut s);
        s
    }

    fn write(&self, out: &mut String) {
        match self {
            Json::Null => out.push_str("null"),
            Json::Bool(true) => out.push_str("true"),
            Json::Bool(false) => out.push_str("false"),
            Json::Num(x) => {
                if x.is_finite() {
                    if x.fract() == 0.0 && x.abs() < 9e15 {
                        out.push_str(&format!("{}", *x as i64));
                    } else {
                        out.push_str(&format!("{}", x));
                    }
                } else {
                    out.push_str("null");
                }
            }
            Json::Str(s) => write_escaped(s, out),
            Json::Arr(items) => {
                out.push('[');
                for (i, v) in items.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    v.write(out);
                }
                out.push(']');
            }
            Json::Obj(m) => {
                out.push('{');
                for (i, (k, v)) in m.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    write_escaped(k, out);
                    out.push(':');
                    v.write(out);
                }
                out.push('}');
            }
        }
    }
}

pub fn write_escaped(s: &str, out: &mut String) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
}

// ——————————————————————————— парсер ———————————————————————————

pub fn parse(src: &str) -> Result<Json, String> {
    let b: Vec<char> = src.chars().collect();
    let mut pos = 0usize;
    let v = parse_value(&b, &mut pos)?;
    skip_ws(&b, &mut pos);
    Ok(v)
}

fn skip_ws(b: &[char], pos: &mut usize) {
    while *pos < b.len() && matches!(b[*pos], ' ' | '\t' | '\n' | '\r') {
        *pos += 1;
    }
}

fn peek(b: &[char], pos: &mut usize) -> Result<char, String> {
    skip_ws(b, pos);
    b.get(*pos)
        .copied()
        .ok_or_else(|| "json: неожиданный конец".into())
}

fn parse_value(b: &[char], pos: &mut usize) -> Result<Json, String> {
    let c = peek(b, pos)?;
    match c {
        '{' => parse_obj(b, pos),
        '[' => parse_arr(b, pos),
        '"' => Ok(Json::Str(parse_str(b, pos)?)),
        't' => lit(b, pos, "true", Json::Bool(true)),
        'f' => lit(b, pos, "false", Json::Bool(false)),
        'n' => lit(b, pos, "null", Json::Null),
        _ => parse_num(b, pos),
    }
}

fn lit(b: &[char], pos: &mut usize, word: &str, v: Json) -> Result<Json, String> {
    for ch in word.chars() {
        if b.get(*pos) != Some(&ch) {
            return Err(format!("json: ожидалось «{}»", word));
        }
        *pos += 1;
    }
    Ok(v)
}

fn parse_obj(b: &[char], pos: &mut usize) -> Result<Json, String> {
    *pos += 1; // {
    let mut m = BTreeMap::new();
    if peek(b, pos)? == '}' {
        *pos += 1;
        return Ok(Json::Obj(m));
    }
    loop {
        let c = peek(b, pos)?;
        if c != '"' {
            return Err("json: в объекте ждут строковый ключ".into());
        }
        let k = parse_str(b, pos)?;
        if peek(b, pos)? != ':' {
            return Err("json: после ключа ждут ':'".into());
        }
        *pos += 1;
        let v = parse_value(b, pos)?;
        m.insert(k, v);
        match peek(b, pos)? {
            ',' => {
                *pos += 1;
            }
            '}' => {
                *pos += 1;
                return Ok(Json::Obj(m));
            }
            _ => return Err("json: в объекте ждут ',' или '}'".into()),
        }
    }
}

fn parse_arr(b: &[char], pos: &mut usize) -> Result<Json, String> {
    *pos += 1; // [
    let mut items = Vec::new();
    if peek(b, pos)? == ']' {
        *pos += 1;
        return Ok(Json::Arr(items));
    }
    loop {
        items.push(parse_value(b, pos)?);
        match peek(b, pos)? {
            ',' => {
                *pos += 1;
            }
            ']' => {
                *pos += 1;
                return Ok(Json::Arr(items));
            }
            _ => return Err("json: в массиве ждут ',' или ']'".into()),
        }
    }
}

fn parse_str(b: &[char], pos: &mut usize) -> Result<String, String> {
    if peek(b, pos)? != '"' {
        return Err("json: ждут '\"'".into());
    }
    *pos += 1;
    let mut s = String::new();
    loop {
        let c = b.get(*pos).copied().ok_or("json: строка не закрыта")?;
        *pos += 1;
        match c {
            '"' => return Ok(s),
            '\\' => {
                let e = b.get(*pos).copied().ok_or("json: escape не закрыт")?;
                *pos += 1;
                match e {
                    '"' => s.push('"'),
                    '\\' => s.push('\\'),
                    '/' => s.push('/'),
                    'b' => s.push('\u{8}'),
                    'f' => s.push('\u{c}'),
                    'n' => s.push('\n'),
                    'r' => s.push('\r'),
                    't' => s.push('\t'),
                    'u' => {
                        let mut code = 0u32;
                        for _ in 0..4 {
                            let h = b.get(*pos).copied().ok_or("json: \\u не закрыт")?;
                            *pos += 1;
                            code = code * 16 + h.to_digit(16).ok_or("json: плохая \\u-цифра")?;
                        }
                        // суррогатная пара → UTF-8
                        if (0xD800..0xDC00).contains(&code)
                            && b.get(*pos) == Some(&'\\')
                            && b.get(*pos + 1) == Some(&'u')
                        {
                            *pos += 2;
                            let mut lo = 0u32;
                            for _ in 0..4 {
                                let h = b.get(*pos).copied().ok_or("json: \\u не закрыт")?;
                                *pos += 1;
                                lo = lo * 16 + h.to_digit(16).ok_or("json: плохая \\u-цифра")?;
                            }
                            code = 0x10000 + ((code - 0xD800) << 10) + (lo - 0xDC00);
                        }
                        s.push(char::from_u32(code).unwrap_or('\u{FFFD}'));
                    }
                    _ => return Err(format!("json: неизвестный escape \\{}", e)),
                }
            }
            c => s.push(c),
        }
    }
}

fn parse_num(b: &[char], pos: &mut usize) -> Result<Json, String> {
    skip_ws(b, pos);
    let start = *pos;
    while *pos < b.len()
        && (b[*pos].is_ascii_digit() || matches!(b[*pos], '-' | '+' | '.' | 'e' | 'E'))
    {
        *pos += 1;
    }
    let s: String = b[start..*pos].iter().collect();
    s.parse::<f64>()
        .map(Json::Num)
        .map_err(|_| format!("json: не число «{}»", s))
}

// ——————————————————————————— Value ↔ Json (для сцены редактора) ———————————————————————————

use crate::interp::Value;

/// Lisp → JSON для API редактора/MCP: table → object, list/vector → array,
/// F32Vec → array, Str/Sym → string, Nil → null, Lambda/Native → краткая строка.
pub fn value_to_json(v: &Value) -> Json {
    use crate::interp::fmt_value;
    match v {
        Value::Nil => Json::Null,
        Value::Bool(b) => Json::Bool(*b),
        Value::Int(i) => Json::int(*i),
        Value::Float(f) => Json::num(*f),
        Value::Sym(s) => Json::Str(s.clone()),
        Value::Str(s) => Json::Str(s.clone()),
        Value::Ptr(id) => Json::int(*id as i64),
        Value::List(items) => Json::Arr(items.iter().map(value_to_json).collect()),
        Value::Vector(r) => Json::Arr(r.borrow().iter().map(value_to_json).collect()),
        Value::F32Vec(r) => Json::Arr(r.borrow().iter().map(|x| Json::num(*x as f64)).collect()),
        Value::Table(t) => {
            let mut m = BTreeMap::new();
            for (k, val) in t.borrow().iter() {
                m.insert(k.clone(), value_to_json(val));
            }
            Json::Obj(m)
        }
        other => Json::Str(fmt_value(other)),
    }
}

/// JSON → Lisp (аргументы MCP-инструментов): obj → table, array → list,
/// строка → Str, число → Int/Float, true/false → Bool, null → Nil.
pub fn json_to_value(j: &Json) -> Value {
    match j {
        Json::Null => Value::Nil,
        Json::Bool(b) => Value::Bool(*b),
        Json::Num(x) => {
            if x.fract() == 0.0 && x.abs() < 9e15 {
                Value::Int(*x as i64)
            } else {
                Value::Float(*x)
            }
        }
        Json::Str(s) => Value::Str(s.clone()),
        Json::Arr(items) => Value::List(items.iter().map(json_to_value).collect()),
        Json::Obj(m) => {
            use std::cell::RefCell;
            use std::rc::Rc;
            let mut t = std::collections::HashMap::new();
            for (k, v) in m {
                t.insert(k.clone(), json_to_value(v));
            }
            Value::Table(Rc::new(RefCell::new(t)))
        }
    }
}
