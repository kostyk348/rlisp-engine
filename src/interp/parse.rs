// src/interp/parse.rs — Синтаксический анализ Lisp (скалярный reader и точка входа).
//
// Здесь живёт СТАРЫЙ посимвольный reader (v0.1–v0.3). Он остаётся:
//   - fallback'ом на не-x86_64,
//   - ЭТАЛОНОМ взаимной сверки AST для быстрого reader'а
//     (`src/interp/parse_fast.rs`, тесты `src/parse_diff.rs`).
// Поэтому его код не оптимизируется: любое ускорение здесь сломало бы
// честность замера `--bench-parse`.
//
// Быстрый reader (byte-class скан + интерн символов) реэкспортируется ниже
// под прежними именами — внешние вызовы не меняются.

use super::{is_sym, vec_into_chain, Value};

pub use super::parse_fast::{lex_only_simd, parse_program_simd};

// ——————————————————————————— парсер (Reader) ———————————————————————————

/// Точка входа reader'а. На x86_64 — быстрый reader (byte-class скан +
/// интерн символов, `src/interp/parse_fast.rs`), иначе скалярный путь. Оба
/// пути обязаны давать одинаковые AST — сверяются в тестах и в
/// `--bench-parse` на реальном корпусе assets/lisp/*.lisp.
pub fn parse_program(src: &str) -> Result<Vec<Value>, String> {
    #[cfg(target_arch = "x86_64")]
    {
        parse_program_simd(src)
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        parse_program_scalar(src)
    }
}

/// СТАРЫЙ посимвольный reader (v0.1–v0.3): fallback (не-x86_64) и эталон AST.
pub fn parse_program_scalar(src: &str) -> Result<Vec<Value>, String> {
    let chars: Vec<char> = src.chars().collect();
    let mut pos = 0usize;
    let mut out = Vec::new();
    loop {
        skip_ws(&chars, &mut pos)?;
        if pos >= chars.len() {
            break;
        }
        out.push(parse_expr(&chars, &mut pos)?);
    }
    Ok(out)
}

fn skip_ws(chars: &[char], pos: &mut usize) -> Result<(), String> {
    while *pos < chars.len() {
        let c = chars[*pos];
        if c.is_whitespace() {
            *pos += 1;
        } else if c == ';' {
            while *pos < chars.len() && chars[*pos] != '\n' {
                *pos += 1;
            }
        } else {
            break;
        }
    }
    Ok(())
}

