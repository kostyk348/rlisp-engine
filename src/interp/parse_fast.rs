// src/interp/parse_fast.rs — быстрый reader: один проход токенизации + интерн символов.
//
// Контракт с прежним reader'ом: ТОТ ЖЕ язык и ТОТ ЖЕ AST, включая тексты
// ошибок (сверяется `src/parse_diff.rs` на каверзных входах и на реальном
// корпусе `assets/lisp/*.lisp`). Отличия — только стоимость:
//
//  1. Файл сканируется ОДИН раз в компактный массив токенов (12 байт на
//     токен) по таблице классов байт (`crate::simd::CLASS`): один lookup на
//     байт вместо `Vec<char>` старого reader'а (4 байта на символ + декод).
//  2. Дерево строится по токенам, без повторного сканирования источника:
//     длина каждого списка известна заранее (счётчик на стеке токенизатора),
//     поэтому `Vec` списка создаётся сразу нужной ёмкости — ни одного
//     реаллока на список.
//  3. Интерн символов: `Value::Sym` хранит `Rc<str>` из таблицы
//     «текст → один экземпляр». В корпусе 29k символов на 2k уникальных —
//     `String` давал по malloc/free на каждое вхождение.
//  4. Числа: целые собираются вручную (точно при ≤18 цифрах), короткие
//     десятичные дроби — быстрым путём Клингера (`m / 10^k` при m ≤ 2^53 и
//     k ≤ 22 даёт ровно одно округление — результат побитово равен
//     `str::parse::<f64>`); всё остальное — честный `str::parse`.

use super::value::{SymStr, Value};
use super::vec_into_chain;
use crate::simd;
use std::collections::HashMap;
use std::hash::{BuildHasherDefault, Hasher};
use std::rc::Rc;

/// FNV-1a: короткие ключи, дешевле SipHash в интерн-таблице на 29k вхождений.
#[derive(Default, Clone, Copy)]
pub struct Fnv1a(u64);

impl Hasher for Fnv1a {
    #[inline]
    fn finish(&self) -> u64 {
        self.0
    }
    #[inline]
    fn write(&mut self, bytes: &[u8]) {
        let mut h = self.0 ^ 0xcbf2_9ce4_8422_2325;
        for &b in bytes {
            h = (h ^ b as u64).wrapping_mul(0x0000_0100_0000_01b3);
        }
        self.0 = h;
    }
}

type InternMap<'a> = HashMap<&'a str, SymStr, BuildHasherDefault<Fnv1a>>;

// ——————————————————————————— токены ———————————————————————————

const K_ATOM: u8 = 0;
const K_STR: u8 = 1;
const K_LPAREN: u8 = 2;
const K_RPAREN: u8 = 3;
const K_LBRACKET: u8 = 4;
const K_RBRACKET: u8 = 5;
const K_QUOTE: u8 = 6;
const K_QUASI: u8 = 7;
const K_UNQUOTE: u8 = 8;
const K_UNQUOTE_SPLICE: u8 = 9;

/// Токен: kind + флаги + (n — число элементов для открывающей скобки) +
/// диапазон байт в источнике.
#[derive(Clone, Copy)]
struct Tok {
    kind: u8,
    flags: u8,
    n: u16,
    start: u32,
    end: u32,
}

/// Строка не закрыта — ошибка выдаётся при разборе (как у старого reader'а,
/// который сообщает её в порядке обхода, а не в порядке скана).
const FL_UNCLOSED: u8 = 1;
/// Строка содержит экранирование (`\`), нужен медленный путь.
const FL_ESCAPE: u8 = 2;

// ——————————————————————————— reader ———————————————————————————

/// Reader с интерн-таблицей символов (живёт ровно один разбор файла).
pub struct Reader<'a> {
    src: &'a str,
    bytes: &'a [u8],
    intern: InternMap<'a>,
}

impl<'a> Reader<'a> {
    pub fn new(src: &'a str) -> Self {
        Reader {
            src,
            bytes: src.as_bytes(),
            // типичный файл корпуса — 300–2000 уникальных символов
            intern: InternMap::with_capacity_and_hasher(1024, BuildHasherDefault::default()),
        }
    }

    /// Разбор всех форм файла.
    pub fn read_program(&mut self) -> Result<Vec<Value>, String> {
        let toks = self.tokenize();
        let mut out = Vec::new();
        let mut i = 0usize;
        while i < toks.len() {
            out.push(self.build(&toks, &mut i)?);
        }
        Ok(out)
    }

