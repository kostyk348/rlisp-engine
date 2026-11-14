use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

use crate::gc;
use crate::interp::builtins::register_core;
use crate::interp::env::{env_new, CoreCtx, GlobalCell};
use crate::interp::eval::eval;
use crate::interp::parse::parse_program;
use crate::interp::value::{fmt_value, Value};

/// Изолированный контекст: тесты cargo исполняются в разных потоках,
/// thread_local heap у каждого свой — контексты не пересекаются.
fn ctx() -> (CoreCtx, GlobalCell) {
    let env0 = env_new(None);
    let cell: GlobalCell = Rc::new(RefCell::new(env0));
    let c = CoreCtx {
        global: cell.clone(),
        registry: Rc::new(RefCell::new(vec![])),
        root: PathBuf::from("."),
    };
    register_core(env0, &c);
    (c, cell)
}

/// Вычислить формы по одной (сборка — на границах форм, как в драйверах).
fn ev_res(cell: &GlobalCell, code: &str) -> Result<String, String> {
    let prog = parse_program(code)?;
    let _pin = gc::pin(prog.clone());
    let g = *cell.borrow();
    let mut last = Value::Nil;
    let pin = gc::pin(vec![last.clone()]);
    for e in &prog {
        last = eval(e, g, 0)?;
        gc::pin_replace(&pin, vec![last.clone()]);
        gc::maybe_collect(&[g], &[]);
    }
    drop(pin);
    Ok(fmt_value(&last))
}

fn ev(cell: &GlobalCell, code: &str) -> String {
    ev_res(cell, code).unwrap()
}

#[test]
fn cons_пара_печатается_и_читается() {
    let (_c, cell) = ctx();
    assert_eq!(ev(&cell, "(cons 1 2)"), "(1 . 2)");
    assert_eq!(ev(&cell, "(car (cons 1 2))"), "1");
    assert_eq!(ev(&cell, "(cdr (cons 1 2))"), "2");
    // читается точечная запись
    assert_eq!(ev(&cell, "(quote (a . b))"), "(a . b)");
    assert_eq!(ev(&cell, "(equal? (quote (1 . 2)) (cons 1 2))"), "#t");
    // хвост — Vec-список: печать стандартная, плоская
    assert_eq!(ev(&cell, "(cons 1 (list 2 3))"), "(1 2 3)");
    assert_eq!(ev(&cell, "(cons 1 (quote ()))"), "(1)");
    // car/cdr работают и по цепочке пар
    assert_eq!(ev(&cell, "(car (cdr (cons 1 (cons 2 3))))"), "2");
    assert_eq!(ev(&cell, "(cdr (cdr (cons 1 (cons 2 3))))"), "3");
}

#[test]
fn set_car_set_cdr_и_циклы_без_зависания() {
    let (_c, cell) = ctx();
    assert_eq!(
        ev(
            &cell,
            "(define p (cons 1 2)) (set-car! p 10) (set-cdr! p (list 20 30)) p"
        ),
        "(10 20 30)"
    );
    // циклическая печать не зависает и помечается «…»
    assert_eq!(ev(&cell, "(define x (cons 1 2)) (set-cdr! x x) x"), "(1 …)");
    assert_eq!(
        ev(
            &cell,
            "(define a (cons 1 2)) (define b (cons a a)) (set-car! a b) b"
        ),
        "((… . 2) … . 2)"
    );
    // set-car!/set-cdr! на не-паре — ошибка
    assert!(ev_res(&cell, "(set-car! (list 1 2) 3)")
        .unwrap_err()
        .contains("нужна пара"));
}

#[test]
fn предикаты_пар_и_списков() {
    let (_c, cell) = ctx();
    assert_eq!(
        ev(
            &cell,
            "(list (cons? (cons 1 2)) (cons? (list 1)) (pair? (cons 1 2)) \
             (pair? (list 1)) (pair? (list)) (null? (list)) (null? (cons 1 2)) \
             (list? (cons 1 (list 2 3))) (list? (cons 1 2)) (list? (list 1 2)))"
        ),
        "(#t #f #t #t #f #t #f #t #f #t)"
    );
}