fn parse_expr(chars: &[char], pos: &mut usize) -> Result<Value, String> {
    skip_ws(chars, pos)?;
    if *pos >= chars.len() {
        return Err("неожиданный конец файла".into());
    }
    let c = chars[*pos];
    match c {
        '(' | '[' => {
            let close = if c == '(' { ')' } else { ']' };
            *pos += 1;
            let mut items = Vec::new();
            let mut tail: Option<Value> = None;
            loop {
                skip_ws(chars, pos)?;
                if *pos >= chars.len() {
                    return Err("не закрыта скобка".into());
                }
                if chars[*pos] == close {
                    *pos += 1;
                    break;
                }
                let item = parse_expr(chars, pos)?;
                // точечная пара: (a . b) — Этап 2
                if is_sym(&item, ".") {
                    if items.is_empty() {
                        return Err("«.» без элемента перед ним".into());
                    }
                    if tail.is_some() {
                        return Err("лишний «.» в списке".into());
                    }
                    tail = Some(parse_expr(chars, pos)?);
                    skip_ws(chars, pos)?;
                    if *pos >= chars.len() || chars[*pos] != close {
                        return Err("после «.» — один элемент и закрывающая скобка".into());
                    }
                    *pos += 1;
                    break;
                }
                items.push(item);
            }
            Ok(match tail {
                None => Value::List(items),
                Some(t) => vec_into_chain(items, t),
            })
        }
        ')' | ']' => Err("неожиданная закрывающая скобка".into()),
        '\'' => {
            *pos += 1;
            let e = parse_expr(chars, pos)?;
            Ok(Value::List(vec![Value::Sym("quote".into()), e]))
        }
        '`' => {
            *pos += 1;
            let e = parse_expr(chars, pos)?;
            Ok(Value::List(vec![Value::Sym("quasiquote".into()), e]))
        }
        ',' => {
            *pos += 1;
            if *pos < chars.len() && chars[*pos] == '@' {
                *pos += 1;
                let e = parse_expr(chars, pos)?;
                Ok(Value::List(vec![Value::Sym("unquote-splicing".into()), e]))
            } else {
                let e = parse_expr(chars, pos)?;
                Ok(Value::List(vec![Value::Sym("unquote".into()), e]))
            }
        }
        '"' => {
            *pos += 1;
            let mut s = String::new();
            while *pos < chars.len() {
                let ch = chars[*pos];
                if ch == '"' {
                    *pos += 1;
                    return Ok(Value::Str(s));
                }
                if ch == '\\' {
                    *pos += 1;
                    if *pos >= chars.len() {
                        break;
                    }
                    match chars[*pos] {
                        'n' => s.push('\n'),
                        't' => s.push('\t'),
                        'r' => s.push('\r'),
                        other => s.push(other),
                    }
                    *pos += 1;
                } else {
                    s.push(ch);
                    *pos += 1;
                }
            }
            Err("не закрыта строка".into())
        }
        _ => {
            let start = *pos;
            while *pos < chars.len() {
                let ch = chars[*pos];
                if ch.is_whitespace() || "()[]'`,;\"".contains(ch) {
                    break;
                }
                *pos += 1;
            }
            let atom: String = chars[start..*pos].iter().collect();
            parse_atom(&atom)
        }
    }
}

/// Только лексический скан без построения Value: сколько лексем в источнике.
/// Эталонная «лексерная» часть старого пути — для честного сравнения с
/// быстрым reader'ом (`lex_only_simd`).
pub fn lex_only_scalar(src: &str) -> usize {
    let chars: Vec<char> = src.chars().collect();
    let mut pos = 0usize;
    let mut count = 0usize;
    loop {
        let _ = skip_ws(&chars, &mut pos);
        if pos >= chars.len() {
            return count;
        }
        let c = chars[pos];
        match c {
            '(' | '[' => {
                pos += 1;
                count += 1;
            }
            ')' | ']' => {
                pos += 1;
            }
            '\'' | '`' | ',' | '@' => {
                pos += 1;
                count += 1;
            }
            '"' => {
                pos += 1;
                count += 1;
                while pos < chars.len() {
                    if chars[pos] == '"' {
                        pos += 1;
                        break;
                    }
                    if chars[pos] == '\\' {
                        pos += 1;
                    }
                    pos += 1;
                }
            }
            _ => {
                let start = pos;
                while pos < chars.len() {
                    let ch = chars[pos];
                    if ch.is_whitespace() || "()[]'`,;\"".contains(ch) {
                        break;
                    }
                    pos += 1;
                }
                if pos > start {
                    count += 1;
                }
            }
        }
    }
}

fn parse_atom(atom: &str) -> Result<Value, String> {
    match atom {
        "#t" | "#true" => return Ok(Value::Bool(true)),
        "#f" | "#false" => return Ok(Value::Bool(false)),
        _ => {}
    }
    // быстрый путь: атом не начинается как число → сразу Sym, без двух
    // безуспешных parse::<i64>/parse::<f64> (главная внелексемная стоимость)
    let b = atom.as_bytes();
    let numeric_ish = match b[0] {
        b'0'..=b'9' => true,
        b'-' | b'+' | b'.' => b.len() > 1 && b[1].is_ascii_digit(),
        _ => false,
    };
    if !numeric_ish {
        return Ok(Value::Sym(atom.into()));
    }
    if let Ok(i) = atom.parse::<i64>() {
        return Ok(Value::Int(i));
    }
    if let Ok(f) = atom.parse::<f64>() {
        return Ok(Value::Float(f));
    }
    Ok(Value::Sym(atom.into()))
}
