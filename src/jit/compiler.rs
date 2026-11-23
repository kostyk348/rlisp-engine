// src/jit/compiler.rs — компилятор: понижение AST Lisp в линейный IR и эмиссия машинного кода.

use super::emit;
use super::ir::IrOp;
use super::runtime::JitFn;
use crate::gc;
use crate::interp::Value;

pub struct Compiler {
    _arity: usize,
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
            _arity: arity,
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

    fn eval_pair_on_stack(&mut self, a: &Value, b: &Value) -> Option<()> {
        self.compile_expr(a)?;
        self.ir.push(IrOp::Push);
        self.sp += 1;
        self.compile_expr(b)?;
        self.ir.push(IrOp::Pop);
        self.sp -= 1;
        Some(())
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
                let (_, slot) = self.vars.iter().rev().find(|(n, _)| n.as_str() == &**s)?;
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
                    Value::Sym(s) => &**s,
                    _ => return None,
                };
                match head {
                    "+" | "-" | "*" | "/" => self.compile_arith(head, &items[1..]),
                    "<" | "=" | ">" | "<=" | ">=" => self.compile_cmp(head, &items[1..]),
                    "if" => self.compile_if(&items[1..]),
                    "and" => self.compile_and(&items[1..]),
                    "or" => self.compile_or(&items[1..]),
                    "let" => self.compile_let(&items[1..]),
                    "cond" => self.compile_cond(&items[1..]),
                    "let*" => self.compile_let_star(&items[1..]),
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
                    // этап 15: числовые нативы как формы — компилируются
                    // в машинный код (паритет значений с интерпретатором:
                    // floor даёт Int в interp, Float в jit — см. LIMITATIONS)
                    "min" | "max" => self.compile_minmax(head, &items[1..]),
                    "abs" | "sqrt" | "floor" => self.compile_un(head, &items[1..]),
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
        self.eval_pair_on_stack(&args[0], &args[1])?;
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
        self.eval_pair_on_stack(&args[0], &args[1])?;
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
            (Value::Sym(s), 3) => (&**s, &items[1], &items[2]),
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
        self.eval_pair_on_stack(a, b)?;
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
                self.eval_pair_on_stack(a, b)?;
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
                self.eval_pair_on_stack(a, b)?;
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
            bound.push((name.to_string(), VarSlot::Slot(self.sp - 1)));
        }
        self.vars.extend(bound);
        let n_slots = self.sp - base_sp;
        self.compile_expr(body)?;
        self.ir.push(IrOp::AdjustStack(n_slots));
        self.sp -= n_slots;
        self.vars.truncate(base_vars);
        Some(())
    }

    /// инверсия кода условия (для cond: истина → fallthrough на тело клауза)
    fn invert_cc(code: u8) -> u8 {
        match code {
            0x82 => 0x83, // jb → jae
            0x83 => 0x82, // jae → jb
            0x84 => 0x85, // je → jne
            0x85 => 0x84, // jne → je
            0x86 => 0x87, // jbe → ja
            0x87 => 0x86, // ja → jbe
            _ => code,
        }
    }

    /// тело клауза cond: все формы вычисляются, значение — последней
    /// (как у begin: промежуточные значения живут только в xmm0 — стек сбалансирован)
    fn compile_body_seq(&mut self, forms: &[Value]) -> Option<()> {
        if forms.is_empty() {
            return None;
        }
        for f in &forms[..forms.len() - 1] {
            self.compile_expr(f)?;
        }
        self.compile_expr(&forms[forms.len() - 1])
    }

    /// (cond (c e…) … (else eN)) — цепочка if. Тест клауза — ТОЛЬКО сравнение
    /// (как у if: прочие условия — честная деоптимизация); else — Sym «else»/«t».
    /// Макет клауза: [test][Branch: инвертированный cc и jp → следующая клауза,
    /// fallthrough → тело][тело][Jump end]. Без else-клауза — деоптимизация
    /// (cond может вернуть Nil — не числовое ядро).
    fn compile_cond(&mut self, clauses: &[Value]) -> Option<()> {
        if clauses.is_empty() {
            return None;
        }
        let mut pending: Vec<(usize, u8)> = Vec::new(); // (ir-позиция Branch, код)
        let mut starts: Vec<usize> = Vec::new(); // ir-позиция старта каждого клауза
        let last = clauses.len() - 1;
        for (k, cl) in clauses.iter().enumerate() {
            let items = match cl {
                Value::List(it) if it.len() >= 2 => it,
                _ => return None,
            };
            starts.push(self.ir.len());
            let is_else = match items.first() {
                Some(Value::Sym(s)) => (&**s == "else" || &**s == "t") && k == last,
                _ => false,
            };
            if !is_else {
                let (code, a, b) = Self::cond_branch(&items[0])?;
                self.eval_pair_on_stack(a, b)?;
                if k == last {
                    // cond без else может вернуть Nil → не числовое ядро
                    return None;
                }
                self.ir.push(IrOp::Placeholder); // Branch — цели после разбора
                pending.push((self.ir.len() - 1, code));
                self.compile_body_seq(&items[1..])?;
                self.ir.push(IrOp::Placeholder); // Jump → end
            } else {
                self.compile_body_seq(&items[1..])?;
            }
        }
        let end = self.ir.len();
        // цели Branch: следующая клауза (или else-тело) — см. starts
        for (k, (pos, code)) in pending.iter().enumerate() {
            let next = starts[k + 1];
            self.ir[*pos] = IrOp::Branch {
                cond: Self::invert_cc(*code),
                then_ir: next,
                else_ir: next,
                end_ir: end,
            };
        }
        // оставшиеся Placeholder — это Jump → общий end
        for op in self.ir.iter_mut() {
            if matches!(op, IrOp::Placeholder) {
                *op = IrOp::Jump(end);
            }
        }
        Some(())
    }

    /// (let* ((a x) (b (+ a 1))) body) — последовательные привязки: каждая
    /// видна следующим выражениям (отличие от параллельного let).
    fn compile_let_star(&mut self, args: &[Value]) -> Option<()> {
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
        let base_vars = self.vars.len();
        let mut n_slots = 0usize;
        for b in &bindings {
            let (name, expr) = match b {
                Value::List(p) if p.len() == 2 => match (&p[0], &p[1]) {
                    (Value::Sym(s), e) => (s.clone(), e),
                    _ => return None,
                },
                _ => return None,
            };
            // как у let: значение — только арифметика (булево не течёт в числа)
            if Self::cond_branch(expr).is_some() {
                return None;
            }
            self.compile_expr(expr)?;
            self.ir.push(IrOp::Push);
            self.sp += 1;
            n_slots += 1;
            self.vars
                .push((name.to_string(), VarSlot::Slot(self.sp - 1)));
        }
        self.compile_expr(body)?;
        self.ir.push(IrOp::AdjustStack(n_slots));
        self.sp -= n_slots;
        self.vars.truncate(base_vars);
        Some(())
    }

    /// (min a b …) / (max a b …): лево-ассоциативная свёртка по стеку
    fn compile_minmax(&mut self, op: &str, args: &[Value]) -> Option<()> {
        if args.is_empty() {
            return None;
        }
        self.compile_expr(&args[0])?;
        for a in &args[1..] {
            self.ir.push(IrOp::Push);
            self.sp += 1;
            self.compile_expr(a)?;
            self.ir.push(IrOp::Pop);
            self.sp -= 1;
            self.ir
                .push(if op == "min" { IrOp::Min } else { IrOp::Max });
        }
        Some(())
    }

    /// (abs x) / (sqrt x) / (floor x) — унарные числовые формы.
    /// Паритет значений с интерпретатором: floor в interp даёт Int,
    /// в jit — Float (числово равно; см. LIMITATIONS).
    fn compile_un(&mut self, op: &str, args: &[Value]) -> Option<()> {
        if args.len() != 1 {
            return None;
        }
        self.compile_expr(&args[0])?;
        self.ir.push(match op {
            "abs" => IrOp::UnAbs,
            "sqrt" => IrOp::UnSqrt,
            "floor" => IrOp::UnFloor,
            _ => return None,
        });
        Some(())
    }
}

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
