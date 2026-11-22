// src/simd/scanner.rs — byte-class скан горячего пути reader'а.
//
// Один lookup таблицы классов на байт вместо цепочки сравнений:
//   C_ATOM (0) — обычный байт атома,
//   C_WS   (1) — ASCII-пробельный (09..0D, 20),
//   C_SYN  (2) — синтаксис/разделитель: ( ) [ ] ' ` , ; ",
//   C_HI   (3) — байт >= 0x80 (юникод: пробел или буква — решает скаляр).
//
// Таблица строится в const-контексте (без OnceLock и без инициализации в
// рантайме). SSE2 оставлен там, где скан ДЛИННЫЙ и окупает подготовку
// вектора: find_byte (строки, комментарии). На коротких атомах (средняя
// длина символа в корпусе — 5 байт) таблица быстрее: подготовка 16-байтного
// вектора дороже самого прогона.

pub const C_ATOM: u8 = 0;
pub const C_WS: u8 = 1;
pub const C_SYN: u8 = 2;
pub const C_HI: u8 = 3;

/// Класс каждого байта; см. константы C_*.
pub static CLASS: [u8; 256] = build_class();

const fn build_class() -> [u8; 256] {
    let mut t = [C_ATOM; 256];
    let mut i = 0usize;
    while i < 256 {
        let b = i as u8;
        t[i] = if b >= 0x80 {
            C_HI
        } else {
            match b {
                0x09..=0x0D | 0x20 => C_WS,
                b'(' | b')' | b'[' | b']' | b'\'' | b'`' | b',' | b';' | b'"' => C_SYN,
                _ => C_ATOM,
            }
        };
        i += 1;
    }
    t
}

/// Длина UTF-8 символа по первому байту (1, если байт не стартовый —
/// defensive: до такого не доходит по инварианту границ среза).
#[inline]
pub fn utf8_len(b: u8) -> usize {
    match b {
        0xC0..=0xDF => 2,
        0xE0..=0xEF => 3,
        0xF0..=0xF7 => 4,
        _ => 1,
    }
}

/// Символ в позиции i (граница UTF-8 гарантирована); None — обрезано на конце.
#[inline(always)]
pub fn char_at(bytes: &[u8], i: usize) -> Option<char> {
    if i >= bytes.len() {
        return None;
    }
    let len = utf8_len(bytes[i]);
    if i + len > bytes.len() {
        return None;
    }
    std::str::from_utf8(&bytes[i..i + len]).ok()?.chars().next()
}

/// Пропуск пробелов и `;`-комментариев. Семантика старого skip_ws:
/// `;` — до конца строки; юникодные пробелы (U+00A0, U+2028, …) — пробелы;
/// прочие байты >= 0x80 — содержимое атома (остановка).
/// Возвращает позицию первого значимого байта или bytes.len().
#[inline(always)]
pub fn skip_ws(bytes: &[u8], mut pos: usize) -> usize {
    loop {
        if pos >= bytes.len() {
            return pos;
        }
        match CLASS[bytes[pos] as usize] {
            C_WS => pos += 1,
            C_SYN if bytes[pos] == b';' => {
                pos = match find_byte(bytes, pos + 1, b'\n') {
                    Some(i) => i,
                    None => return bytes.len(),
                };
            }
            C_HI => match char_at(bytes, pos) {
                Some(ch) if ch.is_whitespace() => pos += ch.len_utf8(),
                _ => return pos,
            },
            _ => return pos,
        }
    }
}

/// Конец атома: первый пробельный/синтаксический байт; юникодные не-пробелы
/// (буквы) — часть атома. Возвращает позицию разделителя или bytes.len().
#[inline(always)]
pub fn atom_end(bytes: &[u8], mut pos: usize) -> usize {
    let n = bytes.len();
    loop {
        if pos >= n {
            return n;
        }
        match CLASS[bytes[pos] as usize] {
            C_ATOM => {
                pos += 1;
                // прогон по ASCII-атому: один lookup на байт, без ветвлений
                // на класс (рядом стоящие байты атома — самый частый случай)
                while pos < n && CLASS[bytes[pos] as usize] == C_ATOM {
                    pos += 1;
                }
            }
            C_HI => match char_at(bytes, pos) {
                Some(ch) if !ch.is_whitespace() => pos += ch.len_utf8(),
                _ => return pos,
            },
            _ => return pos,
        }
    }
}

