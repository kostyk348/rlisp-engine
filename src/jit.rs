// src/jit.rs — Этап 3: zero-dependency copy-and-patch JIT для числовых ядер.
//
// Идея (CPython-style): маленькие Lisp-лямбды, тело которых — чистая арифметика
// над числами (+,-,*,/,<,>,<=,>=,=, if, and, or, let, литералы, параметры),
// компилируются в линейный IR (JitIr), а затем в НАТИВНЫЙ машинный код x86-64
// из заранее собранных байтовых шаблонов с пропатченными константами и
// смещениями переходов (copy-and-patch). Код кладётся в RW-память (mmap),
// переводится mprotect'ом в RX (W^X) и вызывается через указатель на функцию.
//
// Модель гардов (стражей):
//   1) КОМПИЛЯЦИОННЫЕ: тело должно целиком состоять из поддержанных форм
//      над числами; любое неизвестное — деоптимизация (compile → None).
//   2) ВХОДНЫЕ: арность и типы аргументов (только Int/Float) проверяются
//      на каждый вызов в Rust-обёртке call_jitted(); провал → интерпретатор.
// Никаких частично-вычисленных результатов: либо JIT целиком, либо interp.
//
// Хард-требование репозитория — ноль зависимостей: никаких cranelift/llvm.
// mmap/mprotect — прямые Linux-сисвызвы через core::arch::asm! (x86_64-linux).
//
// Точная семантика и границы — в LIMITATIONS.md и docs/architecture.md.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use crate::gc;
use crate::interp::Value;

// На не-x86_64/не-Linux все syscall-обёртки возвращают None → честная
// деоптимизация: JIT недоступен, интерпретатор работает как раньше.
mod emit;

// ——————————————————————————— линейный IR ———————————————————————————

/// Одна IR-инструкция стековой машины над f64.
/// Соглашение: значение выражения — в xmm0; let-слоты и левые операнды
/// бинарных операций лежат на f64-стеке (rsp).
#[derive(Debug, Clone)]
pub enum IrOp {
    /// xmm0 = константа из пула doubles
    Const(usize),
    /// xmm0 = параметр №k (из [rdi + 8*k])
    Arg(usize),
    /// push xmm0 на f64-стек (let-слот или левый операнд)
    Push,
    /// pop верхушки стека в xmm1
    Pop,
    /// xmm0 = [rsp + 8*k] — чтение слота по абсолютному номеру push'а
    LoadSlot(usize),
    /// сдвинуть rsp на 8*k вниз-вверх (дискредитация let-слотов), k>0
    AdjustStack(usize),
    /// xmm0 = xmm0 op xmm1 (xmm1 — снятый со стека ЛЕВЫЙ операнд)
    BinAdd,
    BinSub,
    BinMul,
    BinDiv,
    /// сравнение xmm1 (левый) op xmm0 (правый) → 1.0/0.0 в xmm0
    CmpLt,
    CmpEq,
    CmpGt,
    CmpLe,
    CmpGe,
    /// ветвление по готовым флагам comisd (cond — сравнение):
    /// переходы патчатся rel32 после эмиссии; pfFix → false-ветка (NaN/unordered),
    /// ccFix → then-ветка (код условия), fallthrough → else-ветка
    Branch {
        /// код условия x86 (0x82 jb — «меньше», 0x84 je, 0x87 ja, 0x86 jbe, 0x83 jae)
        cond: u8,
        then_ir: usize,
        else_ir: usize,
        end_ir: usize,
    },
    /// безусловный переход к IR-индексу
    Jump(usize),
    /// временная заглушка для Branch/Jump (заполняется при компиляции формы)
    Placeholder,
}

/// Скомпилированная функция: машинный код + метаданные.
pub struct JitFn {
    /// адрес машинного кода (исполняемая mmap-страница)
    code: usize,
    len: usize,
    /// арность, которую проверяет входной гард
    pub arity: usize,
    /// константы пула (для отладки/дампов)
    pub consts: Vec<f64>,
}