#[test]
fn списочные_операции_работают_с_цепочками() {
    let (_c, cell) = ctx();
    assert_eq!(ev(&cell, "(length (cons 1 (cons 2 (list 3))))"), "3");
    assert_eq!(
        ev(
            &cell,
            "(equal? (list 1 2 3) (cons 1 (cons 2 (cons 3 (quote ())))))"
        ),
        "#t"
    );
    assert_eq!(
        ev(&cell, "(reverse (cons 1 (cons 2 (cons 3 (quote ())))))"),
        "(3 2 1)"
    );
    assert_eq!(
        ev(&cell, "(append (list 1) (cons 2 (list 3)) (list 4))"),
        "(1 2 3 4)"
    );
    // Scheme-семантика append: последний аргумент — произвольный хвост
    assert_eq!(ev(&cell, "(append (list 1 2) 3)"), "(1 2 . 3)");
    assert_eq!(ev(&cell, "(nth (cons 10 (cons 20 30)) 1)"), "20");
    assert_eq!(ev(&cell, "(apply + 1 2 (cons 3 (list 4)))"), "10");
    assert_eq!(
        ev(&cell, "(list->vector (cons 1 (list 2 3)))"),
        "#vec(1 2 3) ; 3"
    );
    assert_eq!(
        ev(
            &cell,
            "(f32vec->list (f32-from-list (cons 1.0 (list 2.0))))"
        ),
        "(1.0 2.0)"
    );
    // map/filter из core-уровня поверх цепочек (проверим здесь примитивами).
    // Этап 3: с фичей `jit` числовая лямбда компилируется и возвращает
    // Float — печатается «1.0 4.0 9.0»; без jit interp даёт Int «1 4 9».
    // Числовые значения совпадают, поэтому сверяем нормализованно.
    {
        let got = ev(&cell, "(define (sq x) (* x x)) (define (my-map f lst) (if (null? lst) (quote ()) (cons (f (car lst)) (my-map f (cdr lst))))) (my-map sq (cons 1 (cons 2 (cons 3 (quote ())))) )");
        let norm = got.replace(".0", "");
        assert!(norm == "(1 4 9)", "my-map sq: {}", got);
    }
    // varargs с точечной сигнатурой: reader строит Cons
    assert_eq!(
        ev(&cell, "(define (f a . rest) (list a rest)) (f 1 2 3)"),
        "(1 (2 3))"
    );
    // квазицитата проходит через пары
    assert_eq!(ev(&cell, "(define t 5) (quasiquote (0 . ,t))"), "(0 . 5)");
}

/// Критерий этапа 2: циклы «замыкание ↔ окружение» собираются.
/// 1000 итераций: строим пару замыканий, захватывающих друг друга через
/// окружение let, снимаем корень, собираем — живых объектов НЕ прибавляется
/// (assert по счётчику heap: live возвращается к базовой линии каждый круг).
#[test]
fn gc_собирает_циклы_замыканий_1000_циклов_без_роста() {
    let (_c, cell) = ctx();
    ev(&cell, "(define (mk) (let ((a #f) (b #f)) (set! a (lambda () b)) (set! b (lambda () a)) (list a b)))");
    ev(&cell, "(define tmp #f)");
    let g = *cell.borrow();
    gc::collect(&[g], &[]);
    let live0 = gc::live_count();
    let freed0 = gc::stats().freed;
    for _ in 0..1000 {
        ev(&cell, "(define tmp (mk))"); // цикл замыканий под корнем tmp
        ev(&cell, "(set! tmp #f)"); // корень снят
        let st = gc::collect(&[g], &[]); // стек eval пуст — сборка честная
                                         // КРИТЕРИЙ: рост кучи от циклов замыканий = 0 на каждой итерации
        assert_eq!(gc::live_count(), live0, "куча выросла: циклы не собираются");
        assert!(st.freed > freed0, "сборка ничего не освободила");
    }
    let st = gc::stats();
    assert!(
        st.freed - freed0 >= 1000 * 3,
        "ожидали ≥3000 освобождённых объектов (2 замыкания + окружение на цикл), освободили {}",
        st.freed - freed0
    );
    // и замыкания по-прежнему работают: счётчик жив
    assert_eq!(
        ev(
            &cell,
            "(define (mkc) (let ((n 0)) (lambda () (set! n (+ n 1)) n))) \
                   (define c (mkc)) (c) (c) (c) (c)"
        ),
        "4"
    );
}

/// Циклы пар тоже собираются: (set-cdr! x x) — 1 пара, корень снят.
#[test]
fn gc_собирает_циклы_пар() {
    let (_c, cell) = ctx();
    ev(&cell, "(define x #f)");
    let g = *cell.borrow();
    gc::collect(&[g], &[]);
    let live0 = gc::live_count();
    for i in 0..100 {
        ev(&cell, "(define x (cons 1 2)) (set-cdr! x x) (set! x #f)");
        gc::collect(&[g], &[]);
        assert_eq!(
            gc::live_count(),
            live0,
            "итерация {}: цикл пар не собран",
            i
        );
    }
}