    /// Один проход по источнику: токены + счётчики элементов открытых списков.
    fn tokenize(&self) -> Vec<Tok> {
        let b = self.bytes;
        let n = b.len();
        let mut toks: Vec<Tok> = Vec::with_capacity(n / 8 + 16);
        // стек открытых списков: (индекс токена, число элементов, ждём значение)
        let mut stack: Vec<(u32, u32, bool)> = Vec::with_capacity(16);
        let mut i = 0usize;

        // учёт «начался элемент» на верхнем открытом списке
        macro_rules! item_start {
            ($pending_value:expr) => {
                if let Some(top) = stack.last_mut() {
                    if top.2 {
                        top.2 = false;
                    } else {
                        top.1 += 1;
                    }
                    if $pending_value {
                        top.2 = true;
                    }
                }
            };
        }

        while i < n {
            let c = b[i];
            let cls = simd::CLASS[c as usize];
            if cls == simd::C_ATOM {
                let start = i;
                i += 1;
                while i < n && simd::CLASS[b[i] as usize] == simd::C_ATOM {
                    i += 1;
                }
                item_start!(false);
                toks.push(Tok {
                    kind: K_ATOM,
                    flags: 0,
                    n: 0,
                    start: start as u32,
                    end: i as u32,
                });
                continue;
            }
            if cls == simd::C_WS {
                i += 1;
                continue;
            }
            if cls == simd::C_HI {
                // юникод: пробел или буква (буква — часть атома)
                match simd::char_at(b, i) {
                    Some(ch) if ch.is_whitespace() => i += ch.len_utf8(),
                    Some(ch) => {
                        let start = i;
                        i += ch.len_utf8();
                        while i < n {
                            match simd::CLASS[b[i] as usize] {
                                simd::C_ATOM => i += 1,
                                simd::C_HI => match simd::char_at(b, i) {
                                    Some(c2) if !c2.is_whitespace() => i += c2.len_utf8(),
                                    _ => break,
                                },
                                _ => break,
                            }
                        }
                        item_start!(false);
                        toks.push(Tok {
                            kind: K_ATOM,
                            flags: 0,
                            n: 0,
                            start: start as u32,
                            end: i as u32,
                        });
                    }
                    None => break,
                }
                continue;
            }
            // C_SYN
            match c {
                b';' => {
                    i = match simd::find_byte(b, i + 1, b'\n') {
                        Some(p) => p,
                        None => n,
                    };
                }
                b'(' | b'[' => {
                    item_start!(false);
                    let idx = toks.len() as u32;
                    toks.push(Tok {
                        kind: if c == b'(' { K_LPAREN } else { K_LBRACKET },
                        flags: 0,
                        n: 0,
                        start: i as u32,
                        end: 0,
                    });
                    stack.push((idx, 0, false));
                    i += 1;
                }
                b')' | b']' => {
                    if let Some((idx, cnt, _)) = stack.pop() {
                        toks[idx as usize].n = cnt.min(u16::MAX as u32) as u16;
                    }
                    toks.push(Tok {
                        kind: if c == b')' { K_RPAREN } else { K_RBRACKET },
                        flags: 0,
                        n: 0,
                        start: i as u32,
                        end: 0,
                    });
                    i += 1;
                }
                b'\'' | b'`' | b',' => {
                    // ,@ — один токен (в счёте лексем старый reader видит две)
                    if c == b',' && i + 1 < n && b[i + 1] == b'@' {
                        item_start!(true);
                        toks.push(Tok {
                            kind: K_UNQUOTE_SPLICE,
                            flags: 0,
                            n: 0,
                            start: i as u32,
                            end: 0,
                        });
                        i += 2;
                    } else {
                        item_start!(true);
                        toks.push(Tok {
                            kind: match c {
                                b'\'' => K_QUOTE,
                                b'`' => K_QUASI,
                                _ => K_UNQUOTE,
                            },
                            flags: 0,
                            n: 0,
                            start: i as u32,
                            end: 0,
                        });
                        i += 1;
                    }
                }
                _ => {
                    // '"' — строка
                    item_start!(false);
                    let start = i + 1;
                    i += 1;
                    let mut flags = 0u8;
                    loop {
                        let q = simd::find_byte(b, i, b'"');
                        let e = simd::find_byte(b, i, b'\\');
                        match (q, e) {
                            (None, _) => {
                                flags |= FL_UNCLOSED;
                                toks.push(Tok {
                                    kind: K_STR,
                                    flags,
                                    n: 0,
                                    start: start as u32,
                                    end: n as u32,
                                });
                                i = n;
                                break;
                            }
                            (Some(qi), Some(ei)) if ei < qi => {
                                flags |= FL_ESCAPE;
                                i = ei + 1;
                                // экранированный символ может быть не-ASCII
                                if let Some(ch) = simd::char_at(b, i) {
                                    i += ch.len_utf8();
                                }
                            }
                            (Some(qi), _) => {
                                toks.push(Tok {
                                    kind: K_STR,
                                    flags,
                                    n: 0,
                                    start: start as u32,
                                    end: qi as u32,
                                });
                                i = qi + 1;
                                break;
                            }
                        }
                    }
                }
            }
        }
        toks
    }