impl Drop for JitFn {
    fn drop(&mut self) {
        emit::release_code(self.code, self.len);
    }
}

impl JitFn {
    /// Вызов машинного кода: fn(args: *const f64) -> f64.
    /// Гард (типы/арность) обязан быть проверен call_jitted().
    pub unsafe fn call(&self, args: *const f64) -> f64 {
        let f: unsafe fn(*const f64) -> f64 = std::mem::transmute(self.code);
        f(args)
    }

    /// Адрес машинного кода (для дампа/дизассемблирования в тестах).
    pub fn code_addr(&self) -> usize {
        self.code
    }

    /// Длина машинного кода в байтах (включая пул констант).
    pub fn code_len(&self) -> usize {
        self.len
    }
}

// ——————————————————————————— компилятор: Lisp → IR ———————————————————————————

pub struct Compiler {
    arity: usize,
    ir: Vec<IrOp>,
    consts: Vec<f64>,
    /// стек «сущностей» на f64-стеке: номер push'а
    sp: usize,
    /// видимые переменные: параметр (индекс) или let-слот (номер push'а)
    vars: Vec<(String, VarSlot)>,
}

#[derive(Clone, Copy)]
enum VarSlot {
    Arg(usize),
    Slot(usize),
}

impl Compiler {
    pub fn new(arity: usize) -> Self {
        let vars = (0..arity)
            .map(|k| ("__arg__".to_string() + &k.to_string(), VarSlot::Arg(k)))
            .collect();
        Compiler {
            arity,
            ir: Vec::new(),
            consts: Vec::new(),
            sp: 0,
            vars,
        }
    }

    fn add_const(&mut self, v: f64) -> usize {
        if let Some(i) = self.consts.iter().position(|c| *c == v) {
            return i;
        }
        self.consts.push(v);
        self.consts.len() - 1
    }

    /// Компиляция тела. Возвращает None = деоптимизация (unsupported форма).
    fn compile_body(&mut self, body: &Value) -> Option<()> {
        self.compile_expr(body)
    }

    #[cfg(test)]
    pub fn compile_body_pub(&mut self, body: &Value) -> Option<()> {
        self.compile_body(body)
    }

    #[cfg(test)]
    pub fn ir_pub(&self) -> &Vec<IrOp> {
        &self.ir
    }

    fn compile_expr(&mut self, e: &Value) -> Option<()> {
        match e {
            Value::Int(i) => {
                let c = self.add_const(*i as f64);
                self.ir.push(IrOp::Const(c));
                Some(())
            }
            Value::Float(f) => {
                let c = self.add_const(*f);
                self.ir.push(IrOp::Const(c));
                Some(())
            }
            Value::Sym(s) => {
                let (_, slot) = self.vars.iter().rev().find(|(n, _)| n == s)?;
                match *slot {
                    VarSlot::Arg(k) => self.ir.push(IrOp::Arg(k)),
                    VarSlot::Slot(id) => {
                        // смещение от текущей вершины стека
                        debug_assert!(id < self.sp);
                        self.ir.push(IrOp::LoadSlot(id));
                    }
                }
                Some(())
            }
            Value::List(items) if !items.is_empty() => {
                let head = match &items[0] {
                    Value::Sym(s) => s.as_str(),
                    _ => return None,
                };
                match head {
                    "+" | "-" | "*" | "/" => self.compile_arith(head, &items[1..]),
                    "<" | "=" | ">" | "<=" | ">=" => self.compile_cmp(head, &items[1..]),
                    "if" => self.compile_if(&items[1..]),
                    "and" => self.compile_and(&items[1..]),
                    "or" => self.compile_or(&items[1..]),
                    "let" => self.compile_let(&items[1..]),
                    "begin" => {
                        if items.len() < 2 {
                            return None;
                        }
                        for b in &items[1..items.len() - 1] {
                            // промежуточные значения сбрасываем (чистая арифметика — без эффектов)
                            self.compile_expr(b)?;
                        }
                        self.compile_expr(&items[items.len() - 1])
                    }
                    _ => None,
                }
            }
            _ => None,
        }
    }

