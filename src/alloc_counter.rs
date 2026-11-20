//! Счётчик аллокаций для критерия ТЗ «аллокаций на токен ≤ 0,5»
//! (`rlisp-engine-tz.md` §2.1: замер через allocator-счётчик или `--bench-parse-allocs`).
//!
//! Включается фичей `count-allocs`: без неё глобальный аллокатор — системный,
//! ни одной лишней инструкции в горячем пути (контракт «ноль зависимостей» не тронут:
//! это std-only обёртка, не crates.io-зависимость).

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicU64, Ordering};

/// Сколько аллокаций произошло с момента последнего `reset()`.
pub static ALLOCS: AtomicU64 = AtomicU64::new(0);

pub struct Counting;

impl Counting {
    pub fn reset() {
        ALLOCS.store(0, Ordering::Relaxed);
    }
    pub fn count() -> u64 {
        ALLOCS.load(Ordering::Relaxed)
    }
}

// realloc считается как аллокация (он же выделяет новый блок): иначе счётчик
// занижал бы горячий путь, где Vec растёт через realloc.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        ALLOCS.fetch_add(1, Ordering::Relaxed);
        System.alloc(layout)
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        System.dealloc(ptr, layout)
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        ALLOCS.fetch_add(1, Ordering::Relaxed);
        System.realloc(ptr, layout, new_size)
    }
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        ALLOCS.fetch_add(1, Ordering::Relaxed);
        System.alloc_zeroed(layout)
    }
}
