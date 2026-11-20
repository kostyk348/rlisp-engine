//! Таблица символов: `Sym` — это `Rc<str>`, скопированный из интерна.
//!
//! Зачем: до этого каждый атом при разборе аллоцировал `String`. В реальном Lisp-коде
//! символы повторяются массово (`car`, `cdr`, `setq`, `define`, `vec-dot`, …), поэтому
//! интернирование убирает большую часть аллокаций фазы Value — именно она съедала
//! преимущество SIMD-скана (замер 2026-11-19, Windows: весь парсер 0.91× против
//! старого reader'а при критерии ≥3×; числа в PR).
//!
//! Почему `thread_local`, как и куча GC и кэш JIT: интерпретатор однопоточен
//! (`src/gc.rs`: «heap живёт в thread_local владельца Lisp»), синхронизация в горячем
//! пути не нужна, а `Value::Cons(Gc)` и так делает `Value` непереносимым между потоками.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

#[derive(Default)]
struct Table {
    /// Ключ — тот же `Rc<str>`, что отдаётся наружу: попадание в кэш не аллоцирует.
    map: HashMap<Rc<str>, ()>,
}

thread_local! {
    static TABLE: RefCell<Table> = RefCell::new(Table::default());
}

/// Интернировать символ: повторное обращение к той же строке не аллоцирует.
pub fn intern(s: &str) -> Rc<str> {
    TABLE.with(|t| {
        let mut t = t.borrow_mut();
        if let Some(existing) = t.map.get_key_value(s) {
            return Rc::clone(existing.0);
        }
        let rc: Rc<str> = Rc::from(s);
        t.map.insert(Rc::clone(&rc), ());
        rc
    })
}

/// Сколько уникальных символов в таблице этого потока (для диагностики бенча).
#[cfg(feature = "count-allocs")]
pub fn unique() -> usize {
    TABLE.with(|t| t.borrow().map.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_same_symbol_is_returned_without_a_new_allocation() {
        let a = intern("car");
        let b = intern("car");
        // Rc::ptr_eq — та же ячейка таблицы, а не вторая копия строки.
        assert!(Rc::ptr_eq(&a, &b), "интернирование возвращает тот же Rc");
        assert_eq!(a.as_ref(), "car");
    }

    #[test]
    fn different_symbols_are_different_cells() {
        let a = intern("car");
        let b = intern("cdr");
        assert!(!Rc::ptr_eq(&a, &b));
    }
}