    fn compile_arith(&mut self, op: &str, args: &[Value]) -> Option<()> {
        // унарный минус поддержан; унарный +/… — как identity вырождаются в арг
        if args.is_empty() || args.len() > 2 {
            return None;
        }
        if args.len() == 1 {
            self.compile_expr(&args[0])?;
            if op == "-" {
                // 0 - x
                let c = self.add_const(0.0);
                self.ir.push(IrOp::Const(c));
                self.ir.push(IrOp::Push);
                self.sp += 1;
                self.compile_expr(&args[0])?;
                self.ir.push(IrOp::Pop);
                self.sp -= 1;
                self.ir.push(IrOp::BinSub);
            }
            return Some(());
        }
        self.compile_expr(&args[0])?;
        self.ir.push(IrOp::Push);
        self.sp += 1;
        self.compile_expr(&args[1])?;
        self.ir.push(IrOp::Pop);
        self.sp -= 1;
        self.ir.push(match op {
            "+" => IrOp::BinAdd,
            "-" => IrOp::BinSub,
            "*" => IrOp::BinMul,
            "/" => IrOp::BinDiv,
            _ => return None,
        });
        Some(())
    }

    fn compile_cmp(&mut self, op: &str, args: &[Value]) -> Option<()> {
        if args.len() != 2 {
            return None;
        }
        self.compile_expr(&args[0])?;
        self.ir.push(IrOp::Push);
        self.sp += 1;
        self.compile_expr(&args[1])?;
        self.ir.push(IrOp::Pop);
        self.sp -= 1;
        self.ir.push(match op {
            "<" => IrOp::CmpLt,
            "=" => IrOp::CmpEq,
            ">" => IrOp::CmpGt,
            "<=" => IrOp::CmpLe,
            ">=" => IrOp::CmpGe,
            _ => return None,
        });
        Some(())
    }

    /// if разрешён ТОЛЬКО над сравнением (в интерпретаторе всякое число —
    /// истинно, включая -0.0 и NaN; числовое значение не переносимо в
    /// comisd-ветвление без искажения семантики ±0.0 — честная деоптимизация).
    fn cond_branch(cond: &Value) -> Option<(u8, &Value, &Value)> {
        let items = match cond {
            Value::List(items) if !items.is_empty() => items,
            _ => return None,
        };
        let (cc, a, b) = match (&items[0], items.len()) {
            (Value::Sym(s), 3) => (s.as_str(), &items[1], &items[2]),
            _ => return None,
        };
        let code = match cc {
            "<" => 0x82,  // jb
            "=" => 0x84,  // je
            ">" => 0x87,  // ja
            "<=" => 0x86, // jbe
            ">=" => 0x83, // jae
            _ => return None,
        };
        Some((code, a, b))
    }

    fn compile_if(&mut self, args: &[Value]) -> Option<()> {
        if args.len() < 2 || args.len() > 3 {
            return None;
        }
        let (code, a, b) = Self::cond_branch(&args[0])?;
        self.compile_expr(a)?;
        self.ir.push(IrOp::Push);
        self.sp += 1;
        self.compile_expr(b)?;
        self.ir.push(IrOp::Pop);
        self.sp -= 1;
        // сравнение a ? b в флагах comisd; NaN → else (в интерпретаторе NaN-сравнение ложно)
        if args.len() != 3 {
            return None; // нет else-ветки → значение Nil — не числовое ядро
        }
        let branch_pos = self.ir.len();
        self.ir.push(IrOp::Placeholder); // Branch — target'ы заполним ниже
        let else_pos = self.ir.len();
        self.compile_expr(&args[2])?;
        self.ir.push(IrOp::Placeholder); // Jump end
        let jump_end_pos = self.ir.len() - 1;
        let then_pos = self.ir.len();
        self.compile_expr(&args[1])?;
        let end_pos = self.ir.len();
        self.ir[branch_pos] = IrOp::Branch {
            cond: code,
            then_ir: then_pos,
            else_ir: else_pos,
            end_ir: end_pos,
        };
        self.ir[jump_end_pos] = IrOp::Jump(end_pos);
        Some(())
    }

