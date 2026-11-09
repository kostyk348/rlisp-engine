// src/gc.rs — Этап 2: tracing-GC (mark-sweep) для пар, замыканий и окружений.
//
// Зачем: до Этапа 2 всё владение было на Rc — цикл «замыкание ↔ окружение»
// (lambda ссылается на env, env держит lambda в vars) никогда не освобождался.
// Здесь heap — арена GC-объектов трёх видов:
//   Pair   — cons-ячейка (car/cdr, мутабельные через set-car!/set-cdr!);
//   Lambda — замыкание/макрос (params/rest/body/env);
//   Env    — окружение (vars + parent-цепочка).
// В Value такие объекты лежат хендлами (Gc = индекс слота + поколение).
// Поколение ловит «висячие» хендлы: обращение к освобождённому слоту —
// паника с внятным сообщением, а не тихая порча памяти.
//
// Семантика сборки (важно, документировано в docs/architecture.md):
//   * интерпретатор однопоточен; heap живёт в thread_local владельца Lisp;
//   * маркировка идёт от корней: глобальное окружение, явно переданные
//     значения (очередь ввода, удерживаемые результаты форм) и pin-наборы
//     (GcPin — для значений, пережидающих сборку в Rust-фреймах);
//   * сборка происходит ТОЛЬКО на границах форм, когда стек вычислителя
//     пуст: значения в локальных переменных рекурсивного eval() недоступны
//     обходу корней. maybe_collect() при активном eval() честно откладывает
//     сборку (ставит флаг), collect() во время eval() — паника (детектор бага);
//   * Rc-контейнеры (Vector/F32Vec/Table/Str, List) для маркировки прозрачны:
//     обход заходит внутрь их содержимого, поэтому пары/замыкания внутри них
//     не освобождаются, пока контейнер достижим. Сами контейнеры освобождаются
//     по Rc, как раньше (циклов через них нет — у Lambda нет Rc-полей).
//
// Ноль зависимостей, как и весь проект: только std.

use crate::interp::Value;
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};

// ——————————————————————————— хендл GC-объекта ———————————————————————————

/// Хендл объекта в GC-куче: индекс слота + поколение (защита от ABA).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Gc {
    pub idx: u32,
    pub gen: u32,
}

/// Замыкание/макрос (в GC-куче). После аллокации неизменяемо.
#[derive(Clone)]
pub struct LambdaData {
    pub params: Vec<String>,
    pub rest: Option<String>,
    pub body: Vec<Value>,
    pub env: Gc,
    pub is_macro: bool,
}

pub struct EnvData {
    pub vars: RefCell<HashMap<String, Value>>,
    pub parent: Option<Gc>,
}

enum GObj {
    Pair(RefCell<(Value, Value)>),
    Lambda(Box<LambdaData>),
    Env(EnvData),
}

struct Slot {
    gen: u32,
    marked: bool,
    obj: Option<GObj>,
}

/// Снимок статистики GC (для примитива (gc-stats) и тестов).
#[derive(Clone, Copy, Debug, Default)]
pub struct GcStats {
    pub collections: u64,
    pub allocs: u64,
    pub freed: u64,
    pub live: u64,
    pub peak_live: u64,
    pub threshold: usize,
    pub requested: bool,
}

struct Heap {
    slots: Vec<Slot>,
    free: Vec<u32>,
    allocs_total: u64,
    freed_total: u64,
    collections: u64,
    allocs_since_gc: usize,
    live_after_gc: usize,
    peak_live: usize,
    threshold: usize,
    requested: bool,
}

impl Heap {
    fn new() -> Self {
        Heap {
            slots: vec![],
            free: vec![],
            allocs_total: 0,
            freed_total: 0,
            collections: 0,
            allocs_since_gc: 0,
            live_after_gc: 0,
            peak_live: 0,
            threshold: GC_THRESHOLD_MIN,
            requested: false,
        }
    }

    /// Объект по хендлу с проверкой поколения. Висячая ссылка — паника:
    /// это всегда баг перечня корней, лучше упасть громко.
    fn obj(&self, g: Gc) -> &GObj {
        let s = self
            .slots
            .get(g.idx as usize)
            .unwrap_or_else(|| panic!("GC: плохой хендл {:?}", g));
        if s.gen != g.gen {
            panic!(
                "GC: ссылка на освобождённый объект (idx {}, gen {} != {}) — \
                 объект не защищён корнями при сборке",
                g.idx, g.gen, s.gen
            );
        }
        s.obj
            .as_ref()
            .unwrap_or_else(|| panic!("GC: пустой слот {}", g.idx))
    }