    /// Символ: один экземпляр строки на уникальный текст.
    #[inline]
    fn sym(&mut self, s: &'a str) -> Value {
        if let Some(x) = self.intern.get(s) {
            return Value::Sym(x.clone());
        }
        let x = SymStr::from(Rc::from(s));
        self.intern.insert(s, x.clone());
        Value::Sym(x)
    }

    fn build(&mut self, toks: &[Tok], i: &mut usize) -> Result<Value, String> {
        let src = self.src;
        if *i >= toks.len() {
            return Err("неожиданный конец файла".into());
        }
        let t = toks[*i];
        *i += 1;
        match t.kind {
            K_ATOM => {
                let s: &'a str = &src[t.start as usize..t.end as usize];
                Ok(self.atom_value(s))
            }
            K_STR => {
                if t.flags & FL_UNCLOSED != 0 {
                    return Err("не закрыта строка".into());
                }
                let s: &'a str = &src[t.start as usize..t.end as usize];
                if t.flags & FL_ESCAPE == 0 {
                    return Ok(Value::Str(s.to_string()));
                }
                Ok(Value::Str(unescape(s)))
            }
            K_LPAREN | K_LBRACKET => {
                let close = if t.kind == K_LPAREN {
                    K_RPAREN
                } else {
                    K_RBRACKET
                };
                let mut items: Vec<Value> = Vec::with_capacity(t.n as usize);
                let mut tail: Option<Value> = None;
                loop {
                    if *i >= toks.len() {
                        return Err("не закрыта скобка".into());
                    }
                    let nt = toks[*i];
                    if nt.kind == K_RPAREN || nt.kind == K_RBRACKET {
                        if nt.kind != close {
                            return Err("неожиданная закрывающая скобка".into());
                        }
                        *i += 1;
                        break;
                    }
                    let item = self.build(toks, i)?;
                    // точечная пара: (a . b) — Этап 2
                    if let Value::Sym(s) = &item {
                        if &**s == "." {
                            if items.is_empty() {
                                return Err("«.» без элемента перед ним".into());
                            }
                            if tail.is_some() {
                                return Err("лишний «.» в списке".into());
                            }
                            tail = Some(self.build(toks, i)?);
                            if *i >= toks.len() || toks[*i].kind != close {
                                return Err("после «.» — один элемент и закрывающая скобка".into());
                            }
                            *i += 1;
                            break;
                        }
                    }
                    items.push(item);
                }
                Ok(match tail {
                    None => Value::List(items),
                    Some(x) => vec_into_chain(items, x),
                })
            }
            K_RPAREN | K_RBRACKET => Err("неожиданная закрывающая скобка".into()),
            K_QUOTE | K_QUASI | K_UNQUOTE | K_UNQUOTE_SPLICE => {
                let e = self.build(toks, i)?;
                let name = match t.kind {
                    K_QUOTE => "quote",
                    K_QUASI => "quasiquote",
                    K_UNQUOTE => "unquote",
                    _ => "unquote-splicing",
                };
                let q = self.sym(name);
                Ok(Value::List(vec![q, e]))
            }
            _ => Err("неожиданная закрывающая скобка".into()),
        }
    }