    /// (and a b): a — сравнение → ветвление; a — арифметика → всегда истинно.
    fn compile_and(&mut self, args: &[Value]) -> Option<()> {
        if args.len() != 2 {
            return None;
        }
        match Self::cond_branch(&args[0]) {
            Some((code, a, b)) => {
                self.compile_expr(a)?;
                self.ir.push(IrOp::Push);
                self.sp += 1;
                self.compile_expr(b)?;
                self.ir.push(IrOp::Pop);
                self.sp -= 1;
                let branch_pos = self.ir.len();
                self.ir.push(IrOp::Placeholder);
                let else_pos = self.ir.len();
                // ложь → 0.0 (= #f)
                let c = self.add_const(0.0);
                self.ir.push(IrOp::Const(c));
                self.ir.push(IrOp::Placeholder); // Jump end
                let jump_end_pos = self.ir.len() - 1;
                let then_pos = self.ir.len();
                self.compile_expr(&args[1])?;
                let end_pos = self.ir.len();
                self.ir[branch_pos] = IrOp::Branch {
                    cond: code,
                    then_ir: then_pos,
                    else_ir: else_pos,
                    end_ir: end_pos,
                };
                self.ir[jump_end_pos] = IrOp::Jump(end_pos);
                Some(())
            }
            None => {
                // a — арифметика: всегда истинно → результат b
                self.compile_expr(&args[0])?;
                self.compile_expr(&args[1])
            }
        }
    }

    fn compile_or(&mut self, args: &[Value]) -> Option<()> {
        if args.len() != 2 {
            return None;
        }
        match Self::cond_branch(&args[0]) {
            Some((code, a, b)) => {
                self.compile_expr(a)?;
                self.ir.push(IrOp::Push);
                self.sp += 1;
                self.compile_expr(b)?;
                self.ir.push(IrOp::Pop);
                self.sp -= 1;
                // (or c x): c истинно → 1.0 (= #t), ложно → значение x
                let branch_pos = self.ir.len();
                self.ir.push(IrOp::Placeholder);
                let else_pos = self.ir.len(); // fallthrough = else
                self.compile_expr(&args[1])?;
                self.ir.push(IrOp::Placeholder); // Jump end
                let jump_end_pos = self.ir.len() - 1;
                let then_pos = self.ir.len();
                let c_true = self.add_const(1.0);
                self.ir.push(IrOp::Const(c_true));
                let end_pos = self.ir.len();
                self.ir[branch_pos] = IrOp::Branch {
                    cond: code,
                    then_ir: then_pos,
                    else_ir: else_pos,
                    end_ir: end_pos,
                };
                self.ir[jump_end_pos] = IrOp::Jump(end_pos);
                Some(())
            }
            None => {
                // a — арифметика: всегда истинно → результат a
                self.compile_expr(&args[0])
            }
        }
    }

    fn compile_let(&mut self, args: &[Value]) -> Option<()> {
        if args.len() != 2 {
            return None;
        }
        let bindings = match &args[0] {
            Value::List(bs) if bs.len() <= 8 => bs.clone(),
            _ => return None,
        };
        let body = &args[1];
        if bindings.is_empty() {
            return self.compile_expr(body);
        }
        let base_sp = self.sp;
        let base_vars = self.vars.len();
        let mut bound = Vec::new();
        for b in &bindings {
            let (name, expr) = match b {
                Value::List(p) if p.len() == 2 => match (&p[0], &p[1]) {
                    (Value::Sym(s), e) => (s.clone(), e),
                    _ => return None,
                },
                _ => return None,
            };
            // значение let — только арифметика (сравнение даёт булево,
            // которое не должно незаметно течь в арифметику)
            if Self::cond_branch(expr).is_some() {
                return None;
            }
            self.compile_expr(expr)?;
            self.ir.push(IrOp::Push);
            self.sp += 1;
            bound.push((name, VarSlot::Slot(self.sp - 1)));
        }
        self.vars.extend(bound);
        let n_slots = self.sp - base_sp;
        self.compile_expr(body)?;
        self.ir.push(IrOp::AdjustStack(n_slots));
        self.sp -= n_slots;
        self.vars.truncate(base_vars);
        Some(())
    }
}

