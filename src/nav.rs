// src/nav.rs — навигационная сетка и A* для игрового ИИ.
//
// Зачем натив, а не Lisp: поиск пути — это цикл по сотням клеток каждый кадр
// на каждого врага. В интерпретаторе это тысячи операций на кадр (мы это уже
// мерили: поэлементная интеграция 400 тел даёт ~1107× разницы с пакетным
// нативом). Поэтому решётка и A* живут в Rust, а сцена только спрашивает
// «дай путь из A в B» — как она спрашивает растр или физику.
//
// Философия та же, что у всей HAL-части: ноль зависимостей, никакой
// привязки к игре, полная детерминированность (порядок обхода и разрешение
// ничьих фиксированы — путь воспроизводится побитово вместе с прогоном).
//
// Модель: 4-связная сетка (без срезания углов), стоимость шага 1,
// эвристика — манхэттен, ничьи разрешаются меньшим индексом клетки.

/// Навигационная сетка: `blocked[y * w + x]` — проходимость клетки.
pub struct NavGrid {
    pub w: usize,
    pub h: usize,
    pub blocked: Vec<bool>,
    pub searches: u64,
    pub last_len: usize,
}

impl NavGrid {
    pub fn new(w: usize, h: usize) -> Self {
        let w = w.max(1);
        let h = h.max(1);
        NavGrid {
            w,
            h,
            blocked: vec![false; w * h],
            searches: 0,
            last_len: 0,
        }
    }

    pub fn idx(&self, x: usize, y: usize) -> usize {
        y * self.w + x
    }

    pub fn inside(&self, x: i64, y: i64) -> bool {
        x >= 0 && y >= 0 && (x as usize) < self.w && (y as usize) < self.h
    }

    pub fn free(&self, x: i64, y: i64) -> bool {
        self.inside(x, y) && !self.blocked[self.idx(x as usize, y as usize)]
    }

    pub fn set_blocked(&mut self, x: i64, y: i64, blocked: bool) -> Result<(), String> {
        if !self.inside(x, y) {
            return Err(format!(
                "nav: клетка ({}, {}) за пределами {}x{}",
                x, y, self.w, self.h
            ));
        }
        let i = self.idx(x as usize, y as usize);
        self.blocked[i] = blocked;
        Ok(())
    }

    /// Простейший возраст: клетка блокируется, если проходимость < 0.5.
    pub fn set_walkable(&mut self, x: i64, y: i64, walkable: bool) -> Result<(), String> {
        self.set_blocked(x, y, !walkable)
    }