    /// Атом: #t/#f, число или интернированный символ.
    #[inline]
    fn atom_value(&mut self, s: &'a str) -> Value {
        let b = s.as_bytes();
        match b[0] {
            b'#' => match s {
                "#t" | "#true" => return Value::Bool(true),
                "#f" | "#false" => return Value::Bool(false),
                _ => {}
            },
            b'0'..=b'9' | b'-' | b'+' | b'.' => {
                if let Some(v) = fast_number(s) {
                    return v;
                }
                // сложное число — честный std, как у старого reader'а
                let numeric_ish = match b[0] {
                    b'0'..=b'9' => true,
                    b'-' | b'+' | b'.' => b.len() > 1 && b[1].is_ascii_digit(),
                    _ => false,
                };
                if numeric_ish {
                    if let Ok(x) = s.parse::<i64>() {
                        return Value::Int(x);
                    }
                    if let Ok(f) = s.parse::<f64>() {
                        return Value::Float(f);
                    }
                }
            }
            _ => {}
        }
        self.sym(s)
    }
}

/// Экранирование строки (медленный путь): `\n`, `\t`, `\r`, прочее — как есть.
fn unescape(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = String::with_capacity(s.len());
    let mut i = 0usize;
    while i < b.len() {
        if b[i] == b'\\' && i + 1 < b.len() {
            let e = match b[i + 1] {
                b'n' => Some('\n'),
                b't' => Some('\t'),
                b'r' => Some('\r'),
                _ => simd::char_at(b, i + 1),
            };
            match e {
                Some(ch) => {
                    out.push(ch);
                    i += 1;
                    i += simd::char_at(b, i).map(|c| c.len_utf8()).unwrap_or(1);
                }
                None => break,
            }
        } else {
            match simd::char_at(b, i) {
                Some(ch) => {
                    out.push(ch);
                    i += ch.len_utf8();
                }
                None => break,
            }
        }
    }
    out
}

/// Быстрый разбор числа; None — «сложный» случай (решает `str::parse`).
#[inline]
fn fast_number(s: &str) -> Option<Value> {
    let b = s.as_bytes();
    let mut i = 0usize;
    let neg = match b[0] {
        b'-' => {
            i = 1;
            true
        }
        b'+' => {
            i = 1;
            false
        }
        _ => false,
    };
    let int_start = i;
    let mut m: u64 = 0;
    let mut digits = 0usize;
    while i < b.len() && b[i].is_ascii_digit() {
        m = m * 10 + (b[i] - b'0') as u64;
        i += 1;
        digits += 1;
        if digits > 18 {
            return None; // возможное переполнение i64 — пусть решает std
        }
    }
    let int_digits = i - int_start;
    if i == b.len() {
        // чистое целое
        if int_digits == 0 {
            return None;
        }
        let v = if neg { -(m as i64) } else { m as i64 };
        return Some(Value::Int(v));
    }
    if b[i] != b'.' {
        return None; // экспонента/мусор — std
    }
    i += 1;
    let frac_start = i;
    while i < b.len() && b[i].is_ascii_digit() {
        m = m * 10 + (b[i] - b'0') as u64;
        i += 1;
        digits += 1;
        if digits > 15 {
            return None; // мантисса > 2^53 — точность не гарантирована, std
        }
    }
    let frac_digits = i - frac_start;
    if i != b.len() || int_digits + frac_digits == 0 || digits == 0 || frac_digits > 22 {
        return None;
    }
    // Путь Клингера: m ≤ 2^53 и 10^k (k ≤ 22) представимы точно ⇒
    // единственное деление даёт корректно округлённый результат,
    // побитово равный str::parse::<f64>.
    const POW10: [f64; 23] = [
        1e0, 1e1, 1e2, 1e3, 1e4, 1e5, 1e6, 1e7, 1e8, 1e9, 1e10, 1e11, 1e12, 1e13, 1e14, 1e15,
        1e16, 1e17, 1e18, 1e19, 1e20, 1e21, 1e22,
    ];
    let v = m as f64 / POW10[frac_digits];
    Some(Value::Float(if neg { -v } else { v }))
}

/// Точка входа быстрого reader'а (на x86_64 — основной путь).
pub fn parse_program_simd(src: &str) -> Result<Vec<Value>, String> {
    Reader::new(src).read_program()
}