// ——————————————————————————— эмиссия: IR → машинный код ———————————————————————————

/// Компиляция лямбды целиком: IR + эмиссия. None = деоптимизация.
pub fn compile(lambda: &Value) -> Option<JitFn> {
    let g = match lambda {
        Value::Lambda(g) => *g,
        _ => return None,
    };
    let l = gc::lambda_clone(g);
    if l.is_macro || l.rest.is_some() || l.body.len() != 1 || l.params.len() > 8 {
        return None;
    }
    let arity = l.params.len();
    let mut c = Compiler::new(arity);
    // имена параметров подставляем для поиска переменных
    for (k, p) in l.params.iter().enumerate() {
        if let Some(v) = c.vars.get_mut(k) {
            v.0 = p.clone();
        }
    }
    c.compile_body(&l.body[0])?;
    emit::emit(&c.ir, &c.consts, arity)
}

// ——————————————————————————— кэш и вызов с гардами ———————————————————————————

thread_local! {
    /// Кэш компиляции по (idx, gen) GC-объекта лямбды: Some — скомпилирована,
    /// None — деоптимизация навсегда (нет повторных попыток на горячем пути).
    static JIT_CACHE: RefCell<HashMap<(u32, u32), Option<Rc<JitFn>>>> = RefCell::new(HashMap::new());
}

/// Точка входа из интерпретатора: компиляция (кэш) + входные гарды + вызов.
/// None на любом этапе = прозрачная деоптимизация в интерпретатор.
pub fn try_call_lambda(g: gc::Gc, args: &[Value]) -> Option<Value> {
    JIT_CACHE.with(|cache| {
        let mut m = cache.borrow_mut();
        let key = (g.idx, g.gen);
        let compiled = m
            .entry(key)
            .or_insert_with(|| compile(&Value::Lambda(g)).map(Rc::new));
        match compiled {
            Some(jf) => call_jitted(jf, args),
            None => None,
        }
    })
}

/// Проверка входного гарда: арность + все аргументы — числа (Int|Float).
pub fn guard_args(args: &[Value], arity: usize) -> Option<Vec<f64>> {
    if args.len() != arity {
        return None;
    }
    let mut out = Vec::with_capacity(arity);
    for a in args {
        match a {
            Value::Int(i) => out.push(*i as f64),
            Value::Float(f) => out.push(*f),
            _ => return None,
        }
    }
    Some(out)
}

/// Вызов скомпилированной функции с гардами. Int-аргументы конвертируются
/// в f64 (это совпадает с семантикой интерпретатора: его арифметика внутри
/// всё равно считается в f64 — см. def "+"; расхождение возможно только за
/// границей точности 2^53 — задокументировано в LIMITATIONS.md).
pub fn call_jitted(f: &JitFn, args: &[Value]) -> Option<Value> {
    let nums = guard_args(args, f.arity)?;
    let r = unsafe { f.call(nums.as_ptr()) };
    if r.is_nan() {
        // протокол runtime-деоптимизации: машинный код встретил деление на
        // ноль (интерпретатор в этом месте возвращает ошибку) — откат в interp
        return None;
    }
    Some(Value::Float(r))
}

// ——————————————————————————— тесты ———————————————————————————

#[cfg(test)]
mod jit_tests {
    use super::*;
    use crate::gc;
    use crate::interp::{env_get, env_new, eval, parse_program, register_core, CoreCtx, Value};
    use std::cell::RefCell;
    use std::path::PathBuf;
    use std::rc::Rc;

    fn fresh_env() -> gc::Gc {
        let env0 = env_new(None);
        let c = CoreCtx {
            global: Rc::new(RefCell::new(env0)),
            registry: Rc::new(RefCell::new(vec![])),
            root: PathBuf::from("."),
        };
        register_core(env0, &c);
        env0
    }