/// Сборка откладывается во время вычисления: (gc) внутри формы не собирает,
/// а ставит флаг; следующая граница форм собирает.
#[test]
fn gc_откладывается_во_время_eval() {
    let (_c, cell) = ctx();
    let g = *cell.borrow();
    let before = gc::stats().collections;
    // внутри begin стек eval активен — (gc) только просит сборку
    let prog = parse_program("(begin (gc) 42)").unwrap();
    let v = eval(&prog[0], g, 0).unwrap();
    assert_eq!(fmt_value(&v), "42");
    assert_eq!(
        gc::stats().collections,
        before,
        "сборка прошла во время eval"
    );
    assert!(gc::stats().requested, "флаг запроса не выставлен");
    // граница форм (как в ev/драйверах) выполняет отложенную сборку
    gc::maybe_collect(&[g], &[]);
    assert!(
        gc::stats().collections > before,
        "отложенная сборка не выполнена"
    );
    // примитив gc-stats отдаёт счётчики
    assert_eq!(
        ev(&cell, "(table-get (gc-stats) (quote live) -1)"),
        (gc::live_count() as i64).to_string()
    );
}

/// Пары/замыкания внутри Rc-контейнеров (вектор/таблица) не собираются,
/// пока контейнер достижим: маркировка прозрачна для Rc.
#[test]
fn пары_внутри_контейнеров_выживают_сборку() {
    let (_c, cell) = ctx();
    ev(
        &cell,
        "(define v (make-vector 1 0)) (vset! v 0 (cons 41 (quote ()))) \
        (define t (make-table)) (table-set! t (quote p) (cons 7 8))",
    );
    let g = *cell.borrow();
    gc::collect(&[g], &[]);
    assert_eq!(ev(&cell, "(car (vref v 0))"), "41");
    assert_eq!(ev(&cell, "(cdr (table-get t (quote p) 0))"), "8");
    // замыкание в таблице вызывается после сборки
    assert_eq!(
        ev(&cell,
           "(define t2 (make-table)) (table-set! t2 (quote f) (lambda () 99))                 (table-count t2)"),
        "1"
    );
    gc::collect(&[g], &[]);
    // Этап 3: с `jit` числовое ядро возвращает Float («99.0»), без — Int («99»);
    // значение совпадает — сверяем нормализованно
    {
        let got = ev(&cell, "((table-get t2 (quote f) 0))");
        let norm = got.replace(".0", "");
        assert!(norm == "99", "lambda () 99: {}", got);
    }
}

#[test]
fn векторный_alu_и_страйд_рассылка() {
    let (_c, cell) = ctx();
    ev(&cell, "(define a (f32-from-list (list 1.0 2.0 4.0)))");
    ev(&cell, "(define b (f32-from-list (list 10.0 20.0 40.0)))");
    ev(&cell, "(define d (make-f32vec 3 0.0))");
    // vec+vec, vec×scalar, scalar−vec (первый операнд — число), min/max
    ev(&cell, "(f32vec-op! d '+ a b)");
    assert_eq!(ev(&cell, "(f32ref d 2)"), "44.0");
    ev(&cell, "(f32vec-op! d '* a 0.5)");
    assert_eq!(ev(&cell, "(f32ref d 1)"), "1.0");
    ev(&cell, "(f32vec-op! d '- 10.0 a)");
    assert_eq!(ev(&cell, "(f32ref d 0)"), "9.0");
    ev(&cell, "(f32vec-op! d 'min a b)");
    assert_eq!(ev(&cell, "(f32ref d 1)"), "2.0");
    ev(&cell, "(f32vec-op! d 'max a b)");
    assert_eq!(ev(&cell, "(f32ref d 0)"), "10.0");
    // унарные: sqrt
    ev(&cell, "(f32vec-op! d 'sqrt a)");
    assert_eq!(ev(&cell, "(f32ref d 2)"), "2.0");
    // in-place: dst совпадает с операндом (алиасинг)
    ev(&cell, "(f32vec-op! b '+ b b)");
    assert_eq!(ev(&cell, "(f32ref b 0)"), "20.0");
    // страйд-рассылка: канал Y в [x,y,z]×N
    ev(&cell, "(define pos (make-f32vec 9 0.0))");
    ev(&cell, "(define ys (f32-from-list (list 7.0 8.0 9.0)))");
    ev(&cell, "(f32vec-strided-set! pos 3 1 ys)");
    assert_eq!(ev(&cell, "(f32ref pos 1)"), "7.0");
    assert_eq!(ev(&cell, "(f32ref pos 4)"), "8.0");
    assert_eq!(ev(&cell, "(f32ref pos 7)"), "9.0");
    assert_eq!(ev(&cell, "(f32ref pos 0)"), "0.0");
    // ошибки: несовпадение длин, offset ≥ stride, неизвестная операция
    ev(&cell, "(define e (make-f32vec 4 1.0))");
    assert!(ev_res(&cell, "(f32vec-op! d '+ a e)").is_err());
    assert!(ev_res(&cell, "(f32vec-strided-set! pos 0 0 ys)").is_err());
    assert!(ev_res(&cell, "(f32vec-op! d 'pow a b)").is_err());
}