/// Только лексический счёт лексем (без построения Value) — для честного
/// замера вклада скана в `--bench-parse`. Счётные правила совпадают со
/// скалярным эталоном `lex_only_scalar` (у `,@` — две лексемы).
pub fn lex_only_simd(src: &str) -> usize {
    let bytes = src.as_bytes();
    let mut pos = 0usize;
    let mut count = 0usize;
    loop {
        pos = simd::skip_ws(bytes, pos);
        if pos >= bytes.len() {
            return count;
        }
        match bytes[pos] {
            b'(' | b'[' => {
                pos += 1;
                count += 1;
            }
            b')' | b']' => pos += 1,
            b'\'' | b'`' | b'@' => {
                pos += 1;
                count += 1;
            }
            b',' => {
                // ,@ — две лексемы в скалярном эталоне
                pos += 1;
                count += 1;
                if pos < bytes.len() && bytes[pos] == b'@' {
                    pos += 1;
                    count += 1;
                }
            }
            b'"' => {
                pos += 1;
                count += 1;
                loop {
                    let q = simd::find_byte(bytes, pos, b'"');
                    let e = simd::find_byte(bytes, pos, b'\\');
                    let i = match (q, e) {
                        (None, None) => return count,
                        (Some(qi), None) => qi,
                        (None, Some(ei)) => ei,
                        (Some(qi), Some(ei)) => qi.min(ei),
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
                let end = simd::atom_end(bytes, pos);
                if end > pos {
                    count += 1;
                }
                pos = end.max(pos + 1);
            }
        }
    }
}

// ————————————————————— тесты reader'а —————————————————————

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_intern_shares_one_instance() {
        let a = parse_program_simd("(foo foo foo)").unwrap();
        if let Value::List(items) = &a[0] {
            let ptrs: Vec<*const str> = items
                .iter()
                .map(|v| match v {
                    Value::Sym(s) => s.as_str() as *const str,
                    _ => panic!("ожидался символ"),
                })
                .collect();
            assert_eq!(ptrs[0], ptrs[1], "повтор символа обязан делить строку");
            assert_eq!(ptrs[1], ptrs[2], "повтор символа обязан делить строку");
        } else {
            panic!("ожидался список");
        }
    }

    #[test]
    fn test_fast_numbers_bitwise_equal_std() {
        // быстрый путь обязан совпадать со std побитово (включая -0.0)
        let cases = [
            "0",
            "1",
            "-1",
            "+42",
            "007",
            "123456789012345678",
            "0.0",
            "-0.0",
            "1.",
            ".5",
            "0.5",
            "-0.93",
            "12.9898",
            "78.233",
            "0.61803399",
            "3.14159265358979",
            "1e5",
            "1E-3",
            "-2.5e10",
            "1.2.3",
            "1e999",
            "0.1",
            "0.2",
            "0.3",
            "2.675",
            "1.0000000000000002",
            "9007199254740993",
            "1.7976931348623157e308",
        ];
        for c in cases {
            let got = parse_program_simd(c).unwrap();
            let want = crate::interp::parse_program_scalar(c).unwrap();
            let g = crate::interp::fmt_value(&got[0]);
            let w = crate::interp::fmt_value(&want[0]);
            assert_eq!(g, w, "число {:?}: быстрый {:?} vs std {:?}", c, g, w);
            if let (Value::Float(a), Value::Float(bb)) = (&got[0], &want[0]) {
                assert_eq!(a.to_bits(), bb.to_bits(), "биты {:?}", c);
            }
            if let (Value::Int(a), Value::Int(bb)) = (&got[0], &want[0]) {
                assert_eq!(a, bb, "целое {:?}", c);
            }
        }
    }

    #[test]
    fn test_allocations_per_token_ascii() {
        // 200 форм (+ 1 2): по правилам скалярного лексера 4 лексемы на форму
        // ('(' , '+', '1', '2'; закрывающая скобка в счёт не входит) = 800.
        let mut src = String::new();
        for _ in 0..200 {
            src.push_str("(+ 1 2) ");
        }
        let before = crate::alloc_probe::snapshot();
        crate::alloc_probe::start();
        let ast = parse_program_simd(&src).unwrap();
        crate::alloc_probe::stop();
        let after = crate::alloc_probe::snapshot();
        drop(ast);

        let allocs = after.0 - before.0;
        let tokens = lex_only_simd(&src) as u64;
        assert_eq!(tokens, 800, "лексем в 200 формах (+ 1 2)");
        let per_token = allocs as f64 / tokens as f64;
        // критерий ТЗ §2 п.1: ≤ 0,5 аллокации на токен для ASCII-файла
        assert!(
            per_token <= 0.5,
            "{} аллокаций на {} токенов = {:.3}/токен (критерий ≤ 0,5)",
            allocs,
            tokens,
            per_token
        );
    }
}