/// memchr-стиль: первый байт `target` с позиции pos (строки, комментарии).
/// Портативно, на x86_64 — SSE2.
pub fn find_byte(bytes: &[u8], pos: usize, target: u8) -> Option<usize> {
    #[cfg(target_arch = "x86_64")]
    {
        if std::arch::is_x86_feature_detected!("sse2") {
            return unsafe { find_byte_sse2(bytes, pos, target) };
        }
    }
    (pos..bytes.len()).find(|&i| bytes[i] == target)
}

#[cfg(target_arch = "x86_64")]
pub(crate) unsafe fn find_byte_sse2(bytes: &[u8], mut pos: usize, target: u8) -> Option<usize> {
    use std::arch::x86_64::*;
    let t = _mm_set1_epi8(target as i8);
    while pos + 16 <= bytes.len() {
        let v = _mm_loadu_si128(bytes.as_ptr().add(pos) as *const __m128i);
        let m = _mm_movemask_epi8(_mm_cmpeq_epi8(v, t));
        if m != 0 {
            return Some(pos + m.trailing_zeros() as usize);
        }
        pos += 16;
    }
    (pos..bytes.len()).find(|&i| bytes[i] == target)
}

// ————————————————————— тесты сканера —————————————————————

#[cfg(test)]
mod tests {
    use super::*;

    /// Эталонный скан «конца атома» прямым посимвольным перебором
    /// (тот же язык, что у старого reader'а).
    fn atom_end_ref(bytes: &[u8], pos: usize) -> usize {
        let mut i = pos;
        while i < bytes.len() {
            match char_at(bytes, i) {
                Some(ch) if !ch.is_whitespace() && !"()[]'`,;\"".contains(ch) => {
                    i += ch.len_utf8()
                }
                _ => break,
            }
        }
        i
    }

    fn skip_ws_ref(bytes: &[u8], pos: usize) -> usize {
        let mut i = pos;
        loop {
            match char_at(bytes, i) {
                Some(ch) if ch.is_whitespace() => i += ch.len_utf8(),
                Some(';') => {
                    while i < bytes.len() && bytes[i] != b'\n' {
                        i += 1;
                    }
                }
                _ => return i,
            }
        }
    }

    #[test]
    fn test_class_table_consistency() {
        // таблица обязана совпадать с посимвольной проверкой на всех 256 байтах
        for b in 0u8..=255 {
            let one = [b];
            let cls = CLASS[b as usize];
            // невалидный одиночный байт >= 0x80 в &str невозможен — эталон
            // для него не определён (char_at вернёт None), пропускаем
            if char_at(&one, 0).is_none() {
                continue;
            }
            let ref_atom = atom_end_ref(&one, 0);
            match cls {
                C_ATOM => assert_eq!(ref_atom, 1, "байт {:#04x} должен быть атомом", b),
                C_HI => assert_eq!(ref_atom, 1, "не-ASCII байт {:#04x} — атом", b),
                _ => assert_eq!(ref_atom, 0, "байт {:#04x} — разделитель/пробел", b),
            }
        }
    }

    #[test]
    fn test_scanner_equivalence() {
        let long = vec![b'a'; 100];
        let cases: &[&[u8]] = &[
            b"",
            b"   \t\r\n",
            b"(+ 1 2)",
            b"; comment here\n(+ x y)",
            b"\"string with \\\"escapes\\\"\"",
            b"identifier-with-dashes and numbers 12345",
            b"hello \xD0\xBF non-ascii \xFF world",
            b"\xC2\xA0nbsp\xE2\x80\xA8sep",
            &long,
        ];
        for bytes in cases {
            let mut pos = 0usize;
            loop {
                assert_eq!(
                    skip_ws(bytes, pos),
                    skip_ws_ref(bytes, pos),
                    "skip_ws на {:?} с {}",
                    bytes,
                    pos
                );
                let p = skip_ws(bytes, pos);
                if p >= bytes.len() {
                    break;
                }
                assert_eq!(
                    atom_end(bytes, p),
                    atom_end_ref(bytes, p),
                    "atom_end на {:?} с {}",
                    bytes,
                    p
                );
                pos = atom_end(bytes, p).max(p + 1);
            }
        }
    }

    #[test]
    fn test_scanner_find_byte() {
        let haystack = b"0123456789abcdef0123456789abcdef"; // 32 байта
        assert_eq!(find_byte(haystack, 0, b'0'), Some(0));
        assert_eq!(find_byte(haystack, 1, b'0'), Some(16));
        assert_eq!(find_byte(haystack, 17, b'0'), None);
        assert_eq!(find_byte(haystack, 0, b'f'), Some(15));
        assert_eq!(find_byte(haystack, 16, b'f'), Some(31));
        assert_eq!(find_byte(haystack, 0, b'z'), None);
    }
}