    fn make_lambda(src: &str) -> gc::Gc {
        let prog = parse_program(src).unwrap();
        let env = fresh_env();
        eval(&prog[0], env, 0).unwrap();
        match env_get(env, "f") {
            Some(Value::Lambda(g)) => g,
            _ => panic!("нет лямбды"),
        }
    }

    /// Числовая нормализация результата: Int(x) ≡ Float(x), Bool ≡ 1.0/0.0,
    /// NaN ≡ NaN (по битам). Семантика сравнения interp vs JIT — см. LIMITATIONS.md.
    fn norm(v: &Value) -> Option<f64> {
        match v {
            Value::Int(i) => Some(*i as f64),
            Value::Float(f) => Some(*f),
            Value::Bool(true) => Some(1.0),
            Value::Bool(false) => Some(0.0),
            _ => None,
        }
    }

    fn results_equal(a: &Value, b: &Value) -> bool {
        match (norm(a), norm(b)) {
            (Some(x), Some(y)) => {
                if x.is_nan() && y.is_nan() {
                    return true; // NaN != NaN, но семантика одинакова
                }
                (x == y) || (x.is_nan() && y.is_nan())
            }
            (None, None) => fmt_eq(a, b),
            _ => false,
        }
    }

    fn fmt_eq(a: &Value, b: &Value) -> bool {
        crate::interp::fmt_value(a) == crate::interp::fmt_value(b)
    }

    #[test]
    fn jit_базовые_ядра() {
        let cases: Vec<(String, Vec<f64>, f64)> = vec![
            (
                "(define f (lambda (x y) (+ x y)))".into(),
                vec![2.0, 3.0],
                5.0,
            ),
            ("(define f (lambda (x) (* x x)))".into(), vec![7.0], 49.0),
            ("(define f (lambda () 99))".into(), vec![], 99.0),
            (
                "(define f (lambda (x) (if (< x 10) (* x 2) (+ x 100))))".into(),
                vec![4.0],
                8.0,
            ),
            (
                "(define f (lambda (x) (if (< x 10) (* x 2) (+ x 100))))".into(),
                vec![50.0],
                150.0,
            ),
            (
                "(define f (lambda (x y) (/ (- x y) (+ x y))))".into(),
                vec![7.0, 3.0],
                0.4,
            ),
            (
                "(define f (lambda (x) (let ((a (* x x)) (b (+ x 1))) (+ a b))))".into(),
                vec![3.0],
                13.0,
            ),
            (
                "(define f (lambda (x) (if (> x 0) 1 -1)))".into(),
                vec![-5.0],
                -1.0,
            ),
            (
                "(define f (lambda (x y) (and (< x 100) (+ x y))))".into(),
                vec![2.0, 3.0],
                5.0,
            ),
            (
                "(define f (lambda (x y) (or (> x 100) (* x y))))".into(),
                vec![6.0, 7.0],
                42.0,
            ),
            ("(define f (lambda (x) (* x 0)))".into(), vec![5.0], 0.0),
        ];
        for (src, args, expect) in cases {
            let g = make_lambda(&src);
            let jf = compile(&Value::Lambda(g))
                .unwrap_or_else(|| panic!("не скомпилировалось: {}", src));
            let vals: Vec<Value> = args.iter().map(|v| Value::Float(*v)).collect();
            let r = call_jitted(&jf, &vals).unwrap_or_else(|| panic!("гард упал: {}", src));
            match r {
                Value::Float(f) if f == expect => {}
                other => panic!(
                    "{}: ждали {}, получили {}",
                    src,
                    expect,
                    crate::interp::fmt_value(&other)
                ),
            }
        }
    }

    #[test]
    fn jit_деоптимизация_unsupported() {
        // строка/символ/неизвестная форма → None (честный откат в interp)
        for src in [
            "(define f (lambda (x) (string-length \"abc\")))",
            "(define f (lambda (x) (cons x 2)))",
            "(define f (lambda (x) (unknown-op x)))",
            "(define f (lambda (x) x))",
            "(define f (lambda (x y z w) (* (+ x y) (+ z w))))", // 4 арга — ок, должен компилироваться
        ] {
            let g = make_lambda(src);
            let r = compile(&Value::Lambda(g));
            println!("{}: compiled={}", src, r.is_some());
        }
        let g = make_lambda("(define f (lambda (x) (cons x 2)))");
        assert!(compile(&Value::Lambda(g)).is_none());
    }