#[test]
fn atan2_четверти_и_детерминизм() {
    let (c, cell) = ctx();
    // четверть I: 45°
    let r = ev_res(&cell, "(atan2 1 1)").unwrap();
    assert!(r.contains("0.7853"), "atan2(1,1) ≈ π/4, получено {}", r);
    // четверть II: 135°
    let r = ev_res(&cell, "(atan2 1 -1)").unwrap();
    assert!(r.contains("2.356"), "atan2(1,-1) ≈ 3π/4, получено {}", r);
    // нулевой вектор → 0
    let r = ev_res(&cell, "(atan2 0 1)").unwrap();
    assert!(r.contains("0"), "atan2(0,1) = 0, получено {}", r);
    let _ = c;
}

#[test]
fn hash_int_детерминизм_диапазон_и_лавина() {
    let (_c, cell) = ctx();
    // детерминизм: одинаковые аргументы — одинаковый результат
    assert_eq!(ev(&cell, "(hash-int 7 3)"), ev(&cell, "(hash-int 7 3)"));
    // диапазон: целое [0, 2^32)
    let a = ev(&cell, "(hash-int 42)").parse::<f64>().unwrap();
    assert!((0.0..4294967295.0).contains(&a));
    // порядок аргументов и разные значения меняют хэш (лавина)
    assert_ne!(ev(&cell, "(hash-int 1 2)"), ev(&cell, "(hash-int 2 1)"));
    assert_ne!(ev(&cell, "(hash-int 5 9 1)"), ev(&cell, "(hash-int 5 9 2)"));
    // распределение по 64 корзинам, 1024 точки: без «кривой Лиссажу»
    // (у sin-хэшей половина корзин пустая, есть кластеры)
    assert!(ev_res(&cell,
        "(define (count-bucket b i n) (if (>= i 1024) n (count-bucket b (+ i 1) (+ n (if (= (mod (hash-int i 77) 64) b) 1 0)))))"
    ).is_ok());
    assert!(ev_res(&cell,
        "(define (walk-buckets b mn mx) (if (>= b 64) (list mn mx) (let ((c (count-bucket b 0 0))) (walk-buckets (+ b 1) (min mn c) (max mx c)))))"
    ).is_ok());
    let fill = ev(&cell, "(walk-buckets 0 99 0)");
    let nums: Vec<f64> = fill
        .trim_start_matches('(')
        .trim_end_matches(')')
        .split_whitespace()
        .map(|x| x.parse().unwrap())
        .collect();
    assert_eq!(nums.len(), 2, "формат: {}", fill);
    assert!(nums[0] >= 1.0, "пустые корзины: {}", fill);
    assert!(nums[1] <= 34.0, "кластеризация: {}", fill);
    // нецелый аргумент — честная ошибка
    assert!(ev_res(&cell, "(hash-int 1.5)").is_err());
}

// —— Этап 32: навигация и «игровые» нативы ————————————————————————

#[test]
fn навигация_доступна_из_лиспа() {
    let (_c, cell) = ctx();
    // путь по пустой сетке 5x5 из (0,0) в (2,0) — три клетки
    assert_eq!(
        ev_res(&cell, "(define g (nav-new 5 5)) (nav-path g 0 0 2 0)").unwrap(),
        "(0 0 1 0 2 0)"
    );
    // стена на (1,0) заставляет обойти: путь длиннее трёх клеток
    let p = ev_res(&cell, "(nav-block! g 1 0 #t) (length (nav-path g 0 0 2 0))").unwrap();
    assert_eq!(
        p, "10",
        "обход: (0,0),(0,1),(1,1),(2,1),(2,0) = 5 клеток = 10 чисел"
    );
    // недостижимая цель — #f, а не ошибка
    assert_eq!(
        ev_res(
            &cell,
            "(define h (nav-new 3 1)) (nav-block! h 1 0 #t) (nav-path h 0 0 2 0)"
        )
        .unwrap(),
        "#f"
    );
    // stats: ширина, высота, число стен, счётчик поисков
    assert_eq!(
        ev_res(&cell, "(table-get (nav-stats g) 'searches 0)").unwrap(),
        "2"
    );
    assert_eq!(
        ev_res(&cell, "(table-get (nav-stats h) 'blocked 0)").unwrap(),
        "1"
    );
    // вне сетки — честная ошибка, а не молчаливый no-op
    assert!(ev_res(&cell, "(nav-block! g 9 9 #t)").is_err());
    assert!(ev_res(&cell, "(nav-free? 999 0 0)").is_err());
}

