// src/alloc_probe.rs — счётчик аллокаций для `--bench-parse-allocs` (ноль зависимостей).
//
// Критерий ТЗ §2 п.1 требует не только «≥3×», но и «аллокаций на ASCII-файл
// ≤ 0,5/токен» — значит нужен честный счётчик malloc/realloc, а не оценка.
//
// Счёт ПОУМОЛЧАНИЮ ВЫКЛЮЧЕН (один Relaxed-load на аллокацию в горячем пути
// движка — пренебрежимо на фоне самого malloc) и включается только на время
// замера. Считаем в thread-local: тесты идут параллельно, глобальный атомик
// поймал бы чужие аллокации и тест стал бы плавающим.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::atomic::{AtomicBool, Ordering};

static COUNTING: AtomicBool = AtomicBool::new(false);

thread_local! {
    static LOCAL: Cell<(u64, u64)> = const { Cell::new((0, 0)) };
}

/// Аллокатор, считающий число и байты аллокаций текущего потока.
pub struct Counting;

#[inline]
fn bump(bytes: usize) {
    if COUNTING.load(Ordering::Relaxed) {
        let _ = LOCAL.try_with(|c| {
            let (n, b) = c.get();
            c.set((n + 1, b + bytes as u64));
        });
    }
}

unsafe impl GlobalAlloc for Counting {
    #[inline]
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        bump(l.size());
        System.alloc(l)
    }

    #[inline]
    unsafe fn alloc_zeroed(&self, l: Layout) -> *mut u8 {
        bump(l.size());
        System.alloc_zeroed(l)
    }

    #[inline]
    unsafe fn realloc(&self, p: *mut u8, l: Layout, new_size: usize) -> *mut u8 {
        bump(new_size);
        System.realloc(p, l, new_size)
    }

    #[inline]
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        System.dealloc(p, l)
    }
}

/// Сбросить счётчик текущего потока и включить счёт.
pub fn start() {
    let _ = LOCAL.try_with(|c| c.set((0, 0)));
    COUNTING.store(true, Ordering::SeqCst);
}

/// Выключить счёт (значения остаются доступны через [`snapshot`]).
pub fn stop() {
    COUNTING.store(false, Ordering::SeqCst);
}

/// (число аллокаций, байты) текущего потока с момента [`start`].
pub fn snapshot() -> (u64, u64) {
    LOCAL.with(|c| c.get())
}

/// Число лексем в строке тем же правилом, что и сканер (`@`/`,@` считаются
/// по отдельности) — для нормировки «аллокаций на токен» в bench-режимах.
pub fn tokens_of(src: &str) -> u64 {
    let mut n = 0u64;
    let b = src.as_bytes();
    let mut i = 0usize;
    while i < b.len() {
        match crate::simd::CLASS[b[i] as usize] {
            crate::simd::C_ATOM => {
                if b[i] == b'@' {
                    // скалярный лексер считает '@' отдельной лексемой
                    i += 1;
                } else {
                    i = crate::simd::atom_end(b, i).max(i + 1);
                }
                n += 1;
            }
            crate::simd::C_HI => match crate::simd::char_at(b, i) {
                Some(ch) if !ch.is_whitespace() => {
                    i = crate::simd::atom_end(b, i).max(i + 1);
                    n += 1;
                }
                Some(ch) => i += ch.len_utf8(),
                None => break,
            },
            crate::simd::C_WS => i += 1,
            _ => match b[i] {
                b')' | b']' => i += 1,
                b'"' => {
                    // строка: одна лексема целиком
                    n += 1;
                    i += 1;
                    while i < b.len() {
                        match b[i] {
                            b'\\' => i += 2,
                            b'"' => {
                                i += 1;
                                break;
                            }
                            _ => i += 1,
                        }
                    }
                }
                b';' => i = crate::simd::skip_ws(b, i),
                _ => {
                    i += 1;
                    n += 1;
                }
            },
        }
    }
    n
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_probe_counts_only_while_enabled() {
        // вне замера счётчик не двигается
        stop();
        let (a0, _) = snapshot();
        let v: Vec<u8> = Vec::with_capacity(4096);
        let (a1, _) = snapshot();
        assert_eq!(a0, a1, "без start() аллокации не считаются");
        drop(v);

        start();
        let mut keep = Vec::new();
        for _ in 0..3 {
            keep.push(Vec::<u8>::with_capacity(64));
        }
        stop();
        let (n, bytes) = snapshot();
        assert!(n >= 3, "три аллокации обязаны посчитаться: {}", n);
        assert!(bytes >= 192, "байты обязаны посчитаться: {}", bytes);
        drop(keep);
    }

    #[test]
    fn test_tokens_of_matches_lexers() {
        for src in ["(+ 1 2)", "`(a ,b ,@c)", "(f \"s t r\" ;ком\n x)"] {
            let n = tokens_of(src) as usize;
            assert_eq!(n, crate::interp::lex_only_scalar(src), "скаляр: {:?}", src);
            assert_eq!(n, crate::interp::lex_only_simd(src), "быстрый: {:?}", src);
        }
    }
}
