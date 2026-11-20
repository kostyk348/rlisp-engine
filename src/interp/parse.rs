use crate::interp::symbols::intern;
// src/interp/parse.rs — Синтаксический анализ Lisp (скалярный и SIMD-парсеры, лексеры).

use super::{is_sym, vec_into_chain, Value};
use crate::simd;

// ——————————————————————————— парсер (Reader) ———————————————————————————

/// Точка входа reader'а. На x86_64 — SIMD-токенизатор (Этап 3, src/simd.rs:
/// byte-class скан SSE2 вместо посимвольного is_whitespace/contains), иначе
/// скалярный путь. Оба пути обязаны давать одинаковые AST — сверяются в
/// тестах и в --bench-parse на реальном корпусе assets/lisp/*.lisp.
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

/// СТАРЫЙ посимвольный reader (v0.1–v0.3): оставлен как fallback (не-x86_64)
/// и как эталон для взаимной сверки AST в бенчмарке токенизатора.
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

// ——— Этап 3: SIMD-токенизатор — тот же язык, байтовый скан ———
//
// Инвариант безопасности срезов: все позиции pos всегда лежат на границах
// UTF-8 символов (старт 0, либо сразу после ASCII-спецбайта, либо после
// пропуска целого символа через utf8_len).

pub fn parse_program_simd(src: &str) -> Result<Vec<Value>, String> {
    let bytes = src.as_bytes();
    let mut pos = 0usize;
    let mut out = Vec::new();
    loop {
        pos = skip_ws_b(bytes, pos);
        if pos >= bytes.len() {
            break;
        }
        out.push(parse_expr_b(bytes, &mut pos)?);
    }
    Ok(out)
}

/// Длина UTF-8 символа по первому байту (1, если не стартовый байт —
/// defensive: до такого не доходит по инварианту границ).
fn utf8_len(b: u8) -> usize {
    match b {
        0xC0..=0xDF => 2,
        0xE0..=0xEF => 3,
        0xF0..=0xF7 => 4,
        _ => 1,
    }
}

/// Символ в позиции i (граница guaranteed); None — обрезано на конце.
fn char_at_b(bytes: &[u8], i: usize) -> Option<char> {
    let len = utf8_len(bytes[i]);
    if i + len > bytes.len() {
        return None;
    }
    std::str::from_utf8(&bytes[i..i + len]).ok()?.chars().next()
}

/// Пропуск пробелов и ;-комментариев (SIMD-скан). Возвращает позицию
/// первого значимого байта или bytes.len().
/// Семантика старого skip_ws: смотрим ТОЛЬКО на текущую позицию — если
/// значимый байт уже здесь, останавливаемся немедленно (next_hit при i > pos
/// означает «текущий байт — содержимое атома», дальше не сканируем).
fn skip_ws_b(bytes: &[u8], mut pos: usize) -> usize {
    loop {
        match simd::next_hit(bytes, pos) {
            // дальше ни одного спец-байта: текущий байт (если он есть) —
            // содержимое атома, останавливаемся здесь
            None => return pos.min(bytes.len()),
            Some((i, _)) if i > pos => return pos, // текущий байт значим
            Some((i, simd::Hit::Ws(_))) => pos = i + 1,
            Some((i, simd::Hit::Delim(b';'))) => {
                // комментарий до конца строки
                pos = simd::find_byte(bytes, i + 1, b'\n').unwrap_or(bytes.len());
            }
            Some((i, simd::Hit::Delim(_))) => return i,
            Some((i, simd::Hit::NonAscii(_))) => {
                // юникодный пробел (U+00A0, U+2028, …) — по-старому пробел;
                // символ имени — по-старому значимый
                match char_at_b(bytes, i) {
                    Some(ch) if ch.is_whitespace() => pos = i + ch.len_utf8(),
                    _ => return i,
                }
            }
        }
    }
}

/// Конец атома: первый пробельный/спец-байт; юникодные не-пробелы — часть атома.
fn atom_end_b(bytes: &[u8], mut pos: usize) -> usize {
    loop {
        match simd::next_hit(bytes, pos) {
            None => return bytes.len(),
            Some((i, simd::Hit::Ws(_))) => return i,
            Some((i, simd::Hit::Delim(_))) => return i,
            Some((i, simd::Hit::NonAscii(_))) => match char_at_b(bytes, i) {
                Some(ch) if ch.is_whitespace() => return i,
                Some(ch) => pos = i + ch.len_utf8(),
                None => return i,
            },
        }
    }
}