    /// A* из (x0,y0) в (x1,y1). Возвращает плоский список клеток
    /// `[x0,y0, x1,y1, …]` включая обе конечные точки, либо None.
    ///
    /// Детерминированность: очередь — двоичная куча по (f, cell_index),
    /// поэтому при равных f порядок задаёт индекс клетки, а не порядок
    /// вставки. Один и тот же запрос даёт один и тот же путь.
    pub fn path(&mut self, x0: i64, y0: i64, x1: i64, y1: i64) -> Option<Vec<i32>> {
        let (w, h) = (self.w, self.h);
        if !self.free(x0, y0) || !self.free(x1, y1) {
            return None;
        }
        let n = w * h;
        let start = self.idx(x0 as usize, y0 as usize);
        let goal = self.idx(x1 as usize, y1 as usize);
        let mut g = vec![u32::MAX; n];
        let mut prev = vec![u32::MAX; n];
        let mut closed = vec![false; n];
        g[start] = 0;

        // двоичная куча (f, idx) — минимальная f, при равенстве меньший idx
        let mut heap: BinaryHeapMin = BinaryHeapMin::new();
        heap.push(0, start);

        let goal_xy = (goal % w, goal / w);
        while let Some((_f, cur)) = heap.pop() {
            if cur == goal {
                break;
            }
            if closed[cur] {
                continue;
            }
            closed[cur] = true;
            let cx = (cur % w) as i64;
            let cy = (cur / w) as i64;
            for (dx, dy) in [(-1i64, 0i64), (0, -1), (1, 0), (0, 1)] {
                let nx = cx + dx;
                let ny = cy + dy;
                if !self.free(nx, ny) {
                    continue;
                }
                let ni = self.idx(nx as usize, ny as usize);
                if closed[ni] {
                    continue;
                }
                let ng = g[cur].saturating_add(1);
                if ng < g[ni] {
                    g[ni] = ng;
                    prev[ni] = cur as u32;
                    let hx = (goal_xy.0 as i64 - nx).unsigned_abs() as u32;
                    let hy = (goal_xy.1 as i64 - ny).unsigned_abs() as u32;
                    heap.push(ng + hx + hy, ni);
                }
            }
        }

        self.searches += 1;
        if g[goal] == u32::MAX {
            self.last_len = 0;
            return None;
        }
        // обратный проход по prev
        let mut cells = Vec::new();
        let mut cur = goal;
        loop {
            cells.push(cur);
            if cur == start {
                break;
            }
            let p = prev[cur];
            if p == u32::MAX {
                self.last_len = 0;
                return None;
            }
            cur = p as usize;
            if cells.len() > n {
                self.last_len = 0;
                return None; // защита от битой цепочки
            }
        }
        cells.reverse();
        self.last_len = cells.len();
        let mut out = Vec::with_capacity(cells.len() * 2);
        for c in cells {
            out.push((c % w) as i32);
            out.push((c / w) as i32);
        }
        let _ = h;
        Some(out)
    }

    /// Сколько клеток вокруг проходимы (для отладки/визуализации).
    pub fn open_neighbors(&self, x: i64, y: i64) -> usize {
        [(-1i64, 0i64), (0, -1), (1, 0), (0, 1)]
            .iter()
            .filter(|(dx, dy)| self.free(x + dx, y + dy))
            .count()
    }
}

/// Минимальная двоичная куча без зависимостей: (f, idx).
struct BinaryHeapMin {
    items: Vec<(u32, usize)>,
}

impl BinaryHeapMin {
    fn new() -> Self {
        BinaryHeapMin { items: Vec::new() }
    }

    fn less(a: &(u32, usize), b: &(u32, usize)) -> bool {
        a.0 < b.0 || (a.0 == b.0 && a.1 < b.1)
    }

    fn push(&mut self, f: u32, idx: usize) {
        self.items.push((f, idx));
        let mut i = self.items.len() - 1;
        while i > 0 {
            let parent = (i - 1) / 2;
            if Self::less(&self.items[i], &self.items[parent]) {
                self.items.swap(i, parent);
                i = parent;
            } else {
                break;
            }
        }
    }