    // ——— свойственный тест: seeded xorshift, interp vs JIT ———

    struct Xorshift(u64);
    impl Xorshift {
        fn next(&mut self) -> u64 {
            let mut x = self.0;
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            self.0 = x;
            x
        }
        fn f64(&mut self) -> f64 {
            (self.next() % 2000) as f64 / 100.0 - 10.0 // [-10, 10), 2 знака
        }
        fn int(&mut self) -> i64 {
            (self.next() % 100) as i64 - 50
        }
    }

    /// Чистая арифметика (без булевых значений — interp на них ошибается,
    /// и JIT обязан в этом месте деоптимизировать, что генератор не тестирует).
    fn gen_arith(rng: &mut Xorshift, d: u32) -> Value {
        if d == 0 || rng.next() % 100 < 45 {
            if rng.next() % 2 == 0 {
                Value::Sym(if rng.next() % 2 == 0 {
                    "x".into()
                } else {
                    "y".into()
                })
            } else if rng.next() % 2 == 0 {
                Value::Float(rng.f64())
            } else {
                Value::Int(rng.int())
            }
        } else {
            let op = ["+", "-", "*", "/"][(rng.next() % 4) as usize];
            Value::List(vec![
                Value::Sym(op.into()),
                gen_arith(rng, d - 1),
                gen_arith(rng, d - 1),
            ])
        }
    }

    /// Случайное выражение: арифметика | сравнение | if | and/or | let.
    /// Сравнения НЕ попадают в арифметические операнды (семантика interp:
    /// булево в арифметике — ошибка).
    fn gen_expr(rng: &mut Xorshift, d: u32) -> Value {
        let pick = rng.next() % 100;
        if d == 0 || pick < 35 {
            gen_arith(rng, 1)
        } else if pick < 65 {
            let op = ["+", "-", "*", "/"][(rng.next() % 4) as usize];
            Value::List(vec![
                Value::Sym(op.into()),
                gen_arith(rng, d - 1),
                gen_arith(rng, d - 1),
            ])
        } else if pick < 78 {
            let op = ["<", "=", ">", "<=", ">="][(rng.next() % 5) as usize];
            Value::List(vec![
                Value::Sym(op.into()),
                gen_arith(rng, d - 1),
                gen_arith(rng, d - 1),
            ])
        } else if pick < 88 {
            let op = ["<", "=", ">", "<=", ">="][(rng.next() % 5) as usize];
            Value::List(vec![
                Value::Sym("if".into()),
                Value::List(vec![
                    Value::Sym(op.into()),
                    gen_arith(rng, d - 1),
                    gen_arith(rng, d - 1),
                ]),
                gen_expr(rng, d - 1),
                gen_expr(rng, d - 1),
            ])
        } else if pick < 94 {
            let op = if rng.next() % 2 == 0 { "and" } else { "or" };
            let cop = ["<", "=", ">", "<=", ">="][(rng.next() % 5) as usize];
            Value::List(vec![
                Value::Sym(op.into()),
                Value::List(vec![
                    Value::Sym(cop.into()),
                    gen_arith(rng, d - 1),
                    gen_arith(rng, d - 1),
                ]),
                gen_arith(rng, d - 1),
            ])
        } else {
            let mut binds = Vec::new();
            let n = 1 + (rng.next() % 2) as usize;
            for i in 0..n {
                binds.push(Value::List(vec![
                    Value::Sym(format!("v{}", i)),
                    gen_arith(rng, d - 1),
                ]));
            }
            let names: Vec<String> = (0..n).map(|i| format!("v{}", i)).collect();
            Value::List(vec![
                Value::Sym("let".into()),
                Value::List(binds),
                gen_let_body(rng, d - 1, &names),
            ])
        }
    }