#[test]
fn сортировка_устойчива_и_с_компаратором() {
    let (_c, cell) = ctx();
    assert_eq!(ev_res(&cell, "(sort (list 3 1 2))").unwrap(), "(1 2 3)");
    assert_eq!(
        ev_res(&cell, "(sort (list 3 1 2) (lambda (a b) (> a b)))").unwrap(),
        "(3 2 1)"
    );
    // строки — по естественному порядку
    assert_eq!(
        ev_res(&cell, "(sort (list \"b\" \"a\" \"c\"))").unwrap(),
        "(a b c)"
    );
    // смешанные числа: int и float сравнимы между собой
    assert_eq!(ev_res(&cell, "(sort (list 2.5 1 3))").unwrap(), "(1 2.5 3)");
    // пустой список и список из одного элемента — без падений
    assert_eq!(ev_res(&cell, "(sort (list))").unwrap(), "()");
    assert_eq!(ev_res(&cell, "(sort (list 7))").unwrap(), "(7)");
}

#[test]
fn интерполяция_и_ступеньки() {
    let (_c, cell) = ctx();
    assert_eq!(ev_res(&cell, "(lerp 0.0 10.0 0.25)").unwrap(), "2.5");
    assert_eq!(ev_res(&cell, "(sign -3)").unwrap(), "-1.0");
    assert_eq!(ev_res(&cell, "(sign 0)").unwrap(), "0.0");
    assert_eq!(ev_res(&cell, "(approach 0.0 10.0 3.0)").unwrap(), "3.0");
    // не перескакивает цель
    assert_eq!(ev_res(&cell, "(approach 8.0 10.0 3.0)").unwrap(), "10.0");
    // симметрия smoothstep и зажатие
    assert_eq!(ev_res(&cell, "(smoothstep 0.0 1.0 0.0)").unwrap(), "0.0");
    assert_eq!(ev_res(&cell, "(smoothstep 0.0 1.0 1.0)").unwrap(), "1.0");
    assert_eq!(ev_res(&cell, "(smoothstep 0.0 1.0 0.5)").unwrap(), "0.5");
    assert_eq!(ev_res(&cell, "(smoothstep 0.0 1.0 2.0)").unwrap(), "1.0");
}

#[test]
fn строки_split_join_substring_find() {
    let (_c, cell) = ctx();
    assert_eq!(
        ev_res(&cell, "(string-split \"a,b,c\" \",\")").unwrap(),
        "(a b c)"
    );
    assert_eq!(
        ev_res(&cell, "(string-join (list \"a\" \"b\") \"-\")").unwrap(),
        "a-b"
    );
    // пустой разделитель — посимвольно
    assert_eq!(
        ev_res(&cell, "(string-split \"abc\" \"\")").unwrap(),
        "(a b c)"
    );
    // substring считает символы, а не байты: кириллица не рвётся
    assert_eq!(
        ev_res(&cell, "(string-substring \"привет\" 1 3)").unwrap(),
        "ри"
    );
    assert_eq!(ev_res(&cell, "(string-substring \"abc\" 1)").unwrap(), "bc");
    // выход за границы зажимается, а не паникует
    assert_eq!(
        ev_res(&cell, "(string-substring \"abc\" 0 99)").unwrap(),
        "abc"
    );
    assert_eq!(
        ev_res(&cell, "(string-find \"привет\" \"вет\")").unwrap(),
        "3"
    );
    assert_eq!(ev_res(&cell, "(string-find \"abc\" \"zz\")").unwrap(), "-1");
    assert_eq!(ev_res(&cell, "(string-upcase \"abв\")").unwrap(), "ABВ");
    assert_eq!(
        ev_res(&cell, "(string-replace \"a-b-c\" \"-\" \"+\")").unwrap(),
        "a+b+c"
    );
    assert_eq!(ev_res(&cell, "(string-trim \"  x  \")").unwrap(), "x");
}