fn parse_expr_b(bytes: &[u8], pos: &mut usize) -> Result<Value, String> {
    *pos = skip_ws_b(bytes, *pos);
    if *pos >= bytes.len() {
        return Err("неожиданный конец файла".into());
    }
    let c = bytes[*pos];
    match c {
        b'(' | b'[' => {
            let close: u8 = if c == b'(' { b')' } else { b']' };
            *pos += 1;
            let mut items = Vec::new();
            let mut tail: Option<Value> = None;
            loop {
                *pos = skip_ws_b(bytes, *pos);
                if *pos >= bytes.len() {
                    return Err("не закрыта скобка".into());
                }
                if bytes[*pos] == close {
                    *pos += 1;
                    break;
                }
                let item = parse_expr_b(bytes, pos)?;
                // точечная пара: (a . b) — Этап 2
                if is_sym(&item, ".") {
                    if items.is_empty() {
                        return Err("«.» без элемента перед ним".into());
                    }
                    if tail.is_some() {
                        return Err("лишний «.» в списке".into());
                    }
                    tail = Some(parse_expr_b(bytes, pos)?);
                    *pos = skip_ws_b(bytes, *pos);
                    if *pos >= bytes.len() || bytes[*pos] != close {
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
        b')' | b']' => Err("неожиданная закрывающая скобка".into()),
        b'\'' => {
            *pos += 1;
            let e = parse_expr_b(bytes, pos)?;
            Ok(Value::List(vec![Value::Sym(intern("quote")), e]))
        }
        b'`' => {
            *pos += 1;
            let e = parse_expr_b(bytes, pos)?;
            Ok(Value::List(vec![Value::Sym(intern("quasiquote")), e]))
        }
        b',' => {
            *pos += 1;
            if *pos < bytes.len() && bytes[*pos] == b'@' {
                *pos += 1;
                let e = parse_expr_b(bytes, pos)?;
                Ok(Value::List(vec![Value::Sym(intern("unquote-splicing")), e]))
            } else {
                let e = parse_expr_b(bytes, pos)?;
                Ok(Value::List(vec![Value::Sym(intern("unquote")), e]))
            }
        }
        b'"' => {
            *pos += 1;
            let mut s = Vec::new();
            loop {
                // скан до ближайшей кавычки или бэкслеша (memchr-стиль)
                let q = simd::find_byte(bytes, *pos, b'"');
                let bs = simd::find_byte(bytes, *pos, b'\\');
                let (i, is_bs) = match (q, bs) {
                    (None, None) => return Err("не закрыта строка".into()),
                    (Some(qi), None) => (qi, false),
                    (None, Some(bi)) => (bi, true),
                    (Some(qi), Some(bi)) => (qi.min(bi), bi <= qi),
                };
                s.extend_from_slice(&bytes[*pos..i]);
                *pos = i;
                if !is_bs {
                    *pos += 1;
                    return Ok(Value::Str(String::from_utf8_lossy(&s).into_owned()));
                }
                // экранирование: копируем ЦЕЛЫЙ следующий символ (может быть не-ASCII)
                *pos += 1;
                if *pos >= bytes.len() {
                    break;
                }
                let ch = char_at_b(bytes, *pos);
                match ch {
                    Some('n') => {
                        s.push(b'\n');
                        *pos += 1;
                    }
                    Some('t') => {
                        s.push(b'\t');
                        *pos += 1;
                    }
                    Some('r') => {
                        s.push(b'\r');
                        *pos += 1;
                    }
                    Some(other) => {
                        let start = *pos;
                        *pos += other.len_utf8();
                        s.extend_from_slice(&bytes[start..*pos]);
                    }
                    None => break,
                }
            }
            Err("не закрыта строка".into())
        }
        _ => {
            let start = *pos;
            let end = atom_end_b(bytes, start);
            *pos = end;
            let atom = std::str::from_utf8(&bytes[start..end])
                .map_err(|_| "невалидный UTF-8 в атоме".to_string())?;
            parse_atom(atom)
        }
    }
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
            Ok(Value::List(vec![Value::Sym(intern("quote")), e]))
        }
        '`' => {
            *pos += 1;
            let e = parse_expr(chars, pos)?;
            Ok(Value::List(vec![Value::Sym(intern("quasiquote")), e]))
        }
        ',' => {
            *pos += 1;
            if *pos < chars.len() && chars[*pos] == '@' {
                *pos += 1;
                let e = parse_expr(chars, pos)?;
                Ok(Value::List(vec![Value::Sym(intern("unquote-splicing")), e]))
            } else {
                let e = parse_expr(chars, pos)?;
                Ok(Value::List(vec![Value::Sym(intern("unquote")), e]))
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
/// Эталонная «лексерная» часть старого пути — для честного сравнения с SIMD.
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

/// Аналогично для SIMD-пути (skip_ws_b + atom_end_b + find_byte), без Value.
pub fn lex_only_simd(src: &str) -> usize {
    let bytes = src.as_bytes();
    let mut pos = 0usize;
    let mut count = 0usize;
    loop {
        pos = skip_ws_b(bytes, pos);
        if pos >= bytes.len() {
            return count;
        }
        let c = bytes[pos];
        match c {
            b'(' | b'[' => {
                pos += 1;
                count += 1;
            }
            b')' | b']' => {
                pos += 1;
            }
            b'\'' | b'`' | b',' | b'@' => {
                pos += 1;
                count += 1;
            }
            b'"' => {
                pos += 1;
                count += 1;
                loop {
                    let q = simd::find_byte(bytes, pos, b'"');
                    let bs = simd::find_byte(bytes, pos, b'\\');
                    let i = match (q, bs) {
                        (None, None) => return count,
                        (Some(qi), None) => qi,
                        (None, Some(bi)) => bi,
                        (Some(qi), Some(bi)) => qi.min(bi),
                    };
                    if bytes[i] == b'\\' {
                        pos = i + 2;
                    } else {
                        pos = i + 1;
                        break;
                    }
                }
            }
            _ => {
                let end = atom_end_b(bytes, pos);
                if end > pos {
                    count += 1;
                }
                pos = end.max(pos + 1);
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
        return Ok(Value::Sym(intern(atom)));
    }
    if let Ok(i) = atom.parse::<i64>() {
        return Ok(Value::Int(i));
    }
    if let Ok(f) = atom.parse::<f64>() {
        return Ok(Value::Float(f));
    }
    Ok(Value::Sym(intern(atom)))
}