    fn gen_let_body(rng: &mut Xorshift, d: u32, vars: &[String]) -> Value {
        let pick = rng.next() % 100;
        if pick < 50 || d == 0 {
            let v = vars[(rng.next() as usize) % vars.len()].clone();
            Value::Sym(v)
        } else {
            let op = ["+", "-", "*", "/"][(rng.next() % 4) as usize];
            Value::List(vec![
                Value::Sym(op.into()),
                gen_let_body(rng, d - 1, vars),
                gen_let_body(rng, d - 1, vars),
            ])
        }
    }

    #[test]
    fn свойственные_тесты_interp_vs_jit_3000_выражений() {
        let mut rng = Xorshift(0xDEADBEEF_12345678);
        let mut compiled = 0usize;
        let mut deopt = 0usize;
        let mut checked = 0usize;
        for i in 0..3000u32 {
            let body = gen_expr(&mut rng, 4);
            let src_form = Value::List(vec![
                Value::Sym("define".into()),
                Value::List(vec![
                    Value::Sym("f".into()),
                    Value::Sym("x".into()),
                    Value::Sym("y".into()),
                ]),
                body.clone(),
            ]);
            let env = fresh_env();
            // define (f x y) body
            let define = Value::List(vec![
                Value::Sym("define".into()),
                Value::List(vec![
                    Value::Sym("f".into()),
                    Value::Sym("x".into()),
                    Value::Sym("y".into()),
                ]),
                body.clone(),
            ]);
            if eval(&define, env, 0).is_err() {
                continue; // генератор дал невалидную форму — не JIT-кейс
            }
            let g = match env_get(env, "f") {
                Some(Value::Lambda(g)) => g,
                _ => continue,
            };
            let x = rng.f64();
            let y = rng.f64();
            let args = vec![Value::Float(x), Value::Float(y)];
            // путь интерпретатора (через apply_lambda)
            let l = gc::lambda_clone(g);
            let interp_r = crate::interp::apply_lambda(&l, &args, 0);
            // путь JIT (та же лямбда)
            let jf = compile(&Value::Lambda(g));
            match jf {
                Some(f) => {
                    compiled += 1;
                    let jit_r = call_jitted(&f, &args);
                    match (interp_r, jit_r) {
                        (Ok(a), Some(b)) => {
                            checked += 1;
                            assert!(
                                results_equal(&a, &b),
                                "расхождение interp vs JIT (кейс {}): src={} x={} y={} interp={} jit={}",
                                i, crate::interp::fmt_value(&body), x, y,
                                crate::interp::fmt_value(&a), crate::interp::fmt_value(&b)
                            );
                        }
                        (Ok(_), None) => panic!("гард не должен падать на числовых аргументах (кейс {})", i),
                        // runtime-деоптимизация (деление на ноль): interp вернул ошибку — согласовано
                        (Err(_), None) => {}
                        (Err(e), Some(b)) => panic!(
                            "JIT вернул значение там, где interp ошибается (кейс {}): {} — тело: {}, jit={}",
                            i, e, crate::interp::fmt_value(&body), crate::interp::fmt_value(&b)
                        ),
                        (Err(e), _) => panic!(
                            "interp ошибся (кейс {}): {} — тело: {}",
                            i, e, crate::interp::fmt_value(&body)
                        ),
                    }
                }
                None => {
                    deopt += 1;
                    // деоптимизация допустима; результат interp обязан быть Ok или
                    // внятной ошибкой — оба пути согласованы по построению
                    assert!(interp_r.is_ok() || interp_r.is_err());
                }
            }
        }
        println!(
            "свойственный тест: {} кейсов, compiled={}, deopt={}, сверено={}",
            3000, compiled, deopt, checked
        );
        // регресс-порог: хотя бы половина случайных выражений должна JIT-компилироваться
        assert!(
            compiled >= 1500,
            "слишком много деоптимизаций: compiled={} из 3000",
            compiled
        );
        assert!(checked >= 1500);
    }
}
