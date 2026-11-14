// src/interp/rng.rs — единственный источник случайности в движке.
//
// Вынесено из builtins.rs по правилу §1 AI_AGENTS.md (файл близился к лимиту)
// и по существу: случайность — это то, что делает прогон воспроизводимым или
// нет, поэтому у неё отдельный, маленький и проверяемый модуль.
//
// Контракт: (random-seed n) фиксирует состояние; любой прогон с одинаковым
// seed, одинаковым вводом и фиксированным dt обязан дать тот же кадр
// (проверка — hash-цепочка в src/replay.rs).

use std::sync::atomic::{AtomicU64, Ordering};

static RND_STATE: AtomicU64 = AtomicU64::new(0);
/// Seed, с которым прогон начался: его пишем в файл записи, а не текущее
/// состояние ГПСЧ (иначе запись, начатая с середины, не воспроизводится).
static INITIAL_SEED: AtomicU64 = AtomicU64::new(0);

/// Чистый шаг ГПСЧ: состояние → следующее состояние. Никакого глобального
/// состояния, поэтому шаг проверяется тестом без гонок между потоками.
pub fn lcg_step(x: u64) -> u64 {
    x.wrapping_mul(6364136223846793005)
        .wrapping_add(1442695040888963407)
}

/// Состояние → число в [0,1) с 53 битами мантиссы.
pub fn to_unit(x: u64) -> f64 {
    (x >> 11) as f64 / (1u64 << 53) as f64
}

/// Линейный конгруэнтный шаг (как было с v0.1). Значение в [0,1).
pub fn next_random() -> f64 {
    let mut x = RND_STATE.load(Ordering::Relaxed);
    if x == 0 {
        x = time_seed();
    }
    x = lcg_step(x);
    RND_STATE.store(x, Ordering::Relaxed);
    to_unit(x)
}

fn time_seed() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0x9E37_79B9_7F4A_7C15)
        | 1
}

/// Зафиксировать seed ровно тем числом, что передали (0 — «взять из времени»).
pub fn set_seed(s: u64) {
    let s = if s == 0 { time_seed() } else { s };
    RND_STATE.store(s, Ordering::Relaxed);
    // первый установленный seed и есть seed прогона
    let _ = INITIAL_SEED.compare_exchange(0, s, Ordering::Relaxed, Ordering::Relaxed);
}

/// Текущее состояние ГПСЧ — то, что попадает в файл прогона.
pub fn seed() -> u64 {
    RND_STATE.load(Ordering::Relaxed)
}

/// Ненулевой seed: инициализировать при первом обращении и вернуть.
pub fn ensure_seed() -> u64 {
    let s = RND_STATE.load(Ordering::Relaxed);
    if s == 0 {
        let t = time_seed();
        RND_STATE.store(t, Ordering::Relaxed);
        let _ = INITIAL_SEED.compare_exchange(0, t, Ordering::Relaxed, Ordering::Relaxed);
        t
    } else {
        s
    }
}

/// Seed, с которого начался прогон (0 — ещё не инициализирован).
pub fn initial_seed() -> u64 {
    let init = INITIAL_SEED.load(Ordering::Relaxed);
    if init == 0 {
        ensure_seed();
        INITIAL_SEED.load(Ordering::Relaxed)
    } else {
        init
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Чистая цепочка из 16 значений от seed — воспроизводима побитово.
    fn chain(seed: u64, n: usize) -> Vec<f64> {
        let mut x = seed;
        (0..n)
            .map(|_| {
                x = lcg_step(x);
                to_unit(x)
            })
            .collect()
    }

    #[test]
    fn одинаковый_seed_даёт_одинаковую_последовательность() {
        // Глобальный RND_STATE общий на процесс, а cargo гоняет тесты
        // параллельно: сравнивать два прогона next_random() нельзя —
        // между ними может влезть другой тест. Проверяем чистый шаг.
        let a = chain(20261014, 16);
        let b = chain(20261014, 16);
        assert_eq!(a, b);
        assert!(a.iter().all(|v| (0.0..1.0).contains(v)));
        assert!(a.windows(2).all(|w| w[0] != w[1]), "значения не залипают");
    }

    #[test]
    fn разный_seed_даёт_разную_последовательность() {
        assert_ne!(chain(1, 8), chain(2, 8));
    }

    #[test]
    fn next_random_в_диапазоне_и_не_залипает() {
        set_seed(424242);
        let v: Vec<f64> = (0..64).map(|_| next_random()).collect();
        assert!(
            v.iter().all(|x| (0.0..1.0).contains(x)),
            "вне [0,1): {:?}",
            v
        );
        assert!(v.windows(2).any(|w| w[0] != w[1]), "ГПСЧ вырожден");
    }

    #[test]
    fn initial_seed_фиксируется_первым() {
        // INITIAL_SEED — на процесс (как и сам прогон), поэтому проверяем
        // инвариант: дальнейшие смены seed не двигают seed начала прогона
        set_seed(424242);
        let first = initial_seed();
        assert_ne!(first, 0);
        let _ = next_random();
        set_seed(777);
        assert_eq!(initial_seed(), first, "seed прогона не должен меняться");
        assert_eq!(seed(), 777, "а рабочий seed — меняется");
    }

    #[test]
    fn seed_не_искажается() {
        set_seed(20261014);
        assert_eq!(seed(), 20261014);
        set_seed(0);
        assert!(seed() != 0);
    }
}