    fn alloc(&mut self, o: GObj) -> Gc {
        let (idx, gen) = match self.free.pop() {
            Some(i) => {
                let s = &mut self.slots[i as usize];
                s.obj = Some(o);
                s.marked = false;
                (i, s.gen)
            }
            None => {
                let idx = self.slots.len() as u32;
                self.slots.push(Slot {
                    gen: 0,
                    marked: false,
                    obj: Some(o),
                });
                (idx, 0)
            }
        };
        self.allocs_total += 1;
        self.allocs_since_gc += 1;
        Gc { idx, gen }
    }

    fn live_count(&self) -> usize {
        self.slots.iter().filter(|s| s.obj.is_some()).count()
    }

    fn stats(&self) -> GcStats {
        GcStats {
            collections: self.collections,
            allocs: self.allocs_total,
            freed: self.freed_total,
            live: self.live_count() as u64,
            peak_live: self.peak_live as u64,
            threshold: self.threshold,
            requested: self.requested,
        }
    }
}

thread_local! {
    static HEAP: RefCell<Heap> = RefCell::new(Heap::new());
    /// Глубина стека вычислителя: сборка разрешена только при 0.
    static EVAL_DEPTH: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
    /// Pin-наборы: значения, удерживаемые Rust-фреймами через границы форм
    /// (результаты REPL, перенос persistent при hot reload). RAII: GcPin::drop
    /// снимает набор. Содержимое участвует в маркировке каждой сборки.
    static PINS: RefCell<Vec<(u64, Vec<Value>)>> = const { RefCell::new(Vec::new()) };
}

/// Порог авто-сборки (аллокаций с прошлой сборки). После сборки порог
/// адаптируется: max(GC_THRESHOLD_MIN, 2 × живых объектов).
const GC_THRESHOLD_MIN: usize = 8192;

static PIN_SEQ: AtomicU64 = AtomicU64::new(1);

pub fn eval_depth() -> u32 {
    EVAL_DEPTH.with(|d| d.get())
}

pub struct EvalGuard;

impl EvalGuard {
    pub fn enter() -> Self {
        EVAL_DEPTH.with(|d| d.set(d.get() + 1));
        EvalGuard
    }
}

impl Drop for EvalGuard {
    fn drop(&mut self) {
        EVAL_DEPTH.with(|d| d.set(d.get().saturating_sub(1)));
    }
}

// ——————————————————————————— аллокация ———————————————————————————

pub fn alloc_pair(car: Value, cdr: Value) -> Gc {
    HEAP.with(|h| h.borrow_mut().alloc(GObj::Pair(RefCell::new((car, cdr)))))
}

pub fn alloc_lambda(l: LambdaData) -> Gc {
    HEAP.with(|h| h.borrow_mut().alloc(GObj::Lambda(Box::new(l))))
}

pub fn alloc_env(parent: Option<Gc>) -> Gc {
    HEAP.with(|h| {
        h.borrow_mut().alloc(GObj::Env(EnvData {
            vars: RefCell::new(HashMap::new()),
            parent,
        }))
    })
}

// ——————————————————————————— пары ———————————————————————————

pub fn pair_get(g: Gc) -> (Value, Value) {
    HEAP.with(|h| {
        let h = h.borrow();
        match h.obj(g) {
            GObj::Pair(cell) => {
                let b = cell.borrow();
                (b.0.clone(), b.1.clone())
            }
            _ => panic!("GC: объект {} — не пара", g.idx),
        }
    })
}

pub fn pair_car(g: Gc) -> Value {
    pair_get(g).0
}

pub fn pair_cdr(g: Gc) -> Value {
    pair_get(g).1
}

pub fn pair_set_car(g: Gc, v: Value) {
    HEAP.with(|h| {
        let h = h.borrow();
        match h.obj(g) {
            GObj::Pair(cell) => cell.borrow_mut().0 = v,
            _ => panic!("GC: объект {} — не пара", g.idx),
        }
    });
}

pub fn pair_set_cdr(g: Gc, v: Value) {
    HEAP.with(|h| {
        let h = h.borrow();
        match h.obj(g) {
            GObj::Pair(cell) => cell.borrow_mut().1 = v,
            _ => panic!("GC: объект {} — не пара", g.idx),
        }
    });
}