    fn pop(&mut self) -> Option<(u32, usize)> {
        if self.items.is_empty() {
            return None;
        }
        let top = self.items[0];
        let last = self.items.pop().unwrap();
        if !self.items.is_empty() {
            self.items[0] = last;
            let mut i = 0;
            loop {
                let l = 2 * i + 1;
                let r = 2 * i + 2;
                let mut best = i;
                if l < self.items.len() && Self::less(&self.items[l], &self.items[best]) {
                    best = l;
                }
                if r < self.items.len() && Self::less(&self.items[r], &self.items[best]) {
                    best = r;
                }
                if best == i {
                    break;
                }
                self.items.swap(i, best);
                i = best;
            }
        }
        Some(top)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grid(w: usize, h: usize) -> NavGrid {
        NavGrid::new(w, h)
    }

    #[test]
    fn пустая_сетка_даёт_манхэттенский_путь() {
        let mut g = grid(5, 5);
        let p = g.path(0, 0, 4, 4).expect("путь есть");
        // 9 клеток: 8 шагов + старт
        assert_eq!(p.len(), 18);
        assert_eq!(&p[..2], &[0, 0]);
        assert_eq!(&p[16..], &[4, 4]);
        assert_eq!(g.searches, 1);
        assert_eq!(g.last_len, 9);
        // длина пути = манхэттенское расстояние + 1
        let mut steps = 0;
        for w in p.chunks(2) {
            let _ = w;
            steps += 1;
        }
        assert_eq!(steps, 9);
    }

    #[test]
    fn путь_обходит_стену() {
        let mut g = grid(7, 5);
        for y in 0..4 {
            g.set_blocked(3, y, true).unwrap();
        }
        let p = g.path(0, 0, 6, 0).expect("обход снизу");
        // стена до y=3 включительно, проход только в y=4
        assert!(p.chunks(2).any(|c| c[1] == 4), "путь должен уйти на y=4");
        // длина: (0,0)→(0,4)=4, →(6,4)=6, →(6,0)=4 = 14 шагов + старт
        assert_eq!(p.len() / 2, 15);
    }

    #[test]
    fn недостижимая_цель_даёт_none() {
        let mut g = grid(5, 5);
        for x in 0..5 {
            g.set_blocked(x, 2, true).unwrap();
        }
        assert!(g.path(0, 0, 0, 4).is_none());
        assert_eq!(g.last_len, 0);
    }

    #[test]
    fn занятая_стартовая_или_конечная_клетка_даёт_none() {
        let mut g = grid(4, 4);
        g.set_blocked(0, 0, true).unwrap();
        assert!(g.path(0, 0, 3, 3).is_none());
        let mut g2 = grid(4, 4);
        g2.set_blocked(3, 3, true).unwrap();
        assert!(g2.path(0, 0, 3, 3).is_none());
    }

    #[test]
    fn путь_детерминирован_и_не_зависит_от_порядка_запросов() {
        let mut a = grid(12, 12);
        let mut b = grid(12, 12);
        for g in [&mut a, &mut b] {
            for y in 3..9 {
                g.set_blocked(6, y, true).unwrap();
            }
            g.set_blocked(4, 5, true).unwrap();
        }
        let p1 = a.path(0, 0, 11, 11).unwrap();
        let p2 = a.path(0, 0, 11, 11).unwrap();
        assert_eq!(p1, p2, "повторный запрос обязан дать тот же путь");
        let p3 = b.path(0, 0, 11, 11).unwrap();
        assert_eq!(p1, p3, "другая сетка с той же геометрией — тот же путь");
    }

    #[test]
    fn путь_соседних_клеток_и_сам_в_себя() {
        let mut g = grid(3, 3);
        assert_eq!(g.path(1, 1, 1, 2).unwrap(), vec![1, 1, 1, 2]);
        assert_eq!(g.path(2, 2, 2, 2).unwrap(), vec![2, 2]);
    }

    #[test]
    fn выход_за_границы_это_ошибка() {
        let mut g = grid(3, 3);
        assert!(g.set_blocked(3, 0, true).is_err());
        assert!(g.set_blocked(-1, 0, true).is_err());
        assert!(!g.free(3, 3));
        assert!(g.free(2, 2));
    }

    #[test]
    fn граничная_сетка_1x1() {
        let mut g = grid(1, 1);
        assert_eq!(g.path(0, 0, 0, 0).unwrap(), vec![0, 0]);
        g.set_blocked(0, 0, true).unwrap();
        assert!(g.path(0, 0, 0, 0).is_none());
    }

    #[test]
    fn лабиринт_коридором() {
        // змейка: путь существует ровно один и он длинный
        let mut g = grid(5, 5);
        for y in [1usize, 3] {
            for x in 0..4 {
                g.set_blocked(x as i64, y as i64, true).unwrap();
            }
        }
        let p = g.path(0, 0, 0, 4).expect("змейка проходима");
        assert!(p.len() / 2 > 9, "путь длиннее прямого: {}", p.len() / 2);
    }
}