// ——————————————————————————— замыкания ———————————————————————————

pub fn lambda_clone(g: Gc) -> LambdaData {
    HEAP.with(|h| {
        let h = h.borrow();
        match h.obj(g) {
            GObj::Lambda(l) => (**l).clone(),
            _ => panic!("GC: объект {} — не замыкание", g.idx),
        }
    })
}

pub fn lambda_is_macro(g: Gc) -> bool {
    HEAP.with(|h| {
        let h = h.borrow();
        match h.obj(g) {
            GObj::Lambda(l) => l.is_macro,
            _ => false,
        }
    })
}

// ——————————————————————————— окружения ———————————————————————————

pub fn env_get(mut env: Gc, name: &str) -> Option<Value> {
    loop {
        let step = HEAP.with(|h| {
            let h = h.borrow();
            match h.obj(env) {
                GObj::Env(e) => (e.vars.borrow().get(name).cloned(), e.parent),
                _ => panic!("GC: объект {} — не окружение", env.idx),
            }
        });
        if let Some(v) = step.0 {
            return Some(v);
        }
        env = step.1?;
    }
}

pub fn env_define(env: Gc, name: &str, val: Value) {
    HEAP.with(|h| {
        let h = h.borrow();
        match h.obj(env) {
            GObj::Env(e) => e.vars.borrow_mut().insert(name.to_string(), val),
            _ => panic!("GC: объект {} — не окружение", env.idx),
        };
    });
}

pub fn env_set(mut env: Gc, name: &str, val: Value) -> Result<(), String> {
    loop {
        enum Step {
            Set,
            Parent(Option<Gc>),
        }
        let step = HEAP.with(|h| {
            let h = h.borrow();
            match h.obj(env) {
                GObj::Env(e) => {
                    let mut vars = e.vars.borrow_mut();
                    if vars.contains_key(name) {
                        vars.insert(name.to_string(), val.clone());
                        Step::Set
                    } else {
                        Step::Parent(e.parent)
                    }
                }
                _ => panic!("GC: объект {} — не окружение", env.idx),
            }
        });
        match step {
            Step::Set => return Ok(()),
            Step::Parent(Some(p)) => env = p,
            Step::Parent(None) => return Err(format!("set!: переменная не объявлена: {}", name)),
        }
    }
}

// ——————————————————————————— pin-наборы ———————————————————————————

/// Зафиксировать значения как корни до конца жизни Guard'а (RAII).
pub fn pin(values: Vec<Value>) -> GcPin {
    let id = PIN_SEQ.fetch_add(1, Ordering::Relaxed);
    PINS.with(|p| p.borrow_mut().push((id, values)));
    GcPin { id }
}

/// Обновить содержимое pin-набора (для «последний результат REPL»).
pub fn pin_replace(guard: &GcPin, values: Vec<Value>) {
    PINS.with(|p| {
        let mut p = p.borrow_mut();
        if let Some(slot) = p.iter_mut().find(|(id, _)| *id == guard.id) {
            slot.1 = values;
        }
    });
}

pub struct GcPin {
    id: u64,
}

impl Drop for GcPin {
    fn drop(&mut self) {
        PINS.with(|p| p.borrow_mut().retain(|(id, _)| *id != self.id));
    }
}

// ——————————————————————————— маркировка ———————————————————————————

const MARK_DEPTH_MAX: u32 = 3000; // как лимит рекурсии eval — патологий глубже не бывает

fn mark_value(v: &Value, work: &mut Vec<Gc>, seen_rc: &mut HashSet<usize>, depth: u32) {
    if depth > MARK_DEPTH_MAX {
        return; // патологическая глубина — защите служит seen_rc для циклов
    }
    let d = depth + 1;
    match v {
        Value::Cons(g) => work.push(*g),
        Value::Lambda(g) => work.push(*g),
        Value::List(items) => {
            for x in items {
                mark_value(x, work, seen_rc, d);
            }
        }
        // Rc-контейнеры прозрачны для маркировки: заходим внутрь, чтобы
        // пары/замыкания внутри живого контейнера не были собраны.
        // seen_rc по адресу контейнера защищает от циклов (таблица в себе).
        Value::Vector(r) => {
            if seen_rc.insert(std::rc::Rc::as_ptr(r) as *const () as usize) {
                for x in r.borrow().iter() {
                    mark_value(x, work, seen_rc, d);
                }
            }
        }
        Value::F32Vec(_) => {} // числа, ссылок нет
        Value::Table(t) => {
            if seen_rc.insert(std::rc::Rc::as_ptr(t) as *const () as usize) {
                let vals: Vec<Value> = t.borrow().values().cloned().collect();
                for x in &vals {
                    mark_value(x, work, seen_rc, d);
                }
            }
        }
        _ => {} // Nil/Bool/Int/Float/Sym/Str/Native/Ptr — листья
    }
}

// ——————————————————————————— сборка ———————————————————————————

/// Полная сборка. Вызывать только на границах форм (стек eval пуст):
/// во время вычисления — паника (детектор бага перечня корней).
pub fn collect(roots: &[Gc], extra: &[Value]) -> GcStats {
    assert_eq!(
        eval_depth(),
        0,
        "GC: сборка во время вычисления — вызывайте maybe_collect на границах форм"
    );
    HEAP.with(|h| {
        let mut h = h.borrow_mut();
        // — mark —
        let mut work: Vec<Gc> = roots.to_vec();
        let mut seen_rc: HashSet<usize> = HashSet::new();
        for v in extra {
            mark_value(v, &mut work, &mut seen_rc, 0);
        }
        PINS.with(|p| {
            let pins = p.borrow();
            for (_, vals) in pins.iter() {
                for v in vals {
                    mark_value(v, &mut work, &mut seen_rc, 0);
                }
            }
        });
        while let Some(g) = work.pop() {
            let slot = match h.slots.get_mut(g.idx as usize) {
                Some(s) if s.gen == g.gen && s.obj.is_some() && !s.marked => s,
                _ => continue,
            };
            slot.marked = true;
            match &slot.obj {
                Some(GObj::Pair(cell)) => {
                    let (a, d) = {
                        let b = cell.borrow();
                        (b.0.clone(), b.1.clone())
                    };
                    mark_value(&a, &mut work, &mut seen_rc, 0);
                    mark_value(&d, &mut work, &mut seen_rc, 0);
                }
                Some(GObj::Lambda(l)) => {
                    work.push(l.env);
                    for b in &l.body {
                        mark_value(b, &mut work, &mut seen_rc, 0);
                    }
                }
                Some(GObj::Env(e)) => {
                    let vals: Vec<Value> = e.vars.borrow().values().cloned().collect();
                    for v in &vals {
                        mark_value(v, &mut work, &mut seen_rc, 0);
                    }
                    if let Some(p) = e.parent {
                        work.push(p);
                    }
                }
                None => {}
            }
        }
        // — sweep —
        let mut freed = 0usize;
        let mut live = 0usize;
        let mut freed_idx: Vec<u32> = vec![];
        for (i, s) in h.slots.iter_mut().enumerate() {
            if s.obj.is_some() {
                if s.marked {
                    s.marked = false;
                    live += 1;
                } else {
                    s.obj = None;
                    s.gen = s.gen.wrapping_add(1);
                    freed_idx.push(i as u32);
                    freed += 1;
                }
            }
        }
        h.free.extend(freed_idx);
        h.collections += 1;
        h.freed_total += freed as u64;
        h.allocs_since_gc = 0;
        h.live_after_gc = live;
        h.peak_live = h.peak_live.max(live);
        h.threshold = (GC_THRESHOLD_MIN).max(live * 2);
        h.requested = false;
        h.stats()
    })
}

/// Запросить сборку у следующей границы форм.
pub fn request_gc() {
    HEAP.with(|h| h.borrow_mut().requested = true);
}

/// Авто-сборка на границе форм: по флагу (gc) или по порогу аллокаций.
/// Во время вычисления (eval_depth > 0) честно откладывает: ставит флаг.
pub fn maybe_collect(roots: &[Gc], extra: &[Value]) -> bool {
    let deferred = eval_depth() > 0;
    let need = HEAP.with(|h| {
        let mut h = h.borrow_mut();
        if deferred {
            h.requested = true;
            false
        } else {
            h.requested || h.allocs_since_gc >= h.threshold
        }
    });
    if need {
        collect(roots, extra);
        true
    } else {
        false
    }
}

pub fn stats() -> GcStats {
    HEAP.with(|h| h.borrow().stats())
}

#[cfg(test)]
pub fn live_count() -> usize {
    HEAP.with(|h| h.borrow().live_count())
}
